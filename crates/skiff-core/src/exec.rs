//! Interactive exec sessions (a shell inside a container).

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

/// State shared with the background task. The task holds only this, never
/// the `ExecSession` handle, so dropping the handle ends the remote process.
struct Shared {
    output: Mutex<Vec<u8>>,
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
}

impl ExecSession {
    /// Sends keystrokes / pasted text to the remote stdin.
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.input.send(bytes.to_vec());
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.resize.send((cols, rows));
    }

    /// Removes and returns everything the process printed since last call.
    pub fn take_output(&self) -> Vec<u8> {
        std::mem::take(&mut *self.shared.output.lock())
    }

    pub fn state(&self) -> ExecState {
        self.shared.state.lock().clone()
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

async fn drive(
    shared: Arc<Shared>,
    target: Target,
    client: Client,
    command: Vec<String>,
    mut input: mpsc::UnboundedReceiver<Vec<u8>>,
    mut resize: mpsc::UnboundedReceiver<(u16, u16)>,
    closing: Arc<Notify>,
    notify: Notifier,
) -> Result<String, String> {
    let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
    let ap = AttachParams::interactive_tty().container(target.container.clone());
    let mut proc = tokio::select! {
        p = pods.exec(&target.pod, command, &ap) => p.map_err(|e| friendly(&e))?,
        _ = closing.notified() => return Ok("closed".into()),
    };
    let mut stdin = proc.stdin().ok_or("no stdin")?;
    let mut stdout = proc.stdout().ok_or("no stdout")?;
    let mut size_tx = proc.terminal_size();
    let status = proc.take_status();
    *shared.state.lock() = ExecState::Running;
    notify.ping();

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            _ = closing.notified() => {
                // End of input first, so the remote shell sees EOF and
                // exits; then give the connection a moment to wind down
                // before it is dropped.
                drop(stdin);
                let _ = tokio::time::timeout(std::time::Duration::from_millis(800), proc.join()).await;
                return Ok("closed".into());
            }
            n = stdout.read(&mut buf) => {
                match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        shared.output.lock().extend_from_slice(&buf[..n]);
                        notify.ping();
                    }
                }
            }
            Some(bytes) = input.recv() => {
                if stdin.write_all(&bytes).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
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

pub(crate) fn open(
    handle: &tokio::runtime::Handle,
    client: impl std::future::Future<Output = Result<Client, String>> + Send + 'static,
    namespace: &str,
    pod: &str,
    container: &str,
    command: Vec<String>,
    notify: Notifier,
) -> Arc<ExecSession> {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (rs_tx, rs_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared { output: Mutex::new(Vec::new()), state: Mutex::new(ExecState::Connecting) });
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
    });
    let target = Target { namespace: namespace.into(), pod: pod.into(), container: container.into() };
    let s = shared;
    let task = handle.spawn(async move {
        let result = match client.await {
            Ok(c) => drive(s.clone(), target, c, command, in_rx, rs_rx, closing, notify.clone()).await,
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
