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

/// Requests of one kind the API server currently holds open (follow logs,
/// exec sessions, port-forwards), from its own metrics. This is the
/// server's view: it only goes down when the connection is really closed.
fn held_open(subresource: &str) -> u64 {
    let out = std::process::Command::new("kubectl")
        .args(["--context", "demo", "get", "--raw", "/metrics"])
        .output()
        .expect("kubectl is needed for this test");
    let needle = format!("subresource=\"{subresource}\"");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("apiserver_longrunning_requests{") && l.contains(&needle))
        .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
        .sum::<f64>() as u64
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

    // Let start-up work (discovery) finish so the task baseline is stable.
    wait_for("discovery", Duration::from_secs(20), || !session.discovered().is_empty());
    let metrics = core.handle().metrics();
    let mut baseline = metrics.num_alive_tasks();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(150));
        let now = metrics.num_alive_tasks();
        if now == baseline {
            break;
        }
        baseline = now;
    }
    let (logs_before, exec_before) = (held_open("log"), held_open("exec"));

    let target = LogTarget { namespace: ns.clone(), pod: pod.clone(), container: container.clone() };
    let logs = session.logs(vec![target], LogOptions::default());
    wait_for("log stream", Duration::from_secs(15), || logs.states() == [SourceState::Streaming]);
    assert!(metrics.num_alive_tasks() > baseline, "log stream should be running tasks");
    wait_for("server sees the follow", Duration::from_secs(5), || held_open("log") > logs_before);
    drop(logs);
    wait_for("log tasks to stop after drop", Duration::from_secs(5), || metrics.num_alive_tasks() <= baseline);
    wait_for("server-side log stream to close", Duration::from_secs(5), || held_open("log") <= logs_before);

    // Dropped and explicitly closed shells must both end on the server,
    // including shells whose process ignores end-of-input.
    for explicit_close in [false, true] {
        let shell = session.exec(&ns, &pod, &container, vec!["sh".into()]);
        wait_for("shell", Duration::from_secs(15), || shell.state() == ExecState::Running);
        wait_for("server sees the exec", Duration::from_secs(5), || held_open("exec") > exec_before);
        if explicit_close {
            shell.close();
            assert_eq!(shell.state(), ExecState::Exited("closed".into()));
        }
        drop(shell);
        wait_for("exec tasks to stop", Duration::from_secs(6), || metrics.num_alive_tasks() <= baseline);
        wait_for("server-side exec to close", Duration::from_secs(6), || held_open("exec") <= exec_before);
    }
}

#[test]
fn session_shutdown_ends_streams_and_shells() {
    let Some(kc) = kubeconfig() else { return };
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    let core = Core::new(Notifier::noop()).unwrap();
    let session = connected(&core, "demo");
    let (ns, pod) = some_running_pod(&session);
    let containers = session.pod_containers(&ns, &pod);
    wait_for("containers", Duration::from_secs(10), || containers.is_done());
    let container = containers.take().unwrap().unwrap().remove(0).name;
    let (logs_before, exec_before) = (held_open("log"), held_open("exec"));

    let target = LogTarget { namespace: ns.clone(), pod: pod.clone(), container: container.clone() };
    let logs = session.logs(vec![target], LogOptions::default());
    let shell = session.exec(&ns, &pod, &container, vec!["sh".into()]);
    wait_for("log stream", Duration::from_secs(15), || logs.states() == [SourceState::Streaming]);
    wait_for("shell", Duration::from_secs(15), || shell.state() == ExecState::Running);

    // The handles are still held: shutdown alone must end both.
    session.shutdown();
    wait_for("stream ended", Duration::from_secs(5), || logs.states() == [SourceState::Ended]);
    wait_for("shell ended", Duration::from_secs(5), || matches!(shell.state(), ExecState::Exited(_)));
    wait_for("server-side log stream to close", Duration::from_secs(6), || held_open("log") <= logs_before);
    wait_for("server-side exec to close", Duration::from_secs(6), || held_open("exec") <= exec_before);
    let lines = logs.with(|b| b.generation);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(logs.with(|b| b.generation), lines, "no lines may arrive after shutdown");
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

/// A process that prints without end must not fill our memory when nobody
/// drains its output: reading pauses at a cap and resumes when drained.
#[test]
fn undrained_shell_output_is_capped() {
    let Some(kc) = kubeconfig() else { return };
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    let core = Core::new(Notifier::noop()).unwrap();
    let session = connected(&core, "demo");
    let (ns, pod) = some_running_pod(&session);
    let containers = session.pod_containers(&ns, &pod);
    wait_for("containers", Duration::from_secs(10), || containers.is_done());
    let container = containers.take().unwrap().unwrap().remove(0).name;
    let before = held_open("exec");

    let flood = session.exec(&ns, &pod, &container, vec!["sh".into(), "-c".into(), "yes skiff-flood".into()]);
    wait_for("flood", Duration::from_secs(15), || flood.state() == ExecState::Running);
    // Nobody is reading. Give it time to produce far more than the cap.
    std::thread::sleep(Duration::from_secs(4));
    let cap = 4 * 1024 * 1024;
    let first = flood.take_output().len();
    assert!(first <= cap + 64 * 1024, "{first} bytes were buffered: the cap does not hold");
    if first < cap {
        eprintln!("note: only {first} bytes arrived in 4 s; the cap was not reached on this machine");
    }
    // Draining lets it continue.
    wait_for("more output after draining", Duration::from_secs(5), || !flood.take_output().is_empty());
    // And a paused session still closes.
    std::thread::sleep(Duration::from_millis(500));
    flood.close();
    wait_for("server-side exec to close", Duration::from_secs(5), || held_open("exec") <= before);
}
