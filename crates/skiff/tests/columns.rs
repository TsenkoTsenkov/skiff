//! Column resizing, column filters, find shortcuts and the settings page,
//! driven through the real app against the dev clusters from
//! `hack/cluster-up.sh` (skipped when they are not there).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers, Pos2};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp};

type H = Harness<'static, SkiffApp>;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn test_context() -> Option<String> {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *OK.get_or_init(|| {
        let main = std::env::var("SKIFF_TEST_KUBECONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| workspace().join("hack/kubeconfig"));
        if !main.exists() {
            eprintln!("skipping: no kubeconfig at {}", main.display());
            return false;
        }
        // SAFETY: set once, before any test thread reads it.
        unsafe { std::env::set_var("KUBECONFIG", main) };
        true
    });
    ok.then(|| std::env::var("SKIFF_TEST_CONTEXT").unwrap_or_else(|_| "demo".to_string()))
}

fn harness(context: &str, view: &str, size: [f32; 2]) -> H {
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light: false, namespace: None };
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts))
}

fn settle(h: &mut H, what: &str, mut done: impl FnMut(&H) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        h.step();
        if done(h) {
            for _ in 0..4 {
                std::thread::sleep(Duration::from_millis(20));
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

fn shot(h: &mut H, name: &str) {
    let dir = workspace().join("target/shots");
    std::fs::create_dir_all(&dir).unwrap();
    if let Ok(img) = h.render() {
        let _ = img.save(dir.join(format!("{name}.png")));
    }
}

fn has_label(h: &H, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

/// Rect of a column header (the header row is the topmost occurrence of the
/// title under the toolbar).
fn header(h: &H, title: &str) -> Option<egui::Rect> {
    // (The sorted column's label also holds the arrow icon.)
    let plain = |l: &str| l.chars().filter(|c| c.is_ascii()).collect::<String>().trim().to_string();
    h.root()
        .query_all(egui_kittest::kittest::by().predicate(move |n: &egui_kittest::kittest::AccessKitNode<'_>| {
            n.label().is_some_and(|l| plain(&l) == title)
        }))
        .map(|n| n.rect())
        .filter(|r| r.top() > 60.0 && r.top() < 130.0)
        .min_by(|a, b| a.top().partial_cmp(&b.top()).unwrap())
}

/// Header titles from left to right.
fn headers(h: &H, candidates: &[&str]) -> Vec<(String, egui::Rect)> {
    let mut v: Vec<(String, egui::Rect)> =
        candidates.iter().filter_map(|t| header(h, t).map(|r| (t.to_string(), r))).collect();
    v.sort_by(|a, b| a.1.left().partial_cmp(&b.1.left()).unwrap());
    v
}

const POD_COLUMNS: [&str; 8] = ["Name", "Namespace", "Ready", "Status", "Restarts", "IP", "Node", "Age"];

/// Drags from `from` to `to` over a few frames, like a hand would.
fn drag(h: &mut H, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    for i in 1..=6 {
        let t = i as f32 / 6.0;
        h.hover_at(from + (to - from) * t);
        h.step();
    }
    h.drop_at(to);
    h.step();
    h.step();
}

#[test]
fn columns_resize_by_dragging_and_stay_inside_the_window() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods", |h| h.state().table_stats().1 && header(h, "Status").is_some());
    shot(&mut h, "col_01_initial");
    let before = headers(&h, &POD_COLUMNS);
    let titles: Vec<&str> = before.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(titles, POD_COLUMNS, "all pod columns fit at 1440 wide");
    for (t, r) in &before {
        assert!(r.right() <= 1440.0, "{t} ends at {} beyond the window", r.right());
    }

    // Widen Namespace by 80 by dragging its right edge (just left of the
    // next header).
    let status = header(&h, "Status").unwrap();
    let restarts_before = header(&h, "Restarts").unwrap();
    let ns = header(&h, "Namespace").unwrap();
    let edge = Pos2::new(header(&h, "Ready").unwrap().left() - 4.0, ns.center().y);
    drag(&mut h, edge, edge + egui::vec2(80.0, 0.0));
    shot(&mut h, "col_02_widened");
    // The boundary follows the pointer: Ready starts 80 further right,
    // Namespace stays where it was, and nothing else moves.
    let ready_after = header(&h, "Ready").unwrap();
    let edge_moved = ready_after.left() - (edge.x + 4.0);
    assert!((edge_moved - 80.0).abs() < 4.0, "the boundary moved by {edge_moved} (expected 80)");
    assert!((header(&h, "Namespace").unwrap().left() - ns.left()).abs() < 1.0, "Namespace itself stays put");
    // Ready gave what it could down to its minimum, then Status: the
    // columns after those stay where they were.
    let _ = status;
    let restarts = header(&h, "Restarts").unwrap();
    assert!((restarts.left() - restarts_before.left()).abs() < 1.0, "Restarts stays put");
    let after = headers(&h, &POD_COLUMNS);
    assert_eq!(after.len(), POD_COLUMNS.len(), "no column was pushed out");
    assert!(after.iter().all(|(_, r)| r.right() <= 1440.0));

    // The width stays when the detail panel opens and closes.
    h.key_press(Key::ArrowDown);
    h.step();
    h.key_press(Key::Enter);
    settle(&mut h, "detail panel", |h| has_label(h, "CONTAINERS"));
    shot(&mut h, "col_03_detail_open");
    let detail_left = h
        .query_all_by_label("Close (Esc)")
        .map(|n| n.rect().left())
        .fold(f32::MAX, f32::min)
        - 400.0;
    for (t, r) in headers(&h, &POD_COLUMNS) {
        assert!(r.right() < detail_left + 400.0 - 20.0, "{t} runs under the detail panel");
    }
    h.key_press(Key::Escape);
    settle(&mut h, "detail closed", |h| !has_label(h, "CONTAINERS"));
    let ready_now = header(&h, "Ready").unwrap().left() - header(&h, "Namespace").unwrap().left();
    let ready_then = ready_after.left() - header(&h, "Namespace").unwrap().left();
    assert!((ready_now - ready_then).abs() < 2.0, "Namespace kept its dragged width ({ready_now} vs {ready_then})");

    // A huge drag is capped: everything still fits.
    let edge = Pos2::new(header(&h, "Status").unwrap().left() - 4.0, ns.center().y);
    drag(&mut h, edge, edge + egui::vec2(2000.0, 0.0));
    shot(&mut h, "col_04_huge_drag");
    let after = headers(&h, &POD_COLUMNS);
    assert_eq!(after.len(), POD_COLUMNS.len(), "a drag never pushes columns out: {after:?}");
    assert!(after.iter().all(|(_, r)| r.right() <= 1440.0 + 0.5), "{after:?}");

    // The name column can be made narrower too (it used to have no handle).
    let name_edge = Pos2::new(header(&h, "Namespace").unwrap().left() - 4.0, ns.center().y);
    let name_before = header(&h, "Namespace").unwrap().left();
    drag(&mut h, name_edge, name_edge - egui::vec2(60.0, 0.0));
    let moved = header(&h, "Namespace").unwrap().left() - name_before;
    assert!((moved + 60.0).abs() < 6.0, "Name got narrower by {}", -moved);

    // Reset columns puts the defaults back.
    h.get_by_label("Choose columns").click();
    settle(&mut h, "column menu", |h| h.query_all_by_label_contains("Reset columns").next().is_some());
    h.get_by_label_contains("Reset columns").click();
    h.step();
    h.key_press(Key::Escape);
    h.step();
    h.step();
    let reset = headers(&h, &POD_COLUMNS);
    for ((t, a), (_, b)) in before.iter().zip(reset.iter()) {
        assert!((a.left() - b.left()).abs() < 2.0, "{t} back at {} (was {})", b.left(), a.left());
    }
}

#[test]
fn narrow_window_keeps_the_table_on_screen() {
    let Some(ctx) = test_context() else { return };
    for width in [700.0, 900.0, 1100.0] {
        let mut h = harness(&ctx, "pods", [width, 700.0]);
        settle(&mut h, "pods", |h| h.state().table_stats().1 && header(h, "Name").is_some());
        shot(&mut h, &format!("col_narrow_{width}"));
        for (t, r) in headers(&h, &POD_COLUMNS) {
            assert!(r.right() <= width + 0.5, "{width}: {t} ends at {}", r.right());
        }
        assert!(header(&h, "Name").is_some(), "{width}: the name column stays");
    }
}

#[test]
fn pods_on_a_node() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "nodes", [1440.0, 900.0]);
    settle(&mut h, "nodes", |h| h.state().table_stats().1 && h.state().table_stats().0 > 2);
    let nodes: Vec<String> = (0..h.state().table_stats().0).filter_map(|i| h.state().row_at(i).map(|r| r.0)).collect();
    let node = nodes.iter().find(|n| n.contains("worker-1")).cloned().unwrap_or_else(|| nodes[0].clone());
    h.get_all_by_label(&node).next().expect("node row").click_secondary();
    settle(&mut h, "row menu", |h| h.query_all_by_label_contains("Show pods").next().is_some());
    h.get_by_label_contains("Show pods").click();
    settle(&mut h, "pods of the node", |h| h.state().view_id() == "pods" && h.state().table_stats().1);
    shot(&mut h, "node_01_pods");
    let (shown, _) = h.state().table_stats();
    assert!(shown > 0, "the node runs pods");
    assert_eq!(h.state().table_filter(), format!("node:={node}"), "the filter says what it shows");
    // Only that node's name appears in the Node column (an exact match:
    // demo-worker-1 must not bring in demo-worker-10 and the like).
    for other in nodes.iter().filter(|n| **n != node) {
        assert!(!has_label(&h, other), "pods of {other} are shown");
    }

    // Typing a column filter by hand works the same way.
    h.state_mut().go(skiff::View::Resource(skiff_core::kinds::nodes()));
    h.step();
    h.state_mut().go(skiff::View::Resource(skiff_core::kinds::pods()));
    h.step();
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(egui::Event::Text("status:running !ns:kube".into()));
    settle(&mut h, "column filter", |h| h.state().table_filter() == "status:running !ns:kube");
    shot(&mut h, "node_02_column_filter");
    assert!(h.state().table_stats().0 > 0);
    assert!(!h.query_all_by_label_contains("CrashLoop").any(|_| true), "only running pods");
}

#[test]
fn cmd_f_finds_in_logs_when_working_there_and_in_the_table_otherwise() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods", |h| h.state().table_stats().1 && h.state().table_stats().0 > 3);
    let total = h.state().table_stats().0;
    let (name, ns) = h.state().row_at(0).unwrap();
    let ctx_egui = h.ctx.clone();
    let pods = skiff_core::kinds::pods();
    h.state_mut().run_row_action(&ctx_egui, skiff::table_view::RowAction::Logs, &pods, &ns, &name);
    settle(&mut h, "logs tab", |h| has_label(h, "Jump to latest"));

    // The dock was just opened: Cmd+F goes to its find box.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.step();
    h.event(egui::Event::Text("zzqqxx".into()));
    settle(&mut h, "log find", |h| has_label(h, "no matches"));
    assert_eq!(h.state().table_stats().0, total, "the table filter was left alone");
    shot(&mut h, "find_01_logs");

    // After a click in the table, Cmd+F is for the table.
    h.key_press(Key::Escape);
    h.step();
    let row = h.get_all_by_label(&name).next().expect("row").rect();
    h.hover_at(Pos2::new(row.left() + 5.0, row.top() + 5.0));
    h.step();
    h.drag_at(Pos2::new(row.left() + 5.0, row.top() + 5.0));
    h.step();
    h.drop_at(Pos2::new(row.left() + 5.0, row.top() + 5.0));
    h.step();
    h.key_press(Key::Escape);
    h.step();
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.step();
    h.step();
    h.event(egui::Event::Text("zzqqxx".into()));
    settle(&mut h, "table find", |h| h.state().table_stats().0 == 0);
    shot(&mut h, "find_02_table");
}

#[test]
fn settings_page_lists_files_and_clusters() {
    let Some(ctx) = test_context() else { return };
    let mut h = harness(&ctx, "pods", [1440.0, 900.0]);
    settle(&mut h, "pods", |h| h.state().table_stats().1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    settle(&mut h, "settings", |h| has_label(h, "Kubeconfig files"));
    shot(&mut h, "settings_01");
    assert_eq!(h.state().view_id(), "settings");
    assert!(has_label(&h, "Clusters"));
    let contexts: Vec<String> = h.state().contexts.items.iter().map(|c| c.name.clone()).collect();
    assert!(contexts.len() >= 2, "the dev kubeconfig has demo and scale");
    let other = contexts.iter().find(|c| **c != ctx).unwrap().clone();

    // Hiding a cluster takes it out of the menus, and back.
    let hide = "Hide from the cluster menu (the kubeconfig is not changed)";
    let buttons: Vec<_> = h.query_all_by_label(hide).collect();
    assert_eq!(buttons.len(), contexts.len());
    let index = contexts.iter().position(|c| *c == other).unwrap();
    let mut nodes: Vec<_> = h.query_all_by_label(hide).map(|n| n.rect().top()).collect();
    nodes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let y = nodes[index];
    h.query_all_by_label(hide).find(|n| n.rect().top() == y).unwrap().click();
    h.step();
    h.step();
    assert!(!h.state().contexts.items.iter().any(|c| c.name == other), "{other} hidden");
    shot(&mut h, "settings_02_hidden");
    h.get_by_label_contains("Show hidden clusters").click();
    h.step();
    h.get_by_label("Show in the cluster menu again").click();
    h.step();
    h.step();
    assert!(h.state().contexts.items.iter().any(|c| c.name == other), "{other} back");

    // Reload reads the files again and keeps the connection.
    h.get_by_label_contains("Reload kubeconfig").click();
    h.step();
    assert_eq!(h.state().active.as_deref(), Some(ctx.as_str()));
    settle(&mut h, "toast", |h| h.query_all_by_label_contains("Kubeconfig read").next().is_some());
}
