//! Lifecycle tests against a live cluster (the `demo` cluster from
//! `hack/cluster-up.sh`). Skipped when it is not running.
//!
//! These guard behaviour that unit tests cannot see: background work must
//! stop when its handle is dropped, and reconnects must reach every table.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use skiff_core::kinds;
use skiff_core::{ConnStatus, Core, ExecState, LoadState, LogOptions, LogTarget, Notifier, Session, SourceState};

/// Tests that change `$KUBECONFIG` must not overlap.
static ENV: Mutex<()> = Mutex::new(());

fn kubeconfig() -> Option<PathBuf> {
    let path = std::env::var("SKIFF_TEST_KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hack/kubeconfig"));
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: no kubeconfig at {}", path.display());
        None
    }
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn connected(core: &Core, context: &str) -> std::sync::Arc<Session> {
    let session = core.connect(context);
    wait_for("connection", Duration::from_secs(20), || matches!(&*session.status(), ConnStatus::Connected { .. }));
    session
}

/// A running pod in the demo cluster, as (namespace, name).
fn some_running_pod(session: &Session) -> (String, String) {
    let store = session.table(&kinds::pods());
    wait_for("pods", Duration::from_secs(20), || store.snapshot().state == LoadState::Live);
    let snap = store.snapshot();
    let status = snap.column("Status").map(|i| &snap.columns[i]).expect("status column");
    let row = snap
        .rows
        .iter()
        .find(|r| r.text(status) == "Running" && &*r.namespace == "monitoring")
        .or_else(|| snap.rows.iter().find(|r| r.text(status) == "Running"))
        .expect("a running pod");
    (row.namespace.to_string(), row.name.to_string())
}

#[test]
fn dropping_a_log_stream_or_shell_stops_its_tasks() {
    let Some(kc) = kubeconfig() else { return };
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    let core = Core::new(Notifier::noop()).unwrap();
    let session = connected(&core, "demo");
    let (ns, pod) = some_running_pod(&session);
    let containers = session.pod_containers(&ns, &pod);
    wait_for("containers", Duration::from_secs(10), || containers.is_done());
    let container = containers.take().unwrap().unwrap().remove(0).name;

    let metrics = core.handle().metrics();
    let settle = Duration::from_millis(300);
    std::thread::sleep(settle);
    let baseline = metrics.num_alive_tasks();

    let target = LogTarget { namespace: ns.clone(), pod: pod.clone(), container: container.clone() };
    let logs = session.logs(vec![target], LogOptions::default());
    wait_for("log stream", Duration::from_secs(15), || logs.states() == [SourceState::Streaming]);
    assert!(metrics.num_alive_tasks() > baseline, "log stream should be running tasks");
    drop(logs);
    wait_for("log tasks to stop after drop", Duration::from_secs(5), || metrics.num_alive_tasks() <= baseline);

    let shell = session.exec(&ns, &pod, &container, vec!["sh".into()]);
    wait_for("shell", Duration::from_secs(15), || shell.state() == ExecState::Running);
    assert!(metrics.num_alive_tasks() > baseline, "exec should be running tasks");
    drop(shell);
    wait_for("exec tasks to stop after drop", Duration::from_secs(5), || metrics.num_alive_tasks() <= baseline);
}

#[test]
fn stopping_a_port_forward_closes_open_connections() {
    let Some(kc) = kubeconfig() else { return };
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    let core = Core::new(Notifier::noop()).unwrap();
    let session = connected(&core, "demo");
    let (ns, pod) = some_running_pod(&session);

    let job = session.forward_pod(&ns, &pod, 80, None);
    wait_for("forward", Duration::from_secs(10), || job.is_done());
    let fwd = job.take().unwrap().expect("forward starts");
    session.track_forward(fwd.clone());
    let addr = format!("127.0.0.1:{}", fwd.local_port);

    // One request proves the tunnel works.
    let mut conn = std::net::TcpStream::connect(&addr).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n").unwrap();
    let mut buf = [0u8; 256];
    let n = conn.read(&mut buf).unwrap();
    assert!(n > 0 && buf.starts_with(b"HTTP/1."), "expected an HTTP response through the tunnel");

    // Stopping must close the established connection too, not only refuse
    // new ones.
    session.stop_forward(fwd.id);
    assert!(!fwd.is_active());
    let deadline = Instant::now() + Duration::from_secs(5);
    let closed = loop {
        let _ = conn.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        match conn.read(&mut buf) {
            Ok(0) => break true,
            Err(e) if e.kind() != std::io::ErrorKind::WouldBlock && e.kind() != std::io::ErrorKind::TimedOut => break true,
            _ if Instant::now() > deadline => break false,
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(closed, "existing connection kept forwarding after stop");
    assert!(std::net::TcpStream::connect(&addr).is_err(), "listener should be closed");
}

#[test]
fn reconnect_reaches_tables_opened_afterwards() {
    let Some(kc) = kubeconfig() else { return };
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let good = std::fs::read_to_string(&kc).unwrap();
    let dir = std::env::temp_dir().join(format!("skiff-live-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("kubeconfig");
    // Start with a kubeconfig whose context cannot be loaded at all.
    std::fs::write(&path, "apiVersion: v1\nkind: Config\ncontexts: []\nclusters: []\nusers: []\n").unwrap();
    unsafe { std::env::set_var("KUBECONFIG", &path) };

    let core = Core::new(Notifier::noop()).unwrap();
    let session = core.connect("demo");
    wait_for("first attempt to fail", Duration::from_secs(10), || matches!(&*session.status(), ConnStatus::Failed(_)));
    // No table exists yet, so nothing is subscribed to the client channel.
    assert_eq!(session.active_watches(), 0);

    std::fs::write(&path, good).unwrap();
    session.reconnect();
    wait_for("reconnect", Duration::from_secs(20), || matches!(&*session.status(), ConnStatus::Connected { .. }));

    let store = session.table(&kinds::nodes());
    wait_for("nodes after reconnect", Duration::from_secs(20), || {
        let snap = store.snapshot();
        assert_ne!(snap.state, LoadState::Failed, "table got a stale failed client: {:?}", snap.error);
        snap.state == LoadState::Live && !snap.rows.is_empty()
    });
    // Metrics follow the new client as well.
    wait_for("metrics after reconnect", Duration::from_secs(40), || session.metrics().available);

    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refresh_relists_and_recovers() {
    let Some(kc) = kubeconfig() else { return };
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    let core = Core::new(Notifier::noop()).unwrap();
    let session = connected(&core, "demo");
    let store = session.table(&kinds::builtin("", "configmaps").unwrap().key());
    wait_for("configmaps", Duration::from_secs(20), || store.snapshot().state == LoadState::Live);
    let before = store.snapshot();
    assert!(!before.rows.is_empty());
    // Several rapid refreshes must settle back to Live with the same data
    // rather than piling up extra lists or getting stuck.
    for _ in 0..5 {
        store.refresh();
    }
    wait_for("generation to advance", Duration::from_secs(10), || store.snapshot().generation > before.generation);
    wait_for("live again", Duration::from_secs(20), || store.snapshot().state == LoadState::Live);
    std::thread::sleep(Duration::from_millis(500));
    let after = store.snapshot();
    assert_eq!(after.state, LoadState::Live);
    assert!(after.rows.len() + 3 >= before.rows.len(), "rows lost on refresh: {} -> {}", before.rows.len(), after.rows.len());
}
