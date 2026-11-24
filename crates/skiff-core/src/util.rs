//! Small shared helpers.

use std::sync::Arc;

use parking_lot::Mutex;

/// Wakes the UI when background state changed. The UI only repaints on
/// input or when pinged, so an idle app costs no CPU.
#[derive(Clone)]
pub struct Notifier(Arc<dyn Fn() + Send + Sync>);

impl Notifier {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Notifier(Arc::new(f))
    }
    pub fn noop() -> Self {
        Notifier(Arc::new(|| {}))
    }
    pub fn ping(&self) {
        (self.0)()
    }
}

/// Result slot of a background operation, polled by the UI.
pub struct Job<T> {
    slot: Arc<Mutex<Option<Result<T, String>>>>,
}

impl<T> Clone for Job<T> {
    fn clone(&self) -> Self {
        Job { slot: self.slot.clone() }
    }
}

impl<T> Job<T> {
    pub(crate) fn new() -> Self {
        Job { slot: Arc::new(Mutex::new(None)) }
    }

    /// A job that already finished.
    pub fn done(value: Result<T, String>) -> Self {
        Job { slot: Arc::new(Mutex::new(Some(value))) }
    }

    pub(crate) fn finish(&self, value: Result<T, String>) {
        *self.slot.lock() = Some(value);
    }

    /// Nobody but the task that fills it holds this job any more.
    pub(crate) fn is_abandoned(&self) -> bool {
        Arc::strong_count(&self.slot) == 1
    }

    pub fn is_done(&self) -> bool {
        self.slot.lock().is_some()
    }

    /// Takes the result once it is available.
    pub fn take(&self) -> Option<Result<T, String>> {
        self.slot.lock().take()
    }

    /// Inspects the result without consuming it.
    pub fn peek<R>(&self, f: impl FnOnce(Option<&Result<T, String>>) -> R) -> R {
        f(self.slot.lock().as_ref())
    }
}

/// Percent-encodes a query component.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// Percent-encodes one path segment (an object name). Characters that are
/// legal in a path segment, such as `:`, stay readable.
pub fn enc_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b':'
            | b'@'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'=' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn request(
    method: http::Method,
    url: &str,
    body: Vec<u8>,
    content_type: Option<&str>,
    accept: Option<&str>,
) -> Result<http::Request<Vec<u8>>, String> {
    let mut b = http::Request::builder().method(method).uri(url);
    if let Some(a) = accept {
        b = b.header(http::header::ACCEPT, a);
    }
    if let Some(c) = content_type {
        b = b.header(http::header::CONTENT_TYPE, c);
    }
    b.body(body).map_err(|e| format!("bad request {url}: {e}"))
}

pub(crate) fn get_request(url: &str, accept: Option<&str>) -> Result<http::Request<Vec<u8>>, String> {
    request(http::Method::GET, url, Vec::new(), None, accept)
}

/// What went wrong, in the terms a user can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Name does not resolve, connection refused or reset, no route.
    Unreachable,
    /// No answer in time.
    Timeout,
    /// Certificate or TLS handshake problem.
    Tls,
    /// 401: the cluster did not accept the credentials.
    Unauthorized,
    /// 403: authenticated, but not allowed.
    Forbidden,
    /// The kubeconfig's exec credential plugin is not installed.
    ExecMissing,
    /// The exec credential plugin ran and failed, or took too long.
    ExecFailed,
    /// The kubeconfig is missing, invalid or lacks the context.
    Kubeconfig,
    /// Refused locally: the cluster is marked read-only in Skiff.
    ReadOnly,
    /// Any other answer from the API server.
    Api,
    Other,
}

/// A classified error: a short summary and, where there is one, what to do
/// about it. `to_string()` gives `summary — hint` for places that only
/// have room for one string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorInfo {
    pub kind: ErrorKind,
    /// What happened, in one short line.
    pub summary: String,
    /// What to do next.
    pub hint: Option<String>,
    /// HTTP status when the API server answered, else 0.
    pub code: u16,
}

impl ErrorInfo {
    pub fn new(kind: ErrorKind, summary: impl Into<String>, hint: Option<&str>) -> Self {
        ErrorInfo { kind, summary: summary.into(), hint: hint.map(str::to_string), code: 0 }
    }

    /// An unclassified message, shown as it is.
    pub fn other(summary: impl Into<String>) -> Self {
        ErrorInfo::new(ErrorKind::Other, summary, None)
    }

    /// The same error with text in front of its summary
    /// (`Cannot reach https://…: Connection refused`).
    pub fn prefixed(mut self, prefix: &str) -> Self {
        self.summary = format!("{prefix}{}", self.summary);
        self
    }
}

impl std::fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.hint {
            Some(hint) => write!(f, "{} — {hint}", self.summary),
            None => f.write_str(&self.summary),
        }
    }
}

/// The error every refused operation on a read-only cluster reports.
pub fn read_only_error(context: &str) -> ErrorInfo {
    ErrorInfo::new(
        ErrorKind::ReadOnly,
        format!("{context} is read-only in Skiff"),
        Some("Turn off read-only for this cluster in Skiff's settings to make changes."),
    )
}

const HINT_NETWORK: &str = "Check your network connection and VPN, then reconnect.";
const HINT_LOGIN: &str = "Your credentials have expired or are not valid. Re-run your login command, then reconnect.";

/// A short, human-oriented rendering of a client error: the summary, then
/// ` — ` and the hint when there is one. See [`classify`] for the parts.
pub fn friendly(e: &kube::Error) -> String {
    classify(e).to_string()
}

/// Sorts a client error into a kind, a short summary and a next step.
pub fn classify(e: &kube::Error) -> ErrorInfo {
    classify_kube(e, None)
}

/// The request never left: the kubeconfig's credential plugin, which is
/// run again once its token has expired, is missing or failed. Asking
/// again only runs the plugin again; the user has to sign in first.
pub(crate) fn is_credential_failure(e: &kube::Error) -> bool {
    matches!(classify(e).kind, ErrorKind::ExecFailed | ErrorKind::ExecMissing)
}

/// Like [`classify`], naming the credential plugin the context uses (the
/// error itself only knows the full command line).
pub(crate) fn classify_kube(e: &kube::Error, plugin: Option<&str>) -> ErrorInfo {
    match e {
        kube::Error::Api(status) => classify_status(status.code, &status.message, &status.reason),
        kube::Error::Auth(auth) => classify_auth(auth, plugin),
        other => classify_chain(other, plugin),
    }
}

fn classify_status(code: u16, message: &str, reason: &str) -> ErrorInfo {
    let msg = if message.is_empty() { reason } else { message };
    let (kind, summary, hint) = match code {
        401 => (ErrorKind::Unauthorized, "The cluster rejected your credentials (401)".to_string(), Some(HINT_LOGIN)),
        403 => {
            let what = if msg.is_empty() { "Not allowed" } else { msg };
            (
                ErrorKind::Forbidden,
                format!("{what} (403)"),
                Some("Your account does not have permission for this. Ask a cluster administrator for access."),
            )
        }
        0 => (ErrorKind::Api, msg.to_string(), None),
        _ => (ErrorKind::Api, format!("{msg} ({code})"), None),
    };
    ErrorInfo { code, ..ErrorInfo::new(kind, summary, hint) }
}

/// Errors that are not a direct API answer: walk the cause chain.
fn classify_chain(e: &(dyn std::error::Error + 'static), plugin: Option<&str>) -> ErrorInfo {
    // Typed causes first, and before anything is rendered to text: the
    // Display of a failed credential plugin contains its whole output.
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    let mut depth = 0;
    while let Some(c) = cur {
        if depth > 0 {
            if let Some(k) = c.downcast_ref::<kube::Error>() {
                return classify_kube(k, plugin);
            }
        }
        if let Some(a) = c.downcast_ref::<kube::client::AuthError>() {
            return classify_auth(a, plugin);
        }
        depth += 1;
        cur = c.source();
    }

    // The innermost cause is the specific part ("Connection refused",
    // "certificate has expired", ...); the wrappers around it
    // ("ServiceError: client error (Connect): ...") are noise.
    let mut innermost = e.to_string();
    let mut all = innermost.to_lowercase();
    let mut io_kind = None;
    let mut cur = e.source();
    while let Some(c) = cur {
        let text = c.to_string();
        if !text.is_empty() {
            all.push('\n');
            all.push_str(&text.to_lowercase());
            innermost = text;
        }
        if let Some(io) = c.downcast_ref::<std::io::Error>() {
            io_kind = Some(io.kind());
        }
        cur = c.source();
    }
    classify_text(&innermost, &all, io_kind)
}

/// `innermost` is the deepest cause as text, `all` the whole chain in
/// lower case, `io_kind` the deepest I/O error kind if there was one.
fn classify_text(innermost: &str, all: &str, io_kind: Option<std::io::ErrorKind>) -> ErrorInfo {
    use std::io::ErrorKind as Io;
    let has = |needles: &[&str]| needles.iter().any(|n| all.contains(n));
    let detail = one_line(innermost, 200);

    if has(&[
        "dns error",
        "failed to lookup address",
        "name or service not known",
        "nodename nor servname",
        "no such host",
        "temporary failure in name resolution",
    ]) {
        return ErrorInfo::new(
            ErrorKind::Unreachable,
            "The cluster address could not be found (DNS)",
            Some("Check the server name in your kubeconfig, your network connection and VPN, then reconnect."),
        );
    }
    if has(&["certificate", "unknownissuer", "tls error", "tls handshake", "rustls"]) {
        return ErrorInfo::new(
            ErrorKind::Tls,
            format!("Secure connection failed: {detail}"),
            Some(
                "The cluster's certificate was not accepted. Check the certificate authority in your kubeconfig \
                 and your computer's clock, and whether a proxy or VPN intercepts the connection.",
            ),
        );
    }
    if io_kind == Some(Io::ConnectionRefused) || has(&["connection refused", "actively refused"]) {
        return ErrorInfo::new(
            ErrorKind::Unreachable,
            "Connection refused",
            Some("Nothing is answering at the cluster address. Check that the cluster is running and your VPN is connected, then reconnect."),
        );
    }
    if io_kind == Some(Io::TimedOut) || has(&["deadline has elapsed", "timed out", "timeout"]) {
        return ErrorInfo::new(ErrorKind::Timeout, "Connection timed out", Some(HINT_NETWORK));
    }
    if matches!(
        io_kind,
        Some(Io::ConnectionReset | Io::ConnectionAborted | Io::NotConnected | Io::BrokenPipe | Io::UnexpectedEof)
    ) || has(&[
        "network is unreachable",
        "no route to host",
        "host is unreachable",
        "connection reset",
        "host is down",
    ]) {
        return ErrorInfo::new(ErrorKind::Unreachable, format!("Connection lost: {detail}"), Some(HINT_NETWORK));
    }
    ErrorInfo::other(detail)
}

fn classify_auth(e: &kube::client::AuthError, plugin: Option<&str>) -> ErrorInfo {
    use kube::client::AuthError as A;
    let name = plugin.unwrap_or("the credential plugin");
    match e {
        A::AuthExecStart(io) if io.kind() == std::io::ErrorKind::NotFound => exec_missing(plugin, None),
        A::AuthExecStart(io) => ErrorInfo::new(
            ErrorKind::ExecFailed,
            format!("Could not start {name}: {}", one_line(&io.to_string(), 200)),
            Some("Check that the command in your kubeconfig can be run from a terminal."),
        ),
        A::AuthExecRun { cmd, out } => {
            let from_error = program_of(cmd);
            let name = plugin.map(str::to_string).or(from_error);
            exec_failed(name.as_deref(), &out.status.to_string(), &out.stderr)
        }
        // Parser errors can quote their input, and the input here is the
        // plugin's stdout: a token. Say what happened without the cause.
        A::AuthExecParse(_) | A::ParseTokenKey(_) => ErrorInfo::new(
            ErrorKind::ExecFailed,
            format!("{name} did not return a usable credential"),
            Some("Run the login command from your kubeconfig in a terminal to check what it prints."),
        ),
        A::ExecPluginFailed => {
            ErrorInfo::new(ErrorKind::ExecFailed, format!("{name} did not return a credential"), Some(HINT_LOGIN))
        }
        other => ErrorInfo::new(
            ErrorKind::Other,
            format!("Sign-in failed: {}", one_line(&other.to_string(), 300)),
            Some("Check the user entry for this context in your kubeconfig."),
        ),
    }
}

/// The credential plugin named in the kubeconfig is not installed.
pub(crate) fn exec_missing(command: Option<&str>, install_hint: Option<&str>) -> ErrorInfo {
    let summary = match command {
        Some(c) => format!("Credential plugin `{c}` was not found"),
        None => "The credential plugin in your kubeconfig was not found".to_string(),
    };
    let mut hint = match command {
        Some(c) if c.contains(['/', '\\']) => {
            "Install it, or fix the path in your kubeconfig, then reconnect.".to_string()
        }
        _ => "Install it and make sure it is on your PATH, then reconnect.".to_string(),
    };
    if let Some(install) = install_hint.map(|h| clean_text(h, 600)).filter(|h| !h.is_empty()) {
        hint.push(' ');
        hint.push_str(&install);
    }
    ErrorInfo { kind: ErrorKind::ExecMissing, summary, hint: Some(hint), code: 0 }
}

/// The credential plugin did not finish in time.
pub(crate) fn exec_timeout(command: Option<&str>, secs: u64) -> ErrorInfo {
    let name = command.map(|c| format!("`{c}`")).unwrap_or_else(|| "the credential plugin".to_string());
    ErrorInfo::new(
        ErrorKind::ExecFailed,
        format!("No answer from {name} within {secs} seconds"),
        Some("It may be waiting for a sign-in. Run your login command in a terminal, then reconnect."),
    )
}

/// How many trailing lines of a plugin's stderr are shown.
const STDERR_LINES: usize = 3;

/// The plugin ran and failed. Only its exit status and the end of its
/// stderr are shown: stdout is where credentials are printed.
pub(crate) fn exec_failed(command: Option<&str>, status: &str, stderr: &[u8]) -> ErrorInfo {
    let name = command.map(|c| format!("`{c}`")).unwrap_or_else(|| "The credential plugin".to_string());
    let text = String::from_utf8_lossy(stderr);
    let mut lines: Vec<String> = text.lines().map(|l| clean_text(l, 240)).filter(|l| !l.is_empty()).collect();
    if lines.len() > STDERR_LINES {
        lines.drain(..lines.len() - STDERR_LINES);
    }
    let mut summary = format!("{name} failed ({status})");
    if !lines.is_empty() {
        summary.push_str(": ");
        summary.push_str(&lines.join(" / "));
    }
    let lower = text.to_lowercase();
    // Most plugins say which command signs you in again; repeat it as the
    // next step when it is one we recognise.
    let login = [
        "aws sso login",
        "gcloud auth login",
        "az login",
        "kubelogin",
        "tsh login",
        "oci session authenticate",
        "doctl auth init",
    ]
    .into_iter()
    .find(|c| lower.contains(c));
    let hint = match login {
        Some(c) => format!("Run `{c}` in a terminal, then reconnect."),
        None => "Run your login command in a terminal, then reconnect.".to_string(),
    };
    ErrorInfo { kind: ErrorKind::ExecFailed, summary, hint: Some(hint), code: 0 }
}

/// The program from kube's rendering of a failed command (the `Debug` of a
/// `Command`: `"aws" "eks" "get-token"`, possibly after `VAR="value"`
/// pairs), without its directory. Arguments and environment are dropped.
fn program_of(cmd: &str) -> Option<String> {
    let mut rest = cmd.trim();
    while !rest.is_empty() {
        let quoted = rest.starts_with('"');
        // A token ends at the first space outside quotes.
        let (mut in_quotes, mut escaped, mut end) = (false, false, rest.len());
        for (i, ch) in rest.char_indices() {
            match ch {
                _ if escaped => escaped = false,
                '\\' if in_quotes => escaped = true,
                '"' => in_quotes = !in_quotes,
                ' ' if !in_quotes => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let token = &rest[..end];
        rest = rest[end..].trim_start();
        // `NAME="value"` is an environment assignment, `cd "dir" &&` a
        // working directory: neither is the program.
        let is_env = !quoted && token.contains("=\"");
        if is_env || token == "cd" || token == "&&" || (quoted && rest.starts_with("&&")) {
            continue;
        }
        let name = token.trim_matches('"');
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        return (!base.is_empty()).then(|| clean_text(base, 80));
    }
    None
}

/// Text from outside (a plugin, a kubeconfig) made safe to show on one
/// line: no escape sequences or control characters, bounded length.
fn clean_text(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Drop a CSI sequence (colours): ESC [ ... final byte.
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    let trimmed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() > max {
        let mut cut: String = trimmed.chars().take(max).collect();
        cut.push('…');
        cut
    } else {
        trimmed
    }
}

fn one_line(s: &str, max: usize) -> String {
    clean_text(s, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding() {
        assert_eq!(enc("app=web,tier in (a,b)"), "app%3Dweb%2Ctier%20in%20%28a%2Cb%29");
        assert_eq!(enc("abc-_.~"), "abc-_.~");
    }

    #[test]
    fn job_lifecycle() {
        let j: Job<u32> = Job::new();
        assert!(!j.is_done());
        j.finish(Ok(7));
        assert!(j.is_done());
        assert_eq!(j.peek(|r| r.cloned()), Some(Ok(7)));
        assert_eq!(j.take(), Some(Ok(7)));
        assert!(j.take().is_none());
    }

    fn service(e: impl std::error::Error + Send + Sync + 'static) -> kube::Error {
        kube::Error::Service(Box::new(e))
    }

    /// An error with a message and a cause, like the wrappers hyper adds.
    #[derive(Debug)]
    struct Wrap(&'static str, Box<dyn std::error::Error + Send + Sync>);
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Wrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&*self.1)
        }
    }

    fn io(kind: std::io::ErrorKind, msg: &str) -> std::io::Error {
        std::io::Error::new(kind, msg.to_string())
    }

    fn api(code: u16, message: &str, reason: &str) -> kube::Error {
        let status: kube::core::Status = serde_json::from_value(serde_json::json!({
            "status": "Failure", "message": message, "reason": reason, "code": code
        }))
        .expect("status");
        kube::Error::Api(Box::new(status))
    }

    #[cfg(unix)]
    fn output(code: i32, stdout: &str, stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn classifies_network_errors() {
        let refused = classify(&service(Wrap(
            "client error (Connect)",
            Box::new(Wrap(
                "tcp connect error",
                Box::new(io(std::io::ErrorKind::ConnectionRefused, "Connection refused (os error 111)")),
            )),
        )));
        assert_eq!(refused.kind, ErrorKind::Unreachable);
        assert_eq!(refused.summary, "Connection refused");
        assert!(refused.hint.as_deref().is_some_and(|h| h.contains("VPN")));

        let dns = classify(&service(Wrap(
            "client error (Connect)",
            Box::new(Wrap(
                "dns error",
                Box::new(io(
                    std::io::ErrorKind::Other,
                    "failed to lookup address information: Name or service not known",
                )),
            )),
        )));
        assert_eq!(dns.kind, ErrorKind::Unreachable);
        assert!(dns.summary.contains("DNS"), "{}", dns.summary);
        assert!(dns.hint.as_deref().is_some_and(|h| h.contains("VPN")));

        let timeout = classify(&service(Wrap(
            "client error (Connect)",
            Box::new(io(std::io::ErrorKind::TimedOut, "deadline has elapsed")),
        )));
        assert_eq!(timeout.kind, ErrorKind::Timeout);
        assert_eq!(timeout.summary, "Connection timed out");
        assert!(timeout.hint.is_some());

        let reset =
            classify(&service(io(std::io::ErrorKind::ConnectionReset, "Connection reset by peer (os error 104)")));
        assert_eq!(reset.kind, ErrorKind::Unreachable);
        assert!(reset.summary.contains("Connection reset by peer"));

        let unknown = classify(&service(Wrap("outer noise", Box::new(io(std::io::ErrorKind::Other, "something odd")))));
        assert_eq!((unknown.kind, unknown.summary.as_str(), unknown.hint), (ErrorKind::Other, "something odd", None));
    }

    #[test]
    fn classifies_tls_errors() {
        let tls = classify(&service(Wrap(
            "client error (Connect)",
            Box::new(io(std::io::ErrorKind::InvalidData, "invalid peer certificate: UnknownIssuer")),
        )));
        assert_eq!(tls.kind, ErrorKind::Tls);
        assert!(tls.summary.contains("UnknownIssuer"));
        assert!(tls.hint.as_deref().is_some_and(|h| h.contains("certificate")));
        // A certificate problem is reported as such even when the socket
        // error kind looks like something else.
        let expired = classify(&service(io(std::io::ErrorKind::ConnectionAborted, "certificate has expired")));
        assert_eq!(expired.kind, ErrorKind::Tls);
    }

    #[test]
    fn classifies_api_errors() {
        let unauthorized = classify(&api(401, "Unauthorized", "Unauthorized"));
        assert_eq!((unauthorized.kind, unauthorized.code), (ErrorKind::Unauthorized, 401));
        assert!(unauthorized.hint.as_deref().is_some_and(|h| h.contains("login command")));

        let forbidden =
            classify(&api(403, "pods is forbidden: User \"jane\" cannot list resource \"pods\"", "Forbidden"));
        assert_eq!(forbidden.kind, ErrorKind::Forbidden);
        assert!(forbidden.summary.starts_with("pods is forbidden") && forbidden.summary.ends_with("(403)"));
        assert!(forbidden.hint.is_some());

        // Other answers keep the server's wording and get no hint.
        let conflict = api(409, "the object has been modified", "Conflict");
        assert_eq!(friendly(&conflict), "the object has been modified (409)");
        assert_eq!(friendly(&api(404, "", "NotFound")), "NotFound (404)");
        assert_eq!(classify(&conflict).kind, ErrorKind::Api);
        // The plain string is "summary — hint".
        let s = friendly(&api(401, "Unauthorized", "Unauthorized"));
        assert!(s.starts_with("The cluster rejected your credentials (401) — "), "{s}");
    }

    #[cfg(unix)]
    #[test]
    fn plugin_failures_never_show_stdout() {
        let secret = "eyJhbGciOiJSUzI1NiIsImtpZCI6SECRET";
        let stderr = "line one\nline two\n\x1b[31mError loading SSO Token\x1b[0m: Token for dev does not exist\nRun aws sso login --profile dev\n";
        let run = || kube::client::AuthError::AuthExecRun {
            cmd: "AWS_PROFILE=\"dev\" \"/usr/local/bin/aws\" \"eks\" \"get-token\" \"--cluster-name\" \"prod\""
                .to_string(),
            out: Box::new(output(255, &format!("{{\"status\":{{\"token\":\"{secret}\"}}}}"), stderr)),
        };
        // Both as a direct error (client construction) and wrapped by the
        // request path (token refresh).
        for err in [kube::Error::Auth(run()), service(Wrap("auth layer", Box::new(run())))] {
            let info = classify(&err);
            assert_eq!(info.kind, ErrorKind::ExecFailed);
            let text = info.to_string();
            assert!(!text.contains(secret) && !text.contains("eyJ"), "stdout leaked: {text}");
            assert!(info.summary.starts_with("`aws` failed (exit status: 255)"), "{}", info.summary);
            assert!(info.summary.contains("Error loading SSO Token: Token for dev does not exist"));
            assert!(!info.summary.contains("line one"), "only the last lines are kept");
            assert!(!info.summary.contains('\u{1b}'));
            assert_eq!(info.hint.as_deref(), Some("Run `aws sso login` in a terminal, then reconnect."));
            assert_eq!(friendly(&err), text);
        }
        // The name from the kubeconfig wins over the parsed one.
        let named = classify_kube(&kube::Error::Auth(run()), Some("aws-vault"));
        assert!(named.summary.starts_with("`aws-vault` failed"));

        // Unparseable plugin output: the parser error is not shown either.
        let quiet = exec_failed(Some("tool"), "exit status: 1", b"");
        assert_eq!(quiet.summary, "`tool` failed (exit status: 1)");
        assert!(quiet.hint.is_some());
    }

    #[test]
    fn plugin_missing_and_timeout() {
        let start = kube::Error::Auth(kube::client::AuthError::AuthExecStart(io(
            std::io::ErrorKind::NotFound,
            "No such file or directory (os error 2)",
        )));
        let info = classify_kube(&start, Some("gke-gcloud-auth-plugin"));
        assert_eq!(info.kind, ErrorKind::ExecMissing);
        assert_eq!(info.summary, "Credential plugin `gke-gcloud-auth-plugin` was not found");
        assert!(info.hint.as_deref().is_some_and(|h| h.contains("PATH")));
        assert_eq!(classify(&start).kind, ErrorKind::ExecMissing);

        let with_hint = exec_missing(Some("aws"), Some("  brew install\tawscli\n"));
        assert!(with_hint.hint.as_deref().is_some_and(|h| h.ends_with("brew install awscli")), "{:?}", with_hint.hint);

        let slow = exec_timeout(Some("kubelogin"), 60);
        assert_eq!(slow.kind, ErrorKind::ExecFailed);
        assert_eq!(slow.summary, "No answer from `kubelogin` within 60 seconds");

        let denied = kube::Error::Auth(kube::client::AuthError::AuthExecStart(io(
            std::io::ErrorKind::PermissionDenied,
            "Permission denied",
        )));
        assert_eq!(classify(&denied).kind, ErrorKind::ExecFailed);
    }

    #[test]
    fn program_names() {
        assert_eq!(program_of("\"aws\" \"eks\" \"get-token\"").as_deref(), Some("aws"));
        assert_eq!(program_of("A=\"x y\" B=\"z\" \"/opt/bin/kubelogin\" \"get-token\"").as_deref(), Some("kubelogin"));
        assert_eq!(program_of("cd \"/tmp/a b\" && \"tsh\" \"kube\"").as_deref(), Some("tsh"));
        assert_eq!(program_of("\"C:\\\\Program Files\\\\tool.exe\" \"x\"").as_deref(), Some("tool.exe"));
        assert_eq!(program_of("gcloud config config-helper --format=json").as_deref(), Some("gcloud"));
        assert_eq!(program_of(""), None);
    }

    #[test]
    fn display_and_read_only() {
        assert_eq!(ErrorInfo::other("plain").to_string(), "plain");
        let e =
            ErrorInfo::new(ErrorKind::Timeout, "Connection timed out", Some("Check VPN.")).prefixed("Cannot reach x: ");
        assert_eq!(e.to_string(), "Cannot reach x: Connection timed out — Check VPN.");
        let ro = read_only_error("prod-eu");
        assert_eq!(ro.kind, ErrorKind::ReadOnly);
        assert_eq!(ro.summary, "prod-eu is read-only in Skiff");
        assert_eq!(clean_text("a\tb\r\n  c", 10), "a b c");
        assert_eq!(clean_text("abcdef", 3), "abc…");
    }
}
