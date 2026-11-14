//! Local TCP port forwarding to pods.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use k8s_openapi::api::core::v1::Pod;
use kube::api::Api;
use kube::Client;
use parking_lot::Mutex;
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use crate::util::{friendly, Notifier};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct PortForward {
    pub id: u64,
    pub namespace: String,
    pub pod: String,
    /// What the user asked to forward, e.g. `svc/web` or `pod/web-0`.
    pub label: String,
    pub remote_port: u16,
    pub local_port: u16,
    /// Connections served so far.
    pub connections: AtomicU64,
    pub last_error: Mutex<Option<String>>,
    abort: Mutex<Option<AbortHandle>>,
    /// Tasks serving established connections.
    conns: Mutex<Vec<AbortHandle>>,
    stopped: AtomicBool,
}

impl PortForward {
    pub fn url(&self) -> String {
        format!("http://localhost:{}", self.local_port)
    }

    /// Stops listening and closes every established connection.
    pub fn stop(&self) {
        // Set the flag under the lock the accept loop registers under, so a
        // connection accepted concurrently is either drained here or
        // aborted there.
        let mut conns = self.conns.lock();
        self.stopped.store(true, Ordering::SeqCst);
        for h in conns.drain(..) {
            h.abort();
        }
        drop(conns);
        if let Some(h) = self.abort.lock().take() {
            h.abort();
        }
    }

    pub fn is_active(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst)
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn serve_one(
    pods: Api<Pod>,
    pod: String,
    remote: u16,
    mut conn: tokio::net::TcpStream,
) -> Result<(), String> {
    let mut pf = pods.portforward(&pod, &[remote]).await.map_err(|e| friendly(&e))?;
    let mut upstream = pf.take_stream(remote).ok_or("port stream unavailable")?;
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
    drop(upstream);
    let _ = pf.join().await;
    Ok(())
}

pub(crate) async fn start(
    handle: tokio::runtime::Handle,
    client: Client,
    namespace: String,
    pod: String,
    label: String,
    remote_port: u16,
    local_port: Option<u16>,
    notify: Notifier,
) -> Result<Arc<PortForward>, String> {
    let addr: SocketAddr = ([127, 0, 0, 1], local_port.unwrap_or(0)).into();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    let fwd = Arc::new(PortForward {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        namespace: namespace.clone(),
        pod: pod.clone(),
        label,
        remote_port,
        local_port: local,
        connections: AtomicU64::new(0),
        last_error: Mutex::new(None),
        abort: Mutex::new(None),
        conns: Mutex::new(Vec::new()),
        stopped: AtomicBool::new(false),
    });
    let pods: Api<Pod> = Api::namespaced(client, &namespace);
    let weak = Arc::downgrade(&fwd);
    let h = handle.clone();
    let task = handle.spawn(async move {
        loop {
            let Ok((conn, _)) = listener.accept().await else { break };
            let Some(fwd) = weak.upgrade() else { break };
            fwd.connections.fetch_add(1, Ordering::Relaxed);
            let (pods, pod, notify) = (pods.clone(), pod.clone(), notify.clone());
            let weak = weak.clone();
            let conn_task = h.spawn(async move {
                if let Err(e) = serve_one(pods, pod, remote_port, conn).await {
                    if let Some(fwd) = weak.upgrade() {
                        *fwd.last_error.lock() = Some(e);
                    }
                }
                notify.ping();
            });
            let mut conns = fwd.conns.lock();
            conns.retain(|c| !c.is_finished());
            if fwd.stopped.load(Ordering::SeqCst) {
                conn_task.abort();
                break;
            }
            conns.push(conn_task.abort_handle());
        }
    });
    *fwd.abort.lock() = Some(task.abort_handle());
    Ok(fwd)
}
