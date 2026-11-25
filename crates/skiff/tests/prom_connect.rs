//! The "Prometheus for this cluster" dialog, driven as a user would: from
//! Settings → Clusters, against a small stand-in Prometheus on this
//! computer. Needs no cluster and no GPU (nothing is rendered).

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp};

type H = Harness<'static, SkiffApp>;

const CONTEXT: &str = "aks-prod";

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("skiff-prom-connect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A kubeconfig with one cluster that is not running.
fn fake_kubeconfig(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("kubeconfig");
    std::fs::write(
        &path,
        format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: {CONTEXT}\n  cluster:\n    server: https://127.0.0.1:1\n\
             users:\n- name: u\n  user:\n    token: x\ncontexts:\n- name: {CONTEXT}\n  context:\n    cluster: {CONTEXT}\n    user: u\n\
             current-context: {CONTEXT}\n"
        ),
    )
    .unwrap();
    path
}

/// Answers every query with one series whose value is 3, and keeps the
/// request lines. Returns its address.
fn fake_prometheus(seen: Arc<Mutex<Vec<String>>>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let request = String::from_utf8_lossy(&buf).to_string();
            seen.lock().unwrap().push(request.lines().next().unwrap_or_default().to_string());
            let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{},"value":[1,"3"]}]}}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes());
        }
    });
    format!("http://127.0.0.1:{}", addr.port())
}

fn steps(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn settle(h: &mut H, what: &str, mut done: impl FnMut(&H) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done(h) {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn has_c(h: &H, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

/// The button whose label (after its icon) is `text`.
fn button<'a>(h: &'a H, text: &str) -> egui_kittest::Node<'a> {
    let mut found = h
        .get_all_by_role(egui::accesskit::Role::Button)
        .filter(|n| n.accesskit_node().label().is_some_and(|l| l.trim_start_matches(|c: char| !c.is_ascii()).trim() == text));
    let first = found.next().unwrap_or_else(|| panic!("no button {text:?}"));
    assert!(found.next().is_none(), "two buttons {text:?}");
    first
}

/// Types into the text field that shows `placeholder` while empty.
fn type_into(h: &mut H, placeholder: &'static str, text: &str) {
    let field = h.get_by(move |n| n.placeholder() == Some(placeholder));
    field.focus();
    field.type_text(text);
    steps(h, 2);
}

#[test]
fn a_prometheus_outside_the_cluster_is_set_up_tested_and_saved_from_settings() {
    let dir = temp_dir();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("KUBECONFIG", fake_kubeconfig(&dir)) };
    let config_path = dir.join("config.json");
    std::fs::write(&config_path, "{}").unwrap();
    let (config, warnings) = skiff::config::Config::load_from(&config_path);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = fake_prometheus(seen.clone());

    let opts = AppOptions { context: Some(CONTEXT.to_string()), ..Default::default() };
    let mut h = Harness::builder()
        .with_size([1400.0, 900.0])
        .with_pixels_per_point(1.0)
        .build_eframe(move |cc| SkiffApp::headless_with_config(&cc.egui_ctx, opts, config, warnings));
    steps(&mut h, 3);

    // Settings → Clusters says where the metrics come from, with a way to change it.
    h.state_mut().open_settings(skiff::settings::Section::Clusters);
    steps(&mut h, 4);
    assert!(has_c(&h, "Prometheus: found in the cluster"));
    h.get_by_label(&format!("Prometheus for {CONTEXT}")).click();
    steps(&mut h, 3);
    assert!(h.state().prom_connect.is_some(), "the dialog is open");
    assert!(has_c(&h, "Prometheus for this cluster"));

    // Choose "Another address" and fill it in.
    h.get_by_value("Find it in the cluster (automatic)").click();
    steps(&mut h, 3);
    h.get_by_label("Another address (Mimir, Thanos, VictoriaMetrics, …)").click();
    steps(&mut h, 3);
    type_into(&mut h, "https://prometheus.example.com", &url);
    type_into(&mut h, "empty: all of its metrics", CONTEXT);

    // Test connection asks it the way the charts will, with the label.
    button(&h, "Test connection").click();
    settle(&mut h, "the test", |h| has_c(h, "Connected to 127.0.0.1"));
    assert!(has_c(&h, "3 container series"));
    let requests = seen.lock().unwrap().clone();
    assert!(
        requests.iter().any(|r| r.contains("container_memory_working_set_bytes") && r.contains("cluster%3D%22aks-prod%22")),
        "{requests:?}"
    );

    // Save writes config.json and closes the dialog.
    button(&h, "Save").click();
    steps(&mut h, 3);
    assert!(h.state().prom_connect.is_none(), "saved and closed");
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(saved["clusters"][CONTEXT]["prometheus"]["url"], serde_json::json!(url));
    assert_eq!(saved["clusters"][CONTEXT]["prometheus"]["labels"], serde_json::json!({ "cluster": CONTEXT }));
    assert!(saved["clusters"][CONTEXT]["prometheus"].get("auth").is_none());
    assert!(has_c(&h, &format!("Prometheus: {}", url.trim_start_matches("http://"))));

    // Opened again, it shows what was saved; going back to automatic
    // removes the setting.
    h.get_by_label(&format!("Prometheus for {CONTEXT}")).click();
    steps(&mut h, 3);
    h.get_by_value("Another address (Mimir, Thanos, VictoriaMetrics, …)").click();
    steps(&mut h, 3);
    h.get_by_label("Find it in the cluster (automatic)").click();
    steps(&mut h, 3);
    button(&h, "Save").click();
    steps(&mut h, 3);
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(saved["clusters"][CONTEXT].get("prometheus").is_none(), "{saved}");
    assert!(has_c(&h, "Prometheus: found in the cluster"));
    let _ = std::fs::remove_dir_all(&dir);
}
