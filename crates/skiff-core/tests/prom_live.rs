//! Prometheus through the API server's service proxy, against the `demo`
//! cluster from `hack/cluster-up.sh` (whose Prometheus is
//! `hack/fake-prometheus.py`). Skipped when the cluster is not running.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use skiff_core::prom::{ChartTarget, Unit};
use skiff_core::{
    ConnStatus, Core, KubeconfigSources, Notifier, PromEndpoint, PromError, PromStatus, Session, TimeRange,
};

fn core() -> Option<Core> {
    let path = std::env::var("SKIFF_TEST_KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hack/kubeconfig"));
    if !path.exists() {
        eprintln!("skipping: no kubeconfig at {}", path.display());
        return None;
    }
    let core = Core::new(Notifier::noop()).expect("runtime");
    core.set_kubeconfig_sources(KubeconfigSources::explicit([path.to_string_lossy().into_owned()]));
    Some(core)
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn connected(core: &Core, context: &str) -> Arc<Session> {
    let session = core.connect(context);
    wait_for("connection", || {
        matches!(&*session.status(), ConnStatus::Connected { .. })
    });
    session
}

fn outcome(session: &Session) -> Arc<PromStatus> {
    wait_for("Prometheus to be looked for", || {
        !matches!(&*session.prometheus(&[]), PromStatus::Detecting)
    });
    session.prometheus(&[])
}

/// The demo cluster's Prometheus, or `None` (test skipped) when the test
/// bed was brought up without one.
fn demo_prometheus(session: &Session) -> Option<skiff_core::Prom> {
    match &*outcome(session) {
        PromStatus::Ready(prom) => Some(prom.clone()),
        PromStatus::Unavailable(e) => {
            eprintln!("skipping: the demo cluster has no usable Prometheus ({e}); run hack/cluster-up.sh");
            None
        }
        PromStatus::Detecting => unreachable!(),
    }
}

#[test]
fn prometheus_is_detected_and_charts_and_alerts_load_through_the_proxy() {
    let Some(core) = core() else { return };
    let session = connected(&core, "demo");
    assert!(
        matches!(&*session.prometheus_known(), PromStatus::Detecting),
        "nothing looked for before it is asked"
    );
    let Some(prom) = demo_prometheus(&session) else { return };
    // The operator-labelled Service wins over the plain `prometheus` one
    // (which points at a kwok pod that cannot answer).
    assert_eq!(prom.display(), "monitoring/prometheus-operated:9090");

    // A pod's charts: one series per container, request and limit lines.
    let pods = std::process::Command::new("kubectl")
        .args([
            "--context",
            "demo",
            "-n",
            "payments",
            "get",
            "pods",
            "-l",
            "app=gateway",
            "-o",
            "name",
        ])
        .env(
            "KUBECONFIG",
            core.kubeconfig_sources().entries().unwrap_or_default().join(":"),
        )
        .output()
        .expect("kubectl is needed for this test");
    let pods: Vec<String> = String::from_utf8_lossy(&pods.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("pod/").map(str::to_string))
        .collect();
    assert!(
        pods.len() >= 2,
        "the demo seed has several payments/gateway pods: {pods:?}"
    );

    let range = TimeRange::last(3600, skiff_core::now_unix());
    let job = session.charts(
        &prom,
        ChartTarget::Pod {
            namespace: "payments".into(),
            name: pods[0].clone(),
        },
        range,
    );
    wait_for("pod charts", || job.is_done());
    let charts = job.take().expect("done").expect("charts");
    let titles: Vec<&str> = charts.iter().map(|c| c.title).collect();
    assert_eq!(titles, ["CPU", "Memory", "Network", "Restarts"]);
    let cpu = charts[0].data.as_ref().expect("cpu");
    assert_eq!(charts[0].unit, Unit::Cores);
    let names: Vec<&str> = cpu.series.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        ["gateway", "request", "limit"],
        "container, then the reference lines"
    );
    assert!(cpu.series[0].points.len() > 100 && !cpu.series[0].reference);
    assert!(
        cpu.series[1].reference && cpu.series[1].points.iter().all(|p| p.1 == 0.25),
        "the pod requests 250m"
    );
    assert!(
        cpu.series[2].points.iter().all(|p| p.1 == 1.0),
        "and is limited to one core"
    );
    let network = charts[2].data.as_ref().expect("network");
    assert_eq!(
        network.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["received", "sent"]
    );

    // A workload: one series per pod.
    let job = session.charts(
        &prom,
        ChartTarget::Workload {
            namespace: "payments".into(),
            pods: pods.clone(),
        },
        range,
    );
    wait_for("workload charts", || job.is_done());
    let charts = job.take().expect("done").expect("charts");
    let memory = charts[1].data.as_ref().expect("memory");
    let usage: Vec<&str> = memory
        .series
        .iter()
        .filter(|s| !s.reference)
        .map(|s| s.name.as_str())
        .collect();
    let mut expected: Vec<&str> = pods.iter().map(String::as_str).collect();
    expected.sort();
    let mut got = usage.clone();
    got.sort();
    assert_eq!(got, expected);

    // A node.
    let job = session.charts(
        &prom,
        ChartTarget::Node {
            name: "demo-worker-1".into(),
        },
        range,
    );
    wait_for("node charts", || job.is_done());
    let charts = job.take().expect("done").expect("charts");
    let cpu = charts[0].data.as_ref().expect("node cpu");
    assert_eq!(
        cpu.series.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["used", "allocatable"]
    );
    assert!(
        cpu.series[1].points.iter().all(|p| p.1 == 16.0),
        "demo-worker-1 has 16 cores"
    );

    // Alerts arrive once asked for; pending ones and the Watchdog are left out.
    wait_for("alerts", || session.alerts().loaded);
    let alerts = session.alerts();
    assert_eq!(alerts.error, None);
    let names: Vec<&str> = alerts.items.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names.first(), Some(&"KubeNodeNotReady"), "most severe first: {names:?}");
    assert!(
        !names.contains(&"Watchdog") && !names.contains(&"KubeMemoryOvercommit"),
        "{names:?}"
    );
    let (critical, warning) = alerts.counts();
    assert_eq!(critical, 1);
    assert!(warning >= 3, "{names:?}");

    // A reconnect forgets the answer; it is found again when asked.
    session.reconnect();
    assert!(matches!(&*session.prometheus_known(), PromStatus::Detecting));
    assert!(!session.alerts().loaded);
    wait_for("reconnection", || {
        matches!(&*session.status(), ConnStatus::Connected { .. })
    });
    assert!(matches!(&*outcome(&session), PromStatus::Ready(_)));
}

#[test]
fn a_configured_endpoint_is_used_and_its_failures_are_told_apart() {
    let Some(core) = core() else { return };
    let session = connected(&core, "demo");
    if demo_prometheus(&session).is_none() {
        return;
    }

    // An auth proxy in front of Prometheus says no.
    let mut denied = PromEndpoint::new("monitoring", "prometheus-operated", 9090);
    denied.path_prefix = "/deny".into();
    session.set_prometheus_endpoint(Some(denied.into()));
    match &*outcome(&session) {
        PromStatus::Unavailable(PromError::Forbidden(msg)) => assert!(msg.contains("Forbidden"), "{msg}"),
        other => panic!(
            "expected Forbidden, got {:?}",
            other.ready().map(|p| p.display())
        ),
    }

    // A Service that does not exist.
    session.set_prometheus_endpoint(Some(PromEndpoint::new("monitoring", "no-such-prometheus", 9090).into()));
    match &*outcome(&session) {
        PromStatus::Unavailable(PromError::Unreachable(msg)) => {
            assert!(msg.starts_with("monitoring/no-such-prometheus:9090 ("), "{msg}")
        }
        other => panic!(
            "expected Unreachable, got {:?}",
            other.ready().map(|p| p.display())
        ),
    }

    // Something that answers, but not as Prometheus.
    let mut broken = PromEndpoint::new("monitoring", "prometheus-operated", 9090);
    broken.path_prefix = "/broken".into();
    session.set_prometheus_endpoint(Some(broken.into()));
    assert!(matches!(
        &*outcome(&session),
        PromStatus::Unavailable(PromError::Other(_))
    ));

    // Back to detection.
    session.set_prometheus_endpoint(None);
    assert!(matches!(&*outcome(&session), PromStatus::Ready(_)));
}

#[test]
fn a_cluster_without_prometheus_says_so_and_keeps_usage_history() {
    let Some(core) = core() else { return };
    let session = core.connect("scale");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(&*session.status(), ConnStatus::Connected { .. }) {
        if Instant::now() > deadline || matches!(&*session.status(), ConnStatus::Failed(_)) {
            eprintln!("skipping: the scale cluster is not running");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(matches!(
        &*outcome(&session),
        PromStatus::Unavailable(PromError::NotFound)
    ));
    assert!(!session.alerts().loaded, "no Prometheus, no alerts");

    // Usage history starts when it is first asked for.
    wait_for("metrics-server", || session.metrics().available);
    let node = session
        .metrics()
        .nodes
        .keys()
        .next()
        .expect("a node with metrics")
        .to_string();
    let first = session.node_usage_history(&node);
    assert_eq!(first.cpu.len(), 1, "seeded with the current sample");
    assert_eq!(first.cpu.len(), first.memory.len());
}
