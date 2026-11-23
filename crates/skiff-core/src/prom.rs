//! Prometheus charts and firing alerts, reached through the API server's
//! service proxy.
//!
//! Every request goes to
//! `/api/v1/namespaces/{ns}/services/{scheme}:{svc}:{port}/proxy{prefix}/api/v1/...`
//! on the cluster connection Skiff already has, so there are no extra
//! credentials, no port-forward and no second network path. The price is
//! one RBAC rule (`get services/proxy`); without it the answer is a clean
//! [`PromError::Forbidden`] and the app stays on metrics-server.
//!
//! Nothing here touches [`crate::Session`]: the functions take a
//! `kube::Client`, so a session can own a [`Prom`] next to its client.
//!
//! Names (namespaces, pods, nodes) come from the cluster and are untrusted:
//! every value that ends up inside a PromQL selector goes through
//! [`escape_label_value`] or [`escape_regex`], and every URL part through
//! percent-encoding.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::Duration;

use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ops::{collect_capped, BodyError};
use crate::util::{classify, enc, enc_segment, get_request, ErrorKind};

const JSON: &str = "application/json";
/// A range query for a couple of hundred series is well under a megabyte;
/// anything near this size is a mistake (or a hostile endpoint).
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Series kept from one query. More than this is unreadable in a chart.
pub const MAX_SERIES: usize = 200;
/// Points kept per series (Prometheus itself refuses more than 11 000).
const MAX_POINTS: usize = 11_000;
/// Pods named in one workload selector. Queries travel in the URL of a GET
/// (POST through the proxy would need `create services/proxy`), so the
/// selector has to stay well below the server's URL limits.
pub const MAX_WORKLOAD_PODS: usize = 100;
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// How many detected candidates are probed before giving up.
const MAX_PROBES: usize = 4;

// ---------------------------------------------------------------------------
// Errors

/// Why Prometheus could not be used. The first three are the states the UI
/// tells apart: nothing installed, not allowed, installed but not answering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromError {
    /// No Service in the cluster looks like Prometheus.
    NotFound,
    /// The cluster refused (RBAC): listing Services or `get services/proxy`.
    Forbidden(String),
    /// A Prometheus Service exists but did not answer (no endpoints, wrong
    /// port, timeout, network).
    Unreachable(String),
    /// Prometheus answered and rejected the query.
    Query(String),
    /// The answer is larger than Skiff accepts.
    TooLarge,
    /// Anything else, including an answer that is not Prometheus' format.
    Other(String),
}

impl fmt::Display for PromError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromError::NotFound => f.write_str(
                "No Prometheus found in this cluster. Set clusters.<context>.prometheus in Skiff's config if it runs under an unusual name.",
            ),
            PromError::Forbidden(msg) => write!(
                f,
                "No access to Prometheus: {msg}. Your account needs \"get\" on services/proxy in its namespace."
            ),
            PromError::Unreachable(msg) => write!(f, "Prometheus is not answering: {msg}"),
            PromError::Query(msg) => write!(f, "Prometheus rejected the query: {msg}"),
            PromError::TooLarge => f.write_str("Prometheus returned too much data. Pick a shorter time range."),
            PromError::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for PromError {}

/// Shortens server-supplied text for an error message.
fn clip(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(300).collect();
    if s.chars().count() > 300 {
        out.push('…');
    }
    out
}

fn transport_error(e: &kube::Error) -> PromError {
    let info = classify(e);
    match info.kind {
        ErrorKind::Forbidden | ErrorKind::Unauthorized => PromError::Forbidden(info.summary),
        ErrorKind::Unreachable | ErrorKind::Timeout | ErrorKind::Tls => PromError::Unreachable(info.summary),
        _ => PromError::Other(info.to_string()),
    }
}

/// Sorts a non-success answer. The body is either Prometheus' own error
/// envelope (it did answer) or a Kubernetes `Status` from the proxy.
fn classify_response(status: u16, body: &[u8]) -> PromError {
    if let Ok(env) = serde_json::from_slice::<Envelope>(body) {
        if env.status == "error" {
            return PromError::Query(env.error_text());
        }
    }
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(clip))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| {
            let text = clip(&String::from_utf8_lossy(body));
            // An HTML error page is noise; the status says enough.
            if text.is_empty() || text.starts_with('<') {
                format!("HTTP {status}")
            } else {
                text
            }
        });
    match status {
        401 | 403 => PromError::Forbidden(message),
        // 404: no such Service (or a wrong path prefix); 502/503/504: the
        // proxy found no endpoint or the pod did not answer.
        404 | 502 | 503 | 504 => PromError::Unreachable(message),
        _ => PromError::Other(format!("{message} ({status})")),
    }
}

// ---------------------------------------------------------------------------
// Endpoint

/// Where Prometheus lives: a Service and how to talk to it. This is also
/// the shape of `clusters.<context>.prometheus` in the config file; every
/// field but `namespace` and `service` has a default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromEndpoint {
    pub namespace: String,
    pub service: String,
    pub port: u16,
    /// `http` or `https`: what the pod speaks, not how Skiff reaches the API server.
    pub scheme: String,
    /// Path in front of `/api/v1`, e.g. `/select/0/prometheus` for
    /// VictoriaMetrics' vmselect or a Prometheus started with `--web.route-prefix`.
    pub path_prefix: String,
}

impl Default for PromEndpoint {
    fn default() -> Self {
        PromEndpoint {
            namespace: String::new(),
            service: String::new(),
            port: 9090,
            scheme: "http".into(),
            path_prefix: String::new(),
        }
    }
}

impl PromEndpoint {
    pub fn new(namespace: &str, service: &str, port: u16) -> Self {
        PromEndpoint {
            namespace: namespace.into(),
            service: service.into(),
            port,
            ..Default::default()
        }
    }

    /// Checks a hand-written endpoint (the config override) and says what
    /// to fix. Detected endpoints are valid by construction.
    pub fn validate(&self) -> Result<(), String> {
        if !is_dns_name(&self.namespace) {
            return Err(format!(
                "prometheus.namespace {:?} is not a namespace name",
                self.namespace
            ));
        }
        if !is_dns_name(&self.service) {
            return Err(format!("prometheus.service {:?} is not a Service name", self.service));
        }
        if self.port == 0 {
            return Err("prometheus.port must be between 1 and 65535".into());
        }
        if self.scheme != "http" && self.scheme != "https" {
            return Err(format!(
                "prometheus.scheme must be \"http\" or \"https\", not {:?}",
                self.scheme
            ));
        }
        Ok(())
    }

    /// `namespace/service:port`, for messages.
    pub fn display(&self) -> String {
        format!("{}/{}:{}", self.namespace, self.service, self.port)
    }

    /// The service-proxy path up to and including the prefix, without a
    /// trailing slash.
    fn base_path(&self) -> String {
        // A ':' inside a name would shift the scheme:name:port triple.
        let seg = |s: &str| enc_segment(s).replace(':', "%3A");
        let scheme = if self.scheme.eq_ignore_ascii_case("https") {
            "https"
        } else {
            "http"
        };
        let mut path = format!(
            "/api/v1/namespaces/{}/services/{scheme}:{}:{}/proxy",
            seg(&self.namespace),
            seg(&self.service),
            self.port
        );
        // The prefix is normalised segment by segment: no empty or dot
        // segments, so it cannot climb out of the proxy path.
        for part in self.path_prefix.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                continue;
            }
            path.push('/');
            path.push_str(&enc_segment(part));
        }
        path
    }

    /// Request path for one Prometheus HTTP API call, e.g.
    /// `api_path("query_range", &[("query", "up"), ("step", "30")])`.
    /// Parameter values are percent-encoded here.
    pub fn api_path(&self, api: &str, params: &[(&str, &str)]) -> String {
        let mut path = self.base_path();
        path.push_str("/api/v1/");
        path.push_str(api.trim_matches('/'));
        for (i, (k, v)) in params.iter().enumerate() {
            path.push(if i == 0 { '?' } else { '&' });
            path.push_str(&enc(k));
            path.push('=');
            path.push_str(&enc(v));
        }
        path
    }
}

/// RFC 1123 subdomain, loosely: what a namespace or Service name can be.
fn is_dns_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !s.starts_with(['-', '.'])
        && !s.ends_with(['-', '.'])
}

// ---------------------------------------------------------------------------
// Detection

/// Namespaces tried one by one when the account may not list Services
/// cluster-wide.
const WELL_KNOWN_NAMESPACES: &[&str] = &[
    "monitoring",
    "prometheus",
    "observability",
    "kube-prometheus-stack",
    "victoria-metrics",
    "kube-system",
    "default",
];

/// Services that carry Prometheus-looking names or labels but are not the
/// query endpoint.
const NOT_PROMETHEUS: &[&str] = &[
    "alertmanager",
    "exporter",
    "pushgateway",
    "operator",
    "adapter",
    "kube-state-metrics",
    "blackbox",
    "thanos-discovery",
];

fn label<'a>(svc: &'a Value, key: &str) -> &'a str {
    svc["metadata"]["labels"][key].as_str().unwrap_or("")
}

/// Rank (lower wins), the port expected for that flavour and the path
/// prefix; `None` when the Service does not look like Prometheus.
fn flavour(svc: &Value) -> Option<(u8, u16, &'static str)> {
    let name = svc["metadata"]["name"].as_str().unwrap_or("");
    let app_name = label(svc, "app.kubernetes.io/name");
    let component = match label(svc, "app.kubernetes.io/component") {
        "" => label(svc, "component"),
        c => c,
    };
    let app = label(svc, "app");
    let excluded = NOT_PROMETHEUS.iter().any(|w| name.contains(w));

    // 1. prometheus-operator / kube-prometheus-stack: the governing Service.
    if label(svc, "operated-prometheus") == "true" {
        return Some((0, 9090, ""));
    }
    // 2. The community `prometheus` chart's server.
    if (app_name == "prometheus" || app == "prometheus") && component == "server" && !excluded {
        return Some((1, 80, ""));
    }
    // 3. VictoriaMetrics, operator and chart labels.
    if app_name == "vmsingle" || (app_name == "victoria-metrics-single" && app == "server") {
        return Some((2, 8428, ""));
    }
    if app_name == "vmselect" || (app_name == "victoria-metrics-cluster" && app == "vmselect") {
        return Some((3, 8481, "/select/0/prometheus"));
    }
    if excluded {
        return None;
    }
    // 4. Well-known names.
    if matches!(
        name,
        "prometheus-server" | "prometheus-k8s" | "prometheus" | "prometheus-operated"
    ) {
        return Some((4, 9090, ""));
    }
    // 5. Looser: release-prefixed names and a bare name label.
    if name.ends_with("-prometheus-server") || name.ends_with("-prometheus") || app_name == "prometheus" {
        return Some((5, 9090, ""));
    }
    None
}

/// Chooses the web port of a Prometheus Service from its `spec.ports`:
/// by conventional name first, then by conventional number, then the only
/// port there is. Returns the port and the scheme it implies.
fn pick_port(ports: &[Value], expected: u16) -> Option<(u16, &'static str)> {
    let tcp: Vec<(&str, u16)> = ports
        .iter()
        .filter(|p| matches!(p["protocol"].as_str(), None | Some("TCP")))
        .filter_map(|p| {
            let port = u16::try_from(p["port"].as_u64()?).ok().filter(|n| *n != 0)?;
            Some((p["name"].as_str().unwrap_or(""), port))
        })
        .collect();
    let by_name = |name: &str| tcp.iter().find(|(n, _)| *n == name).map(|(_, p)| *p);
    let by_number = |port: u16| tcp.iter().find(|(_, p)| *p == port).map(|(_, p)| *p);

    for name in ["web", "http-web", "http"] {
        if let Some(p) = by_name(name) {
            return Some((p, "http"));
        }
    }
    for number in [expected, 9090, 80, 8428, 8481] {
        if let Some(p) = by_number(number) {
            return Some((p, "http"));
        }
    }
    for name in ["https", "https-web"] {
        if let Some(p) = by_name(name) {
            return Some((p, "https"));
        }
    }
    match tcp.as_slice() {
        [(name, port)] => Some((
            *port,
            if name.starts_with("https") || *port == 443 {
                "https"
            } else {
                "http"
            },
        )),
        // Several ports and none of them recognisable: do not guess.
        _ => None,
    }
}

/// Orders the Services that look like Prometheus, best first. `services`
/// are Service objects as the API returns them (`items` of a list).
pub fn candidates(services: &[Value]) -> Vec<PromEndpoint> {
    let mut found: Vec<(u8, PromEndpoint)> = Vec::new();
    for svc in services {
        let (Some(ns), Some(name)) = (svc["metadata"]["namespace"].as_str(), svc["metadata"]["name"].as_str()) else {
            continue;
        };
        // An ExternalName Service has nothing to proxy to.
        if svc["spec"]["type"].as_str() == Some("ExternalName") || !is_dns_name(ns) || !is_dns_name(name) {
            continue;
        }
        let Some((rank, expected, prefix)) = flavour(svc) else {
            continue;
        };
        let ports = svc["spec"]["ports"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let Some((port, scheme)) = pick_port(ports, expected) else {
            continue;
        };
        found.push((
            rank,
            PromEndpoint {
                namespace: ns.into(),
                service: name.into(),
                port,
                scheme: scheme.into(),
                path_prefix: prefix.into(),
            },
        ));
    }
    // Deterministic whatever order the API server listed them in.
    found.sort_by(|a, b| (a.0, &a.1.namespace, &a.1.service).cmp(&(b.0, &b.1.namespace, &b.1.service)));
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().map(|(_, ep)| ep).collect()
}

async fn get(client: &Client, path: &str, limit: usize, timeout: Duration) -> Result<Vec<u8>, PromError> {
    let req = get_request(path, Some(JSON)).map_err(PromError::Other)?;
    let exchange = async {
        let res = client
            .send(req.map(kube::client::Body::from))
            .await
            .map_err(|e| transport_error(&e))?;
        let status = res.status();
        let body = collect_capped(res.into_body(), limit).await.map_err(|e| match e {
            BodyError::TooLarge { .. } => PromError::TooLarge,
            BodyError::Read(e) => transport_error(&e),
        })?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(classify_response(status.as_u16(), &body))
        }
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_) => Err(PromError::Unreachable(format!(
            "no answer within {} s",
            timeout.as_secs()
        ))),
    }
}

/// Lists Services (cluster-wide for `ns == None`), following `continue`.
async fn list_services(client: &Client, ns: Option<&str>) -> Result<Vec<Value>, PromError> {
    let base = match ns {
        Some(ns) => format!("/api/v1/namespaces/{}/services", enc_segment(ns)),
        None => "/api/v1/services".to_string(),
    };
    let mut items = Vec::new();
    let mut token = String::new();
    // 40 pages of 500 is far beyond any real cluster; the bound only stops
    // a server that keeps handing out continue tokens.
    for _ in 0..40 {
        let mut path = format!("{base}?limit=500");
        if !token.is_empty() {
            path.push_str("&continue=");
            path.push_str(&enc(&token));
        }
        let body = get(client, &path, crate::ops::MAX_BODY_BYTES, LIST_TIMEOUT).await?;
        let mut page: Value =
            serde_json::from_slice(&body).map_err(|e| PromError::Other(format!("unexpected Service list: {e}")))?;
        if let Some(Value::Array(page_items)) = page.get_mut("items").map(Value::take) {
            items.extend(page_items);
        }
        token = page["metadata"]["continue"].as_str().unwrap_or("").to_string();
        if token.is_empty() {
            break;
        }
    }
    Ok(items)
}

/// Finds Prometheus in the cluster and confirms that it answers.
///
/// `extra_namespaces` are tried (after the well-known ones) when the
/// account may not list Services cluster-wide; pass the namespaces the user
/// works in. Errors: [`PromError::NotFound`] when nothing looks like
/// Prometheus, [`PromError::Forbidden`] when RBAC stopped the search or the
/// proxy call, [`PromError::Unreachable`] when a Service was found but did
/// not answer.
///
/// Call once per session and cache the outcome; a config override should
/// skip this and go to [`Prom::ping`].
pub async fn detect(client: &Client, extra_namespaces: &[&str]) -> Result<Prom, PromError> {
    let services = match list_services(client, None).await {
        Ok(items) => items,
        Err(PromError::Forbidden(cluster_wide)) => {
            let mut items = Vec::new();
            let mut any_allowed = false;
            let mut tried: Vec<&str> = Vec::new();
            for ns in WELL_KNOWN_NAMESPACES.iter().chain(extra_namespaces) {
                if tried.contains(ns) || !is_dns_name(ns) {
                    continue;
                }
                tried.push(ns);
                // Forbidden here, a missing namespace or a hiccup: the
                // next namespace may still work.
                if let Ok(found) = list_services(client, Some(ns)).await {
                    any_allowed = true;
                    items.extend(found);
                }
            }
            if !any_allowed {
                return Err(PromError::Forbidden(cluster_wide));
            }
            items
        }
        Err(e) => return Err(e),
    };

    let mut forbidden = None;
    let mut unreachable = None;
    for endpoint in candidates(&services).into_iter().take(MAX_PROBES) {
        let prom = Prom::new(client.clone(), endpoint);
        match prom.ping().await {
            Ok(()) => return Ok(prom),
            Err(PromError::Forbidden(msg)) => {
                forbidden.get_or_insert(msg);
            }
            Err(e) => {
                let reason = match e {
                    PromError::Unreachable(msg) | PromError::Query(msg) | PromError::Other(msg) => msg,
                    other => other.to_string(),
                };
                unreachable.get_or_insert(format!("{} ({reason})", prom.endpoint.display()));
            }
        }
    }
    // "Not allowed" is the more useful thing to say when both happened: it
    // is the one the user can get fixed.
    Err(match (forbidden, unreachable) {
        (Some(msg), _) => PromError::Forbidden(msg),
        (None, Some(msg)) => PromError::Unreachable(msg),
        (None, None) => PromError::NotFound,
    })
}

// ---------------------------------------------------------------------------
// Client

/// A Prometheus endpoint on one cluster connection. Cheap to clone.
#[derive(Clone)]
pub struct Prom {
    client: Client,
    endpoint: PromEndpoint,
}

impl Prom {
    pub fn new(client: Client, endpoint: PromEndpoint) -> Self {
        Prom { client, endpoint }
    }

    pub fn endpoint(&self) -> &PromEndpoint {
        &self.endpoint
    }

    /// Confirms that the endpoint speaks the Prometheus API, with a query
    /// that costs the server nothing.
    pub async fn ping(&self) -> Result<(), PromError> {
        let path = self.endpoint.api_path("query", &[("query", "vector(1)")]);
        let body = get(&self.client, &path, MAX_BODY_BYTES, PROBE_TIMEOUT).await?;
        parse_query_response(&body).map(|_| ())
    }

    /// Evaluates `expr` over a time range.
    pub async fn query_range(&self, expr: &str, range: &TimeRange) -> Result<QueryResult, PromError> {
        let (start, end, step) = (range.start.to_string(), range.end.to_string(), range.step.to_string());
        let path = self.endpoint.api_path(
            "query_range",
            &[("query", expr), ("start", &start), ("end", &end), ("step", &step)],
        );
        let body = get(&self.client, &path, MAX_BODY_BYTES, QUERY_TIMEOUT).await?;
        parse_query_response(&body)
    }

    /// Evaluates `expr` at one moment (`None`: now, by the server's clock).
    /// Each series has at most one point.
    pub async fn query(&self, expr: &str, at_unix: Option<i64>) -> Result<QueryResult, PromError> {
        let at = at_unix.map(|t| t.to_string());
        let mut params = vec![("query", expr)];
        if let Some(at) = &at {
            params.push(("time", at));
        }
        let body = get(
            &self.client,
            &self.endpoint.api_path("query", &params),
            MAX_BODY_BYTES,
            QUERY_TIMEOUT,
        )
        .await?;
        parse_query_response(&body)
    }

    /// The alerts that are firing right now, most severe first.
    pub async fn firing_alerts(&self) -> Result<Vec<Alert>, PromError> {
        let body = get(
            &self.client,
            &self.endpoint.api_path("alerts", &[]),
            MAX_BODY_BYTES,
            QUERY_TIMEOUT,
        )
        .await?;
        parse_alerts(&body)
    }

    /// Runs every query of a panel (concurrently) and names the series.
    /// A failing reference line (requests and limits need
    /// kube-state-metrics) is left out rather than failing the chart.
    pub async fn panel(&self, panel: &Panel, range: &TimeRange) -> Result<PanelData, PromError> {
        let results = futures::future::join_all(panel.queries.iter().map(|q| self.query_range(&q.expr, range))).await;
        let mut data = PanelData {
            series: Vec::new(),
            truncated: false,
        };
        for (query, result) in panel.queries.iter().zip(results) {
            let result = match result {
                Ok(r) => r,
                Err(_) if query.reference => continue,
                Err(e) => return Err(e),
            };
            data.truncated |= result.truncated;
            for series in result.series {
                data.series.push(PlotSeries {
                    name: query.legend.name(&series),
                    reference: query.reference,
                    points: series.points,
                });
            }
        }
        Ok(data)
    }
}

// ---------------------------------------------------------------------------
// Responses

/// One time series: its labels and `(unix seconds, value)` points in time order.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub labels: BTreeMap<String, String>,
    pub points: Vec<(f64, f64)>,
}

impl Series {
    pub fn label(&self, name: &str) -> &str {
        self.labels.get(name).map(String::as_str).unwrap_or("")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult {
    pub series: Vec<Series>,
    /// The server sent more than [`MAX_SERIES`] series (or more points than
    /// a chart can use); the rest was dropped.
    pub truncated: bool,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    status: String,
    #[serde(default, rename = "errorType")]
    error_type: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    data: Value,
}

impl Envelope {
    fn error_text(&self) -> String {
        match (self.error.trim(), self.error_type.trim()) {
            ("", "") => "unknown error".into(),
            ("", kind) => kind.to_string(),
            (msg, "") => clip(msg),
            (msg, kind) => format!("{} ({kind})", clip(msg)),
        }
    }
}

#[derive(Deserialize)]
struct RawSeries {
    #[serde(default)]
    metric: BTreeMap<String, String>,
    /// Matrix: `[[ts, "value"], ...]`.
    #[serde(default)]
    values: Vec<(f64, String)>,
    /// Vector: `[ts, "value"]`.
    #[serde(default)]
    value: Option<(f64, String)>,
}

/// A sample value as Prometheus writes it: a decimal string, or `NaN`,
/// `+Inf`, `-Inf`. Only finite values are returned: a chart shows a gap
/// either way, and no caller has to guard its min/max against NaN.
fn parse_sample(ts: f64, value: &str) -> Option<(f64, f64)> {
    let v: f64 = value.trim().parse().ok()?;
    (v.is_finite() && ts.is_finite()).then_some((ts, v))
}

fn envelope(body: &[u8]) -> Result<Envelope, PromError> {
    let env: Envelope = serde_json::from_slice(body)
        .map_err(|_| PromError::Other("The service did not answer like Prometheus (unexpected response)".into()))?;
    match env.status.as_str() {
        "success" => Ok(env),
        "error" => Err(PromError::Query(env.error_text())),
        _ => Err(PromError::Other(
            "The service did not answer like Prometheus (no status)".into(),
        )),
    }
}

/// Parses the body of `/api/v1/query` or `/api/v1/query_range`.
pub fn parse_query_response(body: &[u8]) -> Result<QueryResult, PromError> {
    let mut env = envelope(body)?;
    let bad = |e: serde_json::Error| PromError::Other(format!("unexpected Prometheus response: {e}"));
    let result = env.data.get_mut("result").map(Value::take).unwrap_or(Value::Null);
    let mut out = QueryResult {
        series: Vec::new(),
        truncated: false,
    };
    match env.data["resultType"].as_str().unwrap_or("") {
        "matrix" | "vector" => {
            let raw: Vec<RawSeries> = serde_json::from_value(result).map_err(bad)?;
            out.truncated = raw.len() > MAX_SERIES;
            for s in raw.into_iter().take(MAX_SERIES) {
                out.truncated |= s.values.len() > MAX_POINTS;
                let points = s
                    .values
                    .iter()
                    .take(MAX_POINTS)
                    .chain(s.value.iter())
                    .filter_map(|(ts, v)| parse_sample(*ts, v))
                    .collect();
                out.series.push(Series {
                    labels: s.metric,
                    points,
                });
            }
        }
        "scalar" | "string" => {
            let (ts, v): (f64, String) = serde_json::from_value(result).map_err(bad)?;
            out.series.push(Series {
                labels: BTreeMap::new(),
                points: parse_sample(ts, &v).into_iter().collect(),
            });
        }
        other => return Err(PromError::Other(format!("unexpected Prometheus result type {other:?}"))),
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Time range

/// Start, end (unix seconds) and resolution of a range query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub start: i64,
    pub end: i64,
    /// Seconds between points.
    pub step: u32,
}

/// Steps a chart may use: round numbers, so points land on round times.
const NICE_STEPS: &[u32] = &[
    15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14_400, 21_600, 43_200, 86_400,
];

impl TimeRange {
    /// The last `span_secs` seconds up to `now_unix`, at about 120 points.
    pub fn last(span_secs: u64, now_unix: i64) -> Self {
        Self::with_points(span_secs, now_unix, 120)
    }

    /// The last `span_secs` seconds at roughly `target_points` points. The
    /// step is rounded up to a round number (never below 15 s, a common
    /// scrape interval) and the end is aligned to it, so that refreshing a
    /// chart moves it by whole steps instead of re-sampling every series.
    pub fn with_points(span_secs: u64, now_unix: i64, target_points: u32) -> Self {
        // A minute to 90 days; outside that a chart is meaningless.
        let span = span_secs.clamp(60, 90 * 86_400);
        let points = u64::from(target_points.clamp(10, 1000));
        let raw = span.div_ceil(points);
        let step = NICE_STEPS
            .iter()
            .copied()
            .find(|s| u64::from(*s) >= raw)
            // Beyond a day per point: whole days. (Bounded by the clamps
            // above, so this fits easily.)
            .unwrap_or_else(|| (raw.div_ceil(86_400) * 86_400) as u32);
        let now = now_unix.max(0);
        let end = now - now % i64::from(step);
        TimeRange {
            start: end - span as i64,
            end,
            step,
        }
    }

    /// The window for `rate()`: four steps, and at least two minutes so
    /// that it spans several scrapes whatever the scrape interval is.
    pub fn rate_window(&self) -> String {
        format!("{}s", (self.step.saturating_mul(4)).max(120))
    }

    /// The window for `increase()` in counts-per-interval charts: one
    /// step (so neighbouring points do not count the same event twice),
    /// and at least five minutes.
    pub fn increase_window(&self) -> String {
        format!("{}s", self.step.max(300))
    }
}

// ---------------------------------------------------------------------------
// PromQL building

/// Escapes a value for a double-quoted PromQL string literal
/// (`label="<here>"`). Backslash, quote and every control character are
/// escaped, so the value cannot end the literal or the selector.
pub fn escape_label_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Escapes a literal for use inside a PromQL regex matcher (RE2), before
/// string escaping: `a.b` matches only `a.b`.
pub fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `label="value"`.
fn eq(label: &str, value: &str) -> String {
    format!("{label}=\"{}\"", escape_label_value(value))
}

/// `label=~"a|b|c"` matching exactly the given names. Prometheus anchors
/// regex matchers at both ends, and each name is regex-escaped.
fn one_of(label: &str, names: &[&str]) -> String {
    let alternatives: Vec<String> = names.iter().map(|n| escape_regex(n)).collect();
    format!("{label}=~\"{}\"", escape_label_value(&alternatives.join("|")))
}

/// What the values of a chart are, for axis and hover formatting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// CPU cores (1.0 = one core).
    Cores,
    Bytes,
    BytesPerSec,
    Count,
}

/// How the series of a query are named in the legend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Legend {
    /// By the value of this label (`container`, `pod`).
    Label(&'static str),
    /// One fixed name: the query yields a single series.
    Name(&'static str),
}

impl Legend {
    pub fn name(&self, series: &Series) -> String {
        match self {
            Legend::Label(l) => series.label(l).to_string(),
            Legend::Name(n) => (*n).to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelQuery {
    pub expr: String,
    pub legend: Legend,
    /// A reference line (request, limit, allocatable) rather than usage:
    /// drawn thin or dashed, and optional.
    pub reference: bool,
}

/// One chart: a title, a unit and the queries that fill it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panel {
    pub title: &'static str,
    pub unit: Unit,
    pub queries: Vec<PanelQuery>,
}

/// The series of one panel, named and ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelData {
    pub series: Vec<PlotSeries>,
    pub truncated: bool,
}

impl PanelData {
    /// No usage data at all (reference lines do not count): the signal to
    /// try the next fallback, e.g. [`NodeBy::Instance`].
    pub fn is_empty(&self) -> bool {
        self.series.iter().all(|s| s.reference || s.points.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotSeries {
    pub name: String,
    pub reference: bool,
    pub points: Vec<(f64, f64)>,
}

fn usage(expr: String, legend: Legend) -> PanelQuery {
    PanelQuery {
        expr,
        legend,
        reference: false,
    }
}

fn reference(expr: String, name: &'static str) -> PanelQuery {
    PanelQuery {
        expr,
        legend: Legend::Name(name),
        reference: true,
    }
}

/// The shared shape of pod and workload charts. `sel` selects the pods
/// (already escaped) and `by` is the label usage is split by.
fn pod_like_panels(sel: &str, by: &'static str, range: &TimeRange) -> Vec<Panel> {
    let w = range.rate_window();
    let iw = range.increase_window();
    // `container!=""` drops the pod-level cgroup (which would double the
    // sum), `POD` the pause container on older runtimes.
    let containers = format!("{sel},container!=\"\",container!=\"POD\"");
    // What one pod requests and may use: the lines of a workload chart are
    // single pods, so its reference is the (largest) pod's, not the sum.
    let resource = |metric: &str, res: &str| match by {
        "pod" => format!("max(sum by(pod)({metric}{{{sel},resource=\"{res}\"}}))"),
        _ => format!("sum({metric}{{{sel},resource=\"{res}\"}})"),
    };
    vec![
        Panel {
            title: "CPU",
            unit: Unit::Cores,
            queries: vec![
                usage(
                    format!("sum by({by})(rate(container_cpu_usage_seconds_total{{{containers}}}[{w}]))"),
                    Legend::Label(by),
                ),
                reference(resource("kube_pod_container_resource_requests", "cpu"), "request"),
                reference(resource("kube_pod_container_resource_limits", "cpu"), "limit"),
            ],
        },
        Panel {
            title: "Memory",
            unit: Unit::Bytes,
            queries: vec![
                usage(
                    format!("sum by({by})(container_memory_working_set_bytes{{{containers}}})"),
                    Legend::Label(by),
                ),
                reference(resource("kube_pod_container_resource_requests", "memory"), "request"),
                reference(resource("kube_pod_container_resource_limits", "memory"), "limit"),
            ],
        },
        Panel {
            title: "Network",
            unit: Unit::BytesPerSec,
            queries: vec![
                usage(
                    format!("sum(rate(container_network_receive_bytes_total{{{sel}}}[{w}]))"),
                    Legend::Name("received"),
                ),
                usage(
                    format!("sum(rate(container_network_transmit_bytes_total{{{sel}}}[{w}]))"),
                    Legend::Name("sent"),
                ),
            ],
        },
        Panel {
            title: "Restarts",
            unit: Unit::Count,
            queries: vec![usage(
                format!("sum(increase(kube_pod_container_status_restarts_total{{{sel}}}[{iw}]))"),
                Legend::Name("restarts"),
            )],
        },
    ]
}

/// Charts for one pod: CPU and memory by container (with request and limit
/// lines), network received/sent, restarts.
pub fn pod_panels(namespace: &str, pod: &str, range: &TimeRange) -> Vec<Panel> {
    let sel = format!("{},{}", eq("namespace", namespace), eq("pod", pod));
    pod_like_panels(&sel, "container", range)
}

/// Charts for a workload, given the pods it owns: the same as for a pod,
/// split by pod. Only the first [`MAX_WORKLOAD_PODS`] pods are included
/// (the query travels in a URL); no pods, no charts.
pub fn workload_panels(namespace: &str, pods: &[&str], range: &TimeRange) -> Vec<Panel> {
    let pods = &pods[..pods.len().min(MAX_WORKLOAD_PODS)];
    if pods.is_empty() {
        // An empty alternation would match every series without a pod label.
        return Vec::new();
    }
    let sel = format!("{},{}", eq("namespace", namespace), one_of("pod", pods));
    pod_like_panels(&sel, "pod", range)
}

/// Which label identifies a node's cAdvisor series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeBy {
    /// `node="<name>"`: kube-prometheus-stack and most setups. Try first.
    Label,
    /// `instance` is the node name, with or without a port: plain
    /// `kubernetes_sd` scrape configs. Try when [`NodeBy::Label`] gave
    /// nothing ([`PanelData::is_empty`]).
    Instance,
}

/// Charts for one node: CPU and memory of the root cgroup, with the
/// node's allocatable as a reference line.
pub fn node_panels(node: &str, by: NodeBy, range: &TimeRange) -> Vec<Panel> {
    let w = range.rate_window();
    let sel = match by {
        NodeBy::Label => eq("node", node),
        // Exactly the name, optionally followed by a port: `node-1` must
        // not match `node-10`.
        NodeBy::Instance => {
            format!(
                "instance=~\"{}\"",
                escape_label_value(&format!("{}(:[0-9]+)?", escape_regex(node)))
            )
        }
    };
    let allocatable = |res: &str| {
        format!(
            "sum(kube_node_status_allocatable{{{},resource=\"{res}\"}})",
            eq("node", node)
        )
    };
    vec![
        Panel {
            title: "CPU",
            unit: Unit::Cores,
            queries: vec![
                usage(
                    format!("sum(rate(container_cpu_usage_seconds_total{{id=\"/\",{sel}}}[{w}]))"),
                    Legend::Name("used"),
                ),
                reference(allocatable("cpu"), "allocatable"),
            ],
        },
        Panel {
            title: "Memory",
            unit: Unit::Bytes,
            queries: vec![
                usage(
                    format!("sum(container_memory_working_set_bytes{{id=\"/\",{sel}}})"),
                    Legend::Name("used"),
                ),
                reference(allocatable("memory"), "allocatable"),
            ],
        },
        // What the pods on the node send and receive; pods on the host
        // network report the node's own interfaces.
        Panel {
            title: "Pod network",
            unit: Unit::BytesPerSec,
            queries: vec![
                usage(
                    format!("sum(rate(container_network_receive_bytes_total{{{sel}}}[{w}]))"),
                    Legend::Name("received"),
                ),
                usage(
                    format!("sum(rate(container_network_transmit_bytes_total{{{sel}}}[{w}]))"),
                    Legend::Name("sent"),
                ),
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Charts of one object

/// What a set of charts is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChartTarget {
    Pod { namespace: String, name: String },
    /// A workload, by the pods it owns right now.
    Workload { namespace: String, pods: Vec<String> },
    Node { name: String },
}

/// One chart with its data, or why it could not be loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub title: &'static str,
    pub unit: Unit,
    pub data: Result<PanelData, PromError>,
}

impl ChartTarget {
    /// The panels for this target. Nodes are selected by their `node`
    /// label here; [`Prom::charts`] falls back to `instance`.
    pub fn panels(&self, range: &TimeRange) -> Vec<Panel> {
        match self {
            ChartTarget::Pod { namespace, name } => pod_panels(namespace, name, range),
            ChartTarget::Workload { namespace, pods } => {
                let pods: Vec<&str> = pods.iter().map(String::as_str).collect();
                workload_panels(namespace, &pods, range)
            }
            ChartTarget::Node { name } => node_panels(name, NodeBy::Label, range),
        }
    }

    /// The CPU and memory usage queries, for handing to another tool
    /// (Grafana's Explore). Empty for a workload without pods.
    pub fn usage_queries(&self) -> Vec<String> {
        self.panels(&TimeRange::last(3600, 0))
            .into_iter()
            .filter(|p| matches!(p.unit, Unit::Cores | Unit::Bytes))
            .filter_map(|p| p.queries.into_iter().find(|q| !q.reference).map(|q| q.expr))
            .collect()
    }
}

impl Prom {
    async fn run_panels(&self, panels: Vec<Panel>, range: &TimeRange) -> Vec<Chart> {
        let data = futures::future::join_all(panels.iter().map(|p| self.panel(p, range))).await;
        panels
            .into_iter()
            .zip(data)
            .map(|(panel, data)| Chart {
                title: panel.title,
                unit: panel.unit,
                data,
            })
            .collect()
    }

    /// Loads every chart of `target`, all at once. A chart that fails
    /// carries its own error; the others are unaffected. For a node whose
    /// series are not labelled with `node`, the `instance` label is tried.
    pub async fn charts(&self, target: &ChartTarget, range: &TimeRange) -> Vec<Chart> {
        let charts = self.run_panels(target.panels(range), range).await;
        if let ChartTarget::Node { name } = target {
            let nothing = charts.iter().all(|c| matches!(&c.data, Ok(d) if d.is_empty()));
            if nothing && !charts.is_empty() {
                let by_instance = self
                    .run_panels(node_panels(name, NodeBy::Instance, range), range)
                    .await;
                if by_instance.iter().any(|c| matches!(&c.data, Ok(d) if !d.is_empty())) {
                    return by_instance;
                }
            }
        }
        charts
    }
}

/// A moment in the computer's own time zone, for chart axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    /// Seconds east of UTC at that moment.
    pub utc_offset: i32,
}

/// `unix` seconds in the system's time zone (UTC when that is unknown).
pub fn local_time(unix: i64) -> LocalTime {
    let zoned = jiff::Timestamp::from_second(unix)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::system());
    LocalTime {
        month: zoned.month() as u8,
        day: zoned.day() as u8,
        hour: zoned.hour() as u8,
        minute: zoned.minute() as u8,
        utc_offset: zoned.offset().seconds(),
    }
}

// ---------------------------------------------------------------------------
// Alerts

/// A firing alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// The `alertname` label.
    pub name: String,
    /// The `severity` label, lower-cased; empty when the rule sets none.
    pub severity: String,
    /// Always `firing` for alerts returned by [`parse_alerts`].
    pub state: String,
    pub summary: String,
    pub description: String,
    pub labels: BTreeMap<String, String>,
    /// Unix seconds since when the alert is active.
    pub active_since: Option<i64>,
}

#[derive(Deserialize)]
struct RawAlert {
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
    #[serde(default)]
    state: String,
    #[serde(default, rename = "activeAt")]
    active_at: Option<String>,
}

#[derive(Deserialize)]
struct RawAlerts {
    #[serde(default)]
    alerts: Vec<RawAlert>,
}

fn severity_order(severity: &str) -> u8 {
    match severity {
        "critical" | "page" => 0,
        "error" | "high" => 1,
        "warning" => 2,
        "info" => 4,
        _ => 3,
    }
}

/// Parses the body of `/api/v1/alerts` and keeps what is worth showing:
/// firing alerts, without the always-firing `Watchdog` and without
/// `severity="none"` (inhibitor helpers such as `InfoInhibitor`). Sorted
/// most severe first, then by name and age.
pub fn parse_alerts(body: &[u8]) -> Result<Vec<Alert>, PromError> {
    let env = envelope(body)?;
    let raw: RawAlerts = serde_json::from_value(env.data)
        .map_err(|e| PromError::Other(format!("unexpected Prometheus alerts response: {e}")))?;
    let mut alerts: Vec<Alert> = raw
        .alerts
        .into_iter()
        .filter(|a| a.state == "firing")
        .map(|mut a| {
            let mut annotation = |key: &str| a.annotations.remove(key).unwrap_or_default().trim().to_string();
            let (summary, description, message) =
                (annotation("summary"), annotation("description"), annotation("message"));
            Alert {
                name: a.labels.get("alertname").cloned().unwrap_or_default(),
                severity: a
                    .labels
                    .get("severity")
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default(),
                state: a.state,
                summary,
                // Older rules use `message` for what is now `description`.
                description: if description.is_empty() { message } else { description },
                active_since: a
                    .active_at
                    .as_deref()
                    .and_then(|t| t.parse::<jiff::Timestamp>().ok())
                    .map(|t| t.as_second())
                    // Prometheus writes year 1 for "never".
                    .filter(|t| *t > 0),
                labels: a.labels,
            }
        })
        .filter(|a| a.name != "Watchdog" && a.severity != "none")
        .collect();
    alerts.sort_by(|a, b| {
        (severity_order(&a.severity), &a.name, a.active_since, &a.labels).cmp(&(
            severity_order(&b.severity),
            &b.name,
            b.active_since,
            &b.labels,
        ))
    });
    Ok(alerts)
}

/// The kind of object an alert is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlertKind {
    Pod,
    Deployment,
    StatefulSet,
    DaemonSet,
    Job,
    Pvc,
    Node,
    /// A node known only by the `instance` label of a node-level scrape
    /// job: `name` is a host name or an IP address, to be matched against
    /// the nodes' names and addresses.
    NodeAddress,
}

/// The object an alert concerns, for links and badges.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AlertTarget {
    pub kind: AlertKind,
    /// `None` for nodes.
    pub namespace: Option<String>,
    pub name: String,
}

impl Alert {
    fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(String::as_str).filter(|v| !v.is_empty())
    }

    /// The object this alert concerns, from its labels, or `None` when it
    /// is about the cluster as a whole (or about something Skiff cannot
    /// link to).
    ///
    /// Workload labels win over `pod`: kube-state-metrics alerts such as
    /// `KubeDeploymentReplicasMismatch` often also carry the `pod` label of
    /// the kube-state-metrics pod that reported the number.
    pub fn target(&self) -> Option<AlertTarget> {
        if let Some(ns) = self.label("namespace") {
            const NAMESPACED: &[(&str, AlertKind)] = &[
                ("deployment", AlertKind::Deployment),
                ("statefulset", AlertKind::StatefulSet),
                ("daemonset", AlertKind::DaemonSet),
                // `job` is the scrape job; kube-state-metrics says `job_name`.
                ("job_name", AlertKind::Job),
                ("persistentvolumeclaim", AlertKind::Pvc),
                ("pod", AlertKind::Pod),
            ];
            for (label, kind) in NAMESPACED {
                if let Some(name) = self.label(label) {
                    return Some(AlertTarget {
                        kind: *kind,
                        namespace: Some(ns.to_string()),
                        name: name.to_string(),
                    });
                }
            }
        }
        if let Some(node) = self.label("node") {
            return Some(AlertTarget {
                kind: AlertKind::Node,
                namespace: None,
                name: node.to_string(),
            });
        }
        // `instance` is on almost every alert; it only names a node when
        // the scrape job is a per-node one.
        let job = self.label("job").unwrap_or("");
        if job.contains("node-exporter") || job == "node" || job.contains("kubelet") {
            let host = self.label("instance").map(instance_host).filter(|h| !h.is_empty())?;
            return Some(AlertTarget {
                kind: AlertKind::NodeAddress,
                namespace: None,
                name: host.to_string(),
            });
        }
        None
    }
}

/// `host` of `host:port`, `[v6]:port` or a bare host.
fn instance_host(instance: &str) -> &str {
    if let Some(rest) = instance.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    match instance.rsplit_once(':') {
        // More than one colon without brackets: a bare IPv6 address.
        Some((host, port)) if !host.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => instance,
    }
}

// ---------------------------------------------------------------------------
// Sample ring

/// A fixed-capacity ring of `(unix seconds, value)` samples: the fallback
/// sparkline when there is no Prometheus, fed from the metrics-server
/// polls the app makes anyway. The oldest sample is dropped when full.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleRing {
    capacity: usize,
    samples: VecDeque<(f64, f64)>,
}

impl SampleRing {
    /// A ring for `capacity` samples (at least one).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // Not pre-allocated: there may be one ring per pod, most of them
        // short-lived.
        SampleRing {
            capacity,
            samples: VecDeque::new(),
        }
    }

    /// Adds a sample. Time only moves forward: a sample older than the
    /// newest is ignored, one with the same timestamp replaces it, and
    /// non-finite numbers are ignored.
    pub fn push(&mut self, at: f64, value: f64) {
        if !at.is_finite() || !value.is_finite() {
            return;
        }
        if let Some(last) = self.samples.back_mut() {
            if at < last.0 {
                return;
            }
            if at == last.0 {
                last.1 = value;
                return;
            }
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back((at, value));
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// The newest sample.
    pub fn last(&self) -> Option<(f64, f64)> {
        self.samples.back().copied()
    }

    /// Samples, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.samples.iter().copied()
    }

    /// Samples, oldest first, in the shape of [`Series::points`].
    pub fn to_vec(&self) -> Vec<(f64, f64)> {
        self.samples.iter().copied().collect()
    }

    /// Smallest and largest value, for scaling a sparkline.
    pub fn value_range(&self) -> Option<(f64, f64)> {
        self.samples.iter().fold(None, |acc, (_, v)| match acc {
            None => Some((*v, *v)),
            Some((lo, hi)) => Some((lo.min(*v), hi.max(*v))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- endpoint and paths

    #[test]
    fn path_for_operator_endpoint() {
        let ep = PromEndpoint::new("monitoring", "prometheus-operated", 9090);
        assert_eq!(
            ep.api_path("query", &[("query", "up")]),
            "/api/v1/namespaces/monitoring/services/http:prometheus-operated:9090/proxy/api/v1/query?query=up"
        );
        assert_eq!(
            ep.api_path("alerts", &[]),
            "/api/v1/namespaces/monitoring/services/http:prometheus-operated:9090/proxy/api/v1/alerts"
        );
    }

    #[test]
    fn path_with_prefix_and_https() {
        let ep = PromEndpoint {
            namespace: "vm".into(),
            service: "vmselect-main".into(),
            port: 8481,
            scheme: "HTTPS".into(),
            path_prefix: "select/0/prometheus/".into(),
        };
        assert_eq!(
            ep.api_path("query_range", &[("query", "up"), ("start", "1"), ("end", "2"), ("step", "15")]),
            "/api/v1/namespaces/vm/services/https:vmselect-main:8481/proxy/select/0/prometheus/api/v1/query_range?query=up&start=1&end=2&step=15"
        );
    }

    #[test]
    fn query_string_is_percent_encoded() {
        let ep = PromEndpoint::new("m", "p", 9090);
        let q = r#"sum by(container)(rate(x{namespace="a b",pod=~"p\\.1|q+"}[120s])) & 1 # ü"#;
        let path = ep.api_path("query", &[("query", q)]);
        let (_, query) = path.split_once("?query=").unwrap();
        // Nothing that has a meaning in a URL survives unencoded.
        assert!(
            query
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~%".contains(&b)),
            "{query}"
        );
        assert!(query.contains("%22a%20b%22"), "{query}");
        assert!(query.contains("%2B") && query.contains("%26") && query.contains("%23") && query.contains("%5C%5C."));
        assert!(query.contains("%C3%BC"), "{query}");
        // And it is a URI the http crate accepts.
        assert!(path.parse::<http::Uri>().is_ok());
    }

    #[test]
    fn hostile_endpoint_cannot_leave_the_proxy_path() {
        let ep = PromEndpoint {
            namespace: "a/../../b".into(),
            service: "s:1/proxy?x=#".into(),
            port: 9090,
            scheme: "gopher".into(),
            path_prefix: "/../../../api/v1/secrets/./x y?z".into(),
        };
        assert!(ep.validate().is_err());
        let path = ep.api_path("query", &[]);
        assert_eq!(
            path,
            "/api/v1/namespaces/a%2F..%2F..%2Fb/services/http:s%3A1%2Fproxy%3Fx=%23:9090/proxy/api/v1/secrets/x%20y%3Fz/api/v1/query"
        );
        assert!(path.parse::<http::Uri>().is_ok());
    }

    #[test]
    fn endpoint_from_config_json() {
        let ep: PromEndpoint =
            serde_json::from_value(json!({"namespace": "monitoring", "service": "prometheus-operated"})).unwrap();
        assert_eq!(ep, PromEndpoint::new("monitoring", "prometheus-operated", 9090));
        assert_eq!(ep.scheme, "http");
        assert!(ep.validate().is_ok());

        let full: PromEndpoint = serde_json::from_value(json!({
            "namespace": "vm", "service": "vmselect", "port": 8481, "scheme": "http",
            "path_prefix": "/select/0/prometheus", "some_future_key": true
        }))
        .unwrap();
        assert_eq!(full.port, 8481);
        assert_eq!(full.path_prefix, "/select/0/prometheus");

        assert!(PromEndpoint::default().validate().unwrap_err().contains("namespace"));
        assert!(PromEndpoint::new("monitoring", "", 9090)
            .validate()
            .unwrap_err()
            .contains("service"));
        assert!(PromEndpoint::new("m", "p", 0).validate().unwrap_err().contains("port"));
        let bad = PromEndpoint {
            scheme: "ftp".into(),
            ..PromEndpoint::new("m", "p", 1)
        };
        assert!(bad.validate().unwrap_err().contains("scheme"));
        assert!(PromEndpoint::new("Monitoring", "p", 1).validate().is_err());
    }

    // -- detection

    fn svc(ns: &str, name: &str, labels: Value, ports: Value) -> Value {
        json!({"metadata": {"namespace": ns, "name": name, "labels": labels}, "spec": {"ports": ports}})
    }

    #[test]
    fn port_choice() {
        // kube-prometheus-stack's governing Service.
        let operated =
            json!([{"name": "reloader-web", "port": 8080}, {"name": "web", "port": 9090, "protocol": "TCP"}]);
        assert_eq!(pick_port(operated.as_array().unwrap(), 9090), Some((9090, "http")));
        // Name wins over number.
        let named = json!([{"name": "metrics", "port": 9090}, {"name": "http-web", "port": 8080}]);
        assert_eq!(pick_port(named.as_array().unwrap(), 9090), Some((8080, "http")));
        // The community chart: http on 80.
        let chart = json!([{"name": "http", "port": 80, "targetPort": 9090}]);
        assert_eq!(pick_port(chart.as_array().unwrap(), 80), Some((80, "http")));
        // Unnamed: by number, the flavour's own first.
        let numbers = json!([{"port": 8080}, {"port": 9090}, {"port": 8481}]);
        assert_eq!(pick_port(numbers.as_array().unwrap(), 8481), Some((8481, "http")));
        assert_eq!(pick_port(numbers.as_array().unwrap(), 1234), Some((9090, "http")));
        // A single unknown port is taken; several unknown ones are not guessed.
        let single = json!([{"name": "api", "port": 7000}]);
        assert_eq!(pick_port(single.as_array().unwrap(), 9090), Some((7000, "http")));
        let unknown = json!([{"name": "a", "port": 7000}, {"name": "b", "port": 7001}]);
        assert_eq!(pick_port(unknown.as_array().unwrap(), 9090), None);
        // TLS.
        let tls = json!([{"name": "https", "port": 9091}, {"name": "grpc", "port": 10901}]);
        assert_eq!(pick_port(tls.as_array().unwrap(), 9090), Some((9091, "https")));
        let tls_single = json!([{"port": 443}]);
        assert_eq!(pick_port(tls_single.as_array().unwrap(), 9090), Some((443, "https")));
        // UDP, junk and out-of-range numbers are not ports to use.
        let junk = json!([{"name": "web", "port": 9090, "protocol": "UDP"}, {"name": "web", "port": 70000}, {"port": "x"}, {"port": 0}]);
        assert_eq!(pick_port(junk.as_array().unwrap(), 9090), None);
        assert_eq!(pick_port(&[], 9090), None);
    }

    #[test]
    fn candidates_are_ranked_by_the_heuristics() {
        let services = vec![
            svc(
                "monitoring",
                "kps-alertmanager",
                json!({"app.kubernetes.io/name": "alertmanager"}),
                json!([{"name": "http-web", "port": 9093}]),
            ),
            svc(
                "monitoring",
                "kps-prometheus-node-exporter",
                json!({"app.kubernetes.io/name": "prometheus-node-exporter"}),
                json!([{"name": "http-metrics", "port": 9100}]),
            ),
            svc(
                "monitoring",
                "kps-operator",
                json!({"app": "kube-prometheus-stack-operator"}),
                json!([{"name": "https", "port": 443}]),
            ),
            svc(
                "vm",
                "vmselect-main",
                json!({"app.kubernetes.io/name": "vmselect"}),
                json!([{"name": "http", "port": 8481}]),
            ),
            svc(
                "vm",
                "vmsingle-main",
                json!({"app.kubernetes.io/name": "vmsingle"}),
                json!([{"name": "http", "port": 8428}]),
            ),
            svc("default", "prometheus", json!({}), json!([{"port": 9090}])),
            svc(
                "prom",
                "prometheus-server",
                json!({"app.kubernetes.io/name": "prometheus", "app.kubernetes.io/component": "server"}),
                json!([{"name": "http", "port": 80}]),
            ),
            svc(
                "monitoring",
                "kps-prometheus",
                json!({"app": "kube-prometheus-stack-prometheus"}),
                json!([{"name": "http-web", "port": 9090}, {"name": "reloader-web", "port": 8080}]),
            ),
            svc(
                "monitoring",
                "prometheus-operated",
                json!({"operated-prometheus": "true"}),
                json!([{"name": "web", "port": 9090}]),
            ),
            svc(
                "web",
                "shop",
                json!({"app": "shop"}),
                json!([{"name": "http", "port": 80}]),
            ),
        ];
        let found = candidates(&services);
        let names: Vec<String> = found.iter().map(|e| e.display()).collect();
        assert_eq!(
            names,
            [
                "monitoring/prometheus-operated:9090",
                "prom/prometheus-server:80",
                "vm/vmsingle-main:8428",
                "vm/vmselect-main:8481",
                "default/prometheus:9090",
                "monitoring/kps-prometheus:9090",
            ]
        );
        assert_eq!(found[3].path_prefix, "/select/0/prometheus");
        assert!(found
            .iter()
            .filter(|e| e.service != "vmselect-main")
            .all(|e| e.path_prefix.is_empty()));
        // Listing order does not matter.
        let mut reversed = services.clone();
        reversed.reverse();
        assert_eq!(candidates(&reversed), found);
    }

    #[test]
    fn candidates_skip_what_cannot_be_proxied() {
        let mut external = svc("m", "prometheus", json!({}), json!([{"port": 9090}]));
        external["spec"]["type"] = json!("ExternalName");
        let services = vec![
            external,
            // No recognisable port.
            svc(
                "m",
                "prometheus-k8s",
                json!({}),
                json!([{"name": "a", "port": 1}, {"name": "b", "port": 2}]),
            ),
            // Legacy chart labels, and a pushgateway that shares them.
            svc(
                "m",
                "old-prometheus-server",
                json!({"app": "prometheus", "component": "server"}),
                json!([{"name": "http", "port": 80}]),
            ),
            svc(
                "m",
                "old-prometheus-pushgateway",
                json!({"app": "prometheus", "component": "pushgateway"}),
                json!([{"name": "http", "port": 9091}]),
            ),
            // Not objects the API would send, but nothing here may panic.
            json!({"metadata": {"name": "prometheus"}}),
            json!({"metadata": {"namespace": "Bad Namespace", "name": "prometheus"}, "spec": {"ports": [{"port": 9090}]}}),
            json!(null),
            json!("prometheus"),
        ];
        let found = candidates(&services);
        assert_eq!(
            found.iter().map(|e| e.display()).collect::<Vec<_>>(),
            ["m/old-prometheus-server:80"]
        );
    }

    // -- escaping and PromQL

    #[test]
    fn label_values_cannot_break_out() {
        assert_eq!(escape_label_value("plain-pod-0"), "plain-pod-0");
        assert_eq!(escape_label_value(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_label_value(r"a\b"), r"a\\b");
        // A trailing backslash must not swallow the closing quote.
        assert_eq!(escape_label_value("x\\"), "x\\\\");
        assert_eq!(escape_label_value("a\nb\r\tc"), r"a\nb\r\tc");
        assert_eq!(escape_label_value("a\u{0}b\u{1b}c\u{7f}"), r"a\u0000b\u001bc\u007f");
        assert_eq!(escape_label_value("ünï{}`'"), "ünï{}`'");
    }

    /// Reads a double-quoted PromQL string literal starting at `s[0] == '"'`
    /// the way the PromQL lexer does, returning the rest after the closing
    /// quote. Fails on a raw newline, as the lexer would.
    fn skip_string(s: &str) -> Option<&str> {
        let mut chars = s.char_indices();
        assert_eq!(chars.next()?.1, '"');
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    chars.next()?;
                }
                '"' => return Some(&s[i + 1..]),
                '\n' => return None,
                _ => {}
            }
        }
        None
    }

    /// The number of label matchers in the first `{...}` of `expr`, and
    /// what follows the closing brace: a tiny selector parser that proves a
    /// hostile value stayed inside its string.
    fn selector_shape(expr: &str) -> (usize, String) {
        let mut rest = &expr[expr.find('{').unwrap() + 1..];
        let mut matchers = 0;
        loop {
            let op = rest.find(['=', '!']).unwrap();
            assert!(
                rest[..op].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "label name in {rest}"
            );
            rest = rest[op..].trim_start_matches(['=', '!', '~']);
            rest = skip_string(rest).unwrap_or_else(|| panic!("unterminated string in {expr}"));
            matchers += 1;
            match rest.as_bytes()[0] {
                b',' => rest = &rest[1..],
                b'}' => return (matchers, rest[1..].to_string()),
                other => panic!("unexpected {:?} after a matcher in {expr}", other as char),
            }
        }
    }

    const HOSTILE: &[&str] = &[
        r#"x"} or vector(1) #"#,
        r#"x",namespace=~".+"#,
        "x\\",
        "x\\\"",
        "line1\nline2\"}",
        "a{b}c[5m]",
        "a|b",
        ".*",
        "(?i)x",
        "x\u{0}y",
        "",
    ];

    #[test]
    fn chart_targets_pick_their_panels_and_explore_queries() {
        let range = TimeRange::last(3600, 1_700_000_000);
        let pod = ChartTarget::Pod { namespace: "shop".into(), name: "web-1".into() };
        assert_eq!(pod.panels(&range), pod_panels("shop", "web-1", &range));
        let queries = pod.usage_queries();
        assert_eq!(queries.len(), 2, "{queries:?}");
        assert!(queries[0].starts_with("sum by(container)(rate(container_cpu_usage_seconds_total{namespace=\"shop\",pod=\"web-1\""));
        assert!(queries[1].starts_with("sum by(container)(container_memory_working_set_bytes{"));

        let workload = ChartTarget::Workload { namespace: "shop".into(), pods: vec!["a".into(), "b".into()] };
        assert_eq!(workload.panels(&range), workload_panels("shop", &["a", "b"], &range));
        let empty = ChartTarget::Workload { namespace: "shop".into(), pods: vec![] };
        assert!(empty.panels(&range).is_empty() && empty.usage_queries().is_empty());

        let node = ChartTarget::Node { name: "node-1".into() };
        let titles: Vec<&str> = node.panels(&range).iter().map(|p| p.title).collect();
        assert_eq!(titles, ["CPU", "Memory", "Pod network"]);
        assert_eq!(node.usage_queries().len(), 2);
    }

    #[test]
    fn local_time_is_consistent_with_its_offset() {
        let t = 1_700_000_000;
        let local = local_time(t);
        let shifted = t + i64::from(local.utc_offset);
        assert_eq!(i64::from(local.hour), shifted.rem_euclid(86_400) / 3600);
        assert_eq!(i64::from(local.minute), shifted.rem_euclid(3600) / 60);
        assert!((1..=12).contains(&local.month) && (1..=31).contains(&local.day));
    }

    #[test]
    fn hostile_names_stay_inside_their_matcher() {
        let range = TimeRange::last(3600, 1_700_000_000);
        for hostile in HOSTILE {
            for panel in pod_panels(hostile, hostile, &range) {
                for q in &panel.queries {
                    let (matchers, _) = selector_shape(&q.expr);
                    // namespace, pod, and at most two fixed extra matchers.
                    assert!((2..=4).contains(&matchers), "{matchers} matchers in {}", q.expr);
                    assert!(!q.expr.contains('\n'), "{}", q.expr);
                }
            }
            for panel in workload_panels(hostile, &[hostile, "ok-1"], &range) {
                for q in &panel.queries {
                    let (matchers, _) = selector_shape(&q.expr);
                    assert!((2..=4).contains(&matchers), "{matchers} matchers in {}", q.expr);
                }
            }
            for by in [NodeBy::Label, NodeBy::Instance] {
                for panel in node_panels(hostile, by, &range) {
                    for q in &panel.queries {
                        let (matchers, _) = selector_shape(&q.expr);
                        // id="/" and the node; the network panel has only the node.
                        assert!((1..=2).contains(&matchers), "{}", q.expr);
                    }
                }
            }
        }
    }

    #[test]
    fn regex_names_are_literal() {
        assert_eq!(escape_regex("web-1.2"), r"web-1\.2");
        assert_eq!(escape_regex(r"a\b"), r"a\\b");
        assert_eq!(escape_regex("a|b(c)[d]{e}^$*+?"), r"a\|b\(c\)\[d\]\{e\}\^\$\*\+\?");
        // Regex-escaped, then string-escaped: the backslashes double.
        assert_eq!(one_of("pod", &["web-1.2", "db|x"]), r#"pod=~"web-1\\.2|db\\|x""#);
        assert_eq!(one_of("pod", &[r#"a"b"#]), r#"pod=~"a\"b""#);
    }

    #[test]
    fn pod_queries() {
        let range = TimeRange::last(3600, 1_700_000_000);
        assert_eq!(range.step, 30);
        let panels = pod_panels("shop", "web-7d9f-abcde", &range);
        assert_eq!(
            panels.iter().map(|p| p.title).collect::<Vec<_>>(),
            ["CPU", "Memory", "Network", "Restarts"]
        );
        let cpu = &panels[0];
        assert_eq!(cpu.unit, Unit::Cores);
        assert_eq!(
            cpu.queries[0].expr,
            r#"sum by(container)(rate(container_cpu_usage_seconds_total{namespace="shop",pod="web-7d9f-abcde",container!="",container!="POD"}[120s]))"#
        );
        assert_eq!(cpu.queries[0].legend, Legend::Label("container"));
        assert!(!cpu.queries[0].reference);
        assert_eq!(
            cpu.queries[1].expr,
            r#"sum(kube_pod_container_resource_requests{namespace="shop",pod="web-7d9f-abcde",resource="cpu"})"#
        );
        assert_eq!(
            cpu.queries[2].expr,
            r#"sum(kube_pod_container_resource_limits{namespace="shop",pod="web-7d9f-abcde",resource="cpu"})"#
        );
        assert!(cpu.queries[1].reference && cpu.queries[2].reference);
        assert_eq!(
            panels[1].queries[0].expr,
            r#"sum by(container)(container_memory_working_set_bytes{namespace="shop",pod="web-7d9f-abcde",container!="",container!="POD"})"#
        );
        assert_eq!(
            panels[1].queries[2].expr,
            r#"sum(kube_pod_container_resource_limits{namespace="shop",pod="web-7d9f-abcde",resource="memory"})"#
        );
        assert_eq!(
            panels[2].queries[0].expr,
            r#"sum(rate(container_network_receive_bytes_total{namespace="shop",pod="web-7d9f-abcde"}[120s]))"#
        );
        assert_eq!(
            panels[2].queries[1].expr,
            r#"sum(rate(container_network_transmit_bytes_total{namespace="shop",pod="web-7d9f-abcde"}[120s]))"#
        );
        assert_eq!(
            panels[3].queries[0].expr,
            r#"sum(increase(kube_pod_container_status_restarts_total{namespace="shop",pod="web-7d9f-abcde"}[300s]))"#
        );
    }

    #[test]
    fn workload_queries() {
        let range = TimeRange::last(6 * 3600, 1_700_000_000);
        assert_eq!(range.step, 300);
        let panels = workload_panels("shop", &["web-1", "web-2.x"], &range);
        assert_eq!(
            panels[0].queries[0].expr,
            r#"sum by(pod)(rate(container_cpu_usage_seconds_total{namespace="shop",pod=~"web-1|web-2\\.x",container!="",container!="POD"}[1200s]))"#
        );
        assert_eq!(panels[0].queries[0].legend, Legend::Label("pod"));
        assert_eq!(
            panels[1].queries[1].expr,
            // Per pod, like the usage lines it is compared with.
            r#"max(sum by(pod)(kube_pod_container_resource_requests{namespace="shop",pod=~"web-1|web-2\\.x",resource="memory"}))"#
        );
        // No pods: no query that would match everything.
        assert!(workload_panels("shop", &[], &range).is_empty());
        // The selector is bounded.
        let many: Vec<String> = (0..500).map(|i| format!("pod-{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let expr = &workload_panels("shop", &many, &range)[0].queries[0].expr;
        assert!(expr.contains("pod-99") && !expr.contains("pod-100"), "{expr}");
    }

    #[test]
    fn node_queries() {
        let range = TimeRange::last(3600, 1_700_000_000);
        let by_label = node_panels("ip-10-0-1-5.eu-west-1.compute.internal", NodeBy::Label, &range);
        assert_eq!(
            by_label[0].queries[0].expr,
            r#"sum(rate(container_cpu_usage_seconds_total{id="/",node="ip-10-0-1-5.eu-west-1.compute.internal"}[120s]))"#
        );
        assert_eq!(
            by_label[1].queries[0].expr,
            r#"sum(container_memory_working_set_bytes{id="/",node="ip-10-0-1-5.eu-west-1.compute.internal"})"#
        );
        assert_eq!(
            by_label[1].queries[1].expr,
            r#"sum(kube_node_status_allocatable{node="ip-10-0-1-5.eu-west-1.compute.internal",resource="memory"})"#
        );
        let by_instance = node_panels("node-1.lan", NodeBy::Instance, &range);
        assert_eq!(
            by_instance[0].queries[0].expr,
            r#"sum(rate(container_cpu_usage_seconds_total{id="/",instance=~"node-1\\.lan(:[0-9]+)?"}[120s]))"#
        );
        // The reference line always goes by the node label (kube-state-metrics).
        assert_eq!(
            by_instance[0].queries[1].expr,
            r#"sum(kube_node_status_allocatable{node="node-1.lan",resource="cpu"})"#
        );
    }

    // -- time range

    #[test]
    fn step_and_window() {
        let now = 1_700_000_123;
        for (span, step, window) in [
            (300, 15, "120s"),
            (900, 15, "120s"),
            (3600, 30, "120s"),
            (6 * 3600, 300, "1200s"),
            (24 * 3600, 900, "3600s"),
            (7 * 86_400, 7200, "28800s"),
            (30 * 86_400, 21_600, "86400s"),
        ] {
            let r = TimeRange::last(span, now);
            assert_eq!(r.step, step, "span {span}");
            assert_eq!(r.rate_window(), window, "span {span}");
            assert_eq!(r.end - r.start, span as i64);
            // Aligned, never in the future, less than a step behind.
            assert_eq!(r.end % i64::from(step), 0);
            assert!(r.end <= now && now - r.end < i64::from(step));
            let points = (r.end - r.start) / i64::from(step) + 1;
            // (Short spans have fewer points: the step does not go below 15 s.)
            let fewest = if span >= 3600 { 60 } else { 20 };
            assert!((fewest..=121).contains(&points), "span {span}: {points} points");
        }
        assert_eq!(TimeRange::last(3600, now).increase_window(), "300s");
        assert_eq!(TimeRange::last(24 * 3600, now).increase_window(), "900s");
    }

    #[test]
    fn step_survives_silly_input() {
        let tiny = TimeRange::with_points(0, 1_700_000_000, 0);
        assert_eq!((tiny.step, tiny.end - tiny.start), (15, 60));
        let huge = TimeRange::with_points(u64::MAX, i64::MAX, u32::MAX);
        assert_eq!(huge.end - huge.start, 90 * 86_400);
        assert!(huge.step >= 7200 && huge.start < huge.end);
        // Coarse on purpose: beyond the table, whole days.
        let coarse = TimeRange::with_points(90 * 86_400, 1_700_000_000, 10);
        assert_eq!(coarse.step, 9 * 86_400);
        let negative = TimeRange::last(3600, -5);
        assert_eq!(negative.end, 0);
    }

    // -- responses

    /// A range query answer in the shape kube-prometheus-stack gives for
    /// the pod CPU query.
    const MATRIX: &str = r#"{
      "status": "success",
      "data": {
        "resultType": "matrix",
        "result": [
          {
            "metric": {"container": "app"},
            "values": [[1700000000, "0.0123"], [1700000030, "0.0150"], [1700000060.5, "1.5e-2"]]
          },
          {
            "metric": {"container": "istio-proxy"},
            "values": [[1700000000, "NaN"], [1700000030, "+Inf"], [1700000060, "-Inf"], [1700000090, "0.002"], [1700000120, "oops"]]
          },
          {"metric": {}, "values": []}
        ]
      }
    }"#;

    #[test]
    fn parses_a_matrix() {
        let r = parse_query_response(MATRIX.as_bytes()).unwrap();
        assert!(!r.truncated);
        assert_eq!(r.series.len(), 3);
        assert_eq!(r.series[0].label("container"), "app");
        assert_eq!(r.series[0].label("missing"), "");
        assert_eq!(
            r.series[0].points,
            [
                (1_700_000_000.0, 0.0123),
                (1_700_000_030.0, 0.015),
                (1_700_000_060.5, 0.015)
            ]
        );
        // NaN, infinities and junk leave a gap, not a poisoned number.
        assert_eq!(r.series[1].points, [(1_700_000_090.0, 0.002)]);
        assert!(r.series[2].points.is_empty() && r.series[2].labels.is_empty());
    }

    #[test]
    fn parses_vector_and_scalar() {
        let vector = br#"{"status":"success","data":{"resultType":"vector","result":[
            {"metric":{"__name__":"up","job":"kubelet"},"value":[1700000000.123,"1"]},
            {"metric":{"job":"x"},"value":[1700000000.123,"NaN"]}]}}"#;
        let r = parse_query_response(vector).unwrap();
        assert_eq!(r.series[0].points, [(1_700_000_000.123, 1.0)]);
        assert_eq!(r.series[0].label("job"), "kubelet");
        assert!(r.series[1].points.is_empty());

        let scalar = br#"{"status":"success","data":{"resultType":"scalar","result":[1700000000,"42"]}}"#;
        assert_eq!(
            parse_query_response(scalar).unwrap().series[0].points,
            [(1_700_000_000.0, 42.0)]
        );

        let empty = br#"{"status":"success","data":{"resultType":"matrix","result":[]}}"#;
        assert!(parse_query_response(empty).unwrap().series.is_empty());
    }

    #[test]
    fn series_count_is_capped() {
        let result: Vec<Value> = (0..MAX_SERIES + 5)
            .map(|i| json!({"metric": {"pod": format!("p{i}")}, "values": [[1, "1"]]}))
            .collect();
        let body = json!({"status": "success", "data": {"resultType": "matrix", "result": result}}).to_string();
        let r = parse_query_response(body.as_bytes()).unwrap();
        assert_eq!(r.series.len(), MAX_SERIES);
        assert!(r.truncated);
    }

    #[test]
    fn error_answers() {
        let bad_query = br#"{"status":"error","errorType":"bad_data","error":"invalid parameter \"query\": 1:5: parse error: unexpected end of input"}"#;
        match parse_query_response(bad_query) {
            Err(PromError::Query(msg)) => assert!(msg.contains("parse error") && msg.ends_with("(bad_data)"), "{msg}"),
            other => panic!("{other:?}"),
        }
        // The same body with its HTTP status, as the proxy passes it on.
        assert!(matches!(classify_response(400, bad_query), PromError::Query(_)));
        assert!(matches!(
            classify_response(
                503,
                br#"{"status":"error","errorType":"timeout","error":"query timed out"}"#
            ),
            PromError::Query(_)
        ));

        // Not Prometheus at all.
        for body in [
            &b"<html>nginx</html>"[..],
            b"",
            b"{}",
            b"[1,2]",
            br#"{"status":"Failure","message":"x"}"#,
        ] {
            assert!(
                matches!(parse_query_response(body), Err(PromError::Other(_))),
                "{:?}",
                String::from_utf8_lossy(body)
            );
        }
        let odd = br#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"a":1}}]}}"#;
        assert!(matches!(parse_query_response(odd), Err(PromError::Other(_))));
        let odd_type = br#"{"status":"success","data":{"resultType":"streams","result":[]}}"#;
        assert!(matches!(parse_query_response(odd_type), Err(PromError::Other(_))));
    }

    #[test]
    fn proxy_failures_are_told_apart() {
        let forbidden = br#"{"kind":"Status","apiVersion":"v1","status":"Failure","message":"services \"prometheus-operated:9090\" is forbidden: User \"jane\" cannot get resource \"services/proxy\" in API group \"\" in the namespace \"monitoring\"","reason":"Forbidden","code":403}"#;
        match classify_response(403, forbidden) {
            PromError::Forbidden(msg) => assert!(msg.contains("services/proxy"), "{msg}"),
            other => panic!("{other:?}"),
        }
        let no_endpoints = br#"{"kind":"Status","status":"Failure","message":"no endpoints available for service \"prometheus-operated\"","reason":"ServiceUnavailable","code":503}"#;
        assert_eq!(
            classify_response(503, no_endpoints),
            PromError::Unreachable("no endpoints available for service \"prometheus-operated\"".into())
        );
        assert!(
            matches!(classify_response(404, b"404 page not found"), PromError::Unreachable(m) if m == "404 page not found")
        );
        assert_eq!(
            classify_response(502, b"<html><body>Bad Gateway</body></html>"),
            PromError::Unreachable("HTTP 502".into())
        );
        assert_eq!(classify_response(504, b""), PromError::Unreachable("HTTP 504".into()));
        assert!(matches!(
            classify_response(401, b"Unauthorized"),
            PromError::Forbidden(_)
        ));
        assert_eq!(classify_response(500, b"boom"), PromError::Other("boom (500)".into()));
        // A huge error page does not end up in the UI in full.
        let long = "x".repeat(5000);
        match classify_response(500, long.as_bytes()) {
            PromError::Other(msg) => assert!(msg.chars().count() < 320),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn errors_read_well() {
        assert!(PromError::NotFound.to_string().starts_with("No Prometheus found"));
        assert!(PromError::Forbidden("nope".into())
            .to_string()
            .contains("services/proxy"));
        assert_eq!(
            PromError::Unreachable("HTTP 503".into()).to_string(),
            "Prometheus is not answering: HTTP 503"
        );
    }

    // -- alerts

    /// `/api/v1/alerts` as kube-prometheus-stack answers it.
    const ALERTS: &str = r#"{
      "status": "success",
      "data": {
        "alerts": [
          {
            "labels": {"alertname": "Watchdog", "severity": "none"},
            "annotations": {"summary": "An alert that should always be firing."},
            "state": "firing", "activeAt": "2026-11-01T00:00:00Z", "value": "1e+00"
          },
          {
            "labels": {"alertname": "InfoInhibitor", "severity": "none", "namespace": "shop"},
            "annotations": {}, "state": "firing", "activeAt": "2026-11-20T10:00:00Z", "value": "1e+00"
          },
          {
            "labels": {"alertname": "KubePodCrashLooping", "container": "app", "namespace": "shop",
                       "pod": "web-7d9f-abcde", "severity": "warning", "job": "kube-state-metrics",
                       "instance": "10.0.3.7:8080", "reason": "CrashLoopBackOff"},
            "annotations": {"description": "Pod shop/web-7d9f-abcde (app) is in waiting state (reason: \"CrashLoopBackOff\").",
                            "summary": "Pod is crash looping.", "runbook_url": "https://runbooks.prometheus-operator.dev/runbooks/kubernetes/kubepodcrashlooping"},
            "state": "firing", "activeAt": "2026-11-23T08:15:30.123456789Z", "value": "1e+00"
          },
          {
            "labels": {"alertname": "KubeDeploymentReplicasMismatch", "namespace": "shop", "deployment": "web",
                       "pod": "kps-kube-state-metrics-5c7b9-xyz", "severity": "warning", "job": "kube-state-metrics"},
            "annotations": {"message": "Deployment shop/web has not matched the expected number of replicas."},
            "state": "pending", "activeAt": "2026-11-23T08:20:00Z", "value": "2e+00"
          },
          {
            "labels": {"alertname": "NodeFilesystemAlmostOutOfSpace", "severity": "Critical", "job": "node-exporter",
                       "instance": "10.0.1.5:9100", "device": "/dev/nvme0n1p1", "namespace": "monitoring"},
            "annotations": {"summary": "Filesystem has less than 3% space left.", "message": "old style"},
            "state": "firing", "activeAt": "0001-01-01T00:00:00Z", "value": "2.1e+00"
          },
          {
            "labels": {"alertname": "CustomThing"},
            "annotations": {"message": "  only a message  "},
            "state": "firing"
          }
        ]
      }
    }"#;

    #[test]
    fn firing_alerts_are_parsed_filtered_and_sorted() {
        let alerts = parse_alerts(ALERTS.as_bytes()).unwrap();
        let names: Vec<&str> = alerts.iter().map(|a| a.name.as_str()).collect();
        // No Watchdog, no severity none, no pending; critical first.
        assert_eq!(
            names,
            ["NodeFilesystemAlmostOutOfSpace", "KubePodCrashLooping", "CustomThing"]
        );
        let node = &alerts[0];
        assert_eq!(node.severity, "critical");
        assert_eq!(node.summary, "Filesystem has less than 3% space left.");
        // No description: the old `message` annotation stands in.
        assert_eq!(node.description, "old style");
        assert_eq!(node.active_since, None);
        let pod = &alerts[1];
        assert_eq!(pod.state, "firing");
        assert_eq!(pod.summary, "Pod is crash looping.");
        assert!(pod.description.starts_with("Pod shop/web-7d9f-abcde"));
        assert_eq!(pod.labels["reason"], "CrashLoopBackOff");
        assert_eq!(
            pod.active_since,
            Some("2026-11-23T08:15:30Z".parse::<jiff::Timestamp>().unwrap().as_second())
        );
        let custom = &alerts[2];
        assert_eq!(
            (custom.severity.as_str(), custom.description.as_str()),
            ("", "only a message")
        );
        assert_eq!(custom.active_since, None);
    }

    #[test]
    fn alerts_error_answers() {
        assert!(parse_alerts(br#"{"status":"success","data":{"alerts":[]}}"#)
            .unwrap()
            .is_empty());
        // VictoriaMetrics without vmalert answers with no alerts key.
        assert!(parse_alerts(br#"{"status":"success","data":{}}"#).unwrap().is_empty());
        assert!(matches!(
            parse_alerts(br#"{"status":"error","error":"nope"}"#),
            Err(PromError::Query(_))
        ));
        assert!(matches!(parse_alerts(b"not json"), Err(PromError::Other(_))));
        assert!(matches!(
            parse_alerts(br#"{"status":"success","data":{"alerts":[{"labels":[1]}]}}"#),
            Err(PromError::Other(_))
        ));
    }

    fn alert(labels: &[(&str, &str)]) -> Alert {
        Alert {
            name: "X".into(),
            severity: "warning".into(),
            state: "firing".into(),
            summary: String::new(),
            description: String::new(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            active_since: None,
        }
    }

    fn target(kind: AlertKind, ns: Option<&str>, name: &str) -> Option<AlertTarget> {
        Some(AlertTarget {
            kind,
            namespace: ns.map(str::to_string),
            name: name.into(),
        })
    }

    #[test]
    fn alert_targets() {
        let ns = ("namespace", "shop");
        assert_eq!(
            alert(&[ns, ("pod", "web-1")]).target(),
            target(AlertKind::Pod, Some("shop"), "web-1")
        );
        // The kube-state-metrics pod label does not hide the workload.
        assert_eq!(
            alert(&[ns, ("deployment", "web"), ("pod", "ksm-abc")]).target(),
            target(AlertKind::Deployment, Some("shop"), "web")
        );
        assert_eq!(
            alert(&[ns, ("statefulset", "db")]).target(),
            target(AlertKind::StatefulSet, Some("shop"), "db")
        );
        assert_eq!(
            alert(&[ns, ("daemonset", "agent")]).target(),
            target(AlertKind::DaemonSet, Some("shop"), "agent")
        );
        // `job` is the scrape job, `job_name` the Job.
        assert_eq!(
            alert(&[ns, ("job_name", "migrate-1"), ("job", "kube-state-metrics")]).target(),
            target(AlertKind::Job, Some("shop"), "migrate-1")
        );
        assert_eq!(
            alert(&[ns, ("persistentvolumeclaim", "data-db-0"), ("node", "n1")]).target(),
            target(AlertKind::Pvc, Some("shop"), "data-db-0")
        );
        assert_eq!(
            alert(&[("node", "n1"), ("instance", "10.0.0.1:10250")]).target(),
            target(AlertKind::Node, None, "n1")
        );
        // A namespace alone (the scrape target's) does not hide the node.
        assert_eq!(
            alert(&[("namespace", "monitoring"), ("node", "n1")]).target(),
            target(AlertKind::Node, None, "n1")
        );
        // Namespaced labels without a namespace cannot be linked.
        assert_eq!(alert(&[("pod", "web-1")]).target(), None);
        assert_eq!(alert(&[ns, ("pod", "")]).target(), None);
        // instance only counts for per-node scrape jobs.
        assert_eq!(
            alert(&[("job", "node-exporter"), ("instance", "10.0.1.5:9100")]).target(),
            target(AlertKind::NodeAddress, None, "10.0.1.5")
        );
        assert_eq!(
            alert(&[("job", "kubelet"), ("instance", "worker-3")]).target(),
            target(AlertKind::NodeAddress, None, "worker-3")
        );
        assert_eq!(
            alert(&[ns, ("job", "shop-api"), ("instance", "10.0.3.7:8080")]).target(),
            None
        );
        assert_eq!(alert(&[("job", "node-exporter")]).target(), None);
        assert_eq!(alert(&[]).target(), None);

        // And on the recorded payload.
        let alerts = parse_alerts(ALERTS.as_bytes()).unwrap();
        assert_eq!(alerts[0].target(), target(AlertKind::NodeAddress, None, "10.0.1.5"));
        assert_eq!(
            alerts[1].target(),
            target(AlertKind::Pod, Some("shop"), "web-7d9f-abcde")
        );
        assert_eq!(alerts[2].target(), None);
    }

    #[test]
    fn instance_hosts() {
        assert_eq!(instance_host("10.0.1.5:9100"), "10.0.1.5");
        assert_eq!(instance_host("worker-3"), "worker-3");
        assert_eq!(instance_host("worker-3.lan:9100"), "worker-3.lan");
        assert_eq!(instance_host("[fd00::5]:9100"), "fd00::5");
        assert_eq!(instance_host("fd00::5"), "fd00::5");
        assert_eq!(instance_host("host:metrics"), "host:metrics");
        assert_eq!(instance_host("["), "");
    }

    // -- panels and ring

    #[test]
    fn legend_and_empty_panels() {
        let series = Series {
            labels: [("container".to_string(), "app".to_string())].into(),
            points: vec![],
        };
        assert_eq!(Legend::Label("container").name(&series), "app");
        assert_eq!(Legend::Label("pod").name(&series), "");
        assert_eq!(Legend::Name("request").name(&series), "request");

        let line = |reference, points: &[(f64, f64)]| PlotSeries {
            name: "x".into(),
            reference,
            points: points.to_vec(),
        };
        let only_reference = PanelData {
            series: vec![line(true, &[(1.0, 2.0)]), line(false, &[])],
            truncated: false,
        };
        assert!(only_reference.is_empty());
        let with_usage = PanelData {
            series: vec![line(false, &[(1.0, 2.0)])],
            truncated: false,
        };
        assert!(!with_usage.is_empty());
        assert!(PanelData {
            series: vec![],
            truncated: false
        }
        .is_empty());
    }

    #[test]
    fn ring_keeps_the_newest() {
        let mut ring = SampleRing::new(3);
        assert!(ring.is_empty() && ring.last().is_none() && ring.value_range().is_none());
        for i in 0..5 {
            ring.push(f64::from(i) * 15.0, f64::from(i));
        }
        assert_eq!((ring.len(), ring.capacity()), (3, 3));
        assert_eq!(ring.to_vec(), [(30.0, 2.0), (45.0, 3.0), (60.0, 4.0)]);
        assert_eq!(ring.iter().map(|(t, _)| t).collect::<Vec<_>>(), [30.0, 45.0, 60.0]);
        assert_eq!(ring.last(), Some((60.0, 4.0)));
        assert_eq!(ring.value_range(), Some((2.0, 4.0)));
        ring.clear();
        assert!(ring.is_empty());
    }

    #[test]
    fn ring_rejects_bad_samples() {
        let mut ring = SampleRing::new(0);
        assert_eq!(ring.capacity(), 1);
        let mut ring2 = SampleRing::new(4);
        ring2.push(10.0, 1.0);
        ring2.push(5.0, 9.0); // older than the newest
        ring2.push(10.0, 2.0); // same moment: replaces
        ring2.push(f64::NAN, 1.0);
        ring2.push(11.0, f64::INFINITY);
        ring2.push(11.0, f64::NAN);
        assert_eq!(ring2.to_vec(), [(10.0, 2.0)]);
        ring.push(1.0, 1.0);
        ring.push(2.0, 2.0);
        assert_eq!(ring.to_vec(), [(2.0, 2.0)]);
    }
}
