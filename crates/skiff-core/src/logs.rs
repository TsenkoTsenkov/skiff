//! Streaming pod logs from one or many containers into a bounded buffer.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::AsyncBufReadExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, LogParams};
use kube::Client;
use parking_lot::Mutex;
use tokio::task::AbortHandle;

use crate::util::{friendly, Notifier};

const MAX_LINES: usize = 100_000;
const MAX_LINE_BYTES: usize = 64 * 1024;
/// Consecutive failed attempts (connections that could not be made, or that
/// ended at once with nothing new) before a follow stream gives up.
const MAX_RETRIES: u32 = 30;
/// The UI is woken at most this often per stream, however fast lines arrive.
const PING_INTERVAL: Duration = Duration::from_millis(100);
/// On resume, ask the server to replay this much before the newest line we
/// have (its `sinceTime` has whole-second resolution and line timestamps are
/// not strictly ordered), and drop what was already delivered.
const REPLAY_MS: i64 = 2_000;
/// How long delivered lines are remembered for that de-duplication.
const REMEMBER_MS: i64 = REPLAY_MS + 2_000;

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
    /// Whether anything is displaying this stream right now. Lines are
    /// buffered either way; the UI is only woken while it is shown.
    visible: AtomicBool,
}

impl Shared {
    /// Task cancellation is cooperative, so a pump can still be running for
    /// a moment after `stop()`. Nothing it does may be observable.
    fn set_state(&self, idx: usize, s: SourceState) {
        let mut states = self.states.lock();
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if let Some(slot) = states.get_mut(idx) {
            *slot = s;
        }
    }

    fn push(&self, source: u16, time_ms: i64, text: &str) {
        let mut b = self.buf.lock();
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
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
        // `stopped` is already set, so no pump can change a state after us.
        for s in self.states.lock().iter_mut() {
            if matches!(s, SourceState::Connecting | SourceState::Streaming | SourceState::Reconnecting) {
                *s = SourceState::Ended;
            }
        }
        // Take the buffer lock once so an in-flight `push` has finished.
        drop(self.buf.lock());
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

    /// Tell the stream whether it is on screen. Hidden streams keep
    /// collecting lines but do not wake the UI for them.
    pub fn set_visible(&self, visible: bool) {
        self.shared.visible.store(visible, Ordering::Relaxed);
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

/// Lets a resumed stream skip exactly the lines that were already
/// delivered.
///
/// The API can only resume "since" a whole second, and lines are not always
/// stamped in order (stdout and stderr interleave; clocks jump), so neither
/// "newer than the last timestamp" nor "the same count at that timestamp" is
/// a safe rule. Instead we remember the recent lines themselves, ask the
/// server to replay a little more than necessary, and drop each replayed
/// line that matches one we remember.
#[derive(Default)]
struct ResumePoint {
    /// Newest timestamp delivered.
    max_ms: i64,
    /// (timestamp, fingerprint) of recently delivered lines, oldest first.
    recent: VecDeque<(i64, u64)>,
    /// Fingerprints still expected in the replay, with multiplicity.
    skip: HashMap<u64, u32>,
}

fn fingerprint(time_ms: i64, text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    time_ms.hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

impl ResumePoint {
    /// Returns whether the line should be delivered.
    fn admit(&mut self, time_ms: i64, text: &str) -> bool {
        if time_ms <= 0 {
            // Nothing to identify it by; never drop it.
            return true;
        }
        let fp = fingerprint(time_ms, text);
        if let Some(n) = self.skip.get_mut(&fp) {
            *n -= 1;
            if *n == 0 {
                self.skip.remove(&fp);
            }
            return false;
        }
        self.recent.push_back((time_ms, fp));
        self.max_ms = self.max_ms.max(time_ms);
        while self.recent.front().is_some_and(|(t, _)| *t < self.max_ms - REMEMBER_MS) {
            self.recent.pop_front();
        }
        true
    }

    /// Prepares for a replay and returns the time to resume from, if any
    /// line with a timestamp was delivered. `now_ms` guards against a line
    /// stamped in the future pushing the resume point past real time.
    fn begin_resume(&mut self, now_ms: i64) -> Option<i64> {
        if self.max_ms <= 0 {
            return None;
        }
        self.skip.clear();
        for (_, fp) in &self.recent {
            *self.skip.entry(*fp).or_insert(0) += 1;
        }
        Some(self.max_ms.min(now_ms) - REPLAY_MS)
    }
}

fn is_gone(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(s) if matches!(s.code, 401 | 403 | 404))
}

/// Yields the session's current client, so a stream follows a reconnect.
pub(crate) type ClientProvider = Arc<dyn Fn() -> Option<Client> + Send + Sync>;

async fn pump(
    shared: Arc<Shared>,
    first_client: Client,
    provider: ClientProvider,
    target: LogTarget,
    opts: LogOptions,
    idx: usize,
    notify: Notifier,
) {
    let mut resume = ResumePoint::default();
    let mut connected_once = false;
    let mut attempt = 0u32;
    // Wake the UI at most every PING_INTERVAL, and only while shown.
    let mut last_ping = Instant::now() - PING_INTERVAL;
    let mut unannounced = false;
    loop {
        // After a reconnect the session has a new client; an old one may be
        // pointing at a server or credentials that no longer work.
        let client = provider().unwrap_or_else(|| first_client.clone());
        let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
        let mut lp = LogParams {
            container: Some(target.container.clone()),
            follow: opts.follow,
            previous: opts.previous,
            timestamps: true,
            ..Default::default()
        };
        match resume.begin_resume(jiff::Timestamp::now().as_millisecond()) {
            Some(since_ms) => lp.since_time = jiff::Timestamp::from_millisecond(since_ms).ok(),
            None => {
                lp.tail_lines = opts.tail;
                lp.since_seconds = opts.since_seconds;
            }
        }
        let opened = Instant::now();
        let mut delivered = false;
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
                    // `read_until` keeps partial data in `buf` when it is
                    // cancelled, so the timeout loses nothing.
                    let wait = if unannounced {
                        PING_INTERVAL.saturating_sub(last_ping.elapsed())
                    } else {
                        Duration::from_secs(3600)
                    };
                    let read = tokio::time::timeout(wait, reader.read_until(b'\n', &mut buf)).await;
                    match read {
                        Err(_elapsed) => {}
                        Ok(Ok(_)) if buf.last() == Some(&b'\n') => {
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
                            if resume.admit(time_ms, body) {
                                shared.push(idx as u16, time_ms, body);
                                delivered = true;
                                unannounced = true;
                            }
                            buf.clear();
                        }
                        // End of stream (possibly mid-line) or a read error.
                        Ok(_) => break,
                    }
                    if unannounced && last_ping.elapsed() >= PING_INTERVAL {
                        unannounced = false;
                        last_ping = Instant::now();
                        if shared.visible.load(Ordering::Relaxed) {
                            notify.ping();
                        }
                    }
                }
                if unannounced {
                    unannounced = false;
                    notify.ping();
                }
                if !opts.follow || opts.previous {
                    break;
                }
            }
        }
        // The stream ends when the container restarts or the connection
        // drops: back off and resume where we stopped. A connection that
        // delivered something or simply stayed up was healthy, however often
        // an idle timeout in between cuts it; only consecutive attempts that
        // achieve nothing count towards giving up.
        if delivered || opened.elapsed() >= Duration::from_secs(1) {
            attempt = 0;
        }
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
    provider: ClientProvider,
    targets: Vec<LogTarget>,
    options: LogOptions,
    notify: Notifier,
) -> Arc<LogStream> {
    let shared = Arc::new(Shared {
        buf: Mutex::new(LogBuffer::default()),
        states: Mutex::new(vec![SourceState::Connecting; targets.len()]),
        aborts: Mutex::new(Vec::new()),
        stopped: AtomicBool::new(false),
        visible: AtomicBool::new(true),
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
            let t = h.spawn(pump(
                s.clone(),
                client.clone(),
                provider.clone(),
                target,
                options.clone(),
                idx,
                notify.clone(),
            ));
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
    fn resume_skips_exactly_what_was_delivered() {
        let mut r = ResumePoint::default();
        assert_eq!(r.begin_resume(10_000), None, "nothing delivered yet: no resume point");
        // First connection.
        assert!(r.admit(1000, "a"));
        assert!(r.admit(1500, "b"));
        assert!(r.admit(1500, "b"), "identical lines are both real");
        assert!(r.admit(1500, "c"));
        // Resume replays from earlier than the newest line.
        assert_eq!(r.begin_resume(10_000), Some(1500 - REPLAY_MS));
        assert!(!r.admit(1000, "a"));
        assert!(!r.admit(1500, "b"));
        assert!(!r.admit(1500, "b"));
        assert!(r.admit(1500, "b"), "a third identical line was never delivered");
        assert!(!r.admit(1500, "c"));
        assert!(r.admit(1600, "d"));
        // Lines without a timestamp are always delivered.
        assert!(r.admit(0, "x"));
        assert!(r.admit(0, "x"));
    }

    #[test]
    fn resume_keeps_out_of_order_lines_that_were_never_delivered() {
        // stdout/stderr interleaving: a line stamped earlier than one already
        // delivered shows up only in the replay. It must not be dropped.
        let mut r = ResumePoint::default();
        assert!(r.admit(10_000, "seq=1"));
        assert!(r.admit(10_700, "seq=2"));
        r.begin_resume(60_000);
        assert!(!r.admit(10_000, "seq=1"));
        assert!(r.admit(10_600, "seq=3 (stderr, stamped earlier)"));
        assert!(!r.admit(10_700, "seq=2"));
        assert!(r.admit(10_800, "seq=4"));
    }

    #[test]
    fn resume_point_ignores_timestamps_from_the_future() {
        let mut r = ResumePoint::default();
        assert!(r.admit(10_000, "now"));
        assert!(r.admit(99_000, "clock skew: stamped far ahead"));
        // Resuming must not skip the stretch between real time and the
        // bogus timestamp.
        assert_eq!(r.begin_resume(11_000), Some(11_000 - REPLAY_MS));
    }

    #[test]
    fn nothing_is_observable_after_stop() {
        let shared = Shared {
            buf: Mutex::new(LogBuffer::default()),
            states: Mutex::new(vec![SourceState::Streaming]),
            aborts: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            visible: AtomicBool::new(true),
        };
        shared.push(0, 1, "before");
        shared.stop();
        // A pump that has not yet noticed its cancellation.
        shared.set_state(0, SourceState::Streaming);
        shared.push(0, 2, "after");
        assert_eq!(shared.states.lock()[0], SourceState::Ended);
        assert_eq!(shared.buf.lock().lines.len(), 1);
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
                visible: AtomicBool::new(true),
            };
            shared.stop();
            let late = tokio::spawn(std::future::pending::<()>());
            shared.track(late.abort_handle());
            assert!(late.await.unwrap_err().is_cancelled());
            assert_eq!(shared.states.lock()[0], SourceState::Ended);
        });
    }
}
