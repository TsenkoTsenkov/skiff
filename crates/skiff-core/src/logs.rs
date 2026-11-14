//! Streaming pod logs from one or many containers into a bounded buffer.

use std::collections::VecDeque;
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
    Ended,
    Failed(String),
}

pub struct LogStream {
    pub targets: Vec<LogTarget>,
    pub options: LogOptions,
    buf: Mutex<LogBuffer>,
    states: Mutex<Vec<SourceState>>,
    aborts: Mutex<Vec<AbortHandle>>,
}

impl LogStream {
    pub fn with<R>(&self, f: impl FnOnce(&LogBuffer) -> R) -> R {
        f(&self.buf.lock())
    }

    pub fn states(&self) -> Vec<SourceState> {
        self.states.lock().clone()
    }

    pub fn clear(&self) {
        let mut b = self.buf.lock();
        b.lines.clear();
        b.generation += 1;
    }

    pub fn stop(&self) {
        for h in self.aborts.lock().drain(..) {
            h.abort();
        }
        for s in self.states.lock().iter_mut() {
            if matches!(s, SourceState::Connecting | SourceState::Streaming) {
                *s = SourceState::Ended;
            }
        }
    }

    fn set_state(&self, idx: usize, s: SourceState) {
        if let Some(slot) = self.states.lock().get_mut(idx) {
            *slot = s;
        }
    }

    fn push(&self, source: u16, raw: &[u8]) -> i64 {
        let mut raw = raw;
        while let Some((last, rest)) = raw.split_last() {
            if *last == b'\n' || *last == b'\r' {
                raw = rest;
            } else {
                break;
            }
        }
        let text = String::from_utf8_lossy(&raw[..raw.len().min(MAX_LINE_BYTES)]);
        let (time_ms, body) = split_timestamp(&text);
        let mut b = self.buf.lock();
        if b.lines.len() >= MAX_LINES {
            b.lines.pop_front();
            b.dropped += 1;
        }
        b.lines.push_back(LogLine { source, time_ms, text: body.into() });
        b.generation += 1;
        time_ms
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        for h in self.aborts.lock().drain(..) {
            h.abort();
        }
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

async fn pump(stream: Arc<LogStream>, client: Client, idx: usize, notify: Notifier) {
    let target = stream.targets[idx].clone();
    let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
    let opts = stream.options.clone();
    let mut last_ms: i64 = 0;
    let mut attempt = 0u32;
    loop {
        let mut lp = LogParams {
            container: Some(target.container.clone()),
            follow: opts.follow,
            previous: opts.previous,
            timestamps: true,
            ..Default::default()
        };
        if last_ms > 0 {
            // Resuming after a dropped connection: continue where we stopped.
            lp.since_time = jiff::Timestamp::from_millisecond(last_ms + 1).ok();
        } else {
            lp.tail_lines = opts.tail;
            lp.since_seconds = opts.since_seconds;
        }
        let reader = match pods.log_stream(&target.pod, &lp).await {
            Ok(r) => r,
            Err(e) => {
                stream.set_state(idx, SourceState::Failed(friendly(&e)));
                notify.ping();
                return;
            }
        };
        stream.set_state(idx, SourceState::Streaming);
        notify.ping();
        futures::pin_mut!(reader);
        let mut buf = Vec::with_capacity(1024);
        let mut got_any = false;
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(_) => {
                    let t = stream.push(idx as u16, &buf);
                    if t > 0 {
                        last_ms = t;
                    }
                    got_any = true;
                    notify.ping();
                }
                Err(_) => break,
            }
        }
        if !opts.follow || opts.previous {
            break;
        }
        // The stream ends when the container restarts or the connection
        // drops; retry a few times before giving up.
        attempt = if got_any { 0 } else { attempt + 1 };
        if attempt >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    stream.set_state(idx, SourceState::Ended);
    notify.ping();
}

pub(crate) fn open(
    handle: &tokio::runtime::Handle,
    client: impl std::future::Future<Output = Result<Client, String>> + Send + 'static,
    targets: Vec<LogTarget>,
    options: LogOptions,
    notify: Notifier,
) -> Arc<LogStream> {
    let stream = Arc::new(LogStream {
        states: Mutex::new(vec![SourceState::Connecting; targets.len()]),
        targets,
        options,
        buf: Mutex::new(LogBuffer::default()),
        aborts: Mutex::new(Vec::new()),
    });
    let s = stream.clone();
    let h = handle.clone();
    let task = handle.spawn(async move {
        let client = match client.await {
            Ok(c) => c,
            Err(e) => {
                for i in 0..s.targets.len() {
                    s.set_state(i, SourceState::Failed(e.clone()));
                }
                notify.ping();
                return;
            }
        };
        for idx in 0..s.targets.len() {
            let t = h.spawn(pump(s.clone(), client.clone(), idx, notify.clone()));
            s.aborts.lock().push(t.abort_handle());
        }
    });
    stream.aborts.lock().push(task.abort_handle());
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
}
