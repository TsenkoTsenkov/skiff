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

/// Like `read_until(b'\n')`, but keeps at most `cap` bytes of a line in
/// `buf` and drops the rest of it: a container that prints for hours without
/// a newline (progress bars, binary output) must not grow the buffer without
/// end. Partial data stays in `buf` when the future is cancelled (nothing is
/// consumed from the reader without being accounted for first), so a timeout
/// around it loses nothing. Returns the bytes read (0 at the end of input).
async fn read_line_capped<R>(reader: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<usize>
where
    R: futures::AsyncBufRead + Unpin,
{
    let mut total = 0;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        let (chunk, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (&available[..=i], true),
            None => (available, false),
        };
        let n = chunk.len();
        let room = cap.saturating_sub(buf.len());
        if room >= n {
            buf.extend_from_slice(chunk);
        } else {
            buf.extend_from_slice(&chunk[..room]);
            if done {
                buf.push(b'\n');
            }
        }
        reader.consume_unpin(n);
        total += n;
        if done {
            return Ok(total);
        }
    }
}
/// Consecutive failed attempts (connections that could not be made, or that
/// ended at once with nothing new) before a follow stream gives up.
const MAX_RETRIES: u32 = 30;
/// The UI is woken at most this often per stream, however fast lines arrive.
const PING_INTERVAL: Duration = Duration::from_millis(100);
/// On resume, ask the server to replay this much before the point we
/// reached (its `sinceTime` has whole-second resolution and line timestamps
/// are not strictly ordered), and drop what was already delivered.
const REPLAY_MS: i64 = 5_000;
/// How long the server may take to start answering a log request (this also
/// bounds the client's own retries of throttled or unavailable responses).
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound on delivered lines remembered for that de-duplication.
const MAX_REMEMBERED: usize = 200_000;

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
    /// When the UI was last woken for this stream (all of its containers
    /// share one budget).
    last_ping: Mutex<Instant>,
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

    /// Whether a wake-up may be sent now; at most one per `PING_INTERVAL`
    /// for the whole stream.
    fn may_ping(&self) -> bool {
        let mut last = self.last_ping.lock();
        if last.elapsed() >= PING_INTERVAL {
            *last = Instant::now();
            true
        } else {
            false
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
/// stamped in order (stdout and stderr interleave), so neither "newer than
/// the last timestamp" nor "the same count at that timestamp" is a safe
/// rule. Instead we remember the recent lines themselves, ask the server to
/// replay a little more than necessary, and drop each replayed line that
/// matches one we remember.
///
/// Only the stream's own timestamps are used, never the local clock: the
/// node's clock may be ahead of or behind ours by any amount.
#[derive(Default)]
struct ResumePoint {
    /// Timestamps of the last few delivered lines, newest last. Their median
    /// is where we have got to; a single line stamped far in the future (or
    /// past) cannot move it.
    tail: VecDeque<i64>,
    /// (timestamp, fingerprint) of recently delivered lines in arrival order.
    recent: VecDeque<(i64, u64)>,
    /// Fingerprints still expected in the replay, with multiplicity.
    skip: HashMap<u64, u32>,
    /// Lines stamped before this have been dropped from `recent`, so a
    /// replay must not reach back that far: they would not be recognised.
    forgotten_before: i64,
    /// Delivered lines stamped far ahead of the position (the node's clock
    /// jumped forward for a moment, or a line is simply wrong). The server
    /// sends them again on every resume until the stream's time reaches
    /// them, so they are set aside rather than forgotten.
    ahead: VecDeque<(i64, u64)>,
}

/// How far past the position a timestamp must be to count as "far ahead".
const AHEAD_MS: i64 = 60_000;
const MAX_AHEAD: usize = 50_000;

fn fingerprint(time_ms: i64, text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    time_ms.hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

impl ResumePoint {
    const TAIL: usize = 5;

    /// Timestamp the stream has reached, robust to one outlier: the (lower)
    /// median, so that of only two lines the earlier one counts. Resuming
    /// too early costs a few recognised repeats; too late loses lines.
    fn position(&self) -> Option<i64> {
        if self.tail.is_empty() {
            return None;
        }
        let mut sorted: Vec<i64> = self.tail.iter().copied().collect();
        sorted.sort_unstable();
        Some(sorted[(sorted.len() - 1) / 2])
    }

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
        if self.tail.len() == Self::TAIL {
            self.tail.pop_front();
        }
        self.tail.push_back(time_ms);
        self.recent.push_back((time_ms, fp));
        // Forget what a future replay can no longer contain: lines older
        // than its starting point. Lines stamped far ahead of the stream
        // move to `ahead`, so that they neither pin this queue nor come
        // back as new lines after a reconnect.
        let position = self.position().unwrap_or(time_ms);
        let since = position - REPLAY_MS - 1_000;
        while let Some(&(t, fp)) = self.recent.front() {
            if t < since {
                self.forgotten_before = self.forgotten_before.max(since);
            } else if t > position + AHEAD_MS {
                if self.ahead.len() == MAX_AHEAD {
                    self.ahead.pop_front();
                }
                self.ahead.push_back((t, fp));
            } else if self.recent.len() <= MAX_REMEMBERED {
                break;
            }
            self.recent.pop_front();
        }
        true
    }

    /// Prepares for a replay and returns the time to resume from, if any
    /// line with a timestamp was delivered.
    fn begin_resume(&mut self) -> Option<i64> {
        let position = self.position()?;
        let mut since = position - REPLAY_MS;
        if self.forgotten_before > position {
            // The stream's time went back past what was forgotten: the
            // node's clock was set back, or a run of lines was stamped far
            // ahead. Holding out for it to catch up would lose everything
            // written until then; starting from here repeats, once, at
            // most the few seconds of lines before the step.
            self.forgotten_before = 0;
        } else {
            // The position can also dip a little (a burst of lines from the
            // other output stream, stamped slightly earlier). Never ask
            // for lines that were already forgotten; the server rounds the
            // time down to a whole second, hence the rounding up here.
            let forgotten = self.forgotten_before + (-self.forgotten_before).rem_euclid(1000);
            since = since.max(forgotten);
        }
        // Lines set aside as "ahead" that the stream's time has now passed
        // will not be sent again.
        self.ahead.retain(|(t, _)| *t >= since - 1_000);
        self.skip.clear();
        for (_, fp) in self.recent.iter().chain(&self.ahead) {
            *self.skip.entry(*fp).or_insert(0) += 1;
        }
        Some(since)
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
    // Why the last attempt to open the stream failed.
    let mut last_error = String::new();
    // Lines delivered since the UI was last woken for them.
    let mut unannounced = false;
    // The most recent client that the session considered good.
    let mut client = first_client;
    loop {
        // After a reconnect the session has a new client; the old one may
        // point at a server or credentials that no longer work. While the
        // session is (re)connecting there is nothing useful to try, and
        // waiting must not use up the retry budget.
        match provider() {
            Some(current) => client = current,
            None if connected_once => {
                shared.set_state(idx, SourceState::Reconnecting);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            None => {}
        }
        let pods: Api<Pod> = Api::namespaced(client.clone(), &target.namespace);
        let mut lp = LogParams {
            container: Some(target.container.clone()),
            follow: opts.follow,
            previous: opts.previous,
            timestamps: true,
            ..Default::default()
        };
        match resume.begin_resume() {
            Some(since_ms) => lp.since_time = jiff::Timestamp::from_millisecond(since_ms).ok(),
            None => {
                lp.tail_lines = opts.tail;
                lp.since_seconds = opts.since_seconds;
            }
        }
        // Whether this attempt got anywhere: it delivered a line, or the
        // connection was established and stayed up for a while.
        let mut healthy = false;
        // (Later attempts wait longer: a server that needs more than the
        // first allowance every time is slow, and should still be heard.)
        let patience = OPEN_TIMEOUT * (attempt + 1).min(4);
        let opening = match tokio::time::timeout(patience, pods.log_stream(&target.pod, &lp)).await {
            Ok(Ok(reader)) => Ok(reader),
            Ok(Err(e)) => Err((is_gone(&e), false, friendly(&e))),
            Err(_) => Err((false, true, "no response from the API server".to_string())),
        };
        match opening {
            Err((gone, timed_out, why)) => {
                // Final: the pod is gone, this is a one-off read, or the
                // very first attempt was refused (the reason is shown). A
                // first attempt that merely got no answer in time is
                // retried like any later failure: a busy server is slow,
                // not broken.
                if gone || !opts.follow || opts.previous || (!connected_once && !timed_out) {
                    let state = if connected_once { SourceState::Ended } else { SourceState::Failed(why) };
                    shared.set_state(idx, state);
                    notify.ping();
                    return;
                }
                last_error = why;
            }
            Ok(reader) => {
                let opened = Instant::now();
                connected_once = true;
                shared.set_state(idx, SourceState::Streaming);
                notify.ping();
                let mut reader = Box::pin(reader);
                let mut buf = Vec::with_capacity(1024);
                loop {
                    // `read_until` keeps partial data in `buf` when it is
                    // cancelled, so the timeout loses nothing.
                    let wait = if unannounced { PING_INTERVAL } else { Duration::from_secs(3600) };
                    let read = tokio::time::timeout(wait, read_line_capped(&mut reader, &mut buf, MAX_LINE_BYTES)).await;
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
                                healthy = true;
                                unannounced = true;
                            }
                            buf.clear();
                        }
                        // End of stream (possibly mid-line) or a read error.
                        Ok(_) => break,
                    }
                    if unannounced && shared.may_ping() {
                        unannounced = false;
                        if shared.visible.load(Ordering::Relaxed) {
                            notify.ping();
                        }
                    }
                }
                if unannounced {
                    unannounced = false;
                    notify.ping();
                }
                healthy |= opened.elapsed() >= Duration::from_secs(1);
                if !opts.follow || opts.previous {
                    break;
                }
            }
        }
        // The stream ends when the container restarts or the connection
        // drops: back off and resume where we stopped. A connection that
        // delivered something or stayed up was healthy, however often an
        // idle timeout in between cuts it; only consecutive attempts that
        // achieve nothing (including slow failures) count towards giving up.
        if healthy {
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
    // Out of retries (or a one-off read that is complete).
    let state = if connected_once { SourceState::Ended } else { SourceState::Failed(last_error) };
    shared.set_state(idx, state);
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
        last_ping: Mutex::new(Instant::now() - PING_INTERVAL),
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

    #[test]
    fn endless_lines_are_capped() {
        let mut input: Vec<u8> = vec![b'x'; 300_000];
        input.extend_from_slice(b"\nnext\n");
        let mut reader = futures::io::Cursor::new(input);
        let mut buf = Vec::new();
        let n = futures::executor::block_on(read_line_capped(&mut reader, &mut buf, 1024)).unwrap();
        assert_eq!(n, 300_001, "the whole line is consumed");
        assert_eq!(buf.len(), 1025, "but only the cap is kept, plus the newline");
        assert_eq!(buf.last(), Some(&b'\n'));
        buf.clear();
        futures::executor::block_on(read_line_capped(&mut reader, &mut buf, 1024)).unwrap();
        assert_eq!(buf, b"next\n");
        buf.clear();
        assert_eq!(futures::executor::block_on(read_line_capped(&mut reader, &mut buf, 1024)).unwrap(), 0);
    }

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
        assert_eq!(r.begin_resume(), None, "nothing delivered yet: no resume point");
        // First connection.
        assert!(r.admit(100_000, "a"));
        assert!(r.admit(100_500, "b"));
        assert!(r.admit(100_500, "b"), "identical lines are both real");
        assert!(r.admit(100_500, "c"));
        // Resume replays from before the point we reached.
        assert_eq!(r.begin_resume(), Some(100_500 - REPLAY_MS));
        assert!(!r.admit(100_000, "a"));
        assert!(!r.admit(100_500, "b"));
        assert!(!r.admit(100_500, "b"));
        assert!(r.admit(100_500, "b"), "a third identical line was never delivered");
        assert!(!r.admit(100_500, "c"));
        assert!(r.admit(100_600, "d"));
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
        r.begin_resume();
        assert!(!r.admit(10_000, "seq=1"));
        assert!(r.admit(10_600, "seq=3 (stderr, stamped earlier)"));
        assert!(!r.admit(10_700, "seq=2"));
        assert!(r.admit(10_800, "seq=4"));
    }

    #[test]
    fn resume_point_is_not_moved_by_one_wild_timestamp() {
        let hour = 3_600_000;
        let mut r = ResumePoint::default();
        for (i, t) in [10_000, 10_100, 10_200].into_iter().enumerate() {
            assert!(r.admit(t, &format!("line {i}")));
        }
        // One line stamped an hour ahead, even as the very last line before
        // the connection drops, must not make the resume skip an hour.
        assert!(r.admit(10_300 + hour, "from the future"));
        assert_eq!(r.begin_resume(), Some(10_100 - REPLAY_MS));
        // The same with only two lines to go by: the earlier one counts.
        let mut r = ResumePoint::default();
        assert!(r.admit(10_000, "first"));
        assert!(r.admit(10_000 + hour, "from the future"));
        assert_eq!(r.begin_resume(), Some(10_000 - REPLAY_MS));
        // ... and neither may one stamped far in the past pull it back.
        let mut r = ResumePoint::default();
        for t in [50_000_000, 50_000_100, 50_000_200, 1_000, 50_000_300] {
            assert!(r.admit(t, "x"));
        }
        // (The median of the last five: 1_000 is the outlier.)
        assert_eq!(r.begin_resume(), Some(50_000_100 - REPLAY_MS));
    }

    #[test]
    fn resume_works_whatever_the_node_clock_says() {
        // The node's clock is far ahead of ours: every timestamp is "in the
        // future". Nothing may be lost or repeated across a resume.
        let base = 9_000_000_000_000i64;
        let mut r = ResumePoint::default();
        let lines: Vec<(i64, String)> = (0..2_000).map(|i| (base + i * 3, format!("seq={i}"))).collect();
        for (t, text) in &lines {
            assert!(r.admit(*t, text));
        }
        let since = r.begin_resume().unwrap();
        // The server replays everything stamped at or after `since`
        // (rounded down to a whole second, as the API does) plus new lines.
        let floor = since - since.rem_euclid(1000);
        let replayed = lines.iter().filter(|(t, _)| *t >= floor);
        let duplicates = replayed.filter(|(t, text)| r.admit(*t, text)).count();
        assert_eq!(duplicates, 0, "replayed lines must all be recognised");
        assert!(r.admit(base + 2_000 * 3, "seq=2000"), "new lines are delivered");
    }

    /// Plays a container log through kills and resumes the way the kubelet
    /// serves it: after each kill, everything stamped at or after the resume
    /// time (rounded down to a second) is sent again, in file order.
    /// Returns how often each line was delivered.
    fn deliveries(log: &[(i64, String)], kills: &[usize]) -> Vec<u32> {
        let mut r = ResumePoint::default();
        let mut seen = vec![0u32; log.len()];
        let mut from = 0;
        let mut floor = i64::MIN;
        for end in kills.iter().copied().chain([log.len()]) {
            for (i, (t, text)) in log.iter().enumerate().take(end).skip(from) {
                if *t >= floor && r.admit(*t, text) {
                    seen[i] += 1;
                }
            }
            if end == log.len() {
                break;
            }
            let since = r.begin_resume().unwrap();
            floor = since - since.rem_euclid(1000);
            // The replay starts at the first line that qualifies.
            from = log.iter().position(|(t, _)| *t >= floor).unwrap_or(end);
        }
        seen
    }

    #[test]
    fn resume_survives_bursts_stamped_earlier() {
        // stdout lines every 10 ms; after every 20 of them, three stderr
        // lines stamped `skew` earlier. When those three are the latest
        // lines, the position moves back, which must not replay lines that
        // are no longer remembered.
        for skew in [50i64, 300, 2_000, 4_000] {
            let mut log = Vec::new();
            let mut t = 1_700_000_000_000i64;
            for group in 0..600 {
                for i in 0..20 {
                    log.push((t, format!("out {group}/{i}")));
                    t += 10;
                }
                for i in 0..3 {
                    log.push((t - skew, format!("err {group}/{i}")));
                    t += 10;
                }
            }
            // Kills at every phase of a group, including right after a burst.
            let kills: Vec<usize> = (0..40).map(|k| 3_000 + k * 233).collect();
            let seen = deliveries(&log, &kills);
            let repeated = seen.iter().filter(|n| **n > 1).count();
            let missing = seen.iter().filter(|n| **n == 0).count();
            assert_eq!((repeated, missing), (0, 0), "skew {skew} ms");
        }
    }

    #[test]
    fn resume_survives_a_clock_that_jumps_ahead_and_back() {
        // Lines every 10 ms. For a while the node's clock is an hour (or
        // half an hour) ahead, then it is corrected.
        for jump in [3_600_000i64, 1_800_000, 120_000] {
            let base = 1_700_000_000_000i64;
            let mut log = Vec::new();
            for i in 0..3_000i64 {
                let ahead = if (600..800).contains(&i) { jump } else { 0 };
                log.push((base + i * 10 + ahead, format!("seq={i}")));
            }
            // Resumes well after the excursion: nothing lost, nothing
            // repeated (not even the lines stamped ahead, which the server
            // sends again every time).
            let seen = deliveries(&log, &[1_600, 2_000, 2_400]);
            let repeated = seen.iter().filter(|n| **n > 1).count();
            let missing = seen.iter().filter(|n| **n == 0).count();
            assert_eq!((repeated, missing), (0, 0), "jump {jump} ms");
            // A resume right after the correction may repeat the seconds
            // before the jump, but must never lose a line.
            let seen = deliveries(&log, &[830, 1_200, 2_000]);
            let repeated = seen.iter().filter(|n| **n > 1).count();
            let missing = seen.iter().filter(|n| **n == 0).count();
            assert_eq!(missing, 0, "jump {jump} ms");
            assert!(repeated <= 600, "jump {jump} ms: {repeated} lines repeated");
        }
    }

    #[test]
    fn two_lines_with_the_second_stamped_early() {
        let hour = 3_600_000;
        let log = vec![(10 * hour, "first".to_string()), (9 * hour, "second, stamped an hour early".to_string())];
        let mut more = log.clone();
        more.extend((0..20).map(|i| (10 * hour + 1_000 * (i + 1), format!("later {i}"))));
        let seen = deliveries(&more, &[2, 10]);
        assert!(seen.iter().all(|n| *n == 1), "{seen:?}");
    }

    #[test]
    fn remembered_lines_stay_bounded() {
        let mut r = ResumePoint::default();
        // A long stream with one future-stamped line early on.
        assert!(r.admit(1_000_000 + 7_200_000, "wild"));
        for i in 0..50_000i64 {
            r.admit(1_000_000 + i * 10, "line");
        }
        // Only about the replay window is kept: ~6 s at 100 lines/s.
        assert!(r.recent.len() < 2_000, "remembered {} lines", r.recent.len());
    }

    #[test]
    fn nothing_is_observable_after_stop() {
        let shared = Shared {
            buf: Mutex::new(LogBuffer::default()),
            states: Mutex::new(vec![SourceState::Streaming]),
            aborts: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            visible: AtomicBool::new(true),
            last_ping: Mutex::new(Instant::now()),
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
                last_ping: Mutex::new(Instant::now()),
            };
            shared.stop();
            let late = tokio::spawn(std::future::pending::<()>());
            shared.track(late.abort_handle());
            assert!(late.await.unwrap_err().is_cancelled());
            assert_eq!(shared.states.lock()[0], SourceState::Ended);
        });
    }
}
