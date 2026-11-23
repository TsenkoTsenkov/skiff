//! One-shot operations on objects: fetch, edit, delete, scale, restart …

use http::Method;
use kube::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::kinds::{Discovered, ResourceKey};
use crate::util::{friendly, get_request, request};

const MERGE_PATCH: &str = "application/merge-patch+json";
const APPLY_PATCH: &str = "application/apply-patch+yaml";
const JSON: &str = "application/json";

/// A fetched object in the forms the UI needs.
#[derive(Clone, Debug)]
pub struct ObjectData {
    pub value: Value,
    /// YAML with `managedFields` removed.
    pub yaml: String,
    pub rv: String,
}

fn to_object_data(mut value: Value) -> Result<ObjectData, String> {
    if let Some(meta) = value.get_mut("metadata").and_then(|m| m.as_object_mut()) {
        meta.remove("managedFields");
    }
    let rv = value
        .pointer("/metadata/resourceVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let yaml = serde_yaml::to_string(&value).map_err(|e| format!("cannot render YAML: {e}"))?;
    Ok(ObjectData { value, yaml, rv })
}

/// The most one response body (a list page, an object) may be. Far beyond
/// anything a healthy API server sends; a broken or hostile one must not be
/// able to fill our memory. With gzip this counts the decompressed size.
pub(crate) const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

/// Why a response body could not be collected.
#[derive(Debug)]
pub(crate) enum BodyError {
    /// The body is larger than the limit; reading stopped there.
    TooLarge { limit: usize },
    Read(kube::Error),
}

impl BodyError {
    pub(crate) fn message(&self) -> String {
        match self {
            BodyError::TooLarge { limit } => format!(
                "the server's response is larger than {} MiB; narrow it down (a namespace or a filter) and try again",
                limit / (1024 * 1024)
            ),
            BodyError::Read(e) => friendly(e),
        }
    }
}

/// Collects a response body, giving up as soon as it exceeds `limit` bytes
/// (instead of buffering whatever the server chooses to send).
pub(crate) async fn collect_capped(mut body: kube::client::Body, limit: usize) -> Result<Vec<u8>, BodyError> {
    use http_body_util::BodyExt;
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        // (Trailers carry no data.)
        if let Ok(data) = frame.map_err(BodyError::Read)?.into_data() {
            if data.len() > limit - out.len() {
                return Err(BodyError::TooLarge { limit });
            }
            out.extend_from_slice(&data);
        }
    }
    Ok(out)
}

/// A response that was not a success: the HTTP status and the body, which
/// normally is a `Status` object describing the failure.
struct Refused {
    status: http::StatusCode,
    body: Vec<u8>,
}

impl Refused {
    /// The failure in the words `friendly` uses for the same error.
    fn message(&self) -> String {
        if let Ok(st) = serde_json::from_slice::<kube::core::Status>(&self.body) {
            if !st.message.is_empty() || !st.reason.is_empty() {
                let st = if st.code == 0 { st.with_code(self.status.as_u16()) } else { st };
                return friendly(&kube::Error::Api(Box::new(st)));
            }
        }
        // Not every error is a Status: a proxy or ingress in front of the
        // API server answers in plain text or HTML.
        let text = String::from_utf8_lossy(&self.body);
        let text: String = text.trim().chars().take(300).collect();
        if text.is_empty() {
            format!("request failed ({})", self.status.as_u16())
        } else {
            format!("{text} ({})", self.status.as_u16())
        }
    }
}

enum SendError {
    /// The server answered, with an error.
    Refused(Refused),
    /// No (complete) answer.
    Other(String),
}

impl SendError {
    fn message(&self) -> String {
        match self {
            SendError::Refused(r) => r.message(),
            SendError::Other(msg) => msg.clone(),
        }
    }
}

/// Sends a request and returns the status and body of a successful answer.
/// The body is read with a size limit, whatever the outcome.
async fn send_raw(client: &Client, req: http::Request<Vec<u8>>) -> Result<(http::StatusCode, Vec<u8>), SendError> {
    let res = client
        .send(req.map(kube::client::Body::from))
        .await
        .map_err(|e| SendError::Other(friendly(&e)))?;
    let status = res.status();
    let body = collect_capped(res.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|e| SendError::Other(e.message()))?;
    if status.is_success() {
        Ok((status, body))
    } else {
        Err(SendError::Refused(Refused { status, body }))
    }
}

/// Sends a request and parses the answer.
async fn send_as<T: serde::de::DeserializeOwned>(client: &Client, req: http::Request<Vec<u8>>) -> Result<T, String> {
    let (_, body) = send_raw(client, req).await.map_err(|e| e.message())?;
    serde_json::from_slice(&body).map_err(|e| format!("unexpected response: {e}"))
}

async fn send(client: &Client, req: http::Request<Vec<u8>>) -> Result<Value, String> {
    send_as(client, req).await
}

/// A port number from server-supplied JSON; `None` unless it is a number
/// that fits a port (never silently wrapped around).
fn port_of(v: &Value) -> Option<u16> {
    u16::try_from(v.as_u64()?).ok()
}

pub async fn get_object(client: &Client, key: &ResourceKey, ns: &str, name: &str) -> Result<ObjectData, String> {
    let req = get_request(&key.object_path(ns, name), None)?;
    to_object_data(send(client, req).await?)
}

/// Replaces an object with edited YAML. The YAML's `resourceVersion` makes
/// this fail with a conflict if the object changed in the meantime.
pub async fn replace_object(
    client: &Client,
    key: &ResourceKey,
    ns: &str,
    name: &str,
    yaml: &str,
) -> Result<ObjectData, String> {
    let value: Value = serde_yaml::from_str(yaml).map_err(|e| format!("invalid YAML: {e}"))?;
    let edited_name = value.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("");
    if edited_name != name {
        return Err(format!("metadata.name cannot be changed (was {name:?}, now {edited_name:?})"));
    }
    let body = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    let req = request(Method::PUT, &key.object_path(ns, name), body, Some(JSON), None)?;
    to_object_data(send(client, req).await?)
}

pub async fn delete_object(
    client: &Client,
    key: &ResourceKey,
    ns: &str,
    name: &str,
    force: bool,
) -> Result<(), String> {
    let mut opts = json!({"propagationPolicy": "Background"});
    if force {
        opts["gracePeriodSeconds"] = json!(0);
    }
    let req = request(
        Method::DELETE,
        &key.object_path(ns, name),
        serde_json::to_vec(&opts).unwrap_or_default(),
        Some(JSON),
        None,
    )?;
    send(client, req).await.map(|_| ())
}

pub async fn merge_patch(client: &Client, path: &str, patch: &Value) -> Result<(), String> {
    let req = request(
        Method::PATCH,
        path,
        serde_json::to_vec(patch).map_err(|e| e.to_string())?,
        Some(MERGE_PATCH),
        None,
    )?;
    send(client, req).await.map(|_| ())
}

pub async fn scale(client: &Client, key: &ResourceKey, ns: &str, name: &str, replicas: i32) -> Result<(), String> {
    let path = format!("{}/scale", key.object_path(ns, name));
    merge_patch(client, &path, &json!({"spec": {"replicas": replicas}})).await
}

/// `kubectl rollout restart`: bumps a pod-template annotation.
pub async fn restart(client: &Client, key: &ResourceKey, ns: &str, name: &str) -> Result<(), String> {
    let now = jiff::Timestamp::now().to_string();
    let patch = json!({"spec": {"template": {"metadata": {"annotations": {
        "kubectl.kubernetes.io/restartedAt": now
    }}}}});
    merge_patch(client, &key.object_path(ns, name), &patch).await
}

pub async fn cordon(client: &Client, node: &str, unschedulable: bool) -> Result<(), String> {
    let key = crate::kinds::nodes();
    let value = if unschedulable { json!(true) } else { Value::Null };
    merge_patch(client, &key.object_path("", node), &json!({"spec": {"unschedulable": value}})).await
}

pub async fn suspend_cronjob(client: &Client, key: &ResourceKey, ns: &str, name: &str, suspend: bool) -> Result<(), String> {
    merge_patch(client, &key.object_path(ns, name), &json!({"spec": {"suspend": suspend}})).await
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub init: bool,
    /// Declared container ports as (name, port).
    pub ports: Vec<(String, u16)>,
}

/// Containers of a pod, main containers first.
pub fn containers_of(pod: &Value) -> Vec<ContainerInfo> {
    let mut out = Vec::new();
    for (field, init) in [("containers", false), ("initContainers", true), ("ephemeralContainers", false)] {
        let Some(list) = pod.pointer(&format!("/spec/{field}")).and_then(|v| v.as_array()) else { continue };
        for c in list {
            let ports = c
                .get("ports")
                .and_then(|p| p.as_array())
                .map(|ports| {
                    ports
                        .iter()
                        .filter_map(|p| {
                            let port = port_of(p.get("containerPort")?)?;
                            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                            Some((name, port))
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(ContainerInfo {
                name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                image: c.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                init,
                ports,
            });
        }
    }
    out
}

pub async fn pod_containers(client: &Client, ns: &str, pod: &str) -> Result<Vec<ContainerInfo>, String> {
    let key = crate::kinds::pods();
    let req = get_request(&key.object_path(ns, pod), None)?;
    Ok(containers_of(&send(client, req).await?))
}

/// Renders a label selector map as `k=v,k2=v2`.
pub fn selector_string(selector: &Value) -> Option<String> {
    let labels = selector.get("matchLabels").unwrap_or(selector).as_object()?;
    if labels.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = labels
        .iter()
        .filter_map(|(k, v)| Some(format!("{k}={}", v.as_str()?)))
        .collect();
    if let Some(exprs) = selector.get("matchExpressions").and_then(|e| e.as_array()) {
        for e in exprs {
            let key = e.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let values: Vec<&str> = e
                .get("values")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            match e.get("operator").and_then(|v| v.as_str()).unwrap_or("") {
                "In" => parts.push(format!("{key} in ({})", values.join(","))),
                "NotIn" => parts.push(format!("{key} notin ({})", values.join(","))),
                "Exists" => parts.push(key.to_string()),
                "DoesNotExist" => parts.push(format!("!{key}")),
                _ => {}
            }
        }
    }
    Some(parts.join(","))
}

/// The label selector that finds the pods of a workload or service object.
pub fn pod_selector_of(obj: &Value) -> Option<String> {
    let kind = obj.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let sel = obj.pointer("/spec/selector")?;
    match kind {
        // Service selectors are a plain map.
        "Service" | "ReplicationController" => selector_string(sel),
        _ => selector_string(sel),
    }
}

#[derive(Deserialize)]
struct PodListLite {
    #[serde(default)]
    items: Vec<Value>,
}

fn pod_is_ready(pod: &Value) -> bool {
    pod.pointer("/status/conditions")
        .and_then(|c| c.as_array())
        .map(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(|t| t.as_str()) == Some("Ready")
                    && c.get("status").and_then(|s| s.as_str()) == Some("True")
            })
        })
        .unwrap_or(false)
}

/// Picks a ready pod behind a service and resolves the service port to the
/// container port to forward to.
pub async fn resolve_service(client: &Client, ns: &str, service: &str, port: u16) -> Result<(String, u16), String> {
    let svc_key = crate::kinds::builtin("", "services").unwrap().key();
    let svc = send(client, get_request(&svc_key.object_path(ns, service), None)?).await?;
    let selector = pod_selector_of(&svc).ok_or("service has no selector")?;
    let target = svc
        .pointer("/spec/ports")
        .and_then(|p| p.as_array())
        .and_then(|ports| {
            ports
                .iter()
                .find(|p| p.get("port").and_then(|v| v.as_u64()) == Some(u64::from(port)))
        })
        .and_then(|p| p.get("targetPort").cloned())
        .unwrap_or(json!(port));

    let url = format!(
        "{}?labelSelector={}",
        crate::kinds::pods().list_path(Some(ns)),
        crate::util::enc(&selector)
    );
    let pods: PodListLite = send_as(client, get_request(&url, None)?).await?;
    let pod = pods
        .items
        .iter()
        .find(|p| pod_is_ready(p))
        .or(pods.items.first())
        .ok_or("no pods match the service selector")?;
    let pod_name = pod.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let remote = match &target {
        n @ Value::Number(_) => {
            port_of(n).ok_or_else(|| format!("service {service} has an invalid target port: {n}"))?
        }
        Value::String(name) => containers_of(pod)
            .iter()
            .flat_map(|c| c.ports.iter())
            .find(|(n, _)| n == name)
            .map(|(_, p)| *p)
            .ok_or_else(|| format!("named port {name:?} not found on pod {pod_name}"))?,
        _ => port,
    };
    Ok((pod_name, remote))
}

/// Ports that can be forwarded to on a pod (declared container ports) or a
/// service (its service ports), as (name, port).
pub async fn forwardable_ports(client: &Client, service: bool, ns: &str, name: &str) -> Result<Vec<(String, u16)>, String> {
    if service {
        let key = crate::kinds::builtin("", "services").unwrap().key();
        let svc = send(client, get_request(&key.object_path(ns, name), None)?).await?;
        Ok(svc
            .pointer("/spec/ports")
            .and_then(|p| p.as_array())
            .map(|ports| {
                ports
                    .iter()
                    .filter_map(|p| {
                        let port = port_of(p.get("port")?)?;
                        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        Some((name, port))
                    })
                    .collect()
            })
            .unwrap_or_default())
    } else {
        Ok(pod_containers(client, ns, name)
            .await?
            .into_iter()
            .flat_map(|c| c.ports.into_iter())
            .collect())
    }
}

/// Containers to stream logs from for a pod or a workload (all pods matched
/// by its selector). At most `MAX_LOG_PODS` pods are followed.
pub async fn log_targets(
    client: &Client,
    key: &ResourceKey,
    ns: &str,
    name: &str,
) -> Result<Vec<crate::logs::LogTarget>, String> {
    const MAX_LOG_PODS: usize = 20;
    let pods_key = crate::kinds::pods();
    let pods: Vec<Value> = if key.is("", "pods") {
        vec![send(client, get_request(&pods_key.object_path(ns, name), None)?).await?]
    } else {
        let obj = send(client, get_request(&key.object_path(ns, name), None)?).await?;
        let selector = pod_selector_of(&obj).ok_or("this object has no pod selector")?;
        let url = format!(
            "{}?labelSelector={}&limit={}",
            pods_key.list_path(Some(ns)),
            crate::util::enc(&selector),
            MAX_LOG_PODS
        );
        let list: PodListLite = send_as(client, get_request(&url, None)?).await?;
        list.items
    };
    let mut targets = Vec::new();
    for pod in &pods {
        let pod_name = pod.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("");
        for c in containers_of(pod).into_iter().filter(|c| !c.init) {
            targets.push(crate::logs::LogTarget {
                namespace: ns.to_string(),
                pod: pod_name.to_string(),
                container: c.name,
            });
        }
    }
    if targets.is_empty() {
        return Err("no pods found".into());
    }
    Ok(targets)
}

/// How long the API server may take over one request before it counts as
/// not answering.
pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// A field a server-side apply would have to take over from whoever set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyConflict {
    /// The object, as in the report: `deployments.apps/web`.
    pub object: String,
    /// Path of the field, e.g. `.spec.replicas`.
    pub field: String,
    /// The field manager that owns it (`helm`, `argocd-controller`, ...).
    pub manager: String,
}

/// Why `apply_manifest_with` did not apply everything.
#[derive(Clone, Debug, Default)]
pub struct ApplyFailure {
    /// What happened to each document, failures included (as in the `Ok`
    /// report).
    pub report: Vec<String>,
    /// Fields owned by other managers. When not empty, applying again with
    /// `force` takes them over.
    pub conflicts: Vec<ApplyConflict>,
}

impl ApplyFailure {
    /// Whether applying again with `force` could succeed where this failed.
    pub fn is_conflict(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.report.join("\n"))
    }
}

/// Part of every report line about a field conflict (see
/// `is_apply_conflict`).
const CONFLICT_PHRASE: &str = "owned by another manager";

/// Whether an error text from `apply_manifest` (an `ApplyFailure` rendered
/// as text) reports field conflicts, i.e. whether offering "Apply anyway"
/// (apply with `force`) makes sense.
pub fn is_apply_conflict(error: &str) -> bool {
    error.contains(CONFLICT_PHRASE)
}

/// The field conflicts in the body of a 409 answer to an apply; empty if it
/// is some other kind of conflict.
fn conflicts_in(status: &Value, object: &str) -> Vec<ApplyConflict> {
    let Some(causes) = status.pointer("/details/causes").and_then(|c| c.as_array()) else { return Vec::new() };
    causes
        .iter()
        .filter(|c| c.get("reason").and_then(|r| r.as_str()) == Some("FieldManagerConflict"))
        .map(|c| {
            // The message reads: conflict with "manager" using apps/v1
            let message = c.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let manager = message.split('"').nth(1).filter(|m| !m.is_empty()).unwrap_or("unknown");
            ApplyConflict {
                object: object.to_string(),
                field: c.get("field").and_then(|f| f.as_str()).unwrap_or("").to_string(),
                manager: manager.to_string(),
            }
        })
        .collect()
}

/// The report entry for an object that was not applied because of field
/// conflicts.
fn conflict_report(label: &str, conflicts: &[ApplyConflict]) -> String {
    const LISTED: usize = 20;
    let n = conflicts.len();
    let mut out = if n == 1 {
        format!("{label}: not applied, 1 field is {CONFLICT_PHRASE}:")
    } else {
        format!("{label}: not applied, {n} fields are {CONFLICT_PHRASE}:")
    };
    for c in conflicts.iter().take(LISTED) {
        out.push_str(&format!("\n    {} ({})", c.field, c.manager));
    }
    if n > LISTED {
        out.push_str(&format!("\n    and {} more", n - LISTED));
    }
    out
}

/// `apply_manifest_with` without `force`, with the failure as text: a
/// conflict is reported, never overridden. `is_apply_conflict` recognises
/// such a report.
pub async fn apply_manifest(client: &Client, discovered: &[Discovered], yaml: &str) -> Result<Vec<String>, String> {
    apply_manifest_with(client, discovered, yaml, false).await.map_err(|e| e.to_string())
}

/// `kubectl apply --server-side` for every document in a (multi-document)
/// manifest: fields in the manifest are set and owned by Skiff, everything
/// else on an existing object is left alone.
///
/// A field that another manager (Helm, Argo CD, a controller, ...) owns
/// with a different value is a conflict. Without `force` the object is then
/// left untouched and the failure lists the fields and their owners; with
/// `force` (`kubectl apply --server-side --force-conflicts`) Skiff takes the
/// fields over.
///
/// Every document is attempted. `Ok` lists what happened to each; if any
/// document failed, `Err` carries the same per-document report including
/// the failures. Each request has its own time limit, so a large manifest on
/// a slow connection is not cut off half-way; only when the server stops
/// answering are the remaining documents left alone, and the report says so.
pub async fn apply_manifest_with(
    client: &Client,
    discovered: &[Discovered],
    yaml: &str,
    force: bool,
) -> Result<Vec<String>, ApplyFailure> {
    let fail = |message: &str| ApplyFailure { report: vec![message.to_string()], conflicts: Vec::new() };
    // Parse everything up front: the YAML parser is not `Send` and must not
    // live across an await.
    let mut docs = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(yaml) {
        let value = Value::deserialize(doc).map_err(|e| fail(&format!("invalid YAML: {e}")))?;
        if value.is_null() {
            continue;
        }
        // `kubectl get -o yaml` of several objects produces a List.
        let is_list = value.get("kind").and_then(|k| k.as_str()).is_some_and(|k| k.ends_with("List"))
            && value.get("items").is_some_and(|i| i.is_array());
        match value {
            Value::Object(mut map) if is_list => {
                if let Some(Value::Array(items)) = map.remove("items") {
                    docs.extend(items.into_iter().filter(|i| !i.is_null()));
                }
            }
            other => docs.push(other),
        }
    }
    if docs.is_empty() {
        return Err(fail("manifest is empty"));
    }
    let mut report = Vec::new();
    let mut conflicts = Vec::new();
    let mut failed = false;
    let total = docs.len();
    for (index, value) in docs.into_iter().enumerate() {
        let api_version = value.get("apiVersion").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = value.pointer("/metadata/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if api_version.is_empty() || kind.is_empty() || name.is_empty() {
            report.push("skipped a document without apiVersion, kind and metadata.name".to_string());
            failed = true;
            continue;
        }
        // Match on group and kind, then address the object at the version
        // the manifest asks for: discovery only lists each group's preferred
        // version, but the server serves others too (autoscaling/v1, ...).
        let (group, version) = match api_version.split_once('/') {
            Some((g, v)) => (g, v),
            None => ("", api_version.as_str()),
        };
        let key = discovered
            .iter()
            .map(|d| &d.key)
            .find(|k| *k.kind == *kind && *k.group == *group)
            .cloned()
            .or_else(|| crate::kinds::builtin_by_kind(&kind).map(|b| b.key()).filter(|k| &*k.group == group))
            .map(|k| ResourceKey::new(group, version, &k.plural, &k.kind, k.namespaced));
        let Some(key) = key else {
            report.push(format!("{kind}/{name}: unknown resource type {api_version}/{kind}"));
            failed = true;
            continue;
        };
        // Like kubectl: an object without a namespace goes to the namespace
        // of the kubeconfig context.
        let ns = if key.namespaced {
            value
                .pointer("/metadata/namespace")
                .and_then(|v| v.as_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(client.default_namespace())
                .to_string()
        } else {
            String::new()
        };
        let label = format!("{}/{}", key.id(), name);
        let mut url = format!("{}?fieldManager=skiff", key.object_path(&ns, &name));
        if force {
            url.push_str("&force=true");
        }
        let body = match serde_json::to_vec(&value) {
            Ok(b) => b,
            Err(e) => {
                report.push(format!("{label}: {e}"));
                failed = true;
                continue;
            }
        };
        let outcome = match request(Method::PATCH, &url, body, Some(APPLY_PATCH), None) {
            Ok(req) => match tokio::time::timeout(REQUEST_TIMEOUT, send_raw(client, req)).await {
                Ok(outcome) => outcome,
                Err(_) => {
                    report.push(format!(
                        "{label}: no response from the API server within {} s",
                        REQUEST_TIMEOUT.as_secs()
                    ));
                    let rest = total - index - 1;
                    if rest > 0 {
                        report.push(format!("{rest} more {} not attempted", if rest == 1 { "document" } else { "documents" }));
                    }
                    failed = true;
                    break;
                }
            },
            Err(e) => Err(SendError::Other(e)),
        };
        match outcome {
            Ok((status, _)) if status == http::StatusCode::CREATED => report.push(format!("{label} created")),
            Ok(_) => report.push(format!("{label} configured")),
            Err(e) => {
                failed = true;
                let found = match &e {
                    SendError::Refused(r) if r.status == http::StatusCode::CONFLICT => {
                        serde_json::from_slice::<Value>(&r.body).map(|v| conflicts_in(&v, &label)).unwrap_or_default()
                    }
                    _ => Vec::new(),
                };
                if found.is_empty() {
                    report.push(format!("{label}: {}", e.message()));
                } else {
                    report.push(conflict_report(&label, &found));
                    conflicts.extend(found);
                }
            }
        }
    }
    if failed {
        Err(ApplyFailure { report, conflicts })
    } else {
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_strips_managed_fields() {
        let v = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"a","resourceVersion":"7","managedFields":[{"manager":"kubectl"}]},"data":{"k":"line1\nline2\n"}});
        let d = to_object_data(v).unwrap();
        assert_eq!(d.rv, "7");
        assert!(!d.yaml.contains("managedFields"));
        assert!(d.yaml.contains("kind: ConfigMap"));
        // Multi-line strings stay readable.
        assert!(d.yaml.contains("k: |"), "{}", d.yaml);
        let back: Value = serde_yaml::from_str(&d.yaml).unwrap();
        assert_eq!(back["data"]["k"], "line1\nline2\n");
    }

    #[test]
    fn selectors() {
        let dep = json!({"kind":"Deployment","spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"tier","operator":"In","values":["a","b"]}]}}});
        assert_eq!(pod_selector_of(&dep).unwrap(), "app=web,tier in (a,b)");
        let svc = json!({"kind":"Service","spec":{"selector":{"app":"web","role":"api"}}});
        assert_eq!(pod_selector_of(&svc).unwrap(), "app=web,role=api");
        let headless = json!({"kind":"Service","spec":{}});
        assert!(pod_selector_of(&headless).is_none());
    }

    #[test]
    fn containers() {
        let pod = json!({"spec":{"initContainers":[{"name":"init","image":"busybox"}],"containers":[{"name":"app","image":"nginx:1","ports":[{"name":"http","containerPort":8080}]}]}});
        let c = containers_of(&pod);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "app");
        assert!(!c[0].init);
        assert_eq!(c[0].ports, vec![("http".to_string(), 8080)]);
        assert!(c[1].init);
    }

    #[test]
    fn ports_are_checked_not_wrapped() {
        // 65536 + 80 would wrap to port 80.
        let pod = json!({"spec":{"containers":[{"name":"app","ports":[
            {"name":"ok","containerPort":8080},
            {"name":"wraps","containerPort":65616},
            {"name":"negative","containerPort":-1},
            {"name":"text","containerPort":"80"}]}]}});
        assert_eq!(containers_of(&pod)[0].ports, vec![("ok".to_string(), 8080)]);
        assert_eq!(port_of(&json!(65535)), Some(65535));
        assert_eq!(port_of(&json!(65536)), None);
        assert_eq!(port_of(&json!(1.5)), None);
    }

    #[test]
    fn conflicts_are_read_from_the_status() {
        let status = json!({"kind":"Status","status":"Failure","reason":"Conflict","code":409,
            "message":"Apply failed with 2 conflicts: ...",
            "details":{"causes":[
                {"reason":"FieldManagerConflict","message":"conflict with \"helm\" using apps/v1","field":".spec.replicas"},
                {"reason":"FieldManagerConflict","message":"conflict with \"kube-controller-manager\" with subresource \"status\" using apps/v1","field":".status.x"},
                {"reason":"SomethingElse","message":"other","field":".a"}]}});
        let found = conflicts_in(&status, "deployments.apps/web");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].field.as_str(), found[0].manager.as_str()), (".spec.replicas", "helm"));
        assert_eq!(found[1].manager, "kube-controller-manager");
        let text = conflict_report("deployments.apps/web", &found);
        assert!(is_apply_conflict(&text), "{text}");
        assert!(text.contains(".spec.replicas (helm)"), "{text}");
        // An ordinary conflict (a stale resourceVersion) is not one of these.
        let plain = json!({"kind":"Status","code":409,"reason":"Conflict","message":"the object has been modified"});
        assert!(conflicts_in(&plain, "x").is_empty());
        assert!(!is_apply_conflict("configmaps/a: the object has been modified (409)"));
    }

    /// A minimal API server: reads one request per connection, records its
    /// request line and answers with what `answer` makes of it.
    async fn fake_api(
        answer: impl Fn(&str) -> (u16, String) + Send + Sync + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (log, answer) = (seen.clone(), std::sync::Arc::new(answer));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (log, answer) = (log.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut req = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // The head, then as much body as it announces.
                    let body_at = loop {
                        if let Some(i) = req.windows(4).position(|x| x == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&req[..body_at]).to_string();
                    let length = head
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    while req.len() < body_at + length {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let line = head.lines().next().unwrap_or("").to_string();
                    let (code, body) = answer(&line);
                    log.lock().unwrap().push(line);
                    let resp = format!(
                        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        let client = Client::try_from(kube::Config::new(format!("http://{addr}").parse().unwrap())).unwrap();
        (client, seen)
    }

    const MANIFEST: &str = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: settings\n  namespace: shop\ndata:\n  mode: fast\n";

    /// The server refuses an apply that would take a field from another
    /// manager unless it is forced.
    fn conflicting(request_line: &str) -> (u16, String) {
        if request_line.contains("force=true") {
            (200, r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings"}}"#.to_string())
        } else {
            let status = json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Conflict","code":409,
                "message":"Apply failed with 1 conflict: conflict with \"helm\" using v1: .data.mode",
                "details":{"causes":[{"reason":"FieldManagerConflict","message":"conflict with \"helm\" using v1","field":".data.mode"}]}});
            (409, status.to_string())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_without_force_reports_the_conflict() {
        let (client, seen) = fake_api(conflicting).await;
        let failure = apply_manifest_with(&client, &[], MANIFEST, false).await.unwrap_err();
        assert!(failure.is_conflict());
        assert_eq!(
            failure.conflicts,
            vec![ApplyConflict { object: "configmaps/settings".into(), field: ".data.mode".into(), manager: "helm".into() }]
        );
        let text = failure.to_string();
        assert!(is_apply_conflict(&text), "{text}");
        assert!(text.contains(".data.mode (helm)"), "{text}");
        // The entry point without the flag does not force either, and its
        // text error is recognisable.
        let text = apply_manifest(&client, &[], MANIFEST).await.unwrap_err();
        assert!(is_apply_conflict(&text), "{text}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        for line in seen.iter() {
            assert!(line.starts_with("PATCH /api/v1/namespaces/shop/configmaps/settings?fieldManager=skiff "), "{line}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_with_force_takes_the_fields() {
        let (client, seen) = fake_api(conflicting).await;
        let report = apply_manifest_with(&client, &[], MANIFEST, true).await.unwrap();
        assert_eq!(report, vec!["configmaps/settings configured".to_string()]);
        assert!(seen.lock().unwrap()[0].contains("fieldManager=skiff&force=true "));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn errors_keep_their_wording_and_bodies_are_capped() {
        let (client, _) = fake_api(|line| {
            if line.contains("/missing") {
                (404, r#"{"kind":"Status","status":"Failure","message":"configmaps \"missing\" not found","reason":"NotFound","code":404}"#.to_string())
            } else if line.contains("/html") {
                (502, "<html>Bad Gateway</html>".to_string())
            } else {
                (200, format!(r#"{{"data":"{}"}}"#, "x".repeat(4096)))
            }
        })
        .await;
        let key = crate::kinds::builtin("", "configmaps").unwrap().key();
        let err = get_object(&client, &key, "x", "missing").await.unwrap_err();
        assert_eq!(err, "configmaps \"missing\" not found (404)");
        let err = get_object(&client, &key, "x", "html").await.unwrap_err();
        assert_eq!(err, "<html>Bad Gateway</html> (502)");
        assert!(get_object(&client, &key, "x", "big").await.is_ok());
        // The same answer against a small limit is refused, not collected.
        let req = get_request(&key.object_path("x", "big"), None).unwrap();
        let res = client.send(req.map(kube::client::Body::from)).await.unwrap();
        let err = collect_capped(res.into_body(), 1024).await.unwrap_err();
        assert!(matches!(err, BodyError::TooLarge { limit: 1024 }), "{err:?}");
        assert!(err.message().contains("larger than"), "{}", err.message());
    }
}
