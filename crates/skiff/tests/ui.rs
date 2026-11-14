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

/// Points `$KUBECONFIG` at the dev clusters. Every test sets the same value
/// (the environment is shared by all test threads), merging in the
/// restricted-account kubeconfig when one is provided.
fn use_test_kubeconfig() -> bool {
    static VALUE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let value = VALUE.get_or_init(|| {
        let main = std::env::var("SKIFF_TEST_KUBECONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| workspace().join("hack/kubeconfig"));
        if !main.exists() {
            eprintln!("skipping: no kubeconfig at {}", main.display());
            return None;
        }
        let mut value = main.display().to_string();
        if let Ok(extra) = std::env::var("SKIFF_TEST_RESTRICTED_KUBECONFIG") {
            value.push(if cfg!(windows) { ';' } else { ':' });
            value.push_str(&extra);
        }
        Some(value)
    });
    match value {
        Some(v) => {
            // SAFETY: always the same value, see above.
            unsafe { std::env::set_var("KUBECONFIG", v) };
            true
        }
        None => false,
    }
}

fn test_context() -> Option<String> {
    use_test_kubeconfig().then(|| std::env::var("SKIFF_TEST_CONTEXT").unwrap_or_else(|_| "demo".to_string()))
}

fn harness(context: &str, view: &str, size: [f32; 2]) -> Harness<'static, SkiffApp> {
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light: false, namespace: None };
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

#[test]
fn logs_dock_streams_and_filters() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    h.get_by_label("alertmanager-0").click();
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    h.get_by_label("Logs").click();
    settle(&mut h, "log lines", |h| has_label_containing(h, " lines") && has_label_containing(h, "streaming"));
    shot(&mut h, "12_logs");

    h.get_by_label("Show timestamps").click();
    h.get_by_label("Wrap long lines").click();
    settle(&mut h, "toggles", |_| true);
    shot(&mut h, "13_logs_wrapped_timestamps");
}

fn harness_with(context: &str, view: &str, size: [f32; 2], light: bool) -> Harness<'static, SkiffApp> {
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light, namespace: None };
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts))
}

#[test]
fn light_theme_applies_from_the_first_frame() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "pods", [1280.0, 800.0], true);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    shot(&mut h, "14_light_startup_detail");
    let style = h.ctx.global_style();
    let pal = skiff::theme::Palette::light();
    assert!(!style.visuals.dark_mode, "egui must be in light mode");
    assert_eq!(style.visuals.panel_fill, pal.panel);
    assert_eq!(style.visuals.widgets.inactive.bg_fill, pal.raised, "widgets must use the light palette");
    assert_eq!(style.visuals.selection.bg_fill, pal.selected);
}

#[test]
fn columns_stay_on_screen_when_the_detail_panel_opens() {
    let Some(ctx) = test_context() else { return };
    // Same number of columns before and after: the case where stored column
    // widths used to push everything but Name off-screen.
    for (view, column) in [("configmaps", "Data"), ("secrets", "Type"), ("namespaces", "Status")] {
        let mut h = harness_with(&ctx, view, [1440.0, 900.0], false);
        settle(&mut h, "table", |h| has_label(h, column) && h.state().table_stats().1);
        h.key_press(egui::Key::ArrowDown);
        h.step();
        h.key_press(egui::Key::Enter);
        settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
        shot(&mut h, &format!("15_detail_columns_{view}"));
        let detail_left = h.get_by_label("YAML").rect().left();
        for title in [column, "Age"] {
            // The header is the leftmost node with this label (the detail
            // panel may use the same word).
            let rect = h
                .query_all_by_label(title)
                .map(|n| n.rect())
                .min_by(|a, b| a.left().total_cmp(&b.left()))
                .unwrap_or_else(|| panic!("{view}: no column header {title:?}"));
            assert!(
                rect.right() <= detail_left,
                "{view}: column {title:?} at x={:.0}..{:.0} is hidden behind the detail panel (starts at {detail_left:.0})",
                rect.left(),
                rect.right()
            );
        }
    }
}

#[test]
fn end_and_home_work_without_a_selection() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 3);
    h.key_press(egui::Key::End);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| h.state().detail.is_some());
    let last = h.state().detail.as_ref().unwrap().name.clone();
    h.key_press(egui::Key::Home);
    settle(&mut h, "detail follows selection", |h| h.state().detail.as_ref().is_some_and(|d| d.name != last));
    let first = h.state().detail.as_ref().unwrap().name.clone();
    assert_ne!(first, last, "End must select the last row, not the first");
    assert!(first < last, "expected {first:?} (Home) to sort before {last:?} (End)");
}

#[test]
fn names_render_literally() {
    // No cluster needed: the fonts must not rewrite text ("4x4" -> "4×4",
    // "->" -> an arrow, "--" joined). Checked by eye in the screenshot; the
    // assertion guards the feature tables in the bundled font files.
    let mut h = Harness::builder()
        .with_size([520.0, 120.0])
        .with_pixels_per_point(2.0)
        .with_context_setup(|ctx| {
            skiff::theme::install_fonts(ctx);
            skiff::theme::apply(ctx, &skiff::theme::Palette::dark());
        })
        .wgpu()
        .build_ui(|ui| {
            ui.label("cart-5985c6bd96-gr4x4   2x2   a->b   x--y   0x4F");
            ui.monospace("-rw-r--r--  a->b  !=  ==  >=  4x4  www");
        });
    h.run();
    let dir = workspace().join("target/shots");
    std::fs::create_dir_all(&dir).unwrap();
    h.render().unwrap().save(dir.join("16_fonts_literal.png")).unwrap();
    for font in ["Inter-Regular.otf", "Inter-SemiBold.otf", "JetBrainsMono-Regular.ttf"] {
        let bytes = std::fs::read(workspace().join("crates/skiff/assets/fonts").join(font)).unwrap();
        let features = gsub_features(&bytes);
        assert!(!features.is_empty(), "{font}: could not read the GSUB feature list");
        for tag in ["calt", "dlig", "liga", "clig", "rlig"] {
            assert!(
                !features.iter().any(|f| f == tag),
                "{font} still has the {tag} feature; run hack/strip-font-features.py"
            );
        }
    }
}

/// Feature tags in an OpenType font's GSUB table.
fn gsub_features(font: &[u8]) -> Vec<String> {
    let u16_at = |o: usize| u16::from_be_bytes([font[o], font[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_be_bytes([font[o], font[o + 1], font[o + 2], font[o + 3]]) as usize;
    let tables = u16_at(4);
    let Some(gsub) = (0..tables).map(|i| 12 + i * 16).find(|o| &font[*o..*o + 4] == b"GSUB").map(|o| u32_at(o + 8)) else {
        return Vec::new();
    };
    let list = gsub + u16_at(gsub + 6);
    (0..u16_at(list))
        .map(|i| String::from_utf8_lossy(&font[list + 2 + i * 6..list + 6 + i * 6]).into_owned())
        .collect()
}

/// An account that may only read one namespace. Needs a kubeconfig for such
/// an account in `SKIFF_TEST_RESTRICTED_KUBECONFIG` (context `restricted`,
/// allowed namespace in `SKIFF_TEST_RESTRICTED_NAMESPACE`, default `shop`).
#[test]
fn restricted_account_can_work_in_its_namespace() {
    if std::env::var("SKIFF_TEST_RESTRICTED_KUBECONFIG").is_err() {
        eprintln!("skipping: SKIFF_TEST_RESTRICTED_KUBECONFIG not set (see hack/restricted-kubeconfig.sh)");
        return;
    }
    if !use_test_kubeconfig() {
        return;
    }
    let ns = std::env::var("SKIFF_TEST_RESTRICTED_NAMESPACE").unwrap_or_else(|_| "shop".to_string());

    // Without a namespace the cluster-wide list is refused, and the app
    // says what to do about it.
    let opts = AppOptions { context: Some("restricted".into()), view: Some("pods".into()), light: false, namespace: None };
    let mut h = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts));
    settle(&mut h, "forbidden message", |h| has_label(h, "Couldn't load this view"));
    assert!(has_label_containing(&h, "select that one namespace"), "should explain the namespace fallback");
    shot(&mut h, "17_restricted_all_namespaces");

    // With the namespace selected, the table loads from a namespaced watch.
    let opts = AppOptions { context: Some("restricted".into()), view: Some("pods".into()), light: false, namespace: Some(ns.clone()) };
    let mut h = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts));
    settle(&mut h, "pods in the allowed namespace", |h| {
        let (rows, live) = h.state().table_stats();
        live && rows > 0
    });
    assert!(!has_label(&h, "Couldn't load this view"));
    shot(&mut h, "18_restricted_one_namespace");

    // Opening the namespace picker offers a way to name a namespace even
    // though namespaces cannot be listed.
    // The picker button is the topmost node showing the namespace name.
    let top = h
        .query_all_by_label_contains(&ns)
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("namespace picker button");
    top.click();
    settle(&mut h, "picker", |h| has_label_containing(h, "can't be listed"));
    shot(&mut h, "19_restricted_picker");
}

fn kubectl(args: &[&str]) -> String {
    let out = std::process::Command::new("kubectl")
        .args(["--context", "demo"])
        .args(args)
        .output()
        .expect("kubectl is needed for this test");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn unsaved_yaml_edits_block_navigation() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 3);
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    let editing = h.state().detail.as_ref().unwrap().name.clone();
    h.get_by_label("Edit YAML").click();
    settle(&mut h, "editor", |h| has_label(h, "Cancel"));
    assert!(!h.state().detail.as_ref().unwrap().has_unsaved_edit(), "an untouched edit is not unsaved");
    // Focus through the accessibility tree: the editor is taller than the
    // window, so a click at its centre would land off-screen.
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput).type_text("# unsaved ");
    for _ in 0..3 {
        h.step();
    }
    assert!(h.state().detail.as_ref().unwrap().has_unsaved_edit(), "typing should modify the YAML");

    // Clicking another row must not throw the edit away.
    let (other, _) = h.state().row_at(2).expect("a third row");
    assert_ne!(other, editing);
    h.get_all_by_label(&other).next().expect("row label").click();
    for _ in 0..4 {
        h.step();
    }
    let detail = h.state().detail.as_ref().expect("detail stays open");
    assert_eq!(detail.name, editing, "the edited object must stay open");
    assert!(detail.has_unsaved_edit(), "the edit must survive");
    assert!(has_label_containing(&h, "Apply or cancel"), "the user is told why");
    shot(&mut h, "20_unsaved_edit_guard");

    // Esc must not discard typed text either; Cancel does.
    h.key_press(egui::Key::Escape);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.state().detail.as_ref().is_some_and(|d| d.has_unsaved_edit()), "Esc must not drop changes");
    h.get_by_label("Cancel").click();
    for _ in 0..3 {
        h.step();
    }
    assert!(!h.state().detail.as_ref().unwrap().editing());
}

#[test]
fn scale_dialog_starts_from_the_real_replica_count() {
    let Some(ctx) = test_context() else { return };
    // ReplicaSets show READY as a bare number, which used to make the dialog
    // start at 1 whatever the real size was.
    let mut h = harness_with(&ctx, "replicasets.apps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    h.get_by_label("Desired").click();
    h.step();
    h.get_by_label("Desired").click(); // descending: a set with several replicas first
    for _ in 0..3 {
        h.step();
    }
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    let (name, ns) = {
        let d = h.state().detail.as_ref().unwrap();
        (d.name.clone(), d.ns.clone())
    };
    let expected = kubectl(&["-n", &ns, "get", "rs", &name, "-o", "jsonpath={.spec.replicas}"]);
    h.get_by_label("Scale").click();
    settle(&mut h, "prefilled replica count", |h| {
        h.query_all_by_role(Role::TextInput).last().and_then(|n| n.value()).is_some_and(|v| !v.is_empty())
    });
    let shown = h.get_all_by_role(Role::TextInput).last().unwrap().value().unwrap();
    shot(&mut h, "21_scale_dialog");
    assert_eq!(shown, expected, "scale dialog for {ns}/{name}");
    h.get_all_by_label("Cancel").last().unwrap().click();
    h.step();
    assert!(h.state().dialog.is_none());
}

#[test]
fn delete_dialog_waits_for_an_explicit_confirmation() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 3);
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    let (name, ns) = {
        let d = h.state().detail.as_ref().unwrap();
        (d.name.clone(), d.ns.clone())
    };
    h.get_by_label("Delete").click();
    settle(&mut h, "dialog", |h| h.state().dialog.is_some());
    // Nothing happens by itself, however many frames pass.
    for _ in 0..10 {
        h.step();
    }
    assert!(h.state().dialog.is_some(), "the dialog must stay open until answered");
    shot(&mut h, "22_delete_dialog");
    // Enter goes to the default button, which is Cancel: a key held too
    // long on the Delete action can never confirm the deletion.
    h.key_press(egui::Key::Enter);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.state().dialog.is_none(), "Enter activates the default (Cancel) button");
    assert_eq!(kubectl(&["-n", &ns, "get", "cm", &name, "-o", "jsonpath={.metadata.name}"]), name);

    // Esc cancels as well.
    h.get_by_label("Delete").click();
    settle(&mut h, "dialog", |h| h.state().dialog.is_some());
    h.key_press(egui::Key::Escape);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.state().dialog.is_none());
    assert_eq!(kubectl(&["-n", &ns, "get", "cm", &name, "-o", "jsonpath={.metadata.name}"]), name);
}
