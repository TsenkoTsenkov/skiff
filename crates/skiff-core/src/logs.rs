//! Streaming pod logs from one or many containers into a bounded buffer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::AsyncBufReadExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, LogParams};
use kube::Client;
use parking_lot::Mutex;
use tokio::task::AbortHandle;

use crate::util::{friendly, Notifier};

const MAX_LINES: usize = 100_000;
const MAX_LINE_BYTES: usize = 64 * 1024;
/// How often a dropped follow stream is retried before giving up.
const MAX_RETRIES: u32 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTarget {
    pub namespace: String,
    pub pod: String,
    pub container: String,
}

#[derive(Clone, Debug)]
pub struct LogOptions {
    /// Lines of history to fetch per container.
    pub tail: Option<i64>,
    pub follow: bool,
    /// Logs of the previous (crashed) container instance.
    pub previous: bool,
    pub since_seconds: Option<i64>,
}

impl Default for LogOptions {
    fn default() -> Self {
        LogOptions { tail: Some(1000), follow: true, previous: false, since_seconds: None }
    }
}

#[derive(Debug)]
pub struct LogLine {
    /// Index into `LogStream::targets`.
    pub source: u16,
    /// Unix milliseconds, 0 if the server sent no timestamp.
    pub time_ms: i64,
    pub text: Box<str>,
}

#[derive(Default)]
pub struct LogBuffer {
    pub lines: VecDeque<LogLine>,
    /// Lines evicted from the front because the buffer was full.
    pub dropped: u64,
    /// Bumps whenever `lines` changes.
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    Connecting,
    Streaming,
    /// Connection lost; retrying.
    Reconnecting,
    Ended,
    Failed(String),
}

/// State shared between the handle and its background tasks. The tasks hold
/// only this, never the `LogStream` handle, so dropping the handle stops
/// them.
struct Shared {
    buf: Mutex<LogBuffer>,
    states: Mutex<Vec<SourceState>>,
    aborts: Mutex<Vec<AbortHandle>>,
    stopped: AtomicBool,
}

impl Shared {
    fn set_state(&self, idx: usize, s: SourceState) {
        if let Some(slot) = self.states.lock().get_mut(idx) {
            *slot = s;
        }
    }

    fn push(&self, source: u16, time_ms: i64, text: &str) {
        let mut b = self.buf.lock();
        if b.lines.len() >= MAX_LINES {
            b.lines.pop_front();
            b.dropped += 1;
        }
        b.lines.push_back(LogLine { source, time_ms, text: text.into() });
        b.generation += 1;
    }

    fn stop(&self) {
        // Taking the lock orders this against tasks registering new handles.
        let mut aborts = self.aborts.lock();
        self.stopped.store(true, Ordering::SeqCst);
        for h in aborts.drain(..) {
            h.abort();
        }
        drop(aborts);
        for s in self.states.lock().iter_mut() {
            if matches!(s, SourceState::Connecting | SourceState::Streaming | SourceState::Reconnecting) {
                *s = SourceState::Ended;
            }
        }
    }

    /// Registers a task, or aborts it right away if we were stopped already.
    fn track(&self, handle: AbortHandle) {
        let mut aborts = self.aborts.lock();
        if self.stopped.load(Ordering::SeqCst) {
            handle.abort();
        } else {
            aborts.push(handle);
        }
    }
}

/// A set of followed containers. Dropping it (or calling `stop`) ends the
/// streams.
pub struct LogStream {
    pub targets: Vec<LogTarget>,
    pub options: LogOptions,
    shared: Arc<Shared>,
}

impl LogStream {
    pub fn with<R>(&self, f: impl FnOnce(&LogBuffer) -> R) -> R {
        f(&self.shared.buf.lock())
    }

    pub fn states(&self) -> Vec<SourceState> {
        self.shared.states.lock().clone()
    }

    pub fn clear(&self) {
        let mut b = self.shared.buf.lock();
        b.lines.clear();
        b.generation += 1;
    }

    pub fn stop(&self) {
        self.shared.stop();
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        self.shared.stop();
    }
}

/// Splits the RFC 3339 prefix the kubelet adds with `timestamps=true`.
fn split_timestamp(line: &str) -> (i64, &str) {
    if let Some((head, rest)) = line.split_once(' ') {
        let b = head.as_bytes();
        if b.len() >= 20 && b[0].is_ascii_digit() && b[4] == b'-' && b[10] == b'T' {
            if let Ok(ts) = head.parse::<jiff::Timestamp>() {
                return (ts.as_millisecond(), rest);
            }
        }
    }
    (0, line)
}

/// Removes ANSI escape sequences (colour codes etc.) from a log line.
pub fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.as_bytes().contains(&0x1b) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameters end at a byte in 0x40..=0x7e.
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: ends at BEL or ESC \.
                while let Some(n) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Remembers where a stream stopped so a resumed stream can skip what was
/// already delivered. The API only resumes at whole seconds, so the last
/// second arrives again.
#[derive(Default)]
struct ResumePoint {
    /// Timestamp of the newest line delivered.
    last_ms: i64,
    /// How many lines carried exactly that timestamp.
    at_last: usize,
    /// Lines at `last_ms` still to skip after a resume.
    skip_at_last: usize,
    resuming: bool,
}

impl ResumePoint {
    /// Returns whether the line should be delivered.
    fn admit(&mut self, time_ms: i64) -> bool {
        if time_ms <= 0 {
            return true;
        }
        if self.resuming {
            if time_ms < self.last_ms {
                return false;
            }
            if time_ms == self.last_ms && self.skip_at_last > 0 {
                self.skip_at_last -= 1;
                return false;
            }
        }
        if time_ms > self.last_ms {
            self.last_ms = time_ms;
            self.at_last = 1;
            self.resuming = false;
        } else if time_ms == self.last_ms {
            self.at_last += 1;
        }
        true
    }

    fn begin_resume(&mut self) {
        self.resuming = true;
        self.skip_at_last = self.at_last;
    }
}

fn is_gone(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(s) if matches!(s.code, 401 | 403 | 404))
}

async fn pump(
    shared: Arc<Shared>,
    client: Client,
    target: LogTarget,
    opts: LogOptions,
    idx: usize,
    notify: Notifier,
) {
    let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
    let mut resume = ResumePoint::default();
    let mut connected_once = false;
    let mut attempt = 0u32;
    loop {
        let mut lp = LogParams {
            container: Some(target.container.clone()),
            follow: opts.follow,
            previous: opts.previous,
            timestamps: true,
            ..Default::default()
        };
        if resume.last_ms > 0 {
            lp.since_time = jiff::Timestamp::from_millisecond(resume.last_ms).ok();
            resume.begin_resume();
        } else {
            lp.tail_lines = opts.tail;
            lp.since_seconds = opts.since_seconds;
        }
        match pods.log_stream(&target.pod, &lp).await {
            Err(e) => {
                // The very first attempt failing, or the pod being gone, is
                // final; anything else after we were connected is retried.
                if !connected_once || is_gone(&e) || !opts.follow || opts.previous {
                    let state = if connected_once { SourceState::Ended } else { SourceState::Failed(friendly(&e)) };
                    shared.set_state(idx, state);
                    notify.ping();
                    return;
                }
            }
            Ok(reader) => {
                connected_once = true;
                shared.set_state(idx, SourceState::Streaming);
                notify.ping();
                futures::pin_mut!(reader);
                let mut buf = Vec::with_capacity(1024);
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let mut raw = &buf[..];
                            while let Some((last, rest)) = raw.split_last() {
                                if *last == b'\n' || *last == b'\r' {
                                    raw = rest;
                                } else {
                                    break;
                                }
                            }
                            let text = String::from_utf8_lossy(&raw[..raw.len().min(MAX_LINE_BYTES)]);
                            let (time_ms, body) = split_timestamp(&text);
                            if resume.admit(time_ms) {
                                shared.push(idx as u16, time_ms, body);
                                attempt = 0;
                                notify.ping();
                            }
                        }
                    }
                }
                if !opts.follow || opts.previous {
                    break;
                }
            }
        }
        // The stream ends when the container restarts or the connection
        // drops: back off and resume where we stopped.
        attempt += 1;
        if attempt > MAX_RETRIES {
            break;
        }
        shared.set_state(idx, SourceState::Reconnecting);
        notify.ping();
        tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt.min(6)))).await;
    }
    shared.set_state(idx, SourceState::Ended);
    notify.ping();
}

pub(crate) fn open(
    handle: &tokio::runtime::Handle,
    client: impl std::future::Future<Output = Result<Client, String>> + Send + 'static,
    targets: Vec<LogTarget>,
    options: LogOptions,
    notify: Notifier,
) -> Arc<LogStream> {
    let shared = Arc::new(Shared {
        buf: Mutex::new(LogBuffer::default()),
        states: Mutex::new(vec![SourceState::Connecting; targets.len()]),
        aborts: Mutex::new(Vec::new()),
        stopped: AtomicBool::new(false),
    });
    let stream = Arc::new(LogStream { targets: targets.clone(), options: options.clone(), shared: shared.clone() });
    let h = handle.clone();
    let s = shared.clone();
    let task = handle.spawn(async move {
        let client = match client.await {
            Ok(c) => c,
            Err(e) => {
                for i in 0..targets.len() {
                    s.set_state(i, SourceState::Failed(e.clone()));
                }
                notify.ping();
                return;
            }
        };
        for (idx, target) in targets.into_iter().enumerate() {
            let t = h.spawn(pump(s.clone(), client.clone(), target, options.clone(), idx, notify.clone()));
            s.track(t.abort_handle());
        }
    });
    shared.track(task.abort_handle());
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_split() {
        let (t, body) = split_timestamp("2024-01-01T00:00:01.500000000Z hello world");
        assert_eq!(t, 1704067201500);
        assert_eq!(body, "hello world");
        let (t, body) = split_timestamp("no timestamp here");
        assert_eq!(t, 0);
        assert_eq!(body, "no timestamp here");
    }

    #[test]
    fn ansi_stripped() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m plain"), "red plain");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{7}b"), "ab");
    }

    #[test]
    fn resume_skips_what_was_delivered() {
        let mut r = ResumePoint::default();
        // First connection: three lines, two of them in the same millisecond.
        assert!(r.admit(1000));
        assert!(r.admit(1500));
        assert!(r.admit(1500));
        // Resume replays the whole last second, then continues.
        r.begin_resume();
        assert!(!r.admit(1000), "older line is a duplicate");
        assert!(!r.admit(1500), "first line at the resume point was seen");
        assert!(!r.admit(1500), "second line at the resume point was seen");
        assert!(r.admit(1500), "a third line at that instant is new");
        assert!(r.admit(1600));
        assert!(r.admit(1600));
        // Lines without a timestamp are always delivered.
        assert!(r.admit(0));
        // A second resume counts the lines seen since.
        r.begin_resume();
        assert!(!r.admit(1600));
        assert!(!r.admit(1600));
        assert!(r.admit(1700));
    }

    #[test]
    fn stopped_stream_aborts_late_tasks() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let shared = Shared {
                buf: Mutex::new(LogBuffer::default()),
                states: Mutex::new(vec![SourceState::Connecting]),
                aborts: Mutex::new(Vec::new()),
                stopped: AtomicBool::new(false),
            };
            shared.stop();
            let late = tokio::spawn(std::future::pending::<()>());
            shared.track(late.abort_handle());
            assert!(late.await.unwrap_err().is_cancelled());
            assert_eq!(shared.states.lock()[0], SourceState::Ended);
        });
    }
}
