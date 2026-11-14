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

/// A short, human-oriented rendering of a client error.
pub fn friendly(e: &kube::Error) -> String {
    match e {
        kube::Error::Api(status) => {
            let msg = if status.message.is_empty() { status.reason.as_str() } else { status.message.as_str() };
            if status.code > 0 {
                format!("{msg} ({})", status.code)
            } else {
                msg.to_string()
            }
        }
        other => {
            // Walk to the innermost cause: that's the actionable part
            // ("connection refused", "certificate has expired", ...).
            let mut msg = other.to_string();
            let mut src = std::error::Error::source(other);
            while let Some(s) = src {
                let text = s.to_string();
                if !text.is_empty() && !msg.contains(&text) {
                    msg = format!("{msg}: {text}");
                }
                src = s.source();
            }
            msg
        }
    }
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
        assert_eq!(j.peek(|r| r.map(|r| r.clone())), Some(Ok(7)));
        assert_eq!(j.take(), Some(Ok(7)));
        assert!(j.take().is_none());
    }
}
