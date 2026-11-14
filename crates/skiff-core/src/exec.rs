//! Interactive exec sessions (a shell inside a container).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, AttachParams, TerminalSize};
use kube::Client;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Notify};
use tokio::task::AbortHandle;

use crate::util::{friendly, Notifier};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecState {
    Connecting,
    Running,
    Exited(String),
    Failed(String),
}

/// Output waiting to be drained is capped: beyond this the task stops
/// reading, and the remote process blocks on its output like on any slow
/// terminal, instead of filling our memory (`yes`, `cat` of a huge file, a
/// window nobody is looking at).
const MAX_PENDING_OUTPUT: usize = 4 * 1024 * 1024;
/// Input waiting to be written is capped too: a remote that does not read
/// (and a program that keeps asking the terminal questions) must not make
/// the queue grow without end. What does not fit is dropped.
const MAX_PENDING_INPUT: usize = 8 * 1024 * 1024;

/// Output not yet taken: `bytes[start..]`.
#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    start: usize,
}

impl Output {
    fn len(&self) -> usize {
        self.bytes.len() - self.start
    }
}

/// State shared with the background task. The task holds only this, never
/// the `ExecSession` handle, so dropping the handle ends the remote process.
struct Shared {
    output: Mutex<Output>,
    /// Signalled when `output` was drained.
    drained: Notify,
    /// Bytes of input queued and not yet written.
    queued_input: AtomicUsize,
    state: Mutex<ExecState>,
}

/// A running exec. Output bytes accumulate until the terminal widget drains
/// them; input and resize requests are queued to the remote process.
/// Dropping it (or calling `close`) terminates the session.
pub struct ExecSession {
    pub namespace: String,
    pub pod: String,
    pub container: String,
    shared: Arc<Shared>,
    input: mpsc::UnboundedSender<Vec<u8>>,
    resize: mpsc::UnboundedSender<(u16, u16)>,
    /// Asks the task to end the session cleanly.
    closing: Arc<Notify>,
    abort: Mutex<Option<AbortHandle>>,
    handle: tokio::runtime::Handle,
    notify: Notifier,
}

impl ExecSession {
    /// Sends keystrokes / pasted text to the remote stdin. Returns false if
    /// it was dropped: the session is over, or more input is already
    /// waiting than a remote that reads at all would leave.
    pub fn write(&self, bytes: &[u8]) -> bool {
        if self.shared.queued_input.load(Ordering::Relaxed) + bytes.len() > MAX_PENDING_INPUT {
            return false;
        }
        self.shared.queued_input.fetch_add(bytes.len(), Ordering::Relaxed);
        self.input.send(bytes.to_vec()).is_ok()
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.resize.send((cols, rows));
    }

    /// Removes and returns everything the process printed since last call.
    pub fn take_output(&self) -> Vec<u8> {
        self.take_output_up_to(usize::MAX)
    }

    /// Removes and returns at most `max` bytes of what the process printed.
    /// If more is waiting, the UI is woken again so it can come back for
    /// it: this is how a terminal takes a flood in slices without stalling.
    pub fn take_output_up_to(&self, max: usize) -> Vec<u8> {
        let (bytes, more) = {
            let mut out = self.shared.output.lock();
            let n = out.len().min(max);
            let bytes = out.bytes[out.start..out.start + n].to_vec();
            out.start += n;
            if out.start == out.bytes.len() {
                out.bytes.clear();
                out.start = 0;
            } else if out.start >= 1024 * 1024 {
                // Give the taken part of the buffer back now and then.
                let start = out.start;
                out.bytes.drain(..start);
                out.start = 0;
            }
            (bytes, out.len() > 0)
        };
        if !bytes.is_empty() {
            self.shared.drained.notify_one();
        }
        if more {
            self.notify.ping();
        }
        bytes
    }

    pub fn state(&self) -> ExecState {
        self.shared.state.lock().clone()
    }

    /// (input bytes queued and not yet written, output bytes not yet taken).
    pub fn pending(&self) -> (usize, usize) {
        (self.shared.queued_input.load(Ordering::Relaxed), self.shared.output.lock().len())
    }

    /// Ends the session: the remote process gets end-of-input (so a shell
    /// exits instead of being orphaned), then the connection is closed. The
    /// task finishes on its own within about a second.
    pub fn close(&self) {
        self.end();
        let mut s = self.shared.state.lock();
        if matches!(*s, ExecState::Connecting | ExecState::Running) {
            *s = ExecState::Exited("closed".into());
        }
    }
}

impl ExecSession {
    fn end(&self) {
        self.closing.notify_one();
        // Backstop in case the task is stuck somewhere that cannot see the
        // request (for example a write the remote side never reads).
        if let Some(task) = self.abort.lock().take() {
            self.handle.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                task.abort();
            });
        }
    }
}

impl Drop for ExecSession {
    fn drop(&mut self) {
        self.end();
    }
}

/// Tries bash, then falls back to sh, like `kubectl exec -it … -- sh`.
pub fn default_shell() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "command -v bash >/dev/null 2>&1 && exec bash || exec sh".into(),
    ]
}

struct Target {
    namespace: String,
    pod: String,
    container: String,
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    shared: Arc<Shared>,
    target: Target,
    client: Client,
    command: Vec<String>,
    tty: bool,
    mut input: mpsc::UnboundedReceiver<Vec<u8>>,
    mut resize: mpsc::UnboundedReceiver<(u16, u16)>,
    closing: Arc<Notify>,
    notify: Notifier,
) -> Result<String, String> {
    let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
    let ap = if tty {
        AttachParams::interactive_tty()
    } else {
        // Plain pipes: no terminal on the remote side, bytes pass unchanged.
        AttachParams::default().stdin(true).stdout(true).stderr(false).tty(false)
    }
    .container(target.container.clone());
    let mut proc = tokio::select! {
        p = pods.exec(&target.pod, command, &ap) => p.map_err(|e| friendly(&e))?,
        _ = closing.notified() => return Ok("closed".into()),
    };
    let mut stdin = proc.stdin().ok_or("no stdin")?;
    let mut stdout = proc.stdout().ok_or("no stdout")?;
    let mut size_tx = proc.terminal_size();
    let status = proc.take_status();
    {
        // Not if it was closed in the meantime: "exited" is final.
        let mut state = shared.state.lock();
        if matches!(*state, ExecState::Connecting) {
            *state = ExecState::Running;
        }
    }
    notify.ping();

    let mut buf = vec![0u8; 16 * 1024];
    // Input being written to the remote: the bytes, and how many are done.
    let mut writing: Option<(Vec<u8>, usize)> = None;
    loop {
        tokio::select! {
            _ = closing.notified() => {
                // End of input first (an explicit shutdown: merely dropping
                // the writer signals nothing), so the remote shell sees EOF
                // and exits. Give it a moment, then abort the connection
                // whatever happened. `proc` must stay owned here: handing
                // it to `join()` under a timeout would detach its task and
                // leak the connection.
                let _ = stdin.shutdown().await;
                let drain = async {
                    while matches!(stdout.read(&mut buf).await, Ok(n) if n > 0) {}
                };
                let _ = tokio::time::timeout(std::time::Duration::from_millis(800), drain).await;
                proc.abort();
                return Ok("closed".into());
            }
            // Reading pauses while too much output is waiting to be drained.
            _ = shared.drained.notified(), if shared.output.lock().len() >= MAX_PENDING_OUTPUT => {}
            n = stdout.read(&mut buf), if shared.output.lock().len() < MAX_PENDING_OUTPUT => {
                match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        shared.output.lock().bytes.extend_from_slice(&buf[..n]);
                        notify.ping();
                    }
                }
            }
            // Input is written a piece at a time, as one branch among the
            // others: while the remote is slow to take it, its output is
            // still read (or the two sides could wait on each other for
            // ever) and a request to close is still seen.
            written = async {
                let (bytes, done) = writing.as_ref().expect("only polled while there is input");
                stdin.write(&bytes[*done..]).await
            }, if writing.is_some() => {
                match written {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        shared.queued_input.fetch_sub(n, Ordering::Relaxed);
                        let (bytes, done) = writing.as_mut().expect("checked above");
                        *done += n;
                        if *done >= bytes.len() {
                            writing = None;
                            let _ = stdin.flush().await;
                        }
                    }
                }
            }
            Some(bytes) = input.recv(), if writing.is_none() => {
                if !bytes.is_empty() {
                    writing = Some((bytes, 0));
                }
            }
            Some((cols, rows)) = resize.recv() => {
                if let Some(tx) = size_tx.as_mut() {
                    use futures::SinkExt;
                    let _ = tx.send(TerminalSize { width: cols, height: rows }).await;
                }
            }
        }
    }
    let mut reason = "session ended".to_string();
    if let Some(fut) = status {
        if let Ok(Some(st)) = tokio::time::timeout(std::time::Duration::from_secs(2), fut).await {
            if st.status.as_deref() != Some("Success") {
                if let Some(msg) = st.message.filter(|m| !m.is_empty()) {
                    reason = msg;
                }
            }
        }
    }
    Ok(reason)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn open(
    handle: &tokio::runtime::Handle,
    client: impl std::future::Future<Output = Result<Client, String>> + Send + 'static,
    namespace: &str,
    pod: &str,
    container: &str,
    command: Vec<String>,
    tty: bool,
    notify: Notifier,
) -> Arc<ExecSession> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (rs_tx, rs_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        output: Mutex::new(Output::default()),
        drained: Notify::new(),
        queued_input: AtomicUsize::new(0),
        state: Mutex::new(ExecState::Connecting),
    });
    let closing = Arc::new(Notify::new());
    let session = Arc::new(ExecSession {
        namespace: namespace.into(),
        pod: pod.into(),
        container: container.into(),
        shared: shared.clone(),
        input: in_tx,
        resize: rs_tx,
        closing: closing.clone(),
        abort: Mutex::new(None),
        handle: handle.clone(),
        notify: notify.clone(),
    });
    let target = Target { namespace: namespace.into(), pod: pod.into(), container: container.into() };
    let s = shared;
    let task = handle.spawn(async move {
        let result = match client.await {
            Ok(c) => drive(s.clone(), target, c, command, tty, in_rx, rs_rx, closing, notify.clone()).await,
            Err(e) => Err(e),
        };
        let mut state = s.state.lock();
        // `close()` already recorded why the session ended.
        if !matches!(*state, ExecState::Exited(_)) {
            *state = match result {
                Ok(reason) => ExecState::Exited(reason),
                Err(e) => ExecState::Failed(e),
            };
        }
        drop(state);
        notify.ping();
    });
    *session.abort.lock() = Some(task.abort_handle());
    session
}
