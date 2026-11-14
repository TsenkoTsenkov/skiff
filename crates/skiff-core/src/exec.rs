//! Interactive exec sessions (a shell inside a container).

use std::sync::Arc;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, AttachParams, TerminalSize};
use kube::Client;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
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
    abort: Mutex<Option<AbortHandle>>,
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

    pub fn close(&self) {
        if let Some(h) = self.abort.lock().take() {
            h.abort();
        }
        let mut s = self.shared.state.lock();
        if matches!(*s, ExecState::Connecting | ExecState::Running) {
            *s = ExecState::Exited("closed".into());
        }
    }
}

impl Drop for ExecSession {
    fn drop(&mut self) {
        if let Some(h) = self.abort.lock().take() {
            h.abort();
        }
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
    notify: Notifier,
) -> Result<String, String> {
    let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
    let ap = AttachParams::interactive_tty().container(target.container.clone());
    let mut proc = pods.exec(&target.pod, command, &ap).await.map_err(|e| friendly(&e))?;
    let mut stdin = proc.stdin().ok_or("no stdin")?;
    let mut stdout = proc.stdout().ok_or("no stdout")?;
    let mut size_tx = proc.terminal_size();
    let status = proc.take_status();
    *shared.state.lock() = ExecState::Running;
    notify.ping();

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
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
    let session = Arc::new(ExecSession {
        namespace: namespace.into(),
        pod: pod.into(),
        container: container.into(),
        shared: shared.clone(),
        input: in_tx,
        resize: rs_tx,
        abort: Mutex::new(None),
    });
    let target = Target { namespace: namespace.into(), pod: pod.into(), container: container.into() };
    let s = shared;
    let task = handle.spawn(async move {
        let result = match client.await {
            Ok(c) => drive(s.clone(), target, c, command, in_rx, rs_rx, notify.clone()).await,
            Err(e) => Err(e),
        };
        *s.state.lock() = match result {
            Ok(reason) => ExecState::Exited(reason),
            Err(e) => ExecState::Failed(e),
        };
        notify.ping();
    });
    *session.abort.lock() = Some(task.abort_handle());
    session
}
