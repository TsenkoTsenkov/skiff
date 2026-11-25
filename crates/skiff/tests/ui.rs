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
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light: false, namespace: None, ..Default::default() };
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
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light, namespace: None, ..Default::default() };
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
    let opts = AppOptions { context: Some("restricted".into()), view: Some("pods".into()), light: false, namespace: None, ..Default::default() };
    let mut h = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts));
    settle(&mut h, "forbidden message", |h| has_label(h, "Couldn't load this view"));
    assert!(has_label_containing(&h, "select that one namespace"), "should explain the namespace fallback");
    shot(&mut h, "17_restricted_all_namespaces");

    // With the namespace selected, the table loads from a namespaced watch.
    let opts = AppOptions { context: Some("restricted".into()), view: Some("pods".into()), light: false, namespace: Some(ns.clone()), ..Default::default() };
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

    // The overview covers that namespace, and says that this is all the
    // account can see, instead of zeros and errors.
    let opts = AppOptions { context: Some("restricted".into()), namespace: Some(ns.clone()), ..Default::default() };
    let mut h = harness_opts(opts, [1280.0, 800.0]);
    settle(&mut h, "overview of the namespace", |h| has_label(h, "limited access") && has_label_containing(h, "NEEDS ATTENTION"));
    let pods = kubectl(&["-n", &ns, "get", "pods", "-o", "name"]).lines().count();
    assert!(has_label(&h, &pods.to_string()), "the pods card counts the namespace's {pods} pods");
    assert!(has_label(&h, "no access"), "what cannot be read (nodes) says so rather than showing 0");
    assert!(!has_label_containing(&h, "Couldn't list"), "no access is expected here, not a failure to warn about");
    shot(&mut h, "46_restricted_overview");

    // Without a namespace there is nothing to count: the overview says
    // what to do, and offers the namespace the kubeconfig names.
    let opts = AppOptions { context: Some("restricted".into()), ..Default::default() };
    let mut h = harness_opts(opts, [1280.0, 800.0]);
    settle(&mut h, "limited access note", |h| has_label(h, "Limited access"));
    assert!(!has_label_containing(&h, "NEEDS ATTENTION"));
    shot(&mut h, "47_restricted_overview_no_namespace");
    h.get_by_label(&format!("Use namespace {ns}")).click();
    settle(&mut h, "overview of the namespace", |h| has_label_containing(h, "NEEDS ATTENTION"));
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
    // (The demo cluster's churn loop adds and removes pods while this runs.)
    let shown = h.state().table_stats().0;
    assert!(shown.abs_diff(filtered) <= 5 && shown < total, "still the filtered list: {shown} of {total}, was {filtered}");
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

// ----------------------------------------------------------------------
// Motion. These run the app at 60 frames a second (the other tests take a
// quarter of a second per frame, which is longer than any animation).
// ----------------------------------------------------------------------

/// The app with `json` as its config file, stepping 1/60 s per frame.
fn harness_60fps(context: &str, view: &str, dir: &std::path::Path, json: &str) -> Harness<'static, SkiffApp> {
    let path = dir.join("config.json");
    std::fs::write(&path, json).unwrap();
    let (config, warnings) = skiff::config::Config::load_from(&path);
    assert!(warnings.is_empty(), "{warnings:?}");
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), ..Default::default() };
    let mut h = Harness::builder()
        .with_size([1440.0, 900.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless_with_config(&cc.egui_ctx, opts, config, warnings));
    // The test harness turns every animation off; put back what the
    // config file asks for, as the launched app has it.
    let animations = h.state().config().animations();
    skiff::motion::apply(&h.ctx, animations);
    h.step();
    h
}

/// Where the widget called `label` is in each of the next `frames` frames
/// (`None` while it is not there).
fn track(h: &mut Harness<'static, SkiffApp>, label: &str, frames: usize) -> Vec<Option<egui::Rect>> {
    (0..frames)
        .map(|_| {
            h.step();
            h.query_all_by_label(label).next().map(|n| n.rect())
        })
        .collect()
}

/// The values, without the frames in which there was none and without
/// repeats: the path something took.
fn path(values: impl IntoIterator<Item = Option<f32>>) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for v in values.into_iter().flatten() {
        if out.last().is_none_or(|last| (last - v).abs() > 0.5) {
            out.push(v);
        }
    }
    out
}

/// Shows the notification "Saved" and returns, for each of the next 20
/// frames, how far the colour of a point on its background is from the
/// colour it ends up with (0 = there), without repeats. (A notification's
/// rise is drawn, not laid out, so its widgets do not report it.)
fn notification_arriving(h: &mut Harness<'static, SkiffApp>) -> Vec<u32> {
    h.state_mut().toast("Saved", skiff_core::Tone::Ok);
    let images: Vec<_> = (0..20)
        .map(|_| {
            h.step();
            h.render().expect("render")
        })
        .collect();
    // Left of its coloured dot, inside the frame.
    let dot = h.get_by_label("Saved").rect().left_center() - egui::vec2(18.0, 0.0);
    let at = |i: usize| images[i].get_pixel(dot.x as u32, dot.y as u32).0;
    let last = at(images.len() - 1);
    let mut out: Vec<u32> = Vec::new();
    for i in 0..images.len() {
        let d = at(i).iter().zip(last).map(|(a, b)| u32::from(a.abs_diff(b))).sum();
        if out.last() != Some(&d) {
            out.push(d);
        }
    }
    out
}

#[test]
fn the_detail_panel_and_the_dock_slide_and_notifications_fade() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness_60fps(&ctx, "pods", &scratch("motion-on"), "{}");
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));

    // The detail panel comes in from the right edge: its first tab moves
    // left, frame by frame, and comes to rest inside the window.
    h.get_by_label("alertmanager-0").click();
    let frames = track(&mut h, "YAML", 30);
    let xs = path(frames.iter().map(|r| r.map(|r| r.left())));
    assert!(xs.len() >= 4, "the panel should take several frames to arrive: {xs:?}");
    assert!(xs.windows(2).all(|w| w[1] < w[0]), "the panel should only move left while it opens: {xs:?}");
    assert!(xs[0] - xs.last().unwrap() > 200.0, "it comes from the window's edge, not from next door: {xs:?}");
    // It is done within a fifth of a second, and then nothing moves.
    let rest = frames[12].expect("the panel is open after 0.2 s");
    assert!(frames[12..].iter().all(|r| *r == Some(rest)), "still moving after 0.2 s: {:?}", &frames[12..]);
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));

    // The dock rises from the bottom edge.
    h.get_by_label("Logs").click();
    let frames = track(&mut h, "Hide panel", 30);
    let ys = path(frames.iter().map(|r| r.map(|r| r.top())));
    assert!(ys.len() >= 4 && ys.windows(2).all(|w| w[1] < w[0]), "the dock should rise over several frames: {ys:?}");
    settle(&mut h, "log lines", |h| has_label_containing(h, " lines"));
    // Half-way pictures, for a look at what a frame in between is like.
    h.get_by_label("Hide panel").click();
    steps(&mut h, 2);
    shot(&mut h, "60_dock_sliding_out");
    assert!(!h.state().dock.open, "hidden at once, as far as the app is concerned");
    assert_eq!(is_disabled(&h, "Hide panel"), Some(true), "nothing in a dock on its way out reacts");
    let frames = track(&mut h, "Hide panel", 30);
    let ys = path(frames.iter().map(|r| r.map(|r| r.top())));
    assert!(ys.windows(2).all(|w| w[1] > w[0]), "the dock should only sink while it closes: {ys:?}");
    assert_eq!(frames.last(), Some(&None), "and then it is gone");

    // Esc closes the detail panel at once (the table has the keyboard
    // again); what slides out is a picture of it that takes no clicks.
    h.key_press(egui::Key::Escape);
    steps(&mut h, 2);
    assert!(h.state().detail.is_none(), "closed at once, as far as the app is concerned");
    shot(&mut h, "61_detail_sliding_out");
    assert_eq!(is_disabled(&h, "YAML"), Some(true), "nothing in a panel on its way out reacts");
    let frames = track(&mut h, "YAML", 30);
    let xs = path(frames.iter().map(|r| r.map(|r| r.left())));
    assert!(xs.windows(2).all(|w| w[1] > w[0]), "the panel should only move right while it closes: {xs:?}");
    assert_eq!(frames.last(), Some(&None), "and then it is gone");

    // A notification fades in over several frames.
    let fade = notification_arriving(&mut h);
    assert!(fade.len() >= 5, "the notification should fade in: {fade:?}");
    assert!(fade.windows(2).all(|w| w[1] < w[0]) && fade.last() == Some(&0), "and only become more solid: {fade:?}");
}

#[test]
fn with_animations_off_everything_appears_at_once() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("motion-off");
    let mut h = harness_60fps(&ctx, "pods", &dir, r#"{ "animations": false }"#);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));

    h.get_by_label("alertmanager-0").click();
    let xs = path(track(&mut h, "YAML", 30).iter().map(|r| r.map(|r| r.left())));
    assert_eq!(xs.len(), 1, "the panel should be in its place from the first frame: {xs:?}");
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    h.get_by_label("Logs").click();
    settle(&mut h, "log lines", |h| has_label_containing(h, " lines"));

    h.get_by_label("Hide panel").click();
    steps(&mut h, 2);
    assert!(!has_label(&h, "Hide panel"), "the dock is gone with the click");
    h.key_press(egui::Key::Escape);
    steps(&mut h, 2);
    assert!(h.state().detail.is_none() && !has_label(&h, "Close (Esc)"), "the panel is gone with the key");

    // (Not there, then there: nothing in between.)
    let fade = notification_arriving(&mut h);
    assert!(fade.len() <= 2 && fade.last() == Some(&0), "the notification should appear at once: {fade:?}");

    // The setting is in Settings, Appearance, and takes effect at once.
    open_settings(&mut h, skiff::settings::Section::Appearance);
    h.get_all_by_label("Animations").find(|n| n.accesskit_node().role() == Role::CheckBox).expect("check box").click();
    steps(&mut h, 3);
    assert!(skiff::motion::on(&h.ctx), "animations are back on");
    assert_eq!(config_json(&dir)["animations"], serde_json::json!(true));
}

// ----------------------------------------------------------------------
// First run, failures and protection. These bring their own kubeconfig and
// config files (in a temp folder) through `AppOptions.kubeconfigs` and
// `SkiffApp::headless_with_config`.
// ----------------------------------------------------------------------

/// A fresh folder for one test's files.
fn scratch(name: &str) -> PathBuf {
    let root = workspace().canonicalize().unwrap_or_else(|_| workspace());
    let dir = root.join("target/tmp").join(format!("ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn harness_opts(opts: AppOptions, size: [f32; 2]) -> Harness<'static, SkiffApp> {
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts))
}

/// The app with a config file of its own: `json` is written to
/// `dir/config.json`, which is also where the app saves changes.
fn harness_config(opts: AppOptions, dir: &std::path::Path, json: &str, size: [f32; 2]) -> Harness<'static, SkiffApp> {
    let path = dir.join("config.json");
    std::fs::write(&path, json).unwrap();
    let (config, warnings) = skiff::config::Config::load_from(&path);
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless_with_config(&cc.egui_ctx, opts, config, warnings))
}

fn config_json(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap()
}

/// A kubeconfig with one context whose server is a closed local port.
fn offline_kubeconfig(context: &str, insecure: bool) -> String {
    let tls = if insecure { "\n    insecure-skip-tls-verify: true" } else { "" };
    format!(
        "apiVersion: v1\nkind: Config\ncurrent-context: {context}\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:1{tls}\n\
         contexts:\n- name: {context}\n  context:\n    cluster: c\n    user: u\nusers:\n- name: u\n  user:\n    token: not-a-real-token\n"
    )
}

/// Opens the cluster switcher: its button is the topmost thing that shows
/// the cluster's name.
fn open_cluster_menu(h: &mut Harness<'static, SkiffApp>, context: &str) {
    h.query_all_by_label_contains(context)
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("cluster switcher")
        .click();
    settle(h, "cluster menu", |h| has_label_containing(h, "Reload kubeconfig"));
}

fn is_disabled(h: &Harness<'static, SkiffApp>, label: &str) -> Option<bool> {
    h.query_all_by_label(label).next().map(|n| n.accesskit_node().is_disabled())
}

#[test]
fn welcome_page_when_there_is_no_kubeconfig() {
    let dir = scratch("welcome");
    let missing = dir.join("kube").join("config");
    // For the pictures, the file a new user really lacks (shown as
    // ~/.kube/config), where this computer lacks it too; a path in the
    // build folder otherwise. Nothing is written to it.
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let shown = home.map(|h| h.join(".kube").join("config")).filter(|p| !p.exists()).unwrap_or_else(|| missing.clone());
    for light in [false, true] {
        let opts = AppOptions { kubeconfigs: vec![shown.clone()], light, ..Default::default() };
        let mut h = harness_opts(opts, [1280.0, 800.0]);
        steps(&mut h, 4);
        assert!(has_label(&h, "Welcome to Skiff"));
        assert!(has_label_containing(&h, "Add kubeconfig file"), "the way forward is offered");
        assert!(has_label_containing(&h, "Reload"));
        assert!(has_label(&h, "not found"), "the searched file is listed with what became of it");
        assert!(has_label_containing(&h, "aws eks update-kubeconfig"), "says how clusters get there");
        assert!(!has_label(&h, "Workloads") && !has_label(&h, "Pods"), "no sidebar of resource types without a cluster");
        assert_eq!(h.state().window_title(), "Skiff");
        shot(&mut h, if light { "31_welcome_light" } else { "30_welcome" });
    }

    // The file appears while Skiff is in the background (`aws eks
    // update-kubeconfig` in a terminal): coming back picks it up and opens
    // the cluster, without a restart.
    let Some(ctx) = test_context() else { return };
    let opts = AppOptions { kubeconfigs: vec![missing.clone()], ..Default::default() };
    let mut h = harness_opts(opts, [1280.0, 800.0]);
    steps(&mut h, 3);
    assert!(!h.state_mut().reload_kubeconfig_if_changed(), "nothing changed yet: nothing is read");
    h.get_by_label_contains("Reload").click();
    settle(&mut h, "reload says what it found", |h| has_label_containing(h, "Kubeconfig reloaded: 0 contexts"));
    std::fs::create_dir_all(missing.parent().unwrap()).unwrap();
    std::fs::copy(workspace().join("hack/kubeconfig"), &missing).unwrap();
    assert!(h.state_mut().reload_kubeconfig_if_changed(), "a searched file that appears is noticed");
    settle(&mut h, "the cluster to open", |h| has_label_containing(h, "NEEDS ATTENTION"));
    assert_eq!(h.state().active.as_deref(), Some(ctx.as_str()), "the kubeconfig's current context is opened");
    assert_eq!(h.state().window_title(), format!("Skiff — {ctx}"));
    assert!(!has_label(&h, "Welcome to Skiff"));

    // A context that goes away takes its (inactive) session with it, but
    // the cluster in use stays connected.
    let text = std::fs::read_to_string(&missing).unwrap().replace("name: scale", "name: scale-renamed");
    std::fs::write(&missing, text).unwrap();
    assert!(h.state_mut().reload_kubeconfig_if_changed());
    settle(&mut h, "change notice", |h| has_label_containing(h, "Kubeconfig changed"));
    assert_eq!(h.state().active.as_deref(), Some(ctx.as_str()));
    assert!(h.state().contexts.items.iter().any(|c| c.name == "scale-renamed"));
    assert!(has_label_containing(&h, "NEEDS ATTENTION"), "the open cluster is still shown");
}

#[test]
fn a_broken_kubeconfig_is_listed_and_a_good_one_can_be_added() {
    let dir = scratch("broken");
    let broken = dir.join("broken.yaml");
    std::fs::write(&broken, "clusters: [this is: not, a kubeconfig\n").unwrap();
    let empty = dir.join("empty.yaml");
    std::fs::write(&empty, "apiVersion: v1\nkind: Config\n").unwrap();
    let opts = AppOptions { kubeconfigs: vec![broken.clone(), empty.clone(), dir.join("missing.yaml")], ..Default::default() };
    let mut h = harness_config(opts, &dir, "{}", [1280.0, 800.0]);
    steps(&mut h, 4);
    assert!(has_label(&h, "Welcome to Skiff"));
    assert!(has_label(&h, "can't be used"), "the broken file is called out");
    assert!(has_label(&h, "no contexts") && has_label(&h, "not found"));
    assert_eq!(h.state().kubeconfig_report().problems.len(), 1);
    let message = h.state().kubeconfig_report().problems[0].message.clone();
    assert!(has_label(&h, &message), "its problem is shown: {message}");
    shot(&mut h, "32_welcome_broken_file");

    // Without a desktop file dialog (as here), the file is named by path.
    h.get_by_label_contains("Add kubeconfig file").click();
    settle(&mut h, "path dialog", |h| has_label_containing(h, "Enter the path"));
    h.get_by_role(Role::TextInput).type_text(&dir.join("nowhere.yaml").display().to_string());
    steps(&mut h, 2);
    h.get_by_label("Add").click();
    settle(&mut h, "refusal", |h| has_label_containing(h, "There is no file at"));
    shot(&mut h, "33_add_kubeconfig_dialog");
    assert!(h.state().contexts.items.is_empty());

    let Some(ctx) = test_context() else { return };
    let good = workspace().join("hack/kubeconfig").canonicalize().unwrap();
    h.state_mut().dialog = None;
    h.state_mut().add_kubeconfig(&good.display().to_string()).expect("a good kubeconfig is accepted");
    settle(&mut h, "the cluster to open", |h| has_label_containing(h, "NEEDS ATTENTION"));
    assert_eq!(h.state().active.as_deref(), Some(ctx.as_str()));
    assert!(has_label_containing(&h, "Added 2 clusters"));
    // Saved, and without dropping what was read before.
    let saved = config_json(&dir);
    let list: Vec<&str> = saved["kubeconfigs"].as_array().expect("kubeconfigs saved").iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(list.last().copied(), Some(good.to_str().unwrap()), "{list:?}");
    assert!(list.len() >= 2, "the default sources are kept next to the new file: {list:?}");
    // The files that could not be used are still pointed out, in the menu.
    open_cluster_menu(&mut h, &ctx);
    assert!(has_label_containing(&h, "can't be used"));
    shot(&mut h, "34_cluster_menu_with_problem");
}

#[test]
fn an_unreachable_cluster_says_so_on_every_page() {
    let dir = scratch("offline");
    let file = dir.join("offline.yaml");
    std::fs::write(&file, offline_kubeconfig("offline", true)).unwrap();
    for (view, name) in [(Some("pods"), "35_unreachable"), (None, "36_unreachable_overview")] {
        let opts = AppOptions { kubeconfigs: vec![file.clone()], view: view.map(String::from), ..Default::default() };
        let mut h = harness_opts(opts, [1280.0, 800.0]);
        settle(&mut h, "failure page", |h| has_label(h, "Can't connect to offline"));
        assert!(has_label_containing(&h, "Connection refused"), "what happened");
        assert!(has_label_containing(&h, "Check that the cluster is running"), "what to do about it");
        assert!(has_label(&h, "https://127.0.0.1:1"), "the address that was tried");
        assert!(has_label_containing(&h, "Retry") && has_label_containing(&h, "Copy details"));
        assert!(has_label_containing(&h, "Reconnect"), "the top bar offers it as well");
        assert!(has_label(&h, "TLS not verified"), "an insecure context is marked");
        assert!(!has_label(&h, "NEEDS ATTENTION"), "no numbers for a cluster that cannot be seen");
        assert_eq!(h.state().window_title(), "Skiff — offline");
        shot(&mut h, name);
        h.get_by_label_contains("Copy details").click();
        settle(&mut h, "copied", |h| has_label(h, "Copied the connection details"));
        // Retrying goes through "connecting" back to the same page.
        h.get_by_label_contains("Retry").click();
        settle(&mut h, "failure page again", |h| has_label(h, "Can't connect to offline"));
    }
    // The light theme, and the words in the switcher's tooltip.
    let opts = AppOptions { kubeconfigs: vec![file], view: Some("pods".into()), light: true, ..Default::default() };
    let mut h = harness_opts(opts, [1280.0, 800.0]);
    settle(&mut h, "failure page", |h| has_label(h, "Can't connect to offline"));
    shot(&mut h, "37_unreachable_light");
}

#[test]
fn a_read_only_cluster_offers_no_changes() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("readonly");
    let json = format!(r#"{{"clusters": {{"{ctx}": {{"read_only": true}}}}}}"#);
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("deployments.apps".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, &json, [1440.0, 900.0]);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    assert!(h.state().read_only());
    assert!(has_label(&h, "Read-only"), "the lock is shown next to the cluster name");
    assert_eq!(is_disabled(&h, "Create resource from YAML"), Some(true));

    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    for action in ["Scale", "Restart", "Edit YAML", "Delete"] {
        assert_eq!(is_disabled(&h, action), Some(true), "{action} must be disabled on a read-only cluster");
    }
    assert_eq!(is_disabled(&h, "Logs"), Some(false), "reading stays possible");
    shot(&mut h, "38_read_only_detail");

    // The row menu: the same actions, greyed out.
    let (name, ns) = h.state().row_at(0).expect("a row");
    // (The row is the leftmost thing with that name; the panel shows it too.)
    h.query_all_by_label(&name)
        .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .expect("row")
        .click_secondary();
    settle(&mut h, "row menu", |h| h.query_all_by_label_contains("Copy name").count() >= 2);
    let in_menu = |h: &Harness<'static, SkiffApp>, label: &str| {
        h.query_all_by_label_contains(label).filter(|n| n.accesskit_node().is_disabled()).count()
    };
    assert!(in_menu(&h, "Restart") >= 2 && in_menu(&h, "Delete") >= 2, "the menu's changing actions are disabled too");
    shot(&mut h, "39_read_only_row_menu");
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);

    // An attempt that gets past the buttons is refused, by the app and by
    // the session underneath it.
    let key = skiff_core::kinds::builtin("apps", "deployments").unwrap().key();
    let egui_ctx = h.ctx.clone();
    h.state_mut().run_row_action(&egui_ctx, skiff::table_view::RowAction::Restart, &key, &ns, &name);
    settle(&mut h, "refusal", |h| has_label_containing(h, "is read-only in Skiff"));
    let job = h.state().session().unwrap().restart(&key, &ns, &name);
    let refused = job.take().expect("refused at once, without a request");
    assert!(refused.is_err_and(|e| e.contains("read-only")));
    h.state_mut().open_create_dialog();
    h.step();
    assert!(h.state().dialog.is_none(), "nothing can be created either");

    // The cluster menu has the switch; turning it off is saved.
    open_cluster_menu(&mut h, &ctx);
    assert!(has_label(&h, "Ask before changes") && has_label_containing(&h, "Add kubeconfig file…"));
    shot(&mut h, "40_cluster_menu_read_only");
    h.get_all_by_label("Read-only").find(|n| n.accesskit_node().role() == Role::CheckBox).expect("checkbox").click();
    settle(&mut h, "writable again", |h| !h.state().read_only());
    assert_eq!(config_json(&dir)["clusters"][&ctx]["read_only"], serde_json::json!(false));
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);
    assert_eq!(is_disabled(&h, "Restart"), Some(false));

    // And back on from the palette.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.step();
    h.get_all_by_role(Role::TextInput).last().expect("palette input").type_text("read-only");
    steps(&mut h, 3);
    assert!(has_label(&h, &format!("Make {ctx} read-only")));
    h.key_press(egui::Key::Enter);
    settle(&mut h, "read-only again", |h| h.state().read_only());
    assert_eq!(config_json(&dir)["clusters"][&ctx]["read_only"], serde_json::json!(true));
}

/// The standing notice about the config file, if there is one.
fn config_notice(h: &Harness<'static, SkiffApp>) -> Option<skiff::app::Notice> {
    h.state().notices().iter().find(|n| n.key == "config").cloned()
}

#[test]
fn a_config_file_that_is_not_json_does_not_lift_protection() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("brokenconfig");
    let json = format!(r#"{{"zoom": 1.25, "clusters": {{"{ctx}": {{"read_only": true, "confirm": true}}}}}}"#);
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("deployments.apps".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, &json, [1440.0, 900.0]);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    assert!(h.state().read_only() && config_notice(&h).is_none());

    // An edit in an editor that leaves a trailing comma behind.
    let broken = format!(r#"{{"zoom": 1.25, "grafana": "x", "clusters": {{"{ctx}": {{"read_only": true,}}}}}}"#);
    std::fs::write(dir.join("config.json"), &broken).unwrap();
    assert!(!h.state_mut().reload_settings_if_changed(), "an unusable file is not taken as the settings");
    steps(&mut h, 3);
    assert!(h.state().read_only(), "the cluster stays read-only");
    assert!(h.state().session().unwrap().is_read_only());
    assert!(h.state().config().cluster(&ctx).confirm, "and still asks first");
    assert_eq!(h.state().zoom(), 1.25, "the other settings stay too");
    let notice = config_notice(&h).expect("the broken file is reported");
    assert_eq!(notice.tone, skiff_core::Tone::Err);
    assert!(notice.title.contains("config.json is not valid JSON"), "{}", notice.title);
    assert!(notice.lines.iter().any(|l| l.contains("settings in use are unchanged")), "{:?}", notice.lines);
    assert!(!has_label(&h, "Settings reloaded from config.json"));
    assert_eq!(is_disabled(&h, "Create resource from YAML"), Some(true));

    // Fixed (and changed): read again, and the notice goes.
    std::fs::write(dir.join("config.json"), format!(r#"{{"clusters": {{"{ctx}": {{"read_only": false}}}}}}"#)).unwrap();
    assert!(h.state_mut().reload_settings_if_changed());
    steps(&mut h, 3);
    assert!(!h.state().read_only() && config_notice(&h).is_none());
}

#[test]
fn a_broken_config_file_at_startup_holds_every_cluster_read_only() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("brokenstart");
    // What the file meant to say is unknown, "prod is read-only" included.
    let broken = format!(r#"{{"clusters": {{"{ctx}": {{"read_only": true}}}},"#);
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("deployments.apps".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, &broken, [1440.0, 900.0]);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    assert!(h.state().read_only(), "nothing may be changed while the file cannot be read");
    assert!(has_label(&h, "Read-only"));
    let notice = config_notice(&h).expect("the broken file is reported");
    assert_eq!(notice.tone, skiff_core::Tone::Err);
    assert!(notice.title.contains("not valid JSON"), "{}", notice.title);
    assert!(notice.lines.iter().any(|l| l.contains("every cluster is read-only until the file is fixed")), "{:?}", notice.lines);

    // The switch cannot lift it, and the broken file is not overwritten.
    h.state_mut().set_read_only(&ctx, false);
    steps(&mut h, 3);
    assert!(h.state().read_only());
    assert_eq!(std::fs::read_to_string(dir.join("config.json")).unwrap(), broken);

    // Fixing the file does.
    std::fs::write(dir.join("config.json"), "{}").unwrap();
    assert!(h.state_mut().reload_settings_if_changed());
    steps(&mut h, 3);
    assert!(!h.state().read_only() && config_notice(&h).is_none());
}

#[test]
fn a_file_fixed_after_a_failed_save_is_read_again() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("failedsave");
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("deployments.apps".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, "{}", [1440.0, 900.0]);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);

    // The file is broken when a setting is changed in Skiff: not saved.
    std::fs::write(dir.join("config.json"), "{ oops").unwrap();
    h.state_mut().set_zoom(1.1);
    steps(&mut h, 3);
    assert!(h.state().config().is_dirty());
    assert!(has_label_containing(&h, "Settings not saved"));

    // Fixed as the message asks, with a change of the user's own (and a
    // mistake in one key): both are picked up, and the zoom is saved now.
    let fixed = format!(r#"{{"terminal": 5, "clusters": {{"{ctx}": {{"read_only": true}}}}}}"#);
    std::fs::write(dir.join("config.json"), fixed).unwrap();
    assert!(h.state_mut().reload_settings_if_changed());
    steps(&mut h, 3);
    assert!(h.state().read_only(), "what was written by hand is in effect");
    assert!(!h.state().config().is_dirty());
    assert_eq!(config_json(&dir)["zoom"], serde_json::json!(1.1));
    assert_eq!(config_json(&dir)["clusters"][&ctx]["read_only"], serde_json::json!(true));
    let notice = config_notice(&h).expect("the mistake in one key is reported");
    assert!(notice.tone == skiff_core::Tone::Warn && notice.lines.iter().any(|l| l.starts_with("terminal")), "{notice:?}");
    assert!(!h.state_mut().reload_settings_if_changed(), "its own save is not taken for an edit");

    // While the file stays broken, the failure is said once per change of
    // the file, and nothing is lost.
    std::fs::write(dir.join("config.json"), "{ oops again").unwrap();
    h.state_mut().set_zoom(1.25);
    assert!(!h.state_mut().reload_settings_if_changed());
    assert!(h.state().read_only() && h.state().config().is_dirty());
}

#[cfg(feature = "terminal")]
#[test]
fn an_open_shell_tab_starts_no_new_shell_on_a_read_only_cluster() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("shellreadonly");
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("pods".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, "{}", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    h.get_by_label("alertmanager-0").click();
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    h.get_by_label("Shell").click();
    settle(&mut h, "shell to connect", |h| has_label(h, "connected"));
    // (The button's name starts with its icon.)
    let external = |h: &Harness<'static, SkiffApp>| {
        h.query_all_by_label_contains("Open in terminal app").next().map(|n| n.accesskit_node().is_disabled())
    };
    assert_eq!(external(&h), Some(false));

    // Read-only is switched on while the tab is open: the shell that runs
    // stays, a new one in the terminal app is not offered.
    h.state_mut().set_read_only(&ctx, true);
    steps(&mut h, 3);
    assert!(has_label(&h, "connected"), "the open shell keeps running");
    assert_eq!(external(&h), Some(true));
}

#[test]
fn changes_are_confirmed_on_a_cluster_that_asks_first() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("confirm");
    let json = format!(r#"{{"clusters": {{"{ctx}": {{"confirm": true}}}}}}"#);
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("deployments.apps".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, &json, [1440.0, 900.0]);
    settle(&mut h, "table", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "YAML"));
    let (name, ns) = {
        let d = h.state().detail.as_ref().unwrap();
        (d.name.clone(), d.ns.clone())
    };
    let before = kubectl(&["-n", &ns, "get", "deploy", &name, "-o", "jsonpath={.metadata.generation}"]);

    h.get_by_label("Restart").click();
    settle(&mut h, "question", |h| has_label(h, "Restart Deployment?"));
    assert!(has_label(&h, &format!("{ns}/{name}")), "the object is named");
    assert!(has_label(&h, "on cluster") && has_label(&h, &ctx), "the cluster is named");
    shot(&mut h, "41_confirm_restart");
    // Cancel is the default: Enter (even held down) does not go ahead.
    h.key_press(egui::Key::Enter);
    steps(&mut h, 4);
    assert!(h.state().confirm.is_none(), "Enter answers with the default, Cancel");
    assert!(!has_label_containing(&h, "Restarting"));
    let after = kubectl(&["-n", &ns, "get", "deploy", &name, "-o", "jsonpath={.metadata.generation}"]);
    assert_eq!(before, after, "nothing was restarted");

    // Scaling asks after the number is chosen.
    h.get_by_label("Scale").click();
    settle(&mut h, "replica count", |h| {
        h.query_all_by_role(Role::TextInput).last().and_then(|n| n.value()).is_some_and(|v| !v.is_empty())
    });
    h.get_all_by_label("Scale").last().expect("dialog button").click();
    settle(&mut h, "question", |h| has_label_containing(h, "Scale Deployment to"));
    shot(&mut h, "42_confirm_scale");
    h.get_all_by_label("Cancel").last().unwrap().click();
    steps(&mut h, 3);
    assert!(h.state().confirm.is_none() && h.state().dialog.is_none());

    // Applying a manifest asks too, and goes ahead only on a yes.
    let cm = format!("skiff-ui-confirm-{}", std::process::id());
    h.state_mut().dialog = Some(skiff::app::Dialog::Create {
        text: format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {cm}\n  namespace: default\ndata:\n  key: value\n"),
        pristine: String::new(),
        context: ctx.clone(),
        job: None,
        error: None,
    });
    // (The dialog takes a few frames to find its size and place.)
    steps(&mut h, 8);
    h.get_by_label("Apply").click();
    settle(&mut h, "question", |h| has_label(h, "Apply this manifest?"));
    shot(&mut h, "43_confirm_apply");
    assert!(kubectl(&["-n", "default", "get", "cm", &cm, "--ignore-not-found", "-o", "name"]).is_empty(), "not applied before the answer");
    h.get_all_by_label("Apply").last().expect("the confirming button").click();
    settle(&mut h, "applied", |h| h.state().dialog.is_none() && has_label_containing(h, &cm));
    assert_eq!(kubectl(&["-n", "default", "get", "cm", &cm, "-o", "jsonpath={.data.key}"]), "value");
    kubectl(&["-n", "default", "delete", "cm", &cm]);
}

#[test]
fn an_apply_conflict_names_the_owners_and_can_be_overridden() {
    let Some(ctx) = test_context() else { return };
    let cm = format!("skiff-ui-conflict-{}", std::process::id());
    let manifest = |mode: &str| {
        format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {cm}\n  namespace: default\ndata:\n  mode: {mode}\n  other: same\n")
    };
    // Someone else (say, Helm) owns the object's fields.
    let theirs = scratch("conflict").join("theirs.yaml");
    std::fs::write(&theirs, manifest("theirs")).unwrap();
    kubectl(&["apply", "--server-side", "--field-manager=helm", "-f", theirs.to_str().unwrap()]);
    assert_eq!(kubectl(&["-n", "default", "get", "cm", &cm, "-o", "jsonpath={.data.mode}"]), "theirs");

    let mut h = harness_with(&ctx, "configmaps", [1440.0, 900.0], false);
    settle(&mut h, "table", |h| h.state().table_stats().1);
    h.state_mut().dialog = Some(skiff::app::Dialog::Create {
        text: manifest("mine"),
        pristine: String::new(),
        context: ctx.clone(),
        job: None,
        error: None,
    });
    // (The dialog takes a few frames to find its size and place.)
    steps(&mut h, 8);
    h.get_by_label("Apply").click();
    settle(&mut h, "conflict report", |h| has_label(h, "Apply anyway"));
    assert!(has_label(&h, ".data.mode") && has_label(&h, "helm"), "which field, and whose it is");
    assert!(has_label_containing(&h, "belong to another manager"));
    assert_eq!(kubectl(&["-n", "default", "get", "cm", &cm, "-o", "jsonpath={.data.mode}"]), "theirs", "nothing is taken over unasked");
    shot(&mut h, "44_apply_conflict");

    h.get_by_label("Apply anyway").click();
    settle(&mut h, "applied", |h| h.state().dialog.is_none());
    assert_eq!(kubectl(&["-n", "default", "get", "cm", &cm, "-o", "jsonpath={.data.mode}"]), "mine");
    kubectl(&["-n", "default", "delete", "cm", &cm]);
}

#[test]
fn config_problems_are_shown_once_and_can_be_dismissed() {
    let dir = scratch("notice");
    let opts = AppOptions { kubeconfigs: vec![dir.join("none.yaml")], ..Default::default() };
    let json = r#"{"zoom": "big", "updates": {"channel": "weekly"}, "colour": "blue"}"#;
    let mut h = harness_config(opts, &dir, json, [1280.0, 800.0]);
    steps(&mut h, 4);
    assert_eq!(h.state().notices().len(), 1);
    assert!(has_label_containing(&h, "config.json were ignored"), "the notice names the file");
    assert!(has_label_containing(&h, "zoom"), "and what was wrong in it");
    shot(&mut h, "45_config_notice");
    h.get_by_label("Dismiss").click();
    steps(&mut h, 3);
    assert!(h.state().notices().is_empty());
    assert!(!has_label_containing(&h, "were ignored"));
}

// ---------------------------------------------------------------------
// Settings, About, keyboard help, zoom, accessibility

/// Opens the Settings window on a section.
fn open_settings(h: &mut Harness<'static, SkiffApp>, section: skiff::settings::Section) {
    h.state_mut().open_settings(section);
    settle(h, "settings window", |h| has_label(h, "Close settings"));
}

/// Runs frames until the app puts text on the clipboard, and returns it.
fn step_until_copied(h: &mut Harness<'static, SkiffApp>) -> String {
    for _ in 0..40 {
        h.step();
        let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        });
        if let Some(text) = copied {
            return text;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("nothing was copied");
}

/// A kubeconfig whose one context signs in with a credential plugin.
fn plugin_kubeconfig(context: &str, command: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Config\nclusters:\n- name: c-{context}\n  cluster:\n    server: https://127.0.0.1:1\n\
         contexts:\n- name: {context}\n  context:\n    cluster: c-{context}\n    user: u-{context}\nusers:\n- name: u-{context}\n  user:\n    exec:\n      \
         apiVersion: client.authentication.k8s.io/v1beta1\n      command: {command}\n      args: [eks, get-token]\n"
    )
}

/// The labels of buttons that have no name a screen reader could say:
/// nothing at all, or only an icon glyph.
fn unnamed_buttons(h: &Harness<'static, SkiffApp>) -> Vec<String> {
    let glyph = |c: char| ('\u{e000}'..='\u{f8ff}').contains(&c) || c >= '\u{f0000}';
    h.query_all_by_role(Role::Button)
        .filter_map(|n| {
            let label = n.accesskit_node().label().unwrap_or_default();
            let bare = label.chars().filter(|c| !c.is_whitespace()).all(glyph);
            bare.then(|| format!("{label:?} at {:?}", n.rect()))
        })
        .collect()
}

/// The dev clusters' kubeconfig, as an absolute path without `..`.
fn dev_kubeconfig() -> String {
    let path = std::env::var("SKIFF_TEST_KUBECONFIG").map(PathBuf::from).unwrap_or_else(|_| workspace().join("hack/kubeconfig"));
    path.canonicalize().unwrap_or(path).display().to_string()
}

#[test]
fn settings_window_shows_and_saves_every_section() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("settings");
    // A list of kubeconfigs of every kind: used, with a credential plugin
    // that is not installed, broken, and missing.
    let eks = dir.join("eks.yaml");
    std::fs::write(&eks, plugin_kubeconfig("eks-prod", "skiff-test-aws")).unwrap();
    let broken = dir.join("broken.yaml");
    std::fs::write(&broken, "clusters: [unclosed").unwrap();
    let missing = dir.join("staging.yaml");
    let entries = [dev_kubeconfig(), eks.display().to_string(), broken.display().to_string(), missing.display().to_string()];
    let json = serde_json::json!({ "kubeconfigs": entries }).to_string();
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("pods".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, &json, [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));

    // The gear in the top bar opens it; Esc closes it; so does Ctrl+,.
    h.get_by_label_contains("Settings  (").click();
    settle(&mut h, "settings window", |h| has_label(h, "Close settings"));
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);
    assert!(h.state().settings.is_none(), "Esc closes the window");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    settle(&mut h, "settings window", |h| has_label(h, "Close settings"));

    // Appearance: the theme applies at once and is saved.
    assert!(has_label(&h, "Theme") && has_label(&h, "Zoom"));
    shot(&mut h, "50_settings_appearance");
    h.get_by_label("Light").click();
    settle(&mut h, "light theme", |h| !h.state().pal.dark);
    assert_eq!(config_json(&dir)["theme"], "light");
    shot(&mut h, "51_settings_appearance_light");
    h.get_by_label("System").click();
    steps(&mut h, 3);
    assert_eq!(config_json(&dir)["theme"], "system");
    assert!(h.state().pal.dark, "a system that does not say is taken as dark");
    // "System" follows the computer while Skiff runs.
    h.input_mut().system_theme = Some(egui::Theme::Light);
    steps(&mut h, 3);
    assert!(!h.state().pal.dark, "the system switched to light");
    assert!(has_label_containing(&h, "which is set to light now"));
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    steps(&mut h, 3);
    assert!(h.state().pal.dark);
    h.get_by_label("Dark").click();
    steps(&mut h, 3);
    assert_eq!(config_json(&dir)["theme"], "dark");

    // Zoom: the buttons step it, and it is saved.
    h.get_by_label("Zoom in").click();
    settle(&mut h, "zoom", |h| (h.ctx.zoom_factor() - 1.1).abs() < 0.001);
    assert_eq!(config_json(&dir)["zoom"], serde_json::json!(1.1));
    assert!(has_label(&h, "110%"));
    h.get_by_label("Reset").click();
    settle(&mut h, "zoom reset", |h| (h.ctx.zoom_factor() - 1.0).abs() < 0.001);
    assert_eq!(config_json(&dir)["zoom"], serde_json::json!(1.0));

    // Clusters: the files in effect and what became of each.
    h.get_by_label("Clusters").click();
    steps(&mut h, 4);
    for expected in ["2 contexts", "1 context", "can't be used", "not found", "which was not found"] {
        assert!(has_label_containing(&h, expected), "missing {expected:?}");
    }
    shot(&mut h, "52_settings_clusters");
    // Protection is per cluster, saved, and applied to the open one.
    h.get_by_label(&format!("{ctx} is read-only")).click();
    settle(&mut h, "read-only", |h| h.state().read_only());
    assert_eq!(config_json(&dir)["clusters"][&ctx]["read_only"], serde_json::json!(true));
    assert_eq!(is_disabled(&h, &format!("Ask before changes to {ctx}")), Some(true), "nothing to ask while read-only");
    h.get_by_label(&format!("{ctx} is read-only")).click();
    settle(&mut h, "writable", |h| !h.state().read_only());
    h.get_by_label("Ask before changes to eks-prod").scroll_to_me();
    steps(&mut h, 3);
    h.get_by_label("Ask before changes to eks-prod").click();
    steps(&mut h, 3);
    assert_eq!(config_json(&dir)["clusters"]["eks-prod"]["confirm"], serde_json::json!(true));
    // A file can be taken off the list.
    h.get_by_label(&format!("Remove {} from the list", missing.display())).click();
    settle(&mut h, "entry removed", |h| !has_label_containing(h, "staging.yaml"));
    assert_eq!(config_json(&dir)["kubeconfigs"].as_array().map(Vec::len), Some(3));
    h.get_by_label(&format!("Remove {} from the list", eks.display())).click();
    settle(&mut h, "cluster gone", |h| !has_label_containing(h, "eks-prod"));
    assert!(h.state().contexts.items.iter().all(|c| c.name != "eks-prod"));
    assert!(h.state().session().is_some(), "the open cluster stays open");
    // "Add file" asks for one (by path here: no file dialog in a test).
    h.get_by_label_contains("Add file…").click();
    settle(&mut h, "path dialog", |h| has_label(h, "Add kubeconfig file"));
    h.get_all_by_role(Role::TextInput).last().expect("path field").type_text(&eks.display().to_string());
    h.step();
    h.get_by_label("Add").click();
    settle(&mut h, "cluster back", |h| has_label_containing(h, "eks-prod") && h.state().dialog.is_none());
    assert!(h.state().settings.is_some(), "the window stays open behind the dialog");

    // Updates.
    h.get_by_label("Updates").click();
    steps(&mut h, 4);
    shot(&mut h, "53_settings_updates");
    h.get_by_label("Nightly").click();
    steps(&mut h, 3);
    assert_eq!(config_json(&dir)["updates"]["channel"], "nightly");
    h.get_all_by_label("Check for updates automatically").find(|n| n.accesskit_node().role() == Role::CheckBox).expect("check box").click();
    steps(&mut h, 3);
    assert_eq!(config_json(&dir)["updates"]["check"], serde_json::json!(false));

    // Terminal: saved when the field is left.
    h.get_by_label("Terminal").click();
    steps(&mut h, 4);
    if !cfg!(windows) {
        let field = h.get_all_by_role(Role::TextInput).last().expect("terminal field");
        field.focus();
        field.type_text("kitty");
        h.step();
        h.key_press(egui::Key::Enter);
        steps(&mut h, 3);
        assert_eq!(config_json(&dir)["terminal"], "kitty");
    }
    shot(&mut h, "54_settings_terminal");

    // Files: opening creates what is missing, without touching what is there.
    h.get_by_label("Files").click();
    steps(&mut h, 4);
    shot(&mut h, "55_settings_files");
    h.get_by_label_contains("Open keys file").click();
    settle(&mut h, "keys file", |h| has_label_containing(h, "with the standard keys"));
    let keys = std::fs::read_to_string(dir.join("keys.json")).unwrap();
    let (_, errors) = skiff::keymap::Keymap::from_json(&keys);
    assert!(errors.is_empty() && keys.contains("\"help\""), "{keys}");
    let before = std::fs::read_to_string(dir.join("config.json")).unwrap();
    h.get_by_label_contains("Open config file").click();
    steps(&mut h, 3);
    assert_eq!(std::fs::read_to_string(dir.join("config.json")).unwrap(), before);

    // About.
    h.get_by_label("About").click();
    steps(&mut h, 4);
    for expected in ["Version", "Build", "Commit", "Target", "Licence", "MIT", "Repository", "Releases", "Report a problem", "Third-party licences"] {
        assert!(has_label_containing(&h, expected), "missing {expected:?}");
    }
    shot(&mut h, "56_settings_about");
    h.get_by_label_contains("Copy version info").click();
    let info = step_until_copied(&mut h);
    assert!(info.starts_with(&format!("Skiff {}", env!("CARGO_PKG_VERSION"))) && info.contains("Target: "), "{info}");
    assert!(info.contains("Update channel: nightly"));
    assert!(unnamed_buttons(&h).is_empty(), "{:?}", unnamed_buttons(&h));

    h.get_by_label("Close settings").click();
    steps(&mut h, 3);
    assert!(h.state().settings.is_none());
}

#[test]
fn settings_in_the_light_theme_and_from_the_palette() {
    use skiff::settings::Section;
    let Some(ctx) = test_context() else { return };
    let dir = scratch("settings-light");
    let eks = dir.join("eks.yaml");
    std::fs::write(&eks, plugin_kubeconfig("eks-prod", "skiff-test-aws")).unwrap();
    let json = serde_json::json!({
        "theme": "light",
        "kubeconfigs": [dev_kubeconfig(), eks.display().to_string()],
        "clusters": { "eks-prod": { "read_only": true } },
    })
    .to_string();
    let opts = AppOptions { context: Some(ctx.clone()), view: Some("pods".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, &json, [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    assert!(!h.state().pal.dark, "the config file's theme applies from the start");

    // From the palette.
    for (query, title, section) in [("about", "About", Section::About), ("cluster settings", "Clusters", Section::Clusters)] {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.step();
        h.get_all_by_role(Role::TextInput).last().expect("palette input").type_text(query);
        steps(&mut h, 3);
        h.key_press(egui::Key::Enter);
        settle(&mut h, "settings window", |h| has_label(h, "Close settings"));
        assert_eq!(h.state().settings.as_ref().map(|s| s.section), Some(section), "{title}");
        shot(&mut h, if section == Section::About { "57_settings_about_light" } else { "58_settings_clusters_light" });
        h.key_press(egui::Key::Escape);
        steps(&mut h, 3);
    }
    open_settings(&mut h, Section::Updates);
    shot(&mut h, "59_settings_updates_light");
}

#[test]
fn keyboard_help_lists_the_live_keymap() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));

    // "?" opens it (Shift+/ on most keyboards), "?" or Esc closes it.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    settle(&mut h, "help", |h| has_label(h, "Keyboard shortcuts"));
    for expected in ["EVERYWHERE", "MOVING AROUND", "TABLES", "DETAILS", "LOGS", "SHELL", "Search or jump to anything", "Filter the table"] {
        assert!(has_label_containing(&h, expected), "missing {expected:?}");
    }
    assert!(has_label(&h, "j") && has_label(&h, "Shift+G") && has_label(&h, "?"));
    shot(&mut h, "60_keyboard_help");
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Questionmark);
    steps(&mut h, 3);
    assert!(!h.state().help_open, "the same key closes it");

    // Custom bindings are what is shown, and what works.
    let (keymap, errors) = skiff::keymap::Keymap::from_json(r#"{ "down": ["n"], "help": ["F1"], "filter": [] }"#);
    assert!(errors.is_empty());
    h.state_mut().keymap = keymap;
    h.key_press(egui::Key::Questionmark);
    steps(&mut h, 3);
    assert!(!h.state().help_open, "? is no longer bound");
    h.key_press(egui::Key::F1);
    settle(&mut h, "help", |h| has_label(h, "Keyboard shortcuts"));
    assert!(has_label(&h, "n") && has_label(&h, "F1") && has_label(&h, "not set") && !has_label(&h, "j"));
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);
    assert!(!h.state().help_open);
    h.state_mut().keymap = skiff::keymap::Keymap::default();

    // In a text field "?" is a character.
    h.key_press(egui::Key::Slash);
    h.step();
    h.get_by_role(Role::TextInput).type_text("?");
    h.key_press(egui::Key::Questionmark);
    steps(&mut h, 3);
    assert!(!h.state().help_open, "typing ? into the filter does not open the list");
    h.key_press(egui::Key::Escape);
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);

    // The palette has it too.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.step();
    h.get_all_by_role(Role::TextInput).last().expect("palette input").type_text("keyboard");
    steps(&mut h, 3);
    h.key_press(egui::Key::Enter);
    settle(&mut h, "help", |h| h.state().help_open && has_label(h, "Keyboard shortcuts"));
    h.key_press(egui::Key::Escape);
    steps(&mut h, 3);

    let mut light = harness_with(&ctx, "pods", [1100.0, 760.0], true);
    settle(&mut light, "pods to load", |h| has_label(h, "alertmanager-0"));
    light.state_mut().open_help();
    settle(&mut light, "help", |h| has_label(h, "Keyboard shortcuts"));
    shot(&mut light, "61_keyboard_help_light");
}

#[test]
fn first_run_hint_points_at_the_palette_and_the_shortcuts() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "overview", [1440.0, 900.0]);
    settle(&mut h, "overview", |h| has_label_containing(h, "to search or jump to anything"));
    assert!(has_label(&h, "?") && has_label(&h, "Dismiss this hint"));
    shot(&mut h, "62_overview_first_run_hint");
    // Using what it points at is enough to make it go away.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    steps(&mut h, 2);
    h.key_press(egui::Key::Escape);
    steps(&mut h, 4);
    assert!(!has_label_containing(&h, "to search or jump to anything"));

    let mut h = harness_with(&ctx, "overview", [1440.0, 900.0], true);
    settle(&mut h, "overview", |h| has_label_containing(h, "to search or jump to anything"));
    shot(&mut h, "63_overview_first_run_hint_light");
    h.get_by_label("Dismiss this hint").click();
    steps(&mut h, 3);
    assert!(!has_label_containing(&h, "to search or jump to anything"));
}

/// The app in a window of `window` pixels at a zoom factor (from the
/// config file, as at startup).
fn zoomed(dir: &std::path::Path, ctx: &str, zoom: f32, window: [f32; 2], light: bool) -> Harness<'static, SkiffApp> {
    let json = serde_json::json!({ "zoom": zoom, "theme": if light { "light" } else { "dark" } }).to_string();
    let opts = AppOptions { context: Some(ctx.to_string()), view: Some("pods".into()), ..Default::default() };
    // (The harness is sized in points: a window of this many pixels.)
    harness_config(opts, dir, &json, [window[0] / zoom, window[1] / zoom])
}

#[test]
fn zoom_keeps_tables_dialogs_and_the_shell_laid_out() {
    let Some(ctx) = test_context() else { return };
    for (zoom, tag) in [(0.8_f32, "080"), (1.25, "125"), (1.5, "150")] {
        let dir = scratch(&format!("zoom-{tag}"));
        let mut h = zoomed(&dir, &ctx, zoom, [1440.0, 900.0], zoom == 1.25);
        settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
        assert!((h.ctx.zoom_factor() - zoom).abs() < 0.001, "the config file's zoom applies from the start");
        assert!((h.ctx.pixels_per_point() - zoom).abs() < 0.001);
        let screen = h.ctx.content_rect();
        let inside = |h: &Harness<'static, SkiffApp>, label: &str| {
            let rect = h.query_all_by_label(label).next().unwrap_or_else(|| panic!("no {label:?}")).rect();
            assert!(screen.contains_rect(rect), "{label:?} at {rect:?} is outside the window {screen:?} at zoom {zoom}");
        };

        // Table and details.
        h.get_by_label("alertmanager-0").click();
        settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
        for label in ["alertmanager-0", "Logs", "Shell", "Close (Esc)", "Create resource from YAML"] {
            inside(&h, label);
        }
        shot(&mut h, &format!("64_zoom_{tag}_table_and_details"));

        // A shell: the terminal's grid follows the zoom.
        h.get_by_label("Shell").click();
        settle(&mut h, "shell to connect", |h| has_label(h, "connected"));
        h.get_by_label("Terminal").type_text("echo zoom-$((6*7))");
        h.key_press(egui::Key::Enter);
        settle(&mut h, "command output", |h| {
            h.state().dock.shell_screen().unwrap_or_default().lines().any(|l| l.trim() == "zoom-42")
        });
        inside(&h, "Terminal");
        shot(&mut h, &format!("65_zoom_{tag}_shell"));

        // A dialog, and the Settings window.
        h.state_mut().palette = None;
        h.state_mut().open_create_dialog();
        settle(&mut h, "create dialog", |h| has_label(h, "Apply"));
        inside(&h, "Apply");
        inside(&h, "Cancel");
        shot(&mut h, &format!("66_zoom_{tag}_dialog"));
        h.get_by_label("Cancel").click();
        steps(&mut h, 3);
        open_settings(&mut h, skiff::settings::Section::Appearance);
        inside(&h, "Close settings");
        inside(&h, "About");
        inside(&h, "Reset");
        assert!(has_label(&h, &format!("{}%", (zoom * 100.0).round() as i32)));
        shot(&mut h, &format!("67_zoom_{tag}_settings"));
        h.key_press(egui::Key::Escape);
        steps(&mut h, 3);
    }
}

#[test]
fn zoom_shortcuts_step_clamp_and_persist() {
    let Some(ctx) = test_context() else { return };
    let dir = scratch("zoom-keys");
    let opts = AppOptions { context: Some(ctx), view: Some("pods".into()), ..Default::default() };
    let mut h = harness_config(opts, &dir, "{}", [1440.0, 900.0]);
    settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
    let cmd = egui::Modifiers::COMMAND;
    let zoom = |h: &Harness<'static, SkiffApp>| (h.ctx.zoom_factor() * 100.0).round() as i32;

    h.key_press_modifiers(cmd, egui::Key::Equals);
    steps(&mut h, 3);
    assert_eq!(zoom(&h), 110);
    assert!(has_label(&h, "Zoom 110%"), "the new size is said");
    // (Ctrl and + on a keyboard where + needs Shift.)
    h.key_press_modifiers(cmd | egui::Modifiers::SHIFT, egui::Key::Plus);
    steps(&mut h, 3);
    assert_eq!(zoom(&h), 125);
    assert_eq!(config_json(&dir)["zoom"], serde_json::json!(1.25));
    h.key_press_modifiers(cmd, egui::Key::Minus);
    steps(&mut h, 3);
    assert_eq!(zoom(&h), 110);
    h.key_press_modifiers(cmd, egui::Key::Num0);
    steps(&mut h, 3);
    assert_eq!(zoom(&h), 100);
    assert_eq!(config_json(&dir)["zoom"], serde_json::json!(1.0));

    // The ends of the range hold.
    for _ in 0..12 {
        h.key_press_modifiers(cmd, egui::Key::Minus);
        steps(&mut h, 2);
    }
    assert_eq!(zoom(&h), 50);
    for _ in 0..20 {
        h.key_press_modifiers(cmd, egui::Key::Equals);
        steps(&mut h, 2);
    }
    assert_eq!(zoom(&h), 300);
    assert_eq!(config_json(&dir)["zoom"], serde_json::json!(3.0));

    // A change made in the file is picked up when Skiff gets the focus back.
    std::fs::write(dir.join("config.json"), r#"{ "zoom": 1.5, "theme": "light" }"#).unwrap();
    assert!(h.state_mut().reload_settings_if_changed());
    steps(&mut h, 3);
    assert_eq!(zoom(&h), 150);
    assert!(!h.state().pal.dark);
    assert!(!h.state_mut().reload_settings_if_changed(), "nothing changed since");
}

#[test]
fn a_copied_secret_is_cleared_from_the_clipboard() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "secrets", [1440.0, 900.0]);
    settle(&mut h, "secrets to load", |h| h.state().table_stats().1 && h.state().table_stats().0 > 0);
    h.state_mut().secret_clipboard_time = Duration::from_millis(600);
    h.key_press(egui::Key::ArrowDown);
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, "secret details", |h| has_label(h, "Copy decoded value"));

    // The decoded value: copied, announced, and cleared when the time is up.
    h.get_all_by_label("Copy decoded value").next().expect("copy button").click();
    let copied = step_until_copied(&mut h);
    assert!(!copied.is_empty());
    assert!(h.state().secret_on_clipboard());
    steps(&mut h, 2);
    assert!(has_label_containing(&h, "The clipboard will be cleared in"), "the notification says so");
    shot(&mut h, "68_secret_copied");
    std::thread::sleep(Duration::from_millis(650));
    assert_eq!(step_until_copied(&mut h), "", "the clipboard is cleared");
    assert!(!h.state().secret_on_clipboard());
    settle(&mut h, "cleared notice", |h| has_label(h, "Clipboard cleared"));

    // Where the clipboard can be read, it is cleared only while it still
    // holds the secret: something copied in another app is left alone.
    let clipboard = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let reader = clipboard.clone();
    h.state_mut().clipboard_reader = Some(Box::new(move || reader.borrow().clone()));
    h.get_all_by_label("Copy decoded value").next().expect("copy button").click();
    let secret = step_until_copied(&mut h);
    steps(&mut h, 2);
    assert!(has_label_containing(&h, "if it still holds it"), "the notification says when");
    *clipboard.borrow_mut() = Some("https://copied.elsewhere.example".to_string());
    std::thread::sleep(Duration::from_millis(650));
    for _ in 0..3 {
        h.step();
        assert!(h.output().platform_output.commands.is_empty(), "another app's copy is not wiped");
    }
    assert!(!h.state().secret_on_clipboard(), "and the clearing is off");
    // (So is a clipboard that holds no text: an image, say.)
    h.get_all_by_label("Copy decoded value").next().expect("copy button").click();
    step_until_copied(&mut h);
    *clipboard.borrow_mut() = Some(String::new());
    std::thread::sleep(Duration::from_millis(650));
    for _ in 0..3 {
        h.step();
        assert!(h.output().platform_output.commands.is_empty(), "a clipboard without text is not wiped");
    }
    assert!(!h.state().secret_on_clipboard());
    // A clipboard that cannot be read may still hold the secret: cleared.
    h.get_all_by_label("Copy decoded value").next().expect("copy button").click();
    step_until_copied(&mut h);
    *clipboard.borrow_mut() = None;
    std::thread::sleep(Duration::from_millis(650));
    assert_eq!(step_until_copied(&mut h), "", "an unreadable clipboard is cleared");
    assert!(!h.state().secret_on_clipboard());
    // Still the secret: cleared.
    h.get_all_by_label("Copy decoded value").next().expect("copy button").click();
    step_until_copied(&mut h);
    *clipboard.borrow_mut() = Some(secret);
    std::thread::sleep(Duration::from_millis(650));
    assert_eq!(step_until_copied(&mut h), "", "the secret is cleared");
    h.state_mut().clipboard_reader = None;

    // Copying something else from Skiff in the meantime calls it off:
    // that is not ours to clear.
    h.get_all_by_label("Copy decoded value").next().expect("copy button").click();
    step_until_copied(&mut h);
    assert!(h.state().secret_on_clipboard());
    let (name, _) = h.state().selected_row().expect("selected secret");
    let key = skiff_core::kinds::builtin("", "secrets").unwrap().key();
    let (row_name, ns) = (name.clone(), h.state().selected_row().unwrap().1);
    let egui_ctx = h.ctx.clone();
    h.state_mut().run_row_action(&egui_ctx, skiff::table_view::RowAction::CopyName, &key, &ns, &row_name);
    assert_eq!(step_until_copied(&mut h), name);
    assert!(!h.state().secret_on_clipboard(), "another copy cancels the clearing");
    std::thread::sleep(Duration::from_millis(650));
    steps(&mut h, 3);
    assert!(h.output().platform_output.commands.is_empty(), "nothing is cleared after another copy");

    // The Secret's YAML counts as a secret too.
    h.get_by_label("YAML").click();
    settle(&mut h, "yaml tab", |h| has_label_containing(h, "managedFields hidden"));
    h.query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.trim_end().ends_with("Copy")))
        .expect("the YAML tab's Copy button")
        .click();
    let yaml = step_until_copied(&mut h);
    assert!(yaml.contains("kind: Secret"));
    assert!(h.state().secret_on_clipboard());
    std::thread::sleep(Duration::from_millis(650));
    assert_eq!(step_until_copied(&mut h), "");
}

#[test]
fn keyboard_focus_is_outlined_and_icon_buttons_have_names() {
    let Some(ctx) = test_context() else { return };
    for light in [false, true] {
        let mut h = harness_with(&ctx, "pods", [1440.0, 900.0], light);
        settle(&mut h, "pods to load", |h| has_label(h, "alertmanager-0"));
        h.get_by_label("alertmanager-0").click();
        settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
        h.get_by_label("Logs").click();
        settle(&mut h, "logs", |h| h.state().dock.open);
        steps(&mut h, 6);
        assert!(unnamed_buttons(&h).is_empty(), "buttons without a name: {:?}", unnamed_buttons(&h));

        // Tab moves the focus; the focused control is outlined.
        open_settings(&mut h, skiff::settings::Section::Appearance);
        for _ in 0..3 {
            h.key_press(egui::Key::Tab);
            h.step();
        }
        steps(&mut h, 2);
        assert!(h.query_all_by_role(Role::Button).chain(h.query_all_by_role(Role::Tab)).chain(h.query_all_by_role(Role::RadioButton)).any(|n| n.is_focused()));
        assert!(unnamed_buttons(&h).is_empty(), "buttons without a name: {:?}", unnamed_buttons(&h));
        shot(&mut h, if light { "70_focus_outline_light" } else { "69_focus_outline" });
    }
}

// ---------------------------------------------------------------------------
// Metrics charts, firing alerts and Grafana links. The demo cluster's
// Prometheus is `hack/fake-prometheus.py` behind the Service
// monitoring/prometheus-operated, reached through the API server's proxy.

/// Whether the demo cluster has its (fake) Prometheus; the tests that need
/// it are skipped otherwise.
fn demo_has_prometheus() -> bool {
    let path = "/api/v1/namespaces/monitoring/services/http:prometheus-operated:9090/proxy/api/v1/query?query=vector(1)";
    let ok = kubectl(&["get", "--raw", path]).contains("success");
    if !ok {
        eprintln!("skipping: no Prometheus in the demo cluster (run hack/cluster-up.sh)");
    }
    ok
}

/// A pod that stays: one of kube-system's kube-proxy pods. (The test bed
/// keeps deleting pods in the application namespaces.)
fn steady_pod() -> String {
    let out = kubectl(&["-n", "kube-system", "get", "pods", "-o", "name"]);
    let mut names: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("pod/")).filter(|n| n.starts_with("kube-proxy-")).collect();
    names.sort();
    names.first().expect("a kube-proxy pod").to_string()
}

fn in_namespace(context: &str, view: &str, namespace: &str, light: bool) -> Harness<'static, SkiffApp> {
    let opts = AppOptions {
        context: Some(context.to_string()),
        view: Some(view.to_string()),
        light,
        namespace: Some(namespace.to_string()),
        ..Default::default()
    };
    harness_opts(opts, [1440.0, 900.0])
}

/// Opens the Metrics tab of the object in the table row called `name`
/// and waits for `loaded` to be on screen.
fn open_metrics(h: &mut Harness<'static, SkiffApp>, name: &str, loaded: &str) {
    settle(h, "the row", |h| has_label(h, name));
    h.query_all_by_label(name).min_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("row").click();
    settle(h, "detail panel", |h| has_label(h, "Metrics"));
    h.get_by_label("Metrics").click();
    let what = format!("metrics to show {loaded:?}");
    settle(h, &what, |h| has_label_containing(h, loaded));
    steps(h, 4);
}

/// Puts the pointer into the plot of the chart titled `title` in the
/// detail panel (the rightmost thing with that label).
fn hover_chart(h: &mut Harness<'static, SkiffApp>, title: &str, across: f32) {
    let rect = h
        .query_all_by_label(title)
        .max_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
        .expect("chart title")
        .rect();
    h.hover_at(egui::pos2(rect.left() + across, rect.bottom() + 80.0));
    steps(h, 3);
}

const LOADED: &str = "one point every";

#[test]
fn pod_metrics_come_from_prometheus_through_the_api_server() {
    let Some(ctx) = test_context() else { return };
    if ctx != "demo" || !demo_has_prometheus() {
        return;
    }
    let pod = steady_pod();
    for light in [false, true] {
        let mut h = in_namespace(&ctx, "pods", "kube-system", light);
        open_metrics(&mut h, &pod, LOADED);
        // The source is named, and every chart of a pod is there.
        assert!(has_label_containing(&h, "Prometheus · monitoring/prometheus-operated"));
        for title in ["CPU", "Memory", "Network", "Restarts"] {
            assert!(has_label(&h, title), "{title} chart");
        }
        assert!(!has_label(&h, "Open in Grafana"), "no Grafana link unless one is configured");
        shot(&mut h, if light { "81_pod_metrics_light" } else { "80_pod_metrics" });

        if !light {
            hover_chart(&mut h, "CPU", 250.0);
            // The readout names the container and the reference lines.
            assert!(has_label_containing(&h, " · kube-proxy ") && has_label_containing(&h, " · request 50m"));
            assert!(has_label_containing(&h, " · limit 200m"));
            shot(&mut h, "82_pod_metrics_hover");
            h.hover_at(egui::pos2(5.0, 5.0));
            steps(&mut h, 2);

            // Another range loads its own data and is remembered.
            // (The range button, not the Age cell of a pod that happens to
            // be 24 hours old: the details panel is the rightmost.)
            h.get_all_by_label("24h")
                .max_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
                .expect("the 24h range button")
                .click();
            settle(&mut h, "the 24h range", |h| has_label_containing(h, "one point every 15 min"));
            shot(&mut h, "83_pod_metrics_24h");
            h.get_by_label("7d").click();
            settle(&mut h, "the 7d range", |h| has_label_containing(h, "one point every 2 h"));
            hover_chart(&mut h, "Memory", 120.0);
            shot(&mut h, "84_pod_metrics_7d");
        }
    }
}

#[test]
fn workload_and_node_metrics() {
    let Some(ctx) = test_context() else { return };
    if ctx != "demo" || !demo_has_prometheus() {
        return;
    }
    // A deployment: one line per pod.
    let mut h = in_namespace(&ctx, "deployments.apps", "payments", false);
    open_metrics(&mut h, "gateway", LOADED);
    hover_chart(&mut h, "Memory", 300.0);
    // (Any of them: the test bed replaces pods all the time.)
    assert!(has_label_containing(&h, " · gateway-"), "the readout lists the pods by their full names");
    shot(&mut h, "85_deployment_metrics");

    // A crash-looping workload: restarts show up. (A tall window, so
    // that all four charts are in the picture.)
    let opts = AppOptions {
        context: Some(ctx.clone()),
        view: Some("deployments.apps".to_string()),
        light: true,
        namespace: Some("payments".to_string()),
        ..Default::default()
    };
    let mut h = harness_opts(opts, [1440.0, 1100.0]);
    open_metrics(&mut h, "fraud-detector", LOADED);
    shot(&mut h, "86_deployment_metrics_crashloop_light");

    // A node: usage against what is allocatable.
    let mut h = harness(&ctx, "nodes", [1440.0, 900.0]);
    open_metrics(&mut h, "demo-worker-1", LOADED);
    assert!(has_label(&h, "Pod network") && !has_label(&h, "Restarts"));
    hover_chart(&mut h, "CPU", 200.0);
    assert!(has_label_containing(&h, " · allocatable 16 cores"));
    shot(&mut h, "87_node_metrics");

    // A kind without metrics has no such tab, and a tab carried over
    // from one that has falls back to the overview.
    assert_eq!(h.state().detail.as_ref().map(|d| d.tabs().len()), Some(4));
    let cm = skiff_core::kinds::builtin("", "configmaps").unwrap().key();
    h.state_mut().open_object(&cm, "kube-system", "kube-root-ca.crt");
    settle(&mut h, "the config map", |h| has_label_containing(h, "DATA"));
    assert!(!has_label(&h, "Metrics"));
}

#[test]
fn firing_alerts_on_the_overview_lead_to_their_objects() {
    let Some(ctx) = test_context() else { return };
    if ctx != "demo" || !demo_has_prometheus() {
        return;
    }
    for light in [false, true] {
        let mut h = harness_with(&ctx, "overview", [1440.0, 900.0], light);
        settle(&mut h, "alerts", |h| has_label_containing(h, "FIRING ALERTS · "));
        // Critical first; pending alerts and the Watchdog are not shown.
        assert!(has_label(&h, "KubeNodeNotReady") && has_label(&h, "KubeDeploymentReplicasMismatch"));
        assert!(!has_label(&h, "Watchdog") && !has_label(&h, "KubeMemoryOvercommit"));
        // The top bar counts what is critical or a warning (not "info").
        assert!(has_label(&h, "6 alerts"), "badge");
        shot(&mut h, if light { "89_overview_alerts_light" } else { "88_overview_alerts" });
    }

    // An alert names its object; the link opens it, and the object's own
    // panel shows the alert. (The alert also carries the `pod` label of
    // kube-state-metrics, which must not win.)
    let mut h = harness(&ctx, "overview", [1440.0, 900.0]);
    settle(&mut h, "alerts", |h| has_label_containing(h, "FIRING ALERTS · "));
    // (The alerts come first on the page; "Needs attention" lists it too.)
    h.query_all_by_label("ledger").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("link").click();
    settle(&mut h, "the deployment", |h| has_label_containing(h, "firing for"));
    assert_eq!(h.state().view_id(), "deployments.apps");
    let detail = h.state().detail.as_ref().expect("detail");
    assert_eq!((&*detail.ns, &*detail.name), ("payments", "ledger"));
    shot(&mut h, "90_alert_on_its_object");

    // A node known to the alert by `instance` only is found too.
    h.get_by_label("6 alerts").click();
    settle(&mut h, "back on the overview", |h| has_label_containing(h, "FIRING ALERTS · "));
    h.query_all_by_label("demo-worker-2").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("link").click();
    settle(&mut h, "the node", |h| has_label(h, "NodeFilesystemAlmostOutOfSpace") && has_label_containing(h, "firing for"));
    assert_eq!(h.state().view_id(), "nodes");

    // With a namespace selected, alerts of other namespaces are left out;
    // cluster-level ones stay.
    let mut h = in_namespace(&ctx, "overview", "payments", false);
    settle(&mut h, "alerts", |h| has_label_containing(h, "FIRING ALERTS · "));
    assert!(has_label(&h, "KubeNodeNotReady") && has_label(&h, "KubeDeploymentReplicasMismatch"));
    assert!(!has_label(&h, "CPUThrottlingHigh") && !has_label(&h, "TargetDown"));
}

#[test]
fn without_prometheus_usage_is_charted_from_metrics_server() {
    if !use_test_kubeconfig() || !kubectl(&["--context", "scale", "get", "--raw", "/readyz"]).contains("ok") {
        eprintln!("skipping: the scale cluster is not running");
        return;
    }
    let mut h = harness("scale", "nodes", [1440.0, 900.0]);
    settle(&mut h, "nodes", |h| h.state().row_at(0).is_some());
    let (node, _) = h.state().row_at(0).expect("a node");
    open_metrics(&mut h, &node, "metrics-server · collected since");
    // Not an error: one quiet line on what Prometheus would add and how
    // to point Skiff at one.
    assert!(has_label_containing(&h, "Longer history, network and restarts need Prometheus."));
    assert!(has_label_containing(&h, "clusters.scale.prometheus"));
    assert!(has_label(&h, "CPU") && has_label(&h, "Memory") && !has_label(&h, "Network"));
    assert!(!has_label(&h, "1h"), "no range to choose without history");
    // A second sample arrives (metrics-server is polled every 15 s).
    let session = h.state().session().unwrap().clone();
    let deadline = Instant::now() + Duration::from_secs(40);
    while session.node_usage_history(&node).cpu.len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        h.step();
    }
    assert!(session.node_usage_history(&node).cpu.len() >= 2, "history grows while the tab is open");
    steps(&mut h, 3);
    shot(&mut h, "91_metrics_without_prometheus");

    // The overview has no alerts section and the top bar no badge.
    let mut h = harness("scale", "overview", [1440.0, 900.0]);
    settle(&mut h, "overview", |h| has_label_containing(h, "NEEDS ATTENTION"));
    steps(&mut h, 20);
    assert!(!has_label_containing(&h, "FIRING ALERTS") && !has_label_containing(&h, "alerts"));
}

#[test]
fn a_prometheus_that_refuses_or_does_not_answer_is_explained() {
    let Some(ctx) = test_context() else { return };
    if ctx != "demo" || !demo_has_prometheus() {
        return;
    }
    let opts = |view: &str| AppOptions {
        context: Some(ctx.clone()),
        view: Some(view.to_string()),
        namespace: Some("kube-system".to_string()),
        ..Default::default()
    };
    let pod = steady_pod();

    // Configured, but something in front of it says no.
    let dir = scratch("prom-denied");
    let json = r#"{"clusters": {"demo": {"prometheus": {"namespace": "monitoring", "service": "prometheus-operated", "path_prefix": "/deny"}}}}"#;
    let mut h = harness_config(opts("pods"), &dir, json, [1440.0, 900.0]);
    assert!(h.state().notices().is_empty(), "the config is valid: {:?}", h.state().notices().len());
    open_metrics(&mut h, &pod, "No access to Prometheus");
    assert!(has_label_containing(&h, "services/proxy"), "says which permission is missing");
    // Usage from metrics-server is still charted.
    settle(&mut h, "the metrics-server charts", |h| has_label_containing(h, "metrics-server · collected since"));
    shot(&mut h, "92_prometheus_forbidden");
    // Looking again asks again (and is refused again).
    h.get_by_label("Look for Prometheus again").click();
    settle(&mut h, "the second refusal", |h| has_label_containing(h, "No access to Prometheus"));

    // Configured, but there is no such Service.
    let dir = scratch("prom-missing");
    let json = r#"{"clusters": {"demo": {"prometheus": {"namespace": "monitoring", "service": "prometheus-elsewhere"}}}}"#;
    let mut h = harness_config(opts("overview"), &dir, json, [1440.0, 900.0]);
    settle(&mut h, "the explanation", |h| has_label_containing(h, "Prometheus is not answering"));
    assert!(has_label_containing(&h, "monitoring/prometheus-elsewhere:9090"), "names the Service that was tried");
    assert!(!has_label_containing(&h, "alerts"), "no badge without alerts");
    shot(&mut h, "93_overview_prometheus_unreachable");
}

#[test]
fn grafana_link_opens_explore_for_the_object() {
    let Some(ctx) = test_context() else { return };
    if ctx != "demo" || !demo_has_prometheus() {
        return;
    }
    let dir = scratch("grafana");
    let json = r#"{"clusters": {"demo": {"grafana_url": "https://grafana.example.com/"}}}"#;
    let opts = AppOptions {
        context: Some(ctx.clone()),
        view: Some("pods".to_string()),
        namespace: Some("kube-system".to_string()),
        ..Default::default()
    };
    let mut h = harness_config(opts, &dir, json, [1440.0, 900.0]);
    let pod = steady_pod();
    open_metrics(&mut h, &pod, LOADED);
    shot(&mut h, "94_pod_metrics_grafana");
    h.get_by_label_contains("Open in Grafana").click();
    h.step();
    let opened: Vec<String> = h
        .output()
        .platform_output
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(opened[0].starts_with("https://grafana.example.com/explore?schemaVersion=1&panes=%7B"), "{}", opened[0]);
    assert!(opened[0].contains(&pod) && opened[0].contains("container_cpu_usage_seconds_total"), "{}", opened[0]);
    assert!(opened[0].contains("now-1h"));
}
