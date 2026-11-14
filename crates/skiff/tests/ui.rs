//! End-to-end UI tests: the real app, rendered headlessly, against a live
//! cluster. They need the dev clusters from `hack/cluster-up.sh` (or set
//! SKIFF_TEST_KUBECONFIG / SKIFF_TEST_CONTEXT) and are skipped otherwise.
//!
//! Screenshots land in `target/shots/`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn test_context() -> Option<String> {
    let kubeconfig = std::env::var("SKIFF_TEST_KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace().join("hack/kubeconfig"));
    if !kubeconfig.exists() {
        eprintln!("skipping: no kubeconfig at {}", kubeconfig.display());
        return None;
    }
    // SAFETY: tests in this file only ever set the same value.
    unsafe { std::env::set_var("KUBECONFIG", &kubeconfig) };
    Some(std::env::var("SKIFF_TEST_CONTEXT").unwrap_or_else(|_| "demo".to_string()))
}

fn harness(context: &str, view: &str, size: [f32; 2]) -> Harness<'static, SkiffApp> {
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light: false };
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts))
}

/// Runs frames until `done` holds (data arrives from background threads).
fn settle(h: &mut Harness<'static, SkiffApp>, what: &str, mut done: impl FnMut(&Harness<'static, SkiffApp>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        h.step();
        if done(h) {
            // A few more frames so animations and follow-up fetches land.
            for _ in 0..6 {
                std::thread::sleep(Duration::from_millis(30));
                h.step();
            }
            return;
        }
        if Instant::now() >= deadline {
            shot(h, &format!("timeout_{}", what.replace(' ', "_")));
            panic!("timed out waiting for: {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn shot(h: &mut Harness<'static, SkiffApp>, name: &str) {
    let dir = workspace().join("target/shots");
    std::fs::create_dir_all(&dir).unwrap();
    let img = h.render().expect("render");
    img.save(dir.join(format!("{name}.png"))).unwrap();
}

fn has_label(h: &Harness<'static, SkiffApp>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn has_label_containing(h: &Harness<'static, SkiffApp>, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

#[test]
fn pods_table_detail_and_yaml() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    shot(&mut h, "01_pods");

    // Filter narrows the table.
    h.key_press(egui::Key::Slash);
    h.step();
    h.get_by_role(Role::TextInput).type_text("crash");
    settle(&mut h, "filter to apply", |h| has_label_containing(h, "CrashLoopBackOff"));
    assert!(!has_label_containing(&h, "Completed"), "filter should hide non-matching rows");
    shot(&mut h, "02_pods_filtered");

    // Keyboard: leave the filter, select the first row, open details.
    h.key_press(egui::Key::Escape);
    h.step();
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    shot(&mut h, "03_pod_detail");

    h.get_by_label("YAML").click();
    settle(&mut h, "yaml tab", |h| has_label_containing(h, "managedFields hidden"));
    shot(&mut h, "04_pod_yaml");

    h.get_all_by_label_contains("Events").last().expect("events tab").click();
    settle(&mut h, "events tab", |_| true);
    shot(&mut h, "05_pod_events");
}

#[test]
fn palette_navigation_and_overview() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.step();
    h.get_all_by_role(Role::TextInput).last().expect("palette input").type_text("deploy");
    h.step();
    h.step();
    shot(&mut h, "06_palette");
    assert!(has_label(&h, "Deployments"));
    h.key_press(egui::Key::Enter);
    settle(&mut h, "deployments view", |h| has_label(h, "Up-to-date"));
    assert_eq!(h.state().view_id(), "deployments.apps");
    shot(&mut h, "07_deployments");

    h.get_by_label("Nodes").click();
    settle(&mut h, "nodes view", |h| has_label_containing(h, "demo-worker-1"));
    shot(&mut h, "08_nodes");

    h.get_by_label("Events").click();
    settle(&mut h, "events view", |h| has_label(h, "Reason"));
    shot(&mut h, "09_events");

    h.get_by_label("Overview").click();
    settle(&mut h, "overview", |h| has_label_containing(h, "NEEDS ATTENTION"));
    shot(&mut h, "10_overview");
}

#[test]
fn light_theme_and_custom_resources() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "customresourcedefinitions.apiextensions.k8s.io", [1280.0, 800.0]);
    settle(&mut h, "crds", |h| has_label_containing(h, "databases.stack.skiff.dev"));
    h.get_by_label("Toggle light / dark theme").click();
    for _ in 0..4 {
        h.step();
    }
    shot(&mut h, "11_crds_light");
    assert!(!h.state().pal.dark);
}
