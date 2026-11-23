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

use crate::logs::ClientProvider;
use crate::util::{friendly, Notifier};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Connections one forward serves at the same time. Each is a stream to the
/// API server and a task; something local that opens connections in a loop
/// must not be able to use those up. Further connections are closed at once.
const MAX_CONNECTIONS: usize = 256;
/// How long the API server may take to open the stream to the pod.
const OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

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
    let mut pf = match tokio::time::timeout(OPEN_TIMEOUT, pods.portforward(&pod, &[remote])).await {
        Ok(pf) => pf.map_err(|e| friendly(&e))?,
        Err(_) => return Err(format!("no response from the API server within {} s", OPEN_TIMEOUT.as_secs())),
    };
    let mut upstream = pf.take_stream(remote).ok_or("port stream unavailable")?;
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
    drop(upstream);
    let _ = pf.join().await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn start(
    handle: tokio::runtime::Handle,
    client: Client,
    provider: ClientProvider,
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
    let weak = Arc::downgrade(&fwd);
    let h = handle.clone();
    let task = handle.spawn(async move {
        loop {
            let Ok((conn, _)) = listener.accept().await else { break };
            let Some(fwd) = weak.upgrade() else { break };
            {
                let mut conns = fwd.conns.lock();
                conns.retain(|c| !c.is_finished());
                if conns.len() >= MAX_CONNECTIONS {
                    // Refuse: dropping the socket closes it.
                    drop(conns);
                    drop(conn);
                    *fwd.last_error.lock() =
                        Some(format!("more than {MAX_CONNECTIONS} connections at once; further ones are refused"));
                    notify.ping();
                    continue;
                }
            }
            fwd.connections.fetch_add(1, Ordering::Relaxed);
            // Use the session's current client, so forwards keep working
            // across a reconnect.
            let pods: Api<Pod> = Api::namespaced(provider().unwrap_or_else(|| client.clone()), &namespace);
            let (pod, notify) = (pod.clone(), notify.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// With the API server not answering, every accepted connection stays
    /// in flight; the one past the limit is closed straight away.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connections_beyond_the_limit_are_refused() {
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_addr = api.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = api.accept().await {
                held.push(sock);
            }
        });
        let client = Client::try_from(kube::Config::new(format!("http://{api_addr}").parse().unwrap())).unwrap();
        let fwd = start(
            tokio::runtime::Handle::current(),
            client,
            Arc::new(|| None),
            "x".into(),
            "p".into(),
            "pod/p".into(),
            80,
            None,
            Notifier::noop(),
        )
        .await
        .unwrap();
        let addr: SocketAddr = ([127, 0, 0, 1], fwd.local_port).into();
        let mut open = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            open.push(tokio::net::TcpStream::connect(addr).await.unwrap());
        }
        let mut extra = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut byte = [0u8; 1];
        // Closed by the forward: end of stream (or a reset), promptly.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), extra.read(&mut byte)).await;
        assert!(matches!(closed, Ok(Ok(0)) | Ok(Err(_))), "the extra connection stayed open: {closed:?}");
        assert_eq!(fwd.connections.load(Ordering::Relaxed), MAX_CONNECTIONS as u64);
        assert!(fwd.last_error.lock().as_deref().is_some_and(|e| e.contains("refused")));
        // The ones within the limit are still being served.
        let mut first = open.swap_remove(0);
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(200), first.read(&mut byte)).await;
        assert!(waiting.is_err(), "a connection within the limit was closed: {waiting:?}");
        fwd.stop();
    }
}
