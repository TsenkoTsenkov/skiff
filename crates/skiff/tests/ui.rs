//! End-to-end UI tests: the real app, rendered headlessly, against a live
//! cluster. They need the dev clusters from `hack/cluster-up.sh` (or set
//! SKIFF_TEST_KUBECONFIG / SKIFF_TEST_CONTEXT) and are skipped otherwise.
//!
//! Screenshots land in `target/shots/`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
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
    let toast = h.get_by_label_contains("Apply or cancel").rect();
    assert!((toast.center().x - 720.0).abs() < 12.0, "the notification is centred, not at x={:.0}", toast.center().x);
    assert_eq!(
        h.state().selected_row().map(|(name, _)| name).as_deref(),
        Some(editing.as_str()),
        "the highlight stays on the object in the panel"
    );
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

fn steps(h: &mut Harness<'static, SkiffApp>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

#[test]
fn the_last_column_cannot_be_hidden() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 3);
    h.get_by_label("Choose columns").click();
    settle(&mut h, "column menu", |h| h.query_all_by_role(Role::CheckBox).next().is_some());
    // Untick whatever can be unticked, for as long as something can. (This
    // used to end in a table without columns, which crashed.)
    for _ in 0..40 {
        let Some(next) = h
            .query_all_by_role(Role::CheckBox)
            .find(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True) && !n.accesskit_node().is_disabled())
        else {
            break;
        };
        next.click();
        steps(&mut h, 3);
    }
    let ticked = h
        .query_all_by_role(Role::CheckBox)
        .filter(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
        .count();
    assert_eq!(ticked, 1, "exactly one column stays on");
    shot(&mut h, "23_last_column");
    let (rows, _) = h.state().table_stats();
    let (first, _) = h.state().row_at(0).unwrap();
    assert!(rows > 3 && (has_label(&h, &first) || has_label(&h, "Age")), "the table still shows rows");
}

#[test]
fn opening_details_keeps_the_clicked_row_in_view() {
    let Some(ctx) = test_context() else { return };
    // A short window, so that even a filtered list is longer than a screen.
    let mut h = harness_with(&ctx, "pods", [1440.0, 600.0], false);
    settle(&mut h, "pods", |h| h.state().table_stats().1 && h.state().table_stats().0 > 100);
    let total = h.state().table_stats().0;

    // 1. A shorter (but still scrolling) list with the detail panel open:
    //    the narrow table layout now remembers that list's length.
    let mut found = false;
    for term in ["shop", "monitoring", "payments", "running"] {
        h.key_press(egui::Key::Slash);
        h.step();
        h.get_by_role(Role::TextInput).type_text(term);
        steps(&mut h, 4);
        let shown = h.state().table_stats().0;
        if (25..=110).contains(&shown) {
            found = true;
            break;
        }
        h.key_press(egui::Key::Escape); // clears the filter: try another
        steps(&mut h, 2);
    }
    if !found {
        eprintln!("skipping: no filter gives a medium-sized pod list on this cluster");
        return;
    }
    let filtered = h.state().table_stats().0;
    let (first, _) = h.state().row_at(0).unwrap();
    h.get_all_by_label(&first).next().expect("first row").click();
    settle(&mut h, "detail panel", |h| h.state().detail.is_some());
    assert_eq!(h.state().table_stats().0, filtered, "still the filtered list");
    h.key_press(egui::Key::Escape); // closes the panel
    steps(&mut h, 3);
    assert!(h.state().detail.is_none());
    h.key_press(egui::Key::Escape); // clears the filter
    settle(&mut h, "full list", |h| h.state().table_stats().0 == total);

    // 2. Far down the full list, click a row.
    h.key_press(egui::Key::End);
    steps(&mut h, 4);
    let (name, _) = h.state().row_at(total - 4).expect("a row near the end");
    h.get_all_by_label(&name).next().expect("that row is on screen after End").click();
    settle(&mut h, "detail panel", |h| h.state().detail.as_ref().is_some_and(|d| d.name == name));
    // Let any scrolling play out, then the row must still be on screen
    // (once in the table, apart from the panel's title).
    for _ in 0..90 {
        h.step();
    }
    shot(&mut h, "24_row_stays_in_view");
    let table_right = h.get_by_label("YAML").rect().left();
    let visible = h.query_all_by_label(&name).any(|n| n.rect().right() <= table_right);
    assert!(visible, "the table scrolled away from the row that was clicked ({name})");
}

#[test]
fn row_menu_closes_when_rows_move_under_it() {
    let Some(ctx) = test_context() else { return };
    let ns = "default";
    let extra = "000-skiff-menu-test";
    let _ = kubectl(&["-n", ns, "delete", "cm", extra, "--ignore-not-found"]);
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 5);
    let (name, _) = h.state().row_at(3).unwrap();
    assert!(name.as_str() > extra, "the new object must sort above {name}");
    h.get_all_by_label(&name).next().expect("row").click_secondary();
    settle(&mut h, "row menu", |h| has_label_containing(h, "Copy name"));
    assert_eq!(h.query_all_by_label(&name).count(), 2, "the menu names the object it is for");
    shot(&mut h, "25_row_menu");
    // It stays open while nothing moves.
    steps(&mut h, 10);
    assert!(has_label_containing(&h, "Copy name"));

    // An object appears above it: every row below moves down by one, and
    // the menu's position now belongs to another object.
    kubectl(&["-n", ns, "create", "cm", extra]);
    settle(&mut h, "the new row", |h| has_label(h, extra));
    steps(&mut h, 3);
    let closed = !has_label_containing(&h, "Copy name");
    let _ = kubectl(&["-n", ns, "delete", "cm", extra, "--ignore-not-found"]);
    assert!(closed, "a menu opened for {name} must not stay open over a different row");
}

#[test]
fn create_dialog_always_fits_and_can_be_left() {
    let Some(ctx) = test_context() else { return };
    // The smallest window the app allows.
    let mut h = harness_with(&ctx, "configmaps", [900.0, 560.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1);

    // Untouched, Esc dismisses it.
    h.state_mut().open_create_dialog();
    settle(&mut h, "dialog", |h| has_label(h, "Apply"));
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);
    assert!(h.state().dialog.is_none(), "an untouched dialog closes on Esc");

    // Thirty documents that all fail: thirty lines of error.
    h.state_mut().open_create_dialog();
    settle(&mut h, "dialog", |h| has_label(h, "Apply"));
    let mut manifest = String::new();
    for i in 0..30 {
        manifest.push_str(&format!("---\napiVersion: v1\nkind: NoSuchKind\nmetadata:\n  name: thing-{i}\n"));
    }
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    h.get_by_role(Role::MultilineTextInput).type_text(&manifest);
    steps(&mut h, 8);
    h.get_by_label("Apply").click();
    settle(&mut h, "the error", |h| has_label_containing(h, "unknown resource type"));
    shot(&mut h, "26_create_error_fits");
    for label in ["Apply", "Cancel"] {
        let rect = h.get_all_by_label(label).last().unwrap().rect();
        assert!(
            rect.top() >= 0.0 && rect.bottom() <= 560.0,
            "{label} button at y={:.0}..{:.0} is outside the 560 px window",
            rect.top(),
            rect.bottom()
        );
    }
    // With text typed, Esc does not throw it away; Cancel does.
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);
    assert!(h.state().dialog.is_some(), "typed text is not lost to Esc");
    h.get_all_by_label("Cancel").last().unwrap().click();
    steps(&mut h, 3);
    assert!(h.state().dialog.is_none());

    // A very long line (certificates, last-applied annotations) scrolls
    // inside the editor; it must not widen the dialog past the window.
    h.state_mut().open_create_dialog();
    settle(&mut h, "dialog", |h| has_label(h, "Apply"));
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    let long = format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: long\ndata:\n  cert: {}\n", "A".repeat(3_000));
    h.get_by_role(Role::MultilineTextInput).type_text(&long);
    steps(&mut h, 8);
    shot(&mut h, "31_create_long_line");
    for label in ["Apply", "Cancel"] {
        let rect = h.get_all_by_label(label).last().unwrap().rect();
        assert!(
            rect.left() >= 0.0 && rect.right() <= 900.0 && rect.bottom() <= 560.0,
            "{label} button at x={:.0}..{:.0} is outside the 900 px window",
            rect.left(),
            rect.right()
        );
    }
    h.get_all_by_label("Cancel").last().unwrap().click();
    steps(&mut h, 3);
    assert!(h.state().dialog.is_none());
}

#[test]
fn a_failed_background_apply_returns_on_its_own_cluster() {
    let Some(ctx) = test_context() else { return };
    if ctx != "demo" {
        return;
    }
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1);
    h.state_mut().open_create_dialog();
    settle(&mut h, "dialog", |h| has_label(h, "Apply"));
    assert!(has_label(&h, "on demo"), "the dialog says which cluster it applies to");
    // Enough documents (all refused: the namespace does not exist, which
    // the API server takes about 50 ms to say) to still be running when
    // the dialog is closed, and to fail a few seconds later.
    let mut manifest = String::new();
    for i in 0..60 {
        manifest.push_str(&format!(
            "---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: bg-{i}\n  namespace: skiff-no-such-namespace\n"
        ));
    }
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    h.get_by_role(Role::MultilineTextInput).type_text(&manifest);
    // (The dialog grows over a few frames after a large paste.)
    steps(&mut h, 8);
    h.get_by_label("Apply").click();
    steps(&mut h, 2);
    assert!(has_label(&h, "Close"), "the apply is still running, so the button reads Close");
    h.get_by_label("Close").click();
    h.step();
    assert!(h.state().dialog.is_none());

    // On another cluster the failure must not bring the dialog back: its
    // Apply button would send the manifest to the wrong cluster.
    h.state_mut().switch_context("scale");
    let mut told = false;
    for _ in 0..160 {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
        assert!(h.state().dialog.is_none(), "the dialog must not reopen on another cluster");
        told |= has_label_containing(&h, "Switch back to demo");
    }
    assert!(told, "the failure is announced where the user is");

    // Back on its own cluster it returns, manifest and report included.
    h.state_mut().switch_context("demo");
    settle(&mut h, "the dialog to return", |h| h.state().dialog.is_some());
    assert!(has_label(&h, "on demo"));
    assert!(has_label_containing(&h, "skiff-no-such-namespace"), "the report is shown");
    let text = h.get_by_role(Role::MultilineTextInput).value().unwrap_or_default();
    assert!(text.contains("name: bg-59"), "the manifest is back");
    h.get_all_by_label("Cancel").last().unwrap().click();
    steps(&mut h, 3);
    assert!(h.state().dialog.is_none());
}

#[test]
fn held_enter_does_not_confirm_the_scale_dialog() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "deployments.apps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    // Enter goes down before the dialog opens and stays down. (egui works
    // out by itself which key events are repeats of a held key.)
    h.key_down(egui::Key::Enter);
    h.step();
    h.get_by_label("Scale").click();
    settle(&mut h, "prefilled replica count", |h| {
        h.query_all_by_role(Role::TextInput).last().and_then(|n| n.value()).is_some_and(|v| !v.is_empty())
    });
    for _ in 0..8 {
        h.event(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    assert!(h.state().dialog.is_some(), "auto-repeat must not submit the dialog");
    assert!(!has_label_containing(&h, "Scaled "), "nothing was sent");
    assert!(h.get_all_by_role(Role::TextInput).last().unwrap().is_focused(), "the field is still ready for typing");
    h.key_up(egui::Key::Enter);
    h.step();
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);
    assert!(h.state().dialog.is_none());
}

#[test]
fn yaml_editor_follows_or_flags_changes_on_the_server() {
    let Some(ctx) = test_context() else { return };
    let (ns, name) = ("default", "skiff-edit-test");
    let _ = kubectl(&["-n", ns, "delete", "cm", name, "--ignore-not-found"]);
    kubectl(&["-n", ns, "create", "cm", name, "--from-literal=k=v"]);
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 3);
    h.key_press(egui::Key::Slash);
    h.step();
    h.get_by_role(Role::TextInput).type_text(name);
    settle(&mut h, "filter", |h| h.state().table_stats().0 == 1);
    h.get_all_by_label(name).next().expect("the row").click();
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    assert_eq!(h.state().detail.as_ref().unwrap().name, name);
    h.get_by_label("Edit YAML").click();
    settle(&mut h, "editor", |h| has_label(h, "Cancel"));
    let editor_text = |h: &Harness<'static, SkiffApp>| h.get_by_role(Role::MultilineTextInput).value().unwrap_or_default();

    // Untouched: the editor follows the object.
    kubectl(&["-n", ns, "annotate", "cm", name, "skiff-test/one=1"]);
    settle(&mut h, "editor to follow", |h| editor_text(h).contains("skiff-test/one"));
    assert!(!has_label_containing(&h, "has changed on the server"));

    // With changes typed: it keeps them and says that applying will fail.
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput).type_text("# mine ");
    steps(&mut h, 3);
    kubectl(&["-n", ns, "annotate", "cm", name, "skiff-test/two=2"]);
    settle(&mut h, "the notice", |h| has_label_containing(h, "has changed on the server"));
    assert!(editor_text(&h).contains("# mine "), "typed text is kept");
    assert!(!editor_text(&h).contains("skiff-test/two"));
    shot(&mut h, "27_edit_outdated");
    // The notice appearing above the editor must not take the keyboard
    // away from it: typing simply continues, "/" included (which would
    // otherwise jump to the table's filter).
    assert!(h.get_by_role(Role::MultilineTextInput).is_focused(), "the editor keeps the keyboard");
    h.get_by_role(Role::MultilineTextInput).type_text("more/");
    steps(&mut h, 3);
    assert!(editor_text(&h).contains("# mine more/"), "typing continues in the editor: {:?}", editor_text(&h).lines().next());
    assert_eq!(h.state().table_stats().0, 1, "the table's filter is untouched");
    // Applying now is refused by the server (the object moved on). The
    // editor must still have the keyboard afterwards, text intact.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    settle(&mut h, "the refusal", |h| has_label_containing(h, "(409)"));
    assert!(h.get_by_role(Role::MultilineTextInput).is_focused(), "the editor keeps the keyboard after a refused save");
    h.get_by_role(Role::MultilineTextInput).type_text("again/");
    steps(&mut h, 3);
    assert!(editor_text(&h).contains("# mine more/again/"), "typing continues after the refusal");
    assert_eq!(h.state().table_stats().0, 1, "the table's filter is still untouched");
    h.get_by_label("Load the latest version").click();
    settle(&mut h, "latest version", |h| editor_text(h).contains("skiff-test/two"));
    assert!(!h.state().detail.as_ref().unwrap().has_unsaved_edit());
    assert!(!has_label_containing(&h, "has changed on the server"));

    // With the caret in the editor (but nothing typed) the text is not
    // swapped under it either: the notice is shown instead.
    h.get_by_role(Role::MultilineTextInput).focus();
    steps(&mut h, 2);
    kubectl(&["-n", ns, "annotate", "cm", name, "skiff-test/three=3"]);
    settle(&mut h, "the notice", |h| has_label_containing(h, "has changed on the server"));
    assert!(!editor_text(&h).contains("skiff-test/three"));
    assert!(h.get_by_role(Role::MultilineTextInput).is_focused());
    h.get_by_label("Cancel").click();
    h.step();
    let _ = kubectl(&["-n", ns, "delete", "cm", name, "--ignore-not-found"]);
}

#[cfg(feature = "terminal")]
#[test]
fn shell_runs_commands_in_a_container() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    h.get_by_label("alertmanager-0").click();
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    h.get_by_label("Shell").click();
    settle(&mut h, "shell to connect", |h| has_label(h, "connected"));
    let screen = |h: &Harness<'static, SkiffApp>| h.state().dock.shell_screen().unwrap_or_default();
    let shows = |h: &Harness<'static, SkiffApp>, line: &str| screen(h).lines().any(|l| l.trim() == line);

    // The new shell has the keyboard: type a command and read its output.
    assert!(h.get_by_label("Terminal").is_focused(), "a new shell takes the keyboard");
    h.get_by_label("Terminal").type_text("echo skiff-$((6*7))");
    h.key_press(egui::Key::Enter);
    settle(&mut h, "command output", |h| shows(h, "skiff-42"));
    shot(&mut h, "28_shell");

    // Keys typed into the shell are not also shortcuts of the app: "j"
    // would move the table selection, Esc would close the detail panel,
    // and Ctrl+K is the shell's own.
    let before = h.state().selected_row();
    h.get_by_label("Terminal").type_text("j");
    h.key_press(egui::Key::Escape);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    steps(&mut h, 3);
    assert_eq!(h.state().selected_row(), before);
    assert!(h.state().detail.is_some(), "Esc went to the shell, not to the panel");
    assert!(h.state().palette.is_none(), "Ctrl+K went to the shell");
    if h.ctx.os() != egui::os::OperatingSystem::Mac {
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::K);
        steps(&mut h, 3);
        assert!(h.state().palette.is_some(), "Ctrl+Shift+K opens the palette from a shell");
        h.key_press(egui::Key::Escape);
        steps(&mut h, 4);
        assert!(h.state().palette.is_none());
        // Leaving the palette gives the keyboard back to the shell: what is
        // typed next must not run the table's shortcuts instead.
        assert!(h.get_by_label("Terminal").is_focused(), "the keyboard returns to the shell");
    } else {
        h.get_by_label("Terminal").click();
        steps(&mut h, 2);
    }
    // Clear the half-typed line (Ctrl+U), then print something after a
    // delay, while another dock tab is in front.
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::U);
    h.get_by_label("Terminal").type_text("sleep 1; echo late-$((40+2))");
    h.key_press(egui::Key::Enter);
    steps(&mut h, 2);
    h.get_by_label("Logs").click();
    settle(&mut h, "log lines", |h| has_label_containing(h, "streaming"));
    assert!(h.query_by_label("Terminal").is_none(), "the logs tab is in front");
    settle(&mut h, "output while hidden", |h| shows(h, "late-42"));

    // Choosing the shell's tab is asking for the shell: it has the keyboard
    // without a further click.
    h.get_all_by_label_contains("alertmanager-0").find(|n| n.rect().top() > 400.0).expect("the shell's tab").click();
    settle(&mut h, "shell tab in front", |h| h.query_by_label("Terminal").is_some());
    assert!(h.get_by_label("Terminal").is_focused(), "a shell's tab gives the shell the keyboard");

    // A program that prints a lot (15 MB here) must not stall the app:
    // every frame stays short while it is being taken in, and the end of
    // the output arrives.
    h.get_by_label("Terminal").type_text("seq 1 2000000");
    h.key_press(egui::Key::Enter);
    let mut slowest = Duration::ZERO;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !shows(&h, "2000000") {
        assert!(Instant::now() < deadline, "the output never finished");
        let t = Instant::now();
        h.step();
        slowest = slowest.max(t.elapsed());
    }
    assert!(slowest < Duration::from_millis(250), "a frame took {slowest:?} during heavy output");

    // Leaving the shell ends the session.
    h.get_by_label("Terminal").type_text("exit");
    h.key_press(egui::Key::Enter);
    settle(&mut h, "shell to end", |h| has_label(h, "ended"));
    shot(&mut h, "29_shell_ended");
}

#[test]
fn detail_panel_and_table_do_not_overlap_in_a_small_window() {
    let Some(ctx) = test_context() else { return };
    // The smallest window the app allows; this pod has labels far wider
    // than the detail panel can be here.
    let mut h = harness_with(&ctx, "pods", [900.0, 560.0], false);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    h.get_by_label("alertmanager-0").click();
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    steps(&mut h, 5);
    shot(&mut h, "30_small_window_detail");
    // Everything in the panel's header is to the right of every table row.
    let panel_left = ["Logs", "YAML"].iter().map(|l| h.get_by_label(l).rect().left()).fold(f32::MAX, f32::min);
    let (second, _) = h.state().row_at(1).unwrap();
    let row = h.get_all_by_label(&second).next().expect("second row").rect();
    assert!(row.right() <= panel_left, "table row ends at x={:.0}, panel content starts at x={panel_left:.0}", row.right());
    // And a click on the panel's first button goes to that button.
    h.get_by_label("Logs").click();
    settle(&mut h, "logs to open", |h| has_label_containing(h, "streaming"));
    assert_eq!(h.state().detail.as_ref().unwrap().name, "alertmanager-0", "the click must not land on a table row");
}

#[cfg(feature = "terminal")]
#[test]
fn ctrl_s_in_a_shell_does_not_apply_an_open_edit() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    h.get_by_label("alertmanager-0").click();
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    h.get_by_label("Edit YAML").click();
    settle(&mut h, "editor", |h| has_label(h, "Cancel"));
    h.get_by_role(Role::MultilineTextInput).focus();
    h.step();
    h.get_by_role(Role::MultilineTextInput).type_text("# half-finished ");
    steps(&mut h, 3);
    assert!(h.state().detail.as_ref().unwrap().has_unsaved_edit());

    // Into the pod's shell, where Ctrl+S is just a key for the remote
    // program (it must not save what is open in the panel).
    h.get_by_label("Shell").click();
    settle(&mut h, "shell to connect", |h| has_label(h, "connected"));
    assert!(h.get_by_label("Terminal").is_focused());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(20));
        h.step();
    }
    let detail = h.state().detail.as_ref().unwrap();
    assert!(detail.editing() && detail.has_unsaved_edit(), "the edit must still be open and unsaved");
    assert!(!has_label_containing(&h, "Saved"), "nothing was sent to the API server");
    h.get_by_label("Cancel").click();
    steps(&mut h, 3);
}

#[cfg(feature = "terminal")]
#[test]
fn shell_colours_in_both_themes() {
    let Some(ctx) = test_context() else { return };
    for (light, name) in [(false, "32_shell_colours_dark"), (true, "33_shell_colours_light")] {
        let mut h = harness_with(&ctx, "pods", [1280.0, 760.0], light);
        settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
        h.get_by_label("alertmanager-0").click();
        settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
        h.get_by_label("Shell").click();
        settle(&mut h, "shell to connect", |h| has_label(h, "connected"));
        // The 16 colours as text, as text on black / white / blue, and as
        // backgrounds under default text: the pairs programs really use.
        let script = concat!(
            r#"for i in 0 1 2 3 4 5 6 7; do printf '\033[3%sm text%s \033[9%sm bright%s \033[0m' $i $i $i $i; done; echo; "#,
            r#"for i in 0 1 2 3 4 5 6 7; do printf '\033[4%sm on-bg%s \033[0m\033[30;4%sm blk \033[97;4%sm wht \033[0m' $i $i $i $i; done; echo; "#,
            r#"printf '\033[37;44m white-on-blue \033[0m \033[30;47m black-on-white \033[0m \033[2m dim \033[0m \033[1m bold \033[0m \033[7m inverse \033[0m\n'; echo colours-done"#,
        );
        h.get_by_label("Terminal").type_text(script);
        h.key_press(egui::Key::Enter);
        settle(&mut h, "colour table", |h| {
            h.state().dock.shell_screen().unwrap_or_default().lines().any(|l| l.trim() == "colours-done")
        });
        shot(&mut h, name);
    }
}

#[test]
fn notifications_are_centred_each() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1);
    h.state_mut().toast("Short", skiff_core::Tone::Ok);
    h.state_mut().toast("A considerably longer notification than the first one", skiff_core::Tone::Ok);
    steps(&mut h, 3);
    shot(&mut h, "34_toast_stack");
    for text in ["Short", "A considerably longer notification than the first one"] {
        let x = h.get_by_label(text).rect().center().x;
        // (The text sits right of a small dot, hence the allowance.)
        assert!((x - 720.0).abs() < 12.0, "{text:?} is centred at x={x:.0}, not in the middle of the window");
    }
}
