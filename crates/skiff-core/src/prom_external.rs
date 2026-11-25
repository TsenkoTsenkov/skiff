//! Prometheus outside the cluster: a managed service (Azure Monitor managed
//! service for Prometheus, Amazon Managed Service for Prometheus, Google
//! Cloud Managed Service for Prometheus, Grafana Cloud, …) or any
//! Prometheus-compatible HTTP API Skiff can reach directly.
//!
//! Skiff stores no secret for it. A credential is asked for when a query
//! needs one, from the tool you already sign in with (`az`, `aws`,
//! `gcloud`), from a command of your choice, an environment variable or a
//! file: the same way a kubeconfig's exec plugin hands kubectl a token. A
//! token from a command is kept in memory until shortly before it expires
//! and is asked for again when the service refuses it.
//!
//! Requests go out through the same HTTP stack as the cluster connection:
//! TLS is checked against the system's trust store, and `HTTPS_PROXY` /
//! `NO_PROXY` apply.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use http::header::{HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, HOST};
use kube::Client;

use crate::ops::{collect_capped, BodyError};
use crate::prom::{classify_response, transport_error, PromError};
use crate::util::{enc, enc_segment};

/// How long a credential command may take. `az` and `gcloud` take a second
/// or two; a command that asks for a browser sign-in takes longer.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// What a credential command may print.
const MAX_COMMAND_OUTPUT: usize = 256 * 1024;
/// A token from a command that does not say when it expires is used for
/// this long.
const UNDATED_TOKEN_SECS: u64 = 5 * 60;
/// A dated token is asked for again this long before it expires.
const EXPIRY_MARGIN_SECS: i64 = 5 * 60;
/// The Entra ID resource of Azure Monitor's managed Prometheus.
pub const AZURE_PROMETHEUS_RESOURCE: &str = "https://prometheus.monitor.azure.com";

// ---------------------------------------------------------------------------
// Configuration

/// Where a secret comes from. Nothing secret is ever written to Skiff's
/// config: only how to obtain it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretSource {
    /// A program and its arguments (run without a shell); what it prints
    /// is the secret.
    Command(Vec<String>),
    /// An environment variable of Skiff's process.
    Env(String),
    /// A file, read each time (so a rotated token is picked up).
    File(PathBuf),
}

impl SecretSource {
    fn validate(&self, what: &str) -> Result<(), String> {
        match self {
            SecretSource::Command(argv) if argv.first().is_none_or(|p| p.trim().is_empty()) => {
                Err(format!("{what}: the command is empty"))
            }
            SecretSource::Env(name) if name.trim().is_empty() || name.contains('=') => {
                Err(format!("{what}: {name:?} is not an environment variable name"))
            }
            SecretSource::File(path) if path.as_os_str().is_empty() => Err(format!("{what}: the file name is empty")),
            _ => Ok(()),
        }
    }
}

/// How Skiff signs in to a Prometheus outside the cluster.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExternalAuth {
    /// No credentials (a Prometheus on a private network, or one behind a
    /// VPN).
    #[default]
    None,
    /// `Authorization: Bearer <token>`.
    Bearer(SecretSource),
    /// `Authorization: Basic …` (Grafana Cloud: the instance ID and an
    /// access policy token).
    Basic { username: String, password: SecretSource },
    /// Azure Monitor managed service for Prometheus: an Entra ID token from
    /// the Azure CLI (`az account get-access-token`).
    Azure {
        tenant: Option<String>,
        /// Entra ID resource; [`AZURE_PROMETHEUS_RESOURCE`] when not set
        /// (other values for sovereign clouds).
        resource: Option<String>,
    },
    /// Google Cloud Managed Service for Prometheus: an OAuth token from
    /// `gcloud auth print-access-token`.
    Gcp,
    /// Amazon Managed Service for Prometheus: requests signed with AWS
    /// Signature Version 4, with credentials from the AWS CLI
    /// (`aws configure export-credentials`) or a `credential_process`-style
    /// command.
    Aws {
        /// Inferred from an `aps-workspaces.<region>.amazonaws.com` address
        /// when not set.
        region: Option<String>,
        profile: Option<String>,
        credentials_command: Option<Vec<String>>,
    },
}

impl ExternalAuth {
    /// The word used for this kind of sign-in in messages and the config.
    pub fn kind(&self) -> &'static str {
        match self {
            ExternalAuth::None => "none",
            ExternalAuth::Bearer(_) => "bearer",
            ExternalAuth::Basic { .. } => "basic",
            ExternalAuth::Azure { .. } => "azure",
            ExternalAuth::Gcp => "gcp",
            ExternalAuth::Aws { .. } => "aws",
        }
    }

    /// What to check when the service refuses the credentials.
    fn refusal_hint(&self) -> &'static str {
        match self {
            ExternalAuth::None => "It needs credentials: set prometheus.auth in Skiff's config.",
            ExternalAuth::Bearer(_) | ExternalAuth::Basic { .. } => {
                "Check the credentials set in prometheus.auth in Skiff's config."
            }
            ExternalAuth::Azure { .. } => {
                "Run \"az login\" (with the tenant of the workspace), and check that your account has the \
                 Monitoring Data Reader role on the Azure Monitor workspace."
            }
            ExternalAuth::Gcp => {
                "Run \"gcloud auth login\", and check that your account has the Monitoring Viewer role in the project."
            }
            ExternalAuth::Aws { .. } => {
                "Check the AWS profile and region, and that the credentials allow aps:QueryMetrics \
                 (and aps:ListAlerts for alerts) on the workspace."
            }
        }
    }
}

/// A Prometheus-compatible HTTP API outside the cluster: the shape of
/// `clusters.<context>.prometheus` with a `url`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalEndpoint {
    /// Everything in front of `/api/v1`, e.g.
    /// `https://myworkspace-abcd.westeurope.prometheus.monitor.azure.com`.
    pub url: String,
    pub auth: ExternalAuth,
    /// Extra request headers that are not secret, such as
    /// `X-Scope-OrgID` for Mimir, Cortex or Thanos tenants.
    pub headers: Vec<(String, String)>,
    /// Label matchers added to every query, e.g. `cluster = "aks-prod"`:
    /// a managed service usually holds the metrics of many clusters.
    pub labels: Vec<(String, String)>,
}

/// The parts of a checked address.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedUrl {
    https: bool,
    /// `host` or `host:port`, as written.
    authority: String,
    host: String,
    /// The path in front of `/api/v1`, without a trailing slash ("" for none).
    path: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    let url = url.trim();
    let uri: http::Uri = url
        .parse()
        .map_err(|_| format!("prometheus.url {url:?} is not an address like \"https://prometheus.example.com\""))?;
    let https = match uri.scheme_str() {
        Some(s) if s.eq_ignore_ascii_case("https") => true,
        Some(s) if s.eq_ignore_ascii_case("http") => false,
        _ => return Err(format!("prometheus.url {url:?} must start with https:// (or http://)")),
    };
    let Some(authority) = uri.authority() else {
        return Err(format!("prometheus.url {url:?} has no host name"));
    };
    if authority.as_str().contains('@') {
        return Err("prometheus.url must not contain a user name or password; use prometheus.auth".into());
    }
    if uri.query().is_some() || url.contains('#') {
        return Err(format!("prometheus.url {url:?} must not have a query (?…) or fragment (#…)"));
    }
    let mut path = uri.path().trim_end_matches('/').to_string();
    // The address of the API itself was pasted: what is wanted is the part
    // in front of it.
    for suffix in ["/api/v1/query", "/api/v1"] {
        if let Some(stripped) = path.strip_suffix(suffix) {
            path = stripped.to_string();
            break;
        }
    }
    if path.split('/').any(|seg| seg == "." || seg == "..") {
        return Err(format!("prometheus.url {url:?} has a \".\" or \"..\" in its path"));
    }
    Ok(ParsedUrl {
        https,
        authority: authority.as_str().to_string(),
        host: authority.host().to_ascii_lowercase(),
        path,
    })
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1") || host.ends_with(".localhost")
}

/// A Prometheus label name.
fn is_label_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The region of an Amazon Managed Service for Prometheus address,
/// `aps-workspaces.<region>.amazonaws.com`.
fn aws_region_from_host(host: &str) -> Option<String> {
    let rest = host.strip_prefix("aps-workspaces.")?;
    let (region, domain) = rest.split_once('.')?;
    (domain.starts_with("amazonaws.com") && is_region(region)).then(|| region.to_string())
}

fn is_region(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl ExternalEndpoint {
    pub fn new(url: &str) -> Self {
        ExternalEndpoint { url: url.into(), ..Default::default() }
    }

    /// Checks a hand-written endpoint and says what to fix.
    pub fn validate(&self) -> Result<(), String> {
        let url = parse_url(&self.url)?;
        if !url.https && !is_loopback(&url.host) && self.auth != ExternalAuth::None {
            return Err(format!(
                "prometheus.url {:?} is plain http: Skiff sends credentials only over https (or to this computer)",
                self.url.trim()
            ));
        }
        match &self.auth {
            ExternalAuth::None | ExternalAuth::Gcp => {}
            ExternalAuth::Bearer(source) => source.validate("prometheus.auth")?,
            ExternalAuth::Basic { username, password } => {
                if username.is_empty() || username.contains(':') {
                    return Err("prometheus.auth.username must be set and must not contain \":\"".into());
                }
                password.validate("prometheus.auth")?;
            }
            ExternalAuth::Azure { tenant, resource } => {
                if tenant.as_deref().is_some_and(|t| t.trim().is_empty()) {
                    return Err("prometheus.auth.tenant is empty".into());
                }
                if let Some(r) = resource {
                    parse_url(r).map_err(|_| format!("prometheus.auth.resource {r:?} is not an https address"))?;
                }
            }
            ExternalAuth::Aws { region, profile, credentials_command } => {
                match region {
                    Some(r) if !is_region(r) => return Err(format!("prometheus.auth.region {r:?} is not an AWS region")),
                    Some(_) => {}
                    None if aws_region_from_host(&url.host).is_none() => {
                        return Err("prometheus.auth.region is needed: it could not be read from prometheus.url".into())
                    }
                    None => {}
                }
                if profile.as_deref().is_some_and(|p| p.trim().is_empty()) {
                    return Err("prometheus.auth.profile is empty".into());
                }
                if let Some(argv) = credentials_command {
                    SecretSource::Command(argv.clone()).validate("prometheus.auth.credentials_command")?;
                }
            }
        }
        for (name, value) in &self.headers {
            let parsed = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("prometheus.headers: {name:?} is not a header name"))?;
            if parsed == HOST || (parsed == AUTHORIZATION && self.auth != ExternalAuth::None) {
                return Err(format!("prometheus.headers: {name} is set by Skiff and cannot be given here"));
            }
            HeaderValue::from_str(value).map_err(|_| format!("prometheus.headers.{name}: the value has characters a header cannot hold"))?;
        }
        for (name, _) in &self.labels {
            if !is_label_name(name) {
                return Err(format!("prometheus.labels: {name:?} is not a label name"));
            }
        }
        Ok(())
    }

    /// `host/path`, for messages and the source line of the views.
    pub fn display(&self) -> String {
        match parse_url(&self.url) {
            Ok(u) => format!("{}{}", u.authority, u.path),
            Err(_) => self.url.trim().to_string(),
        }
    }

    /// The label matchers of [`Self::labels`], escaped, comma-separated
    /// (empty when there are none).
    pub fn scope(&self) -> String {
        self.labels
            .iter()
            .map(|(k, v)| format!("{k}=\"{}\"", crate::prom::escape_label_value(v)))
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ---------------------------------------------------------------------------
// Credentials

/// What one request is signed with.
#[derive(Clone)]
enum Credential {
    None,
    Header(HeaderValue),
    Aws(AwsCredentials),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

struct Cached {
    credential: Credential,
    until: Instant,
}

/// A program for messages: its name and arguments as typed.
fn shown(argv: &[String]) -> String {
    argv.join(" ")
}

/// The first lines of what a failed command printed, for a message.
fn first_lines(text: &str) -> String {
    let text: String = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(4).collect::<Vec<_>>().join(" ");
    let mut out: String = text.chars().take(400).collect();
    if text.chars().count() > 400 {
        out.push('…');
    }
    out
}

/// Runs a credential command and returns what it printed. It runs without
/// a shell and without input; it inherits Skiff's environment.
async fn run_command(argv: &[String]) -> Result<String, PromError> {
    let Some(program) = argv.first() else {
        return Err(PromError::Credentials("the credential command is empty".into()));
    };
    let Some(path) = crate::kubeconfig::find_executable(program) else {
        return Err(PromError::Credentials(format!(
            "\"{program}\" was not found. Install it (or give its full path), then try again."
        )));
    };
    let mut cmd = tokio::process::Command::new(path);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // No console window flashing up for each token.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = match tokio::time::timeout(COMMAND_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(PromError::Credentials(format!("\"{}\" could not be started: {e}", shown(argv)))),
        Err(_) => {
            return Err(PromError::Credentials(format!(
                "\"{}\" did not finish within {} s",
                shown(argv),
                COMMAND_TIMEOUT.as_secs()
            )))
        }
    };
    if !output.status.success() {
        let said = first_lines(&String::from_utf8_lossy(&output.stderr));
        let said = if said.is_empty() { first_lines(&String::from_utf8_lossy(&output.stdout)) } else { said };
        return Err(PromError::Credentials(format!(
            "\"{}\" failed{}",
            shown(argv),
            if said.is_empty() { String::new() } else { format!(": {said}") }
        )));
    }
    if output.stdout.len() > MAX_COMMAND_OUTPUT {
        return Err(PromError::Credentials(format!("\"{}\" printed more than a credential", shown(argv))));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|_| PromError::Credentials(format!("\"{}\" printed something that is not text", shown(argv))))
}

/// Reads a secret that is not cached: an environment variable or a file.
fn read_now(source: &SecretSource) -> Result<String, PromError> {
    let value = match source {
        SecretSource::Env(name) => std::env::var(name)
            .map_err(|_| PromError::Credentials(format!("the environment variable {name} is not set")))?,
        SecretSource::File(path) => {
            let path = expand_home(path);
            std::fs::read_to_string(&path)
                .map_err(|e| PromError::Credentials(format!("{} could not be read: {e}", path.display())))?
        }
        SecretSource::Command(_) => unreachable!("commands are cached"),
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(PromError::Credentials("the credential is empty".into()));
    }
    Ok(value)
}

fn expand_home(path: &std::path::Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else { return path.to_path_buf() };
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) => PathBuf::from(home).join(rest),
        None => path.to_path_buf(),
    }
}

fn bearer(token: &str) -> Result<HeaderValue, PromError> {
    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| PromError::Credentials("the token has characters a header cannot hold".into()))?;
    value.set_sensitive(true);
    Ok(value)
}

fn basic(username: &str, password: &str) -> Result<HeaderValue, PromError> {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    let mut value = HeaderValue::from_str(&format!("Basic {encoded}"))
        .map_err(|_| PromError::Credentials("the password has characters a header cannot hold".into()))?;
    value.set_sensitive(true);
    Ok(value)
}

/// How long a token that expires at `expires_unix` may be used from now.
fn lifetime_until(expires_unix: i64, now_unix: i64) -> Duration {
    let secs = (expires_unix - now_unix - EXPIRY_MARGIN_SECS).clamp(30, 12 * 3600);
    Duration::from_secs(secs as u64)
}

/// The access token and how long to use it, from the JSON of
/// `az account get-access-token`.
fn parse_azure_token(text: &str, now_unix: i64) -> Result<(String, Duration), PromError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| PromError::Credentials("az account get-access-token printed something other than JSON".into()))?;
    let token = v["accessToken"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| PromError::Credentials("az account get-access-token printed no accessToken".into()))?;
    // `expires_on` (Unix seconds) is in Azure CLI 2.54 and later; the older
    // `expiresOn` is a local time without a zone, so it is not used.
    let expires = match &v["expires_on"] {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    };
    let lifetime = match expires {
        Some(at) => lifetime_until(at, now_unix),
        None => Duration::from_secs(30 * 60),
    };
    Ok((token.to_string(), lifetime))
}

/// AWS credentials and how long to use them, from the JSON a
/// `credential_process` (or `aws configure export-credentials --format
/// process`) prints.
fn parse_aws_credentials(text: &str, now_unix: i64) -> Result<(AwsCredentials, Duration), PromError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| PromError::Credentials("the AWS credential command printed something other than JSON".into()))?;
    let field = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let (Some(access_key_id), Some(secret_access_key)) = (field("AccessKeyId"), field("SecretAccessKey")) else {
        return Err(PromError::Credentials(
            "the AWS credential command printed no AccessKeyId and SecretAccessKey".into(),
        ));
    };
    let lifetime = match field("Expiration").and_then(|e| e.parse::<jiff::Timestamp>().ok()) {
        Some(at) => lifetime_until(at.as_second(), now_unix),
        None => Duration::from_secs(15 * 60),
    };
    Ok((AwsCredentials { access_key_id, secret_access_key, session_token: field("SessionToken") }, lifetime))
}

// ---------------------------------------------------------------------------
// AWS Signature Version 4

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key);
    ring::hmac::sign(&key, data).as_ref().to_vec()
}

/// The headers that sign a GET request: `x-amz-date`, maybe
/// `x-amz-security-token`, and `authorization`. `path` is the path as
/// sent (already percent-encoded once) and `query` the query as sent,
/// already in canonical form (see [`canonical_query`]).
fn sigv4_headers(
    creds: &AwsCredentials,
    region: &str,
    service: &str,
    host: &str,
    path: &str,
    query: &str,
    now: jiff::Timestamp,
) -> Vec<(&'static str, String)> {
    let utc = now.to_zoned(jiff::tz::TimeZone::UTC);
    let amz_date = utc.strftime("%Y%m%dT%H%M%SZ").to_string();
    let date = &amz_date[..8];
    // Every service but S3 encodes each path segment a second time.
    let canonical_uri = if path.is_empty() {
        "/".to_string()
    } else {
        path.split('/').map(enc).collect::<Vec<_>>().join("/")
    };
    let mut canonical_headers = format!("host:{host}\nx-amz-date:{amz_date}\n");
    let mut signed_headers = "host;x-amz-date".to_string();
    if let Some(token) = &creds.session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_headers.push_str(";x-amz-security-token");
    }
    let canonical_request = format!(
        "GET\n{canonical_uri}\n{query}\n{canonical_headers}\n{signed_headers}\n{}",
        sha256_hex(b"")
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical_request.as_bytes()));
    let mut key = hmac(format!("AWS4{}", creds.secret_access_key).as_bytes(), date.as_bytes());
    for part in [region, service, "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));
    let mut headers = vec![("x-amz-date", amz_date)];
    if let Some(token) = &creds.session_token {
        headers.push(("x-amz-security-token", token.clone()));
    }
    headers.push((
        "authorization",
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            creds.access_key_id
        ),
    ));
    headers
}

/// The query string, sorted by name and value and encoded the way AWS
/// expects (RFC 3986, `%20` for a space). Prometheus does not mind the
/// order, so every request is sent in this form.
fn canonical_query(params: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = params.iter().map(|(k, v)| (enc(k), enc(v))).collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

// ---------------------------------------------------------------------------
// Client

/// A connection to a Prometheus outside the cluster. Cheap to share; the
/// credential is fetched once for all queries that need it at a time.
pub(crate) struct External {
    endpoint: ExternalEndpoint,
    url: ParsedUrl,
    client: Client,
    headers: Vec<(HeaderName, HeaderValue)>,
    aws_region: Option<String>,
    cached: tokio::sync::Mutex<Option<Cached>>,
}

impl External {
    /// Checks the endpoint and sets up its HTTP client (nothing is sent).
    pub(crate) async fn connect(endpoint: ExternalEndpoint) -> Result<External, PromError> {
        endpoint
            .validate()
            .map_err(|problem| PromError::Other(format!("The Prometheus set in Skiff's config can't be used: {problem}")))?;
        let url = parse_url(&endpoint.url).map_err(PromError::Other)?;
        let base = format!("{}://{}{}", if url.https { "https" } else { "http" }, url.authority, url.path);
        // A kubeconfig of one "cluster" at that address: the client then
        // follows HTTPS_PROXY and NO_PROXY exactly as the cluster
        // connections do.
        let kubeconfig = format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: prometheus\n  cluster:\n    server: {}\n\
             users:\n- name: prometheus\n  user: {{}}\ncontexts:\n- name: prometheus\n  context:\n    cluster: prometheus\n    user: prometheus\n\
             current-context: prometheus\n",
            serde_json::to_string(&base).unwrap_or_default()
        );
        let kc = kube::config::Kubeconfig::from_yaml(&kubeconfig)
            .map_err(|e| PromError::Other(format!("prometheus.url: {e}")))?;
        let mut config = kube::Config::from_custom_kubeconfig(kc, &kube::config::KubeConfigOptions::default())
            .await
            .map_err(|e| PromError::Other(format!("prometheus.url: {e}")))?;
        config.connect_timeout = Some(Duration::from_secs(8));
        let client = Client::try_from(config).map_err(|e| PromError::Other(format!("prometheus.url: {e}")))?;
        let headers = endpoint
            .headers
            .iter()
            .filter_map(|(k, v)| Some((HeaderName::from_bytes(k.as_bytes()).ok()?, HeaderValue::from_str(v).ok()?)))
            .collect();
        let aws_region = match &endpoint.auth {
            ExternalAuth::Aws { region, .. } => region.clone().or_else(|| aws_region_from_host(&url.host)),
            _ => None,
        };
        Ok(External { endpoint, url, client, headers, aws_region, cached: tokio::sync::Mutex::new(None) })
    }

    pub(crate) fn endpoint(&self) -> &ExternalEndpoint {
        &self.endpoint
    }

    /// Whether the credential comes from a command, so that asking again
    /// after a refusal may give a different one.
    fn refreshable(&self) -> bool {
        match &self.endpoint.auth {
            ExternalAuth::None => false,
            ExternalAuth::Bearer(s) | ExternalAuth::Basic { password: s, .. } => matches!(s, SecretSource::Command(_)),
            ExternalAuth::Azure { .. } | ExternalAuth::Gcp | ExternalAuth::Aws { .. } => true,
        }
    }

    /// The credential to sign the next request with; `fresh` skips the
    /// cached one (it was just refused).
    async fn credential(&self, fresh: bool) -> Result<Credential, PromError> {
        let auth = &self.endpoint.auth;
        // Secrets that cost nothing to read are read for every request.
        match auth {
            ExternalAuth::None => return Ok(Credential::None),
            ExternalAuth::Bearer(s @ (SecretSource::Env(_) | SecretSource::File(_))) => {
                return Ok(Credential::Header(bearer(&read_now(s)?)?))
            }
            ExternalAuth::Basic { username, password: s @ (SecretSource::Env(_) | SecretSource::File(_)) } => {
                return Ok(Credential::Header(basic(username, &read_now(s)?)?))
            }
            _ => {}
        }
        // One fetch at a time: the charts of a view ask concurrently.
        let mut cached = self.cached.lock().await;
        if !fresh {
            if let Some(c) = cached.as_ref().filter(|c| Instant::now() < c.until) {
                return Ok(c.credential.clone());
            }
        }
        let now = crate::now_unix();
        let (credential, lifetime) = match auth {
            ExternalAuth::Bearer(SecretSource::Command(argv)) => {
                (Credential::Header(bearer(&run_command(argv).await?)?), Duration::from_secs(UNDATED_TOKEN_SECS))
            }
            ExternalAuth::Basic { username, password: SecretSource::Command(argv) } => (
                Credential::Header(basic(username, &run_command(argv).await?)?),
                Duration::from_secs(UNDATED_TOKEN_SECS),
            ),
            ExternalAuth::Azure { tenant, resource } => {
                let mut argv: Vec<String> = ["az", "account", "get-access-token", "--output", "json", "--resource"]
                    .map(String::from)
                    .into();
                argv.push(resource.clone().unwrap_or_else(|| AZURE_PROMETHEUS_RESOURCE.to_string()));
                if let Some(tenant) = tenant {
                    argv.extend(["--tenant".to_string(), tenant.clone()]);
                }
                let (token, lifetime) = parse_azure_token(&run_command(&argv).await?, now)?;
                (Credential::Header(bearer(&token)?), lifetime)
            }
            ExternalAuth::Gcp => {
                let argv = ["gcloud", "auth", "print-access-token"].map(String::from);
                // Google's access tokens last an hour.
                (Credential::Header(bearer(&run_command(&argv).await?)?), Duration::from_secs(30 * 60))
            }
            ExternalAuth::Aws { profile, credentials_command, .. } => {
                let argv = match credentials_command {
                    Some(argv) => argv.clone(),
                    None => {
                        let mut argv: Vec<String> =
                            ["aws", "configure", "export-credentials", "--format", "process"].map(String::from).into();
                        if let Some(profile) = profile {
                            argv.extend(["--profile".to_string(), profile.clone()]);
                        }
                        argv
                    }
                };
                let (creds, lifetime) = parse_aws_credentials(&run_command(&argv).await?, now)?;
                (Credential::Aws(creds), lifetime)
            }
            ExternalAuth::None | ExternalAuth::Bearer(_) | ExternalAuth::Basic { .. } => unreachable!("handled above"),
        };
        *cached = Some(Cached { credential: credential.clone(), until: Instant::now() + lifetime });
        Ok(credential)
    }

    /// One GET of the Prometheus HTTP API, e.g. `get("query", [("query",
    /// "up")])`. A refused credential from a command is asked for again
    /// once. A failure comes with the HTTP status when there was an answer.
    pub(crate) async fn get(
        &self,
        api: &str,
        params: &[(&str, &str)],
        limit: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, (PromError, Option<u16>)> {
        let api_path = format!("/api/v1/{}", enc_segment(api.trim_matches('/')).replace("%2F", "/"));
        let query = canonical_query(params);
        let relative = if query.is_empty() { api_path.clone() } else { format!("{api_path}?{query}") };
        let mut fresh = false;
        loop {
            let credential = self.credential(fresh).await.map_err(|e| (e, None))?;
            let mut req = http::Request::get(relative.as_str())
                .body(Vec::new())
                .map_err(|e| (PromError::Other(format!("bad request: {e}")), None))?;
            let headers = req.headers_mut();
            for (k, v) in &self.headers {
                headers.insert(k.clone(), v.clone());
            }
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            match &credential {
                Credential::None => {}
                Credential::Header(value) => {
                    headers.insert(AUTHORIZATION, value.clone());
                }
                Credential::Aws(creds) => {
                    let region = self.aws_region.as_deref().unwrap_or("us-east-1");
                    let full_path = format!("{}{api_path}", self.url.path);
                    let signed =
                        sigv4_headers(creds, region, "aps", &self.url.authority, &full_path, &query, jiff::Timestamp::now());
                    // The signature covers the Host header: send exactly
                    // the one that was signed.
                    if let Ok(host) = HeaderValue::from_str(&self.url.authority) {
                        headers.insert(HOST, host);
                    }
                    for (k, v) in signed {
                        let mut value = HeaderValue::from_str(&v)
                            .map_err(|_| (PromError::Credentials("the AWS credentials cannot be sent".into()), None))?;
                        value.set_sensitive(k != "x-amz-date");
                        headers.insert(HeaderName::from_static(k), value);
                    }
                }
            }
            let exchange = async {
                let res = self
                    .client
                    .send(req.map(kube::client::Body::from))
                    .await
                    .map_err(|e| (transport_error(&e), None))?;
                let status = res.status().as_u16();
                let body = collect_capped(res.into_body(), limit).await.map_err(|e| match e {
                    BodyError::TooLarge { .. } => (PromError::TooLarge, Some(status)),
                    BodyError::Read(e) => (transport_error(&e), None),
                })?;
                Ok::<_, (PromError, Option<u16>)>((status, body))
            };
            let (status, body) = match tokio::time::timeout(timeout, exchange).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err((
                        PromError::Unreachable(format!("no answer within {} s", timeout.as_secs())),
                        None,
                    ))
                }
            };
            if (200..300).contains(&status) {
                return Ok(body);
            }
            if matches!(status, 401 | 403) && !fresh && self.refreshable() {
                // The token may have been revoked or expired early.
                fresh = true;
                continue;
            }
            return Err((self.classify(status, &body), Some(status)));
        }
    }

    fn classify(&self, status: u16, body: &[u8]) -> PromError {
        match classify_response(status, body) {
            PromError::Forbidden(msg) => PromError::Credentials(format!(
                "{} refused the request (HTTP {status}: {msg}). {}",
                self.endpoint.display(),
                self.endpoint.auth.refusal_hint()
            )),
            PromError::Unreachable(msg) if status == 404 => PromError::Unreachable(format!(
                "{msg}. Check prometheus.url: it is the address in front of /api/v1."
            )),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(url: &str, auth: ExternalAuth) -> ExternalEndpoint {
        ExternalEndpoint { url: url.into(), auth, ..Default::default() }
    }

    #[test]
    fn addresses_are_checked_and_normalised() {
        let u = parse_url("https://ws-1.westeurope.prometheus.monitor.azure.com/").unwrap();
        assert_eq!((u.https, u.authority.as_str(), u.path.as_str()), (true, "ws-1.westeurope.prometheus.monitor.azure.com", ""));
        let u = parse_url("https://aps-workspaces.eu-west-1.amazonaws.com/workspaces/ws-abc/api/v1/query").unwrap();
        assert_eq!(u.path, "/workspaces/ws-abc");
        let u = parse_url("http://localhost:9090/prom/api/v1").unwrap();
        assert_eq!((u.https, u.authority.as_str(), u.path.as_str()), (false, "localhost:9090", "/prom"));
        for bad in ["ftp://x", "prometheus.example.com", "https://user:pw@x.example.com", "https://x/?a=b", "https://x/a/../b", ""] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn credentials_only_travel_over_https() {
        let azure = ExternalAuth::Azure { tenant: None, resource: None };
        assert!(endpoint("http://prom.example.com", azure.clone()).validate().is_err());
        assert!(endpoint("http://127.0.0.1:9090", azure.clone()).validate().is_ok());
        assert!(endpoint("https://prom.example.com", azure).validate().is_ok());
        assert!(endpoint("http://prom.example.com", ExternalAuth::None).validate().is_ok());
    }

    #[test]
    fn aws_region_comes_from_the_address_or_the_config() {
        assert_eq!(aws_region_from_host("aps-workspaces.eu-west-1.amazonaws.com").as_deref(), Some("eu-west-1"));
        assert_eq!(aws_region_from_host("aps-workspaces.cn-north-1.amazonaws.com.cn").as_deref(), Some("cn-north-1"));
        assert_eq!(aws_region_from_host("prometheus.example.com"), None);
        let aws = |region: Option<&str>| ExternalAuth::Aws { region: region.map(Into::into), profile: None, credentials_command: None };
        assert!(endpoint("https://aps-workspaces.eu-west-1.amazonaws.com/workspaces/ws-1", aws(None)).validate().is_ok());
        assert!(endpoint("https://amp.internal.example.com", aws(None)).validate().is_err());
        assert!(endpoint("https://amp.internal.example.com", aws(Some("us-east-2"))).validate().is_ok());
        assert!(endpoint("https://amp.internal.example.com", aws(Some("US East"))).validate().is_err());
    }

    #[test]
    fn headers_and_labels_are_checked() {
        let mut e = endpoint("https://mimir.example.com/prometheus", ExternalAuth::Bearer(SecretSource::Env("T".into())));
        e.headers = vec![("X-Scope-OrgID".into(), "team-a".into())];
        e.labels = vec![("cluster".into(), "aks-\"prod\"".into())];
        assert!(e.validate().is_ok());
        assert_eq!(e.scope(), r#"cluster="aks-\"prod\"""#);
        e.headers = vec![("Authorization".into(), "x".into())];
        assert!(e.validate().is_err());
        e.headers = vec![("bad header".into(), "x".into())];
        assert!(e.validate().is_err());
        e.headers.clear();
        e.labels = vec![("1cluster".into(), "x".into())];
        assert!(e.validate().is_err());
    }

    #[test]
    fn display_is_host_and_path() {
        assert_eq!(
            endpoint("https://aps-workspaces.eu-west-1.amazonaws.com/workspaces/ws-1/", ExternalAuth::None).display(),
            "aps-workspaces.eu-west-1.amazonaws.com/workspaces/ws-1"
        );
    }

    #[test]
    fn azure_tokens_are_kept_until_shortly_before_they_expire() {
        let json = r#"{"accessToken":"eyJ0","expiresOn":"2026-11-25 13:00:00.000000","expires_on":1764075600,"tokenType":"Bearer"}"#;
        let (token, lifetime) = parse_azure_token(json, 1764075600 - 3600).unwrap();
        assert_eq!(token, "eyJ0");
        assert_eq!(lifetime, Duration::from_secs(3600 - 300));
        // Older CLIs: no expires_on.
        let (_, lifetime) = parse_azure_token(r#"{"accessToken":"x","expiresOn":"2026-11-25 13:00:00"}"#, 0).unwrap();
        assert_eq!(lifetime, Duration::from_secs(1800));
        assert!(parse_azure_token("ERROR: Please run 'az login'", 0).is_err());
        assert!(parse_azure_token(r#"{"accessToken":""}"#, 0).is_err());
    }

    #[test]
    fn aws_credentials_are_read_from_credential_process_json() {
        let json = r#"{"Version":1,"AccessKeyId":"AKID","SecretAccessKey":"SECRET","SessionToken":"TOKEN","Expiration":"2026-11-25T13:00:00+00:00"}"#;
        let now = "2026-11-25T12:00:00Z".parse::<jiff::Timestamp>().unwrap().as_second();
        let (creds, lifetime) = parse_aws_credentials(json, now).unwrap();
        assert_eq!(creds.session_token.as_deref(), Some("TOKEN"));
        assert_eq!(lifetime, Duration::from_secs(3300));
        let (creds, lifetime) = parse_aws_credentials(r#"{"AccessKeyId":"A","SecretAccessKey":"S"}"#, now).unwrap();
        assert_eq!((creds.session_token, lifetime), (None, Duration::from_secs(900)));
        assert!(parse_aws_credentials(r#"{"AccessKeyId":"A"}"#, now).is_err());
    }

    /// The `get-vanilla` and `get-vanilla-query-order-key-case` cases of
    /// AWS' Signature Version 4 test suite.
    #[test]
    fn sigv4_matches_the_aws_test_suite() {
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let now: jiff::Timestamp = "2015-08-30T12:36:00Z".parse().unwrap();
        let auth = |query: &str| {
            let headers = sigv4_headers(&creds, "us-east-1", "service", "example.amazonaws.com", "/", query, now);
            assert_eq!(headers[0], ("x-amz-date", "20150830T123600Z".to_string()));
            headers.last().unwrap().1.clone()
        };
        assert_eq!(
            auth(""),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert_eq!(
            auth(&canonical_query(&[("Param2", "value2"), ("Param1", "value1")])),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        );
    }

    #[test]
    fn a_session_token_is_signed_and_sent() {
        let creds = AwsCredentials { access_key_id: "A".into(), secret_access_key: "S".into(), session_token: Some("T".into()) };
        let headers = sigv4_headers(&creds, "eu-west-1", "aps", "h", "/workspaces/ws-1/api/v1/query", "query=up", jiff::Timestamp::now());
        assert_eq!(headers[1], ("x-amz-security-token", "T".to_string()));
        assert!(headers[2].1.contains("SignedHeaders=host;x-amz-date;x-amz-security-token,"));
    }

    #[test]
    fn queries_are_sorted_and_strictly_encoded() {
        assert_eq!(
            canonical_query(&[("query", "sum by(pod)(up{a=\"b c\"})"), ("end", "2"), ("start", "1")]),
            "end=2&query=sum%20by%28pod%29%28up%7Ba%3D%22b%20c%22%7D%29&start=1"
        );
    }

    #[test]
    fn secrets_from_env_and_file() {
        std::env::set_var("SKIFF_TEST_PROM_TOKEN", " abc \n");
        assert_eq!(read_now(&SecretSource::Env("SKIFF_TEST_PROM_TOKEN".into())).unwrap(), "abc");
        assert!(read_now(&SecretSource::Env("SKIFF_TEST_PROM_TOKEN_UNSET".into())).is_err());
        let path = std::env::temp_dir().join(format!("skiff-prom-token-{}", std::process::id()));
        std::fs::write(&path, "from-file\n").unwrap();
        assert_eq!(read_now(&SecretSource::File(path.clone())).unwrap(), "from-file");
        std::fs::remove_file(&path).unwrap();
        assert!(read_now(&SecretSource::File(path)).is_err());
    }

    #[cfg(unix)]
    /// A one-connection-at-a-time HTTP server that records each request
    /// (request line and headers) and answers with `respond`.
    async fn serve(
        respond: impl Fn(&str, usize) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, std::sync::Arc<parking_lot::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let request = String::from_utf8_lossy(&buf).to_string();
                let n = {
                    let mut seen = log.lock();
                    seen.push(request.clone());
                    seen.len()
                };
                let (status, body) = respond(&request, n);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://127.0.0.1:{}/prom", addr.port()), seen)
    }

    #[cfg(unix)]
    const VECTOR: &str = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1,"1"]}]}}"#;
    #[cfg(unix)]
    const MATRIX: &str = r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"container":"app"},"values":[[1,"0.5"]]}]}}"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn queries_carry_the_token_and_the_cluster_label() {
        use crate::prom::{ChartTarget, Prom, PromError, TimeRange};
        let (url, seen) = serve(|req, n| {
            if n == 1 {
                // The first token is "expired".
                return (401, r#"{"error":"token expired"}"#.into());
            }
            if req.starts_with("GET /prom/api/v1/alerts") {
                return (404, "404 page not found".into());
            }
            if req.contains("query_range") {
                (200, MATRIX.into())
            } else {
                (200, VECTOR.into())
            }
        })
        .await;
        let endpoint = ExternalEndpoint {
            url,
            auth: ExternalAuth::Bearer(SecretSource::Command(vec!["echo".into(), "tok-123".into()])),
            headers: vec![("X-Scope-OrgID".into(), "team-a".into())],
            labels: vec![("cluster".into(), "aks-prod".into())],
        };
        let prom = Prom::external(endpoint).await.unwrap();
        assert_eq!(prom.scope(), "cluster=\"aks-prod\"");
        prom.ping().await.expect("asked for a new token after the 401");
        {
            let seen = seen.lock();
            assert_eq!(seen.len(), 2);
            for req in seen.iter() {
                let lower = req.to_ascii_lowercase();
                assert!(req.starts_with("GET /prom/api/v1/query?query=vector%281%29 "), "{req}");
                assert!(lower.contains("authorization: bearer tok-123\r\n"), "{req}");
                assert!(lower.contains("x-scope-orgid: team-a\r\n"), "{req}");
            }
        }

        let target = ChartTarget::Pod { namespace: "shop".into(), name: "web-1".into() };
        let charts = prom.charts(&target, &TimeRange::last(3600, 1_764_000_000)).await;
        assert!(charts.iter().all(|c| c.data.is_ok()), "{:?}", charts.iter().map(|c| &c.data).collect::<Vec<_>>());
        let ranges: Vec<String> = seen.lock().iter().filter(|r| r.contains("query_range")).cloned().collect();
        assert!(!ranges.is_empty());
        let scoped = crate::util::enc("cluster=\"aks-prod\",namespace=\"shop\"");
        assert!(ranges.iter().all(|r| r.contains(&scoped)), "{ranges:?}");

        match prom.firing_alerts().await {
            Err(PromError::Unsupported(msg)) => assert!(msg.contains("/api/v1/alerts"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refusals_and_missing_tools_say_what_to_do() {
        use crate::prom::{Prom, PromError};
        let (url, seen) = serve(|_, _| (403, r#"{"error":"Forbidden"}"#.into())).await;
        let azure = |url: &str| ExternalEndpoint {
            url: url.into(),
            auth: ExternalAuth::Bearer(SecretSource::Env("SKIFF_TEST_PROM_REFUSED".into())),
            ..Default::default()
        };
        std::env::set_var("SKIFF_TEST_PROM_REFUSED", "nope");
        let prom = Prom::external(azure(&url)).await.unwrap();
        match prom.ping().await {
            Err(PromError::Credentials(msg)) => assert!(msg.contains("HTTP 403") && msg.contains("prometheus.auth"), "{msg}"),
            other => panic!("{other:?}"),
        }
        // A secret from the environment is not asked for twice.
        assert_eq!(seen.lock().len(), 1);

        let missing = ExternalEndpoint {
            url,
            auth: ExternalAuth::Bearer(SecretSource::Command(vec!["skiff-no-such-tool".into()])),
            ..Default::default()
        };
        let prom = Prom::external(missing).await.unwrap();
        match prom.ping().await {
            Err(e @ PromError::Credentials(_)) => {
                assert!(e.to_string().contains("\"skiff-no-such-tool\" was not found"), "{e}")
            }
            other => panic!("{other:?}"),
        }
        let failing = ExternalEndpoint {
            url: "https://example.invalid".into(),
            auth: ExternalAuth::Bearer(SecretSource::Command(vec!["sh".into(), "-c".into(), "echo 'Please run az login' >&2; exit 1".into()])),
            ..Default::default()
        };
        let prom = Prom::external(failing).await.unwrap();
        match prom.ping().await {
            Err(PromError::Credentials(msg)) => assert!(msg.ends_with("failed: Please run az login"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }
}
