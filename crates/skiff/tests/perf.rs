//! Performance measurements against the large dev cluster. Not a pass/fail
//! test: run with
//!   cargo test --release -p skiff --test perf -- --ignored --nocapture

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp};

fn harness(context: &str, view: &str) -> Harness<'static, SkiffApp> {
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light: false, namespace: None, ..Default::default() };
    Harness::builder()
        .with_size([1440.0, 900.0])
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts))
}

fn frames(h: &mut Harness<'static, SkiffApp>, n: usize) -> (Duration, Duration) {
    let mut ui = Duration::ZERO;
    let started = Instant::now();
    for _ in 0..n {
        h.step();
        ui += h.state().last_ui;
    }
    (started.elapsed() / n as u32, ui / n as u32)
}

#[test]
#[ignore = "performance measurement; needs the scale cluster"]
fn large_cluster() {
    let kubeconfig = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hack/kubeconfig");
    // SAFETY: single-threaded test binary section.
    unsafe { std::env::set_var("KUBECONFIG", &kubeconfig) };
    let context = std::env::var("SKIFF_PERF_CONTEXT").unwrap_or_else(|_| "scale".to_string());

    let t0 = Instant::now();
    let mut h = harness(&context, "pods");
    let created = t0.elapsed();
    let mut first_rows = None;
    let loaded = loop {
        h.step();
        let (shown, live) = h.state().table_stats();
        if first_rows.is_none() && shown > 0 {
            first_rows = Some(t0.elapsed());
        }
        if live && shown > 0 {
            break t0.elapsed();
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "pods never loaded");
        std::thread::sleep(Duration::from_millis(2));
    };
    let (rows, _) = h.state().table_stats();
    println!("app constructed in {created:?}; first rows on screen after {first_rows:?}; all {rows} pods live after {loaded:?}");

    let (step, ui) = frames(&mut h, 300);
    println!("idle frame, {rows} rows: {step:?} per frame total, {ui:?} in app code");

    // Typing into the filter re-filters and re-sorts on every keystroke.
    h.key_press(egui::Key::Slash);
    h.step();
    let mut worst = Duration::ZERO;
    // (The scale cluster's pods are named app-N-...: a filter that narrows
    // the table without emptying it, which is what is worth timing.)
    for ch in ["a", "p", "p", "-", "1"] {
        h.get_by_role(Role::TextInput).type_text(ch);
        let t = Instant::now();
        h.step();
        worst = worst.max(t.elapsed());
    }
    // (The table is filtered at the start of a frame: the last character
    // takes effect in the next one.)
    let t = Instant::now();
    h.step();
    worst = worst.max(t.elapsed());
    let (shown, _) = h.state().table_stats();
    assert!(shown > 0 && shown < rows, "the filter 'app-1' must narrow the table: {shown} of {rows} rows match");
    println!("filter 'app-1': {shown} rows match; slowest keystroke frame {worst:?}; last recompute {:?}", h.state().last_table_cost());

    h.key_press(egui::Key::Escape);
    h.step();
    h.key_press(egui::Key::Escape);
    h.step();
    for header in ["Status", "Namespace", "Age", "CPU"] {
        if h.query_all_by_label(header).next().is_none() {
            continue;
        }
        h.get_by_label(header).click();
        let t = Instant::now();
        h.step();
        h.step();
        println!("sort by {header}: {:?} for the click frames; recompute {:?}", t.elapsed(), h.state().last_table_cost());
    }

    // Opening details costs one GET.
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    let t = Instant::now();
    loop {
        h.step();
        if h.query_all_by_label("CONTAINERS").next().is_some() {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "detail never loaded");
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("detail panel populated {:?} after Enter", t.elapsed());
    let (step, ui) = frames(&mut h, 200);
    println!("frame with detail open: {step:?} total, {ui:?} in app code");

    for view in ["Services", "ConfigMaps", "Secrets", "Events"] {
        h.get_by_label(view).click();
        let t = Instant::now();
        loop {
            h.step();
            let (shown, live) = h.state().table_stats();
            // Events expire after an hour, so an idle cluster may have none.
            if live && h.state().view_id().starts_with(&view.to_lowercase()) {
                println!("{view}: {shown} rows live {:?} after click", t.elapsed());
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(60), "{view} never loaded");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|l| l.starts_with("VmRSS") || l.starts_with("VmHWM")) {
            println!("{line}  (includes the software GPU renderer used for tests)");
        }
    }
}
