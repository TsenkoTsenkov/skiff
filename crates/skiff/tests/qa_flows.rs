//! Exploratory end-to-end flows written during QA. They exercise far more of
//! the UI than `ui.rs` (every sidebar view, sorting, filters, mutations
//! verified with kubectl, logs, port-forwards, palette, layouts), but they
//! are slow, change cluster state in the `qa-skiff` namespace, print
//! `FINDING:` lines rather than asserting everything, and a few depend on
//! fixtures that drift (hard-coded ReplicaSet hashes, rows being on screen).
//! They are therefore opt-in:
//!
//!   KUBECONFIG=hack/kubeconfig QA_SHOTS=/tmp/skiff-qa-shots \
//!     cargo test -p skiff --test qa_flows -- --ignored --test-threads=1 --nocapture
//!
//! QA flows: drive the real app headlessly against the live kwok clusters.
//!
//! Run:  KUBECONFIG=hack/kubeconfig cargo test -p skiff --test qa_flows -- --nocapture --test-threads=1
//! Screenshots go to $QA_SHOTS (default: target/qa_shots). Lines starting with
//! "FINDING" in the output are things that look wrong.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT, Queryable as _};
use egui_kittest::Harness;
use skiff::{AppOptions, SkiffApp, View};

pub type H = Harness<'static, SkiffApp>;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn kubeconfig() -> PathBuf {
    std::env::var("SKIFF_TEST_KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace().join("hack/kubeconfig"))
}

pub fn setup() -> bool {
    let kc = kubeconfig();
    if !kc.exists() {
        eprintln!("skipping: no kubeconfig");
        return false;
    }
    unsafe { std::env::set_var("KUBECONFIG", &kc) };
    true
}

pub fn harness_sized(context: &str, view: &str, size: [f32; 2], light: bool) -> H {
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light, namespace: None, ..Default::default() };
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts))
}

pub fn harness(context: &str, view: &str) -> H {
    harness_sized(context, view, [1440.0, 900.0], false)
}

/// Runs frames until `done` holds. Returns false on timeout.
pub fn settle_for(h: &mut H, secs: u64, mut done: impl FnMut(&H) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        h.step();
        if done(h) {
            for _ in 0..4 {
                std::thread::sleep(Duration::from_millis(30));
                h.step();
            }
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn settle(h: &mut H, what: &str, done: impl FnMut(&H) -> bool) {
    if !settle_for(h, 10, done) {
        shot(h, &format!("timeout_{}", what.replace(' ', "_")));
        panic!("timed out waiting for: {what}");
    }
}

pub fn frames(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn shots_dir() -> PathBuf {
    let d = std::env::var("QA_SHOTS").map(PathBuf::from).unwrap_or_else(|_| workspace().join("target/qa_shots"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn shot(h: &mut H, name: &str) {
    let img = h.render().expect("render");
    img.save(shots_dir().join(format!("{name}.png"))).unwrap();
}

pub fn has(h: &H, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

pub fn has_c(h: &H, label: &str) -> bool {
    h.query_all_by_label_contains(label).next().is_some()
}

pub fn count_c(h: &H, label: &str) -> usize {
    h.query_all_by_label_contains(label).count()
}

/// All labelled nodes in tree order: (role, label).
pub fn labels(h: &H) -> Vec<(Role, String)> {
    let mut out = Vec::new();
    for n in h.root().children_recursive() {
        let an = n.accesskit_node();
        let l = an.label().or_else(|| an.value());
        if let Some(l) = l {
            if !l.is_empty() {
                out.push((an.role(), l.to_string()));
            }
        }
    }
    out
}

pub fn dump(h: &H) {
    for (r, l) in labels(h) {
        eprintln!("  [{r:?}] {l}");
    }
}

/// Labels of nodes whose role is `role`.
pub fn labels_of(h: &H, role: Role) -> Vec<String> {
    labels(h).into_iter().filter(|(r, _)| *r == role).map(|(_, l)| l).collect()
}

pub fn finding(msg: impl AsRef<str>) {
    eprintln!("FINDING: {}", msg.as_ref());
}

pub fn kubectl(ctx: &str, args: &[&str]) -> String {
    let out = Command::new("kubectl")
        .env("KUBECONFIG", kubeconfig())
        .arg("--context")
        .arg(ctx)
        .args(args)
        .output()
        .expect("kubectl");
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

pub fn go_view(h: &mut H, id: &str) {
    let key = skiff_core::BUILTINS
        .iter()
        .find(|b| b.key().id() == id || b.plural == id)
        .map(|b| b.key())
        .unwrap_or_else(|| panic!("no builtin {id}"));
    h.state_mut().go(View::Resource(key));
    h.step();
}

/// Waits until the current table is no longer loading.
pub fn wait_table(h: &mut H) -> bool {
    settle_for(h, 12, |h| !has(h, "Loading…") && (has_c(h, "Name") || has(h, "Nothing here") || has(h, "Couldn't load this view")))
}

pub fn pods_ready(h: &H) -> bool {
    has(h, "alertmanager-0")
}

// ---------------------------------------------------------------------------
// 1. every sidebar view
// ---------------------------------------------------------------------------

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t01_all_views() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    for b in skiff_core::BUILTINS.iter() {
        let id = b.key().id();
        go_view(&mut h, &id);
        let ok = wait_table(&mut h);
        let state = if has(&h, "Couldn't load this view") {
            "FAILED"
        } else if has(&h, "Nothing here") {
            "empty"
        } else if !ok {
            "TIMEOUT"
        } else {
            "ok"
        };
        let snap = h.state().session().unwrap().table(&b.key()).snapshot();
        let cols: Vec<String> = snap.columns.iter().filter(|c| c.visible).map(|c| c.title.to_string()).collect();
        eprintln!("VIEW {id:50} {state:8} rows={:4} cols={:?}", snap.rows.len(), cols);
        if state == "FAILED" || state == "TIMEOUT" {
            finding(format!("view {id} state {state}: {:?}", snap.error));
        }
        shot(&mut h, &format!("view_{}", id.replace('.', "_")));
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t00_dump() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    dump(&h);
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t00_load_time() {
    if !setup() {
        return;
    }
    for i in 0..6 {
        let t = Instant::now();
        let mut h = harness("demo", "pods");
        let ok = settle_for(&mut h, 20, pods_ready);
        eprintln!("load #{i}: ok={ok} in {:?}", t.elapsed());
        if !ok {
            let s = h.state().session().unwrap().clone();
            let snap = s.table(&skiff_core::kinds::pods()).snapshot();
            eprintln!("  status={:?} state={:?} err={:?} rows={}", s.status(), snap.state, snap.error, snap.rows.len());
            shot(&mut h, "stall_pods");
            h.get_by_label("Reload from the API server").click();
            let ok2 = settle_for(&mut h, 20, pods_ready);
            eprintln!("  after reload click: ok={ok2}");
        }
    }
}

/// Core-level stress: connect, then request a table after `delay_ms`.
/// Any iteration where the table never leaves Connecting despite a Connected
/// session is a lost-wakeup bug.
#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t00_race_connect_then_table() {
    if !setup() {
        return;
    }
    let mut stalls = Vec::new();
    for delay_ms in [0u64, 1, 2, 5, 10, 20, 50, 100, 300, 1000].iter().cycle().take(60) {
        let core = skiff_core::Core::new(skiff_core::Notifier::new(|| {})).unwrap();
        let session = core.connect("demo");
        std::thread::sleep(Duration::from_millis(*delay_ms));
        let store = session.table(&skiff_core::kinds::pods());
        let t = Instant::now();
        let mut ok = false;
        while t.elapsed() < Duration::from_secs(4) {
            if store.snapshot().rows.len() > 0 {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ok {
            let st = store.snapshot().state;
            eprintln!("STALL delay={delay_ms}ms status={:?} table={:?}", session.status(), st);
            stalls.push(*delay_ms);
        }
    }
    eprintln!("stalls: {stalls:?}");
    if !stalls.is_empty() {
        finding(format!("table never loads when first requested {stalls:?} ms after connect()"));
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t00_ui_stall_diag() {
    if !setup() {
        return;
    }
    let mut stalls = 0;
    for i in 0..40 {
        let mut h = harness("demo", "pods");
        let ok = settle_for(&mut h, 5, pods_ready);
        if !ok {
            stalls += 1;
            let s = h.state().session().unwrap().clone();
            let snap = s.table(&skiff_core::kinds::pods()).snapshot();
            eprintln!("STALL iter {i}: status={:?} state={:?} watches={}", s.status(), snap.state, s.active_watches());
            // Does a fresh table on the same session load?
            let t2 = s.table_in(&skiff_core::kinds::pods(), Some("monitoring"));
            std::thread::sleep(Duration::from_secs(2));
            eprintln!("   scoped table rows={} state={:?}", t2.snapshot().rows.len(), t2.snapshot().state);
            let jobclient = s.get_object(&skiff_core::kinds::pods(), "monitoring", "alertmanager-0");
            std::thread::sleep(Duration::from_secs(2));
            eprintln!("   get_object finished={:?}", jobclient.take().map(|r| r.is_ok()));
            shot(&mut h, "stall_pods");
        }
    }
    eprintln!("UI stalls: {stalls}/40");
}

// ---------------------------------------------------------------------------
// table helpers
// ---------------------------------------------------------------------------

/// Header titles (trimmed) and the visible rows, from the accessibility tree.
pub fn table_snapshot(h: &H) -> (Vec<String>, Vec<Vec<String>>) {
    let all = labels(h);
    let start = all.iter().position(|(_, l)| l == "Choose columns").map(|i| i + 1).unwrap_or(0);
    let mut headers = Vec::new();
    let mut i = start;
    while i < all.len() && all[i].0 == Role::Button {
        headers.push(clean(&all[i].1));
        i += 1;
    }
    let n = headers.len().max(1);
    let cells: Vec<String> = all[i..].iter().filter(|(r, _)| *r == Role::Label).map(|(_, l)| l.clone()).collect();
    let rows = cells.chunks(n).filter(|c| c.len() == n).map(|c| c.to_vec()).collect();
    (headers, rows)
}

/// Strips icon-font glyphs (private use area) and whitespace.
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !('\u{e000}'..='\u{f8ff}').contains(c)).collect::<String>().trim().to_string()
}

pub fn click_button(h: &mut H, text: &str) {
    let node = h
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().map(|l| clean(&l).as_str() == text).unwrap_or(false))
        .unwrap_or_else(|| panic!("no button {text:?}"));
    node.click();
    h.step();
    h.step();
}

pub fn parse_age(s: &str) -> Option<f64> {
    // 1d2h, 40m, 8m17s, 15s
    let mut total = 0.0;
    let mut num = String::new();
    let mut any = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            let mult = match ch {
                's' => 1.0,
                'm' => 60.0,
                'h' => 3600.0,
                'd' => 86400.0,
                'y' => 31_536_000.0,
                _ => return None,
            };
            let v: f64 = num.parse().ok()?;
            total += v * mult;
            num.clear();
            any = true;
        }
    }
    if !num.is_empty() || !any {
        return None;
    }
    Some(total)
}

#[derive(Debug, PartialEq, PartialOrd, Clone)]
pub enum K {
    Num(f64),
    Txt(String),
}

pub fn sort_key(s: &str, age: bool) -> K {
    let t = s.trim();
    if t == "—" || t.is_empty() {
        return K::Txt(String::new());
    }
    if age {
        if let Some(a) = parse_age(t) {
            return K::Num(a);
        }
    }
    let lead: String = t.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if !lead.is_empty() && lead.len() == t.chars().count() {
        return K::Num(lead.parse().unwrap_or(0.0));
    }
    K::Txt(t.to_lowercase())
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t02_sorting() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    for view in ["pods", "nodes", "deployments.apps", "services", "events", "jobs.batch", "persistentvolumeclaims"] {
        go_view(&mut h, view);
        wait_table(&mut h);
        frames(&mut h, 3);
        let (headers, _) = table_snapshot(&h);
        eprintln!("== {view}: headers {headers:?}");
        for hd in headers.clone() {
            // first click: ascending (or toggle if already active)
            let mut results = Vec::new();
            if !table_snapshot(&h).0.contains(&hd) {
                eprintln!("   {hd:16} (header not present any more; columns changed)");
                continue;
            }
            for click in 0..2 {
                click_button(&mut h, &hd);
                frames(&mut h, 6);
                let (hs, rows) = table_snapshot(&h);
                let idx = hs.iter().position(|x| x == &hd);
                let Some(idx) = idx else { finding(format!("{view}: header {hd} vanished after click")); break };
                let age = matches!(hd.as_str(), "Age" | "Last Seen" | "Duration" | "Last Schedule");
                let keys: Vec<K> = rows.iter().map(|r| sort_key(&r[idx], age)).collect();
                let asc = keys.windows(2).all(|w| w[0].partial_cmp(&w[1]) != Some(std::cmp::Ordering::Greater));
                let desc = keys.windows(2).all(|w| w[0].partial_cmp(&w[1]) != Some(std::cmp::Ordering::Less));
                let dir = if asc && desc { "const" } else if asc { "asc" } else if desc { "desc" } else { "UNSORTED" };
                results.push(dir);
                if click == 1 || dir == "UNSORTED" {
                    let sample: Vec<&str> = rows.iter().take(8).map(|r| r[idx].as_str()).collect();
                    if dir == "UNSORTED" {
                        eprintln!("   {hd:16} click{click}: {dir}  sample={sample:?}");
                    }
                }
            }
            eprintln!("   {hd:16} -> {results:?}");
        }
        shot(&mut h, &format!("sort_{}", view.replace('.', "_")));
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t02_sort_keys() {
    use skiff_core::cells::sort_key;
    for t in ["10.0.0.161", "10.0.0.93", "192.168.1.5", "1.50", "150m", "12Gi", "3 (5m ago)", "31 (55s ago)", "1/1", "0/1", "5d3h", "2m17s", "10.0.0.1,10.0.0.2", "3.7.1", "443/TCP", "80, 443", "1.2.3"] {
        eprintln!("sort_key({t:?}) = {}", sort_key(t));
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t02_ip_sort() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    let snap = h.state().session().unwrap().table(&skiff_core::kinds::pods()).snapshot();
    let ci = snap.columns.iter().position(|c| c.title.as_ref() == "IP").unwrap();
    eprintln!("IP col: {:?} kind={:?} source={:?}", snap.columns[ci].title, snap.columns[ci].kind, snap.columns[ci].source);
    for click in 0..2 {
        click_button(&mut h, "IP");
        frames(&mut h, 3);
        let (hs, rows) = table_snapshot(&h);
        let idx = hs.iter().position(|x| x == "IP").unwrap();
        let ips: Vec<&str> = rows.iter().map(|r| r[idx].as_str()).collect();
        eprintln!("click {click}: headers={hs:?}\n   IPs={ips:?}");
        shot(&mut h, &format!("ipsort_{click}"));
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t02_headers_after_click() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    for hd in ["Name", "Namespace", "Ready", "Status", "Restarts", "IP", "Node", "Node", "Controlled By", "Age"] {
        let (hs, _) = table_snapshot(&h);
        eprintln!("before click {hd}: {hs:?}");
        if !hs.iter().any(|x| x == hd) {
            shot(&mut h, "missing_header");
            break;
        }
        click_button(&mut h, hd);
        frames(&mut h, 6);
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t02_nodes_cpu_and_job_duration() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "nodes");
    settle(&mut h, "nodes", |h| has_c(h, "demo-worker-1"));
    frames(&mut h, 10);
    for col in ["CPU", "Memory"] {
        for click in 0..2 {
            click_button(&mut h, col);
            frames(&mut h, 6);
            let (hs, rows) = table_snapshot(&h);
            let ci = hs.iter().position(|x| x == col).unwrap();
            let shown: Vec<String> = rows.iter().map(|r| format!("{}={}", r[0], r[ci])).collect();
            eprintln!("nodes sort {col} click{click}: {shown:?}");
        }
    }
    shot(&mut h, "nodes_sorted_cpu");
    go_view(&mut h, "jobs.batch");
    wait_table(&mut h);
    frames(&mut h, 4);
    let snap = h.state().session().unwrap().table(&skiff_core::BUILTINS.iter().find(|b| b.plural == "jobs").unwrap().key()).snapshot();
    let di = snap.columns.iter().position(|c| c.title.as_ref() == "Duration").unwrap();
    for r in snap.rows.iter() {
        let c = &r.cells[match snap.columns[di].source { skiff_core::ColSource::Cell(i) => i, _ => 0 }];
        eprintln!("job {} text={:?} key={}", r.name, c.text, c.key);
    }
}

// ---------------------------------------------------------------------------
// filter / namespaces / column chooser
// ---------------------------------------------------------------------------

/// Types into the table filter box via the `/` shortcut.
pub fn set_filter(h: &mut H, text: &str) {
    h.key_press(egui::Key::Slash);
    h.step();
    h.step();
    let n = h.get_all_by_role(Role::TextInput).next().expect("filter input");
    n.focus();
    h.step();
    h.get_all_by_role(Role::TextInput).next().unwrap().type_text(text);
    frames(h, 6);
}

pub fn count_pill(h: &H) -> String {
    // The label right after the view title.
    let all = labels(h);
    let pos = all.iter().position(|(r, l)| *r == Role::Label && matches!(l.as_str(), "Pods" | "Nodes" | "Deployments" | "Services" | "Events" | "ConfigMaps" | "Secrets" | "Namespaces" | "Jobs"));
    pos.and_then(|p| all.get(p + 1)).map(|x| x.1.clone()).unwrap_or_default()
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t03_filter() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    eprintln!("pill before: {}", count_pill(&h));
    let (_, rows) = table_snapshot(&h);
    let total = h.state().session().unwrap().table(&skiff_core::kinds::pods()).snapshot().rows.len();
    eprintln!("total rows {total}, visible {}", rows.len());

    // single word
    set_filter(&mut h, "crash");
    let (hs, rows) = table_snapshot(&h);
    eprintln!("filter 'crash': pill={} rows={} first={:?}", count_pill(&h), rows.len(), rows.first());
    let kc = kubectl("demo", &["get", "pods", "-A", "--no-headers"]);
    let expect = kc.lines().filter(|l| l.to_lowercase().contains("crash")).count();
    eprintln!("  kubectl lines containing crash: {expect}");
    shot(&mut h, "filter_crash");
    // multi word
    h.get_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("payments gateway");
    frames(&mut h, 6);
    let (_, rows) = table_snapshot(&h);
    eprintln!("filter 'payments gateway': pill={} rows={}", count_pill(&h), rows.len());
    let bad: Vec<_> = rows.iter().filter(|r| !(r.join(" ").to_lowercase().contains("payments") && r.join(" ").to_lowercase().contains("gateway"))).collect();
    if !bad.is_empty() {
        finding(format!("multi-word filter returned non-matching rows {bad:?}"));
    }
    // label text
    h.get_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("app=web");
    frames(&mut h, 6);
    let (_, rows) = table_snapshot(&h);
    eprintln!("filter 'app=web': pill={} rows={}", count_pill(&h), rows.len());
    let kw = kubectl("demo", &["get", "pods", "-A", "-l", "app=web", "--no-headers"]);
    eprintln!("  kubectl -l app=web: {} pods", kw.lines().count());
    shot(&mut h, "filter_label");
    // no matches
    h.get_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("zzzznomatch");
    frames(&mut h, 6);
    eprintln!("no match: nothing-here={} pill={}", has(&h, "Nothing here"), count_pill(&h));
    shot(&mut h, "filter_nomatch");
    // can still recover
    h.get_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("");
    frames(&mut h, 6);
    let (_, rows) = table_snapshot(&h);
    eprintln!("cleared: rows={} pill={}", rows.len(), count_pill(&h));
    // namespace column text filter e.g. 'incidents' + status
    h.get_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("incidents Pending");
    frames(&mut h, 6);
    let (_, rows) = table_snapshot(&h);
    eprintln!("'incidents Pending': rows={} sample={:?}", rows.len(), rows.first());
    // Esc behaviour
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    eprintln!("after Esc (1): filter value={:?}", h.get_all_by_role(Role::TextInput).next().and_then(|n| n.value()));
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    eprintln!("after Esc (2): filter value={:?}", h.get_all_by_role(Role::TextInput).next().and_then(|n| n.value()));
    let _ = hs;
}

pub fn click_checkbox(h: &mut H, label: &str) {
    let n = h
        .query_all_by_role(Role::CheckBox)
        .find(|n| n.accesskit_node().label().map(|l| l == label).unwrap_or(false))
        .unwrap_or_else(|| panic!("no checkbox {label}"));
    n.click();
    frames(h, 4);
}

pub fn open_ns_picker(h: &mut H) {
    let n = h
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().map(|l| clean(&l)).map_or(false, |l| l == "All namespaces" || l.ends_with("namespaces") || h_is_ns(&l)))
        .expect("ns picker button");
    n.click();
    frames(h, 4);
}

/// The namespace picker button: the one with the folder glyph (U+E2C7).
pub fn ns_button(h: &H) -> egui_kittest::Node<'_> {
    h.query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().map_or(false, |l| l.contains('\u{e2c7}')))
        .expect("ns picker button")
}

fn h_is_ns(l: &str) -> bool {
    !l.is_empty() && !l.contains("Search") && !l.contains("Ctrl")
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t04_namespace_picker() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    // preload the namespaces table (the picker is tiny otherwise, see t04_ns_popup_size)
    go_view(&mut h, "namespaces");
    settle(&mut h, "namespaces", |h| has(h, "shop"));
    go_view(&mut h, "pods");
    settle(&mut h, "pods", pods_ready);
    let n_shop = kubectl("demo", &["get", "pods", "-n", "shop", "--no-headers"]).lines().count();
    let n_pay = kubectl("demo", &["get", "pods", "-n", "payments", "--no-headers"]).lines().count();
    eprintln!("kubectl: shop={n_shop} payments={n_pay}");

    // open the picker: the second top-bar button
    let btn = ns_button(&h);
    eprintln!("ns button label = {:?}", btn.accesskit_node().label());
    btn.click();
    frames(&mut h, 4);
    shot(&mut h, "ns_picker_open");
    let boxes = h.query_all_by_role(Role::CheckBox).filter_map(|n| n.accesskit_node().label()).collect::<Vec<_>>();
    eprintln!("checkboxes: {boxes:?}");
    click_checkbox(&mut h, "shop");
    eprintln!("after shop: pill={} button={:?}", count_pill(&h), ns_button(&h).accesskit_node().label());
    click_checkbox(&mut h, "payments");
    eprintln!("after shop+payments: pill={} button={:?}", count_pill(&h), ns_button(&h).accesskit_node().label());
    shot(&mut h, "ns_picker_two");
    // search inside the picker
    let ti = h.get_all_by_role(Role::TextInput).last().unwrap();
    ti.focus();
    h.step();
    h.get_all_by_role(Role::TextInput).last().unwrap().type_text("pay");
    frames(&mut h, 4);
    let boxes = h.query_all_by_role(Role::CheckBox).filter_map(|n| n.accesskit_node().label()).collect::<Vec<_>>();
    eprintln!("checkboxes after search 'pay': {boxes:?}");
    shot(&mut h, "ns_picker_search");
    // close popup by pressing Escape / clicking away
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    // go to Nodes (cluster-scoped) and Namespaces with a filter active
    go_view(&mut h, "nodes");
    wait_table(&mut h);
    frames(&mut h, 4);
    eprintln!("nodes with ns filter: pill={}", count_pill(&h));
    go_view(&mut h, "namespaces");
    wait_table(&mut h);
    frames(&mut h, 4);
    eprintln!("namespaces with ns filter: pill={}", count_pill(&h));
    shot(&mut h, "ns_namespaces_view");
    go_view(&mut h, "events");
    wait_table(&mut h);
    frames(&mut h, 4);
    let (hs, rows) = table_snapshot(&h);
    eprintln!("events with ns filter: pill={} headers={hs:?}", count_pill(&h));
    shot(&mut h, "ns_events_view");
    let nsi = hs.iter().position(|x| x == "Namespace");
    if let Some(i) = nsi {
        let bad: Vec<_> = rows.iter().filter(|r| !matches!(r[i].as_str(), "shop" | "payments")).take(3).collect();
        if !bad.is_empty() { finding(format!("events view ignores namespace filter: {bad:?}")); }
    }
    go_view(&mut h, "pods");
    wait_table(&mut h);
    eprintln!("pods with filter: pill={}", count_pill(&h));
    // empty ns
    let btn = ns_button(&h);
    btn.click();
    frames(&mut h, 4);
    click_checkbox(&mut h, "shop");
    click_checkbox(&mut h, "payments");
    click_checkbox(&mut h, "kube-public");
    eprintln!("kube-public only: pill={} nothing={}", count_pill(&h), has(&h, "Nothing here"));
    shot(&mut h, "ns_empty");
    // back to all
    h.get_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map(|l| l == "All namespaces").unwrap_or(false)).map(|n| n.click());
    frames(&mut h, 4);
    eprintln!("back to all: pill={} button={:?}", count_pill(&h), ns_button(&h).accesskit_node().label());
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t04_ns_popup_size() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    ns_button(&h).click();
    for i in 0..6 {
        frames(&mut h, 5);
        shot(&mut h, &format!("ns_popup_f{i}"));
    }
    eprintln!("ns popup after 30 frames");
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t04_ns_popup_size2() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "namespaces");
    settle(&mut h, "namespaces", |h| has(h, "shop"));
    go_view(&mut h, "pods");
    settle(&mut h, "pods", pods_ready);
    ns_button(&h).click();
    frames(&mut h, 8);
    shot(&mut h, "ns_popup_preloaded");
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    // click elsewhere to close
    ns_button(&h).click();
    frames(&mut h, 3);
    ns_button(&h).click();
    frames(&mut h, 8);
    shot(&mut h, "ns_popup_reopened");
    eprintln!("done ns2");
}

pub fn tooltip_button(_h: &H, tip: &str) -> String {
    // icon buttons carry their tooltip text as accessible label
    let _ = tip;
    String::new()
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t05_column_chooser() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    frames(&mut h, 8);
    let (before, _) = table_snapshot(&h);
    eprintln!("headers before: {before:?}");
    h.get_by_label("Choose columns").click();
    frames(&mut h, 4);
    let boxes: Vec<String> = h.query_all_by_role(Role::CheckBox).filter_map(|n| n.accesskit_node().label().map(|s| s.to_string())).collect();
    eprintln!("chooser boxes: {boxes:?}");
    shot(&mut h, "colchooser_open");
    click_checkbox(&mut h, "IP");
    let (after, _) = table_snapshot(&h);
    eprintln!("headers after hiding IP: {after:?}");
    click_checkbox(&mut h, "Controlled By");
    let (a2, _) = table_snapshot(&h);
    eprintln!("headers after toggling Controlled By: {a2:?}");
    // enable a hidden column if any
    for b in &boxes {
        eprintln!("   box {b}");
    }
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    // leave and return: is state kept?
    go_view(&mut h, "nodes");
    wait_table(&mut h);
    go_view(&mut h, "pods");
    wait_table(&mut h);
    frames(&mut h, 6);
    let (a3, _) = table_snapshot(&h);
    eprintln!("headers after round trip: {a3:?}");
    shot(&mut h, "colchooser_after");
    // hide almost all columns
    h.get_by_label("Choose columns").click();
    frames(&mut h, 3);
    for b in ["Namespace", "Ready", "Status", "Restarts", "Node", "Age"] {
        if h.query_all_by_role(Role::CheckBox).any(|n| n.accesskit_node().label().map_or(false, |l| l == b)) {
            click_checkbox(&mut h, b);
        }
    }
    let (a4, _) = table_snapshot(&h);
    eprintln!("headers with few columns: {a4:?}");
    shot(&mut h, "colchooser_few");
}

// ---------------------------------------------------------------------------
// detail panel
// ---------------------------------------------------------------------------

pub fn builtin_key(plural: &str) -> skiff_core::ResourceKey {
    skiff_core::BUILTINS.iter().find(|b| b.plural == plural || b.key().id() == plural).unwrap_or_else(|| panic!("no builtin {plural}")).key()
}

/// Labels belonging to the detail panel (between its close button and the central table).
pub fn detail_labels(h: &H) -> Vec<(Role, String)> {
    let all = labels(h);
    let start = all.iter().position(|(_, l)| l.starts_with("Close (Esc)"));
    let end = all.iter().position(|(_, l)| l == "Reload from the API server").unwrap_or(all.len());
    match start {
        Some(s) if s < end => all[s..end].to_vec(),
        _ => Vec::new(),
    }
}

pub fn open_obj(h: &mut H, plural: &str, ns: &str, name: &str) {
    let key = builtin_key(plural);
    h.state_mut().open_object(&key, ns, name);
    h.step();
    let ok = settle_for(h, 8, |h| has_c(h, "Created") || has(h, "Couldn't load object"));
    if !ok {
        finding(format!("detail for {plural} {ns}/{name} did not load in 8s"));
    }
    frames(h, 4);
}

pub fn open_custom(h: &mut H, kind: &str, ns: &str, name: &str) {
    let key = h.state().session().unwrap().discovered().iter().find(|d| &*d.key.kind == kind).map(|d| d.key.clone()).unwrap_or_else(|| panic!("no CRD kind {kind}"));
    h.state_mut().open_object(&key, ns, name);
    h.step();
    settle_for(h, 8, |h| has_c(h, "Created") || has(h, "Couldn't load object"));
    frames(h, 4);
}

pub fn tab(h: &mut H, name: &str) {
    let n = h
        .query_all_by_role(Role::Button)
        .filter(|n| n.accesskit_node().label().map_or(false, |l| l.starts_with(name)))
        .last()
        .unwrap_or_else(|| panic!("no tab {name}"));
    n.click();
    frames(h, 5);
}

fn show_detail(h: &H, title: &str) {
    eprintln!("---- {title}");
    for (r, l) in detail_labels(h) {
        eprintln!("   [{r:?}] {l}");
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t06_detail_pods() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    let crash = kubectl("demo", &["get", "pods", "-n", "incidents", "-l", "app=crashy", "-o", "name"]);
    let crash = crash.lines().next().unwrap_or("").trim_start_matches("pod/").to_string();
    eprintln!("crash pod: {crash}");
    for (ns, name, tag) in [("monitoring", "alertmanager-0", "healthy"), ("incidents", crash.as_str(), "crash")] {
        open_obj(&mut h, "pods", ns, name);
        show_detail(&h, &format!("pod {tag}"));
        shot(&mut h, &format!("detail_pod_{tag}_overview"));
        tab(&mut h, "YAML");
        shot(&mut h, &format!("detail_pod_{tag}_yaml"));
        tab(&mut h, "Events");
        shot(&mut h, &format!("detail_pod_{tag}_events"));
        show_detail(&h, &format!("pod {tag} events"));
        tab(&mut h, "Overview");
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t06_detail_kinds() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    let cases: Vec<(&str, &str, &str)> = vec![
        ("deployments.apps", "shop", "web"),
        ("deployments.apps", "incidents", "crashy"),
        ("statefulsets.apps", "shop", "postgres"),
        ("daemonsets.apps", "kube-system", "kube-proxy"),
        ("services", "shop", "web"),
        ("services", "shop", "web-lb"),
        ("services", "shop", "postgres-headless"),
        ("services", "shop", "external-payments"),
        ("ingresses.networking.k8s.io", "shop", "shop"),
        ("nodes", "", "demo-worker-5"),
        ("nodes", "", "demo-worker-6"),
        ("nodes", "", "demo-worker-1"),
        ("configmaps", "shop", "feature-flags"),
        ("configmaps", "shop", "kube-root-ca.crt"),
        ("secrets", "shop", "db-credentials"),
        ("secrets", "shop", "shop-tls-secret"),
        ("persistentvolumeclaims", "shop", "data-postgres-0"),
        ("persistentvolumeclaims", "shop", "media-cache"),
        ("cronjobs.batch", "batch", "hourly-report"),
        ("cronjobs.batch", "batch", "weekly-cleanup"),
        ("horizontalpodautoscalers.autoscaling", "shop", "api"),
        ("jobs.batch", "batch", "db-migrate-20261101"),
        ("persistentvolumes", "", "pv-data-kafka-0"),
        ("namespaces", "", "shop"),
        ("serviceaccounts", "shop", "default"),
        ("storageclasses.storage.k8s.io", "", "standard"),
        ("resourcequotas", "shop", ""),
    ];
    for (plural, ns, name) in cases {
        if name.is_empty() {
            continue;
        }
        open_obj(&mut h, plural, ns, name);
        let tag = format!("{}_{}_{}", plural.split('.').next().unwrap(), ns, name);
        show_detail(&h, &tag);
        shot(&mut h, &format!("detail_{tag}"));
    }
}

pub fn click_link(h: &mut H, text: &str) {
    let n = h
        .query_all_by_role(Role::Link)
        .find(|n| n.accesskit_node().label().map_or(false, |l| l == text))
        .unwrap_or_else(|| panic!("no link {text}"));
    n.scroll_to_me();
    frames(h, 3);
    let n = h.query_all_by_role(Role::Link).find(|n| n.accesskit_node().label().map_or(false, |l| l == text)).unwrap();
    n.click();
    frames(h, 6);
}

pub fn detail_title(h: &H) -> String {
    let dl = detail_labels(h);
    dl.iter().find(|(r, _)| *r == Role::Label).map(|x| x.1.clone()).unwrap_or_default()
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t06_links_and_secret() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    // pod -> owner
    open_obj(&mut h, "pods", "monitoring", "alertmanager-0");
    click_link(&mut h, "alertmanager");
    eprintln!("after owner link: view={} detail={}", h.state().view_id(), detail_title(&h));
    shot(&mut h, "link_owner");
    // statefulset -> pod link
    click_link(&mut h, "alertmanager-1");
    eprintln!("after pod link: view={} detail={}", h.state().view_id(), detail_title(&h));
    // pod -> node
    click_link(&mut h, "demo-worker-4");
    eprintln!("after node link: view={} detail={}", h.state().view_id(), detail_title(&h));
    shot(&mut h, "link_node");
    // node pods list -> pod in another ns
    let first_pod_link = h.query_all_by_role(Role::Link).next().map(|n| n.accesskit_node().label().unwrap().to_string());
    eprintln!("first pod link in node: {first_pod_link:?}");
    if let Some(l) = first_pod_link {
        click_link(&mut h, &l);
        eprintln!("after node->pod link: view={} detail={} ", h.state().view_id(), detail_title(&h));
        shot(&mut h, "link_node_pod");
    }
    // replicaset owner chain: pod -> replicaset -> deployment
    open_obj(&mut h, "pods", "shop", "web-58d4dfc678-9xsss");
    click_link(&mut h, "web-58d4dfc678");
    eprintln!("rs: view={} detail={}", h.state().view_id(), detail_title(&h));
    shot(&mut h, "link_rs");
    let labels_rs: Vec<_> = detail_labels(&h).into_iter().filter(|(r, _)| *r == Role::Link).collect();
    eprintln!("links in rs: {labels_rs:?}");
    if labels_rs.iter().any(|(_, l)| l == "web") {
        click_link(&mut h, "web");
        eprintln!("rs->deploy: view={} detail={}", h.state().view_id(), detail_title(&h));
    }
    // ingress -> service, secret
    open_obj(&mut h, "ingresses.networking.k8s.io", "shop", "shop");
    click_link(&mut h, "web:80");
    eprintln!("ingress->svc: view={} detail={}", h.state().view_id(), detail_title(&h));
    open_obj(&mut h, "ingresses.networking.k8s.io", "shop", "shop");
    click_link(&mut h, "shop-tls-secret");
    eprintln!("ingress->secret: view={} detail={}", h.state().view_id(), detail_title(&h));
    // secret reveal / hide
    open_obj(&mut h, "secrets", "shop", "db-credentials");
    for _ in 0..3 {
        h.get_all_by_label("Reveal").next().unwrap().click();
        frames(&mut h, 3);
    }
    let revealed: Vec<String> = detail_labels(&h).into_iter().map(|x| x.1).collect();
    eprintln!("revealed has password text: {}", revealed.iter().any(|l| l == "s3cr3t-not-real"));
    shot(&mut h, "secret_revealed");
    eprintln!("buttons: {:?}", detail_labels(&h).into_iter().filter(|x| x.0 == Role::Button).map(|x| x.1).collect::<Vec<_>>());
    for _ in 0..3 {
        h.get_all_by_label("Hide").next().unwrap().click();
        frames(&mut h, 3);
    }
    let hidden: Vec<String> = detail_labels(&h).into_iter().map(|x| x.1).collect();
    eprintln!("after hide still shows plaintext: {}", hidden.iter().any(|l| l == "s3cr3t-not-real"));
    // reveal, switch to another secret and back: stays revealed?
    h.get_all_by_label("Reveal").next().unwrap().click();
    frames(&mut h, 4);
    open_obj(&mut h, "secrets", "shop", "api-token");
    open_obj(&mut h, "secrets", "shop", "db-credentials");
    let again: Vec<String> = detail_labels(&h).into_iter().map(|x| x.1).collect();
    eprintln!("reopened secret reveals plaintext without click: {}", again.iter().any(|l| l == "s3cr3t-not-real"));
    // resource quota & pdb overview
    open_obj(&mut h, "resourcequotas", "team-a", "team-a-quota");
    show_detail(&h, "resourcequota");
    shot(&mut h, "detail_resourcequota");
    open_obj(&mut h, "poddisruptionbudgets.policy", "shop", "web-pdb");
    show_detail(&h, "pdb");
    shot(&mut h, "detail_pdb");
}

// ---------------------------------------------------------------------------
// mutations (namespace qa-skiff)
// ---------------------------------------------------------------------------

pub fn click_tip(h: &mut H, tip: &str) {
    let n = h
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().map_or(false, |l| clean(&l) == tip))
        .unwrap_or_else(|| panic!("no button/tooltip {tip}"));
    n.click();
    frames(h, 5);
}

fn yaml_editor_text(h: &H) -> Option<String> {
    h.query_all_by_role(Role::MultilineTextInput).next().and_then(|n| n.value())
}

fn toasts(h: &H) -> Vec<String> {
    // toasts are labels; just list labels containing common phrases
    labels(h).into_iter().map(|x| x.1).filter(|l| l.starts_with("Scaled") || l.starts_with("Deleted") || l.starts_with("Restarting") || l.starts_with("Cordoned") || l.starts_with("Uncordoned") || l.contains("failed") || l.contains("Failed") || l.contains("rror")).collect()
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t07_edit_yaml() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "configmaps");
    settle(&mut h, "cms", |h| has(h, "qa-cm"));
    open_obj(&mut h, "configmaps", "qa-skiff", "qa-cm");
    shot(&mut h, "mut_cm_overview");
    click_tip(&mut h, "Edit YAML");
    let text = yaml_editor_text(&h);
    eprintln!("editor text present: {}", text.is_some());
    shot(&mut h, "mut_cm_editing");
    let text = text.expect("editor text");
    // 1. valid edit
    let new = text.replace("color: blue", "color: green");
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().set_text_value_accesskit(&new);
    frames(&mut h, 4);
    eprintln!("editor now contains green: {}", yaml_editor_text(&h).map_or(false, |t| t.contains("green")));
    shot(&mut h, "mut_cm_modified");
    click_button(&mut h, "Apply");
    frames(&mut h, 10);
    let v = kubectl("demo", &["-n", "qa-skiff", "get", "cm", "qa-cm", "-o", "jsonpath={.data.color}"]);
    eprintln!("kubectl color after apply = {v:?}");
    eprintln!("still editing: {}", yaml_editor_text(&h).is_some());
    shot(&mut h, "mut_cm_applied");
    tab(&mut h, "Overview");
    eprintln!("overview shows green: {}", has(&h, "green"));

    // 2. invalid YAML
    click_tip(&mut h, "Edit YAML");
    tab(&mut h, "YAML");
    if yaml_editor_text(&h).is_none() {
        click_tip(&mut h, "Edit YAML");
    }
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().set_text_value_accesskit("apiVersion: v1\nkind: ConfigMap\nmetadata: [oops\n");
    frames(&mut h, 3);
    click_button(&mut h, "Apply");
    frames(&mut h, 10);
    shot(&mut h, "mut_cm_invalid_yaml");
    let errs: Vec<_> = labels(&h).into_iter().filter(|(_, l)| l.contains("invalid") || l.contains("YAML") && l.len() > 8).collect();
    eprintln!("invalid yaml error labels: {errs:?}");
    // 2b. semantically invalid: bad data type
    let cur = yaml_editor_text(&h).unwrap_or_default();
    let _ = cur;
    let bad = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: qa-cm\n  namespace: qa-skiff\ndata:\n  color:\n    nested: map\n";
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().set_text_value_accesskit(bad);
    frames(&mut h, 3);
    click_button(&mut h, "Apply");
    frames(&mut h, 10);
    shot(&mut h, "mut_cm_invalid_schema");
    let errs: Vec<_> = labels(&h).into_iter().filter(|(_, l)| l.contains("data") || l.contains("Invalid") || l.contains("invalid") || l.contains("cannot")).collect();
    eprintln!("schema error labels: {errs:?}");
    // 2c. rename
    let bad = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: qa-cm-renamed\n  namespace: qa-skiff\ndata:\n  color: x\n";
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().set_text_value_accesskit(bad);
    frames(&mut h, 3);
    click_button(&mut h, "Apply");
    frames(&mut h, 8);
    shot(&mut h, "mut_cm_rename");
    let errs: Vec<_> = labels(&h).into_iter().filter(|(_, l)| l.contains("cannot")).collect();
    eprintln!("rename error labels: {errs:?}");
    click_button(&mut h, "Cancel");
    frames(&mut h, 4);

    // 3. conflicting edit
    click_tip(&mut h, "Edit YAML");
    let text = yaml_editor_text(&h).expect("editor");
    kubectl("demo", &["-n", "qa-skiff", "patch", "cm", "qa-cm", "-p", "{\"data\":{\"note\":\"changed-elsewhere\"}}"]);
    let new = text.replace("color: green", "color: red");
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().set_text_value_accesskit(&new);
    frames(&mut h, 3);
    click_button(&mut h, "Apply");
    frames(&mut h, 10);
    shot(&mut h, "mut_cm_conflict");
    let errs: Vec<_> = labels(&h).into_iter().filter(|(_, l)| l.contains("onflict") || l.contains("modified")).collect();
    eprintln!("conflict labels: {errs:?}");
    let v = kubectl("demo", &["-n", "qa-skiff", "get", "cm", "qa-cm", "-o", "jsonpath={.data}"]);
    eprintln!("kubectl data after conflict = {v}");
    click_button(&mut h, "Cancel");
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t08_cm_columns() {
    if !setup() {
        return;
    }
    for size in [[1440.0, 900.0], [1000.0, 640.0], [2200.0, 1300.0]] {
        let mut h = harness_sized("demo", "configmaps", size, false);
        settle(&mut h, "cms", |h| has(h, "qa-cm"));
        frames(&mut h, 4);
        eprintln!("size {size:?} no detail: headers={:?}", table_snapshot(&h).0);
        shot(&mut h, &format!("cm_cols_{}_nodetail", size[0]));
        open_obj(&mut h, "configmaps", "qa-skiff", "qa-cm");
        frames(&mut h, 6);
        eprintln!("size {size:?} detail: headers={:?}", table_snapshot(&h).0);
        shot(&mut h, &format!("cm_cols_{}_detail", size[0]));
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t08_detail_squeezes_columns() {
    if !setup() {
        return;
    }
    for (view, plural, ns, name) in [("deployments.apps", "deployments.apps", "shop", "web"), ("secrets", "secrets", "shop", "db-credentials"), ("namespaces", "namespaces", "", "shop"), ("nodes", "nodes", "", "demo-worker-1")] {
        let mut h = harness(view.split('.').next().map(|_| "demo").unwrap(), view);
        settle(&mut h, "table", |h| has_c(h, "Name"));
        wait_table(&mut h);
        frames(&mut h, 6);
        shot(&mut h, &format!("squeeze_{view}_before"));
        // open through a click on the row, like a user would
        open_obj(&mut h, plural, ns, name);
        frames(&mut h, 10);
        eprintln!("{view}: headers with detail: {:?}", table_snapshot(&h).0);
        shot(&mut h, &format!("squeeze_{view}_after"));
        h.key_press(egui::Key::Escape);
        frames(&mut h, 6);
        eprintln!("{view}: headers after close: {:?}", table_snapshot(&h).0);
        shot(&mut h, &format!("squeeze_{view}_closed"));
        let _ = (plural, ns);
    }
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t07_delete_scale_restart_cordon() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "deployments.apps");
    settle(&mut h, "deployments", |h| has(h, "qa-web"));

    // ---- Scale
    open_obj(&mut h, "deployments.apps", "qa-skiff", "qa-web");
    click_tip(&mut h, "Scale");
    shot(&mut h, "mut_scale_dialog");
    let tin: Vec<_> = h.query_all_by_role(Role::TextInput).filter_map(|n| n.value()).collect();
    eprintln!("scale dialog inputs: {tin:?}");
    // invalid input disables the button
    h.query_all_by_role(Role::TextInput).last().unwrap().set_text_value_accesskit("abc");
    frames(&mut h, 3);
    let scale_btn_disabled = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l == "Scale")).map(|n| n.accesskit_node().is_disabled());
    eprintln!("Scale disabled with 'abc': {scale_btn_disabled:?}");
    h.query_all_by_role(Role::TextInput).last().unwrap().set_text_value_accesskit("-3");
    frames(&mut h, 3);
    let scale_btn_disabled = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l == "Scale")).map(|n| n.accesskit_node().is_disabled());
    eprintln!("Scale disabled with '-3': {scale_btn_disabled:?}");
    h.query_all_by_role(Role::TextInput).last().unwrap().set_text_value_accesskit("4");
    frames(&mut h, 3);
    // the Scale button inside the dialog (last "Scale" button)
    let btn = h.query_all_by_role(Role::Button).filter(|n| n.accesskit_node().label().map_or(false, |l| l == "Scale")).last().unwrap();
    btn.click();
    frames(&mut h, 10);
    std::thread::sleep(Duration::from_millis(800));
    frames(&mut h, 5);
    let r = kubectl("demo", &["-n", "qa-skiff", "get", "deploy", "qa-web", "-o", "jsonpath={.spec.replicas}"]);
    eprintln!("kubectl replicas after scale to 4: {r}");
    eprintln!("toasts: {:?}", toasts(&h));
    shot(&mut h, "mut_scale_done");
    // Enter key in the dialog
    click_tip(&mut h, "Scale");
    eprintln!("dialog prefilled with: {:?}", h.query_all_by_role(Role::TextInput).last().and_then(|n| n.value()));
    h.query_all_by_role(Role::TextInput).last().unwrap().set_text_value_accesskit("1");
    frames(&mut h, 2);
    h.key_press(egui::Key::Enter);
    frames(&mut h, 12);
    std::thread::sleep(Duration::from_millis(500));
    frames(&mut h, 3);
    let r = kubectl("demo", &["-n", "qa-skiff", "get", "deploy", "qa-web", "-o", "jsonpath={.spec.replicas}"]);
    eprintln!("kubectl replicas after Enter-scale to 1: {r}");

    // ---- Restart
    let before = kubectl("demo", &["-n", "qa-skiff", "get", "deploy", "qa-web", "-o", "jsonpath={.spec.template.metadata.annotations}"]);
    click_tip(&mut h, "Restart");
    frames(&mut h, 8);
    std::thread::sleep(Duration::from_millis(500));
    let after = kubectl("demo", &["-n", "qa-skiff", "get", "deploy", "qa-web", "-o", "jsonpath={.spec.template.metadata.annotations}"]);
    eprintln!("restart annotations before={before:?} after={after:?}");
    eprintln!("toasts: {:?}", toasts(&h));
    shot(&mut h, "mut_restart");

    // ---- Delete with confirm dialog (configmap qa-del)
    go_view(&mut h, "configmaps");
    settle(&mut h, "cms", |h| has(h, "qa-del"));
    open_obj(&mut h, "configmaps", "qa-skiff", "qa-del");
    click_tip(&mut h, "Delete");
    shot(&mut h, "mut_delete_dialog");
    eprintln!("delete dialog present: {}", has_c(&h, "Delete ConfigMap"));
    // Cancel
    click_button(&mut h, "Cancel");
    frames(&mut h, 4);
    let r = kubectl("demo", &["-n", "qa-skiff", "get", "cm", "qa-del", "-o", "name"]);
    eprintln!("after cancel kubectl: {r}");
    // Esc closes dialog?
    click_tip(&mut h, "Delete");
    h.key_press(egui::Key::Escape);
    frames(&mut h, 4);
    eprintln!("dialog still open after Esc: {}", has_c(&h, "Delete ConfigMap"));
    if has_c(&h, "Delete ConfigMap") {
        click_button(&mut h, "Cancel");
    }
    // Confirm
    click_tip(&mut h, "Delete");
    let b = h.query_all_by_role(Role::Button).filter(|n| n.accesskit_node().label().map_or(false, |l| l == "Delete")).last().unwrap();
    b.click();
    frames(&mut h, 10);
    std::thread::sleep(Duration::from_millis(600));
    frames(&mut h, 5);
    let r = kubectl("demo", &["-n", "qa-skiff", "get", "cm", "qa-del", "-o", "name"]);
    eprintln!("after delete kubectl: {r}");
    eprintln!("toasts: {:?}", toasts(&h));
    shot(&mut h, "mut_deleted");
    eprintln!("detail still shows deleted object: title={:?} labels={:?}", detail_title(&h), detail_labels(&h).into_iter().filter(|x| x.1.contains("Refresh") || x.1.contains("Couldn")).collect::<Vec<_>>());
    eprintln!("table still lists qa-del: {}", has(&h, "qa-del"));

    // ---- Cordon / Uncordon
    go_view(&mut h, "nodes");
    settle(&mut h, "nodes", |h| has_c(h, "demo-spot-1"));
    open_obj(&mut h, "nodes", "", "demo-spot-1");
    click_tip(&mut h, "Cordon");
    std::thread::sleep(Duration::from_millis(600));
    frames(&mut h, 6);
    let r = kubectl("demo", &["get", "node", "demo-spot-1", "--no-headers"]);
    eprintln!("kubectl after cordon: {r}");
    eprintln!("panel shows Cordoned: {}", has(&h, "Cordoned"));
    shot(&mut h, "mut_cordon");
    click_tip(&mut h, "Uncordon");
    std::thread::sleep(Duration::from_millis(600));
    frames(&mut h, 6);
    let r = kubectl("demo", &["get", "node", "demo-spot-1", "--no-headers"]);
    eprintln!("kubectl after uncordon: {r}");
    eprintln!("panel still shows Cordoned: {}", has(&h, "Cordoned"));
    shot(&mut h, "mut_uncordon");
}

fn dock_labels(h: &H) -> Vec<(Role, String)> {
    let all = labels(h);
    let start = all.iter().position(|(_, l)| l.starts_with("Hide panel"));
    match start {
        Some(s) => all[s..].to_vec(),
        None => vec![],
    }
}

fn log_summary(h: &H) -> String {
    labels(h).into_iter().map(|x| x.1).filter(|l| l.contains(" lines") || l.starts_with("streaming") || l == "ended" || l.contains("failed")).collect::<Vec<_>>().join(" | ")
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t09_logs() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    // ---- single pod
    open_obj(&mut h, "pods", "monitoring", "alertmanager-0");
    click_tip(&mut h, "Logs");
    let ok = settle_for(&mut h, 8, |h| has_c(h, " lines"));
    eprintln!("single pod logs summary: {} (ok={ok})", log_summary(&h));
    shot(&mut h, "logs_single");
    let d: Vec<_> = dock_labels(&h).into_iter().take(14).collect();
    eprintln!("dock head: {d:?}");
    // filter
    let fin = h.query_all_by_role(Role::TextInput).find(|n| n.accesskit_node().label().map_or(false, |l| l.contains("Filter lines")) || n.accesskit_node().value().is_some()).map(|_| ());
    let _ = fin;
    let inputs: Vec<(String,)> = h.query_all_by_role(Role::TextInput).map(|n| (format!("{:?}/{:?}", n.accesskit_node().label(), n.accesskit_node().value()),)).collect();
    eprintln!("text inputs: {inputs:?}");
    h.query_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("level=warn");
    frames(&mut h, 6);
    eprintln!("after filter 'level=warn': {}", log_summary(&h));
    shot(&mut h, "logs_filtered");
    h.query_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("zzzzqq");
    frames(&mut h, 6);
    eprintln!("after nomatch filter: {} nolines={}", log_summary(&h), has(&h, "No lines match the filter"));
    shot(&mut h, "logs_nomatch");
    h.query_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("");
    frames(&mut h, 4);
    // timestamps + wrap
    click_tip(&mut h, "Show timestamps");
    shot(&mut h, "logs_ts");
    click_tip(&mut h, "Wrap long lines");
    shot(&mut h, "logs_ts_wrap");
    click_tip(&mut h, "Wrap long lines");
    click_tip(&mut h, "Show timestamps");
    // tail menu
    let tail_btn = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l.starts_with("last ") || l == "all"));
    eprintln!("tail button: {:?}", tail_btn.as_ref().map(|n| n.accesskit_node().label()));
    if let Some(n) = tail_btn { n.click(); frames(&mut h, 4); }
    shot(&mut h, "logs_tail_menu");
    let items: Vec<_> = labels(&h).into_iter().filter(|x| x.1.starts_with("last ") || x.1 == "all").collect();
    eprintln!("tail menu items: {items:?}");
    if let Some(n) = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l == "last 100")) { n.click(); }
    frames(&mut h, 8);
    eprintln!("after tail=100: {}", log_summary(&h));
    // copy
    click_tip(&mut h, "Copy shown lines");
    eprintln!("copy toast: {:?}", labels(&h).into_iter().filter(|x| x.1.starts_with("Copied")).collect::<Vec<_>>());
    // clear
    click_tip(&mut h, "Clear");
    eprintln!("after clear: {}", log_summary(&h));

    // ---- crashloop pod previous
    let crash = kubectl("demo", &["get", "pods", "-n", "incidents", "-l", "app=crashy", "-o", "name"]);
    let crash = crash.lines().next().unwrap_or("").trim_start_matches("pod/").to_string();
    open_obj(&mut h, "pods", "incidents", &crash);
    click_tip(&mut h, "Logs");
    settle_for(&mut h, 8, |h| has_c(h, " lines") || has_c(h, "No log output"));
    eprintln!("crash current logs: {} noout={}", log_summary(&h), has(&h, "No log output yet"));
    shot(&mut h, "logs_crash_current");
    click_tip(&mut h, "Previous container instance");
    settle_for(&mut h, 8, |h| has_c(h, " lines"));
    frames(&mut h, 8);
    eprintln!("crash previous logs: {}", log_summary(&h));
    shot(&mut h, "logs_crash_previous");
    // kubectl for comparison
    let kl = kubectl("demo", &["logs", "-n", "incidents", &crash, "--previous", "--tail=3"]);
    eprintln!("kubectl logs --previous: {}", kl.lines().take(3).collect::<Vec<_>>().join(" / "));

    // ---- deployment (merged pods)
    go_view(&mut h, "deployments.apps");
    open_obj(&mut h, "deployments.apps", "shop", "web");
    click_tip(&mut h, "Logs");
    settle_for(&mut h, 8, |h| has_c(h, "streaming"));
    frames(&mut h, 8);
    eprintln!("deployment logs: {}", log_summary(&h));
    shot(&mut h, "logs_deployment");
    // tabs in dock
    let tabs: Vec<_> = dock_labels(&h).into_iter().filter(|x| x.0 == Role::Button).collect();
    eprintln!("dock buttons: {tabs:?}");
}

fn curl(port: u16) -> String {
    let o = Command::new("curl").args(["--noproxy", "*", "-s", "-m", "5", &format!("http://127.0.0.1:{port}/")]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn forward_port_from_toast(h: &H) -> Option<u16> {
    labels(h).into_iter().find_map(|(_, l)| {
        l.strip_prefix("Forwarding localhost:").and_then(|r| r.split_whitespace().next().and_then(|p| p.parse().ok()))
    })
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t10_port_forward() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "services");
    settle(&mut h, "services", |h| has(h, "web"));
    // ---- from service detail
    open_obj(&mut h, "services", "shop", "web");
    click_button(&mut h, "Forward");
    let ok = settle_for(&mut h, 8, |h| forward_port_from_toast(h).is_some() || has_c(h, "Port forward failed"));
    let port = forward_port_from_toast(&h);
    eprintln!("service detail forward: ok={ok} port={port:?} toasts={:?}", labels(&h).into_iter().filter(|x| x.1.starts_with("Forward") || x.1.starts_with("Port forward")).collect::<Vec<_>>());
    shot(&mut h, "pf_service_toast");
    if let Some(p) = port {
        eprintln!("curl 127.0.0.1:{p} => {:?}", curl(p));
    }
    // ---- via right click dialog on a pod row
    go_view(&mut h, "pods");
    settle(&mut h, "pods", pods_ready);
    let row = h.get_all_by_label("alertmanager-0").next().unwrap();
    row.click_secondary();
    frames(&mut h, 4);
    shot(&mut h, "pf_context_menu");
    let menu: Vec<_> = labels(&h).into_iter().filter(|x| x.0 == Role::Button && matches!(x.1.as_str(), "Logs" | "Shell" | "Port forward" | "Edit YAML" | "Copy name" | "Delete")).collect();
    eprintln!("context menu items: {menu:?}");
    click_button(&mut h, "Port forward");
    settle_for(&mut h, 6, |h| has_c(h, "Port forward to pod"));
    frames(&mut h, 6);
    shot(&mut h, "pf_dialog");
    let d: Vec<_> = labels(&h).into_iter().filter(|x| x.1.contains("9093") || x.1.contains("Remote") || x.1.contains("Local") || x.1.contains("Start") || x.1.contains("Looking") || x.1.contains("No declared")).collect();
    eprintln!("dialog labels: {d:?}");
    let ti: Vec<_> = h.query_all_by_role(Role::TextInput).map(|n| n.accesskit_node().value()).collect();
    eprintln!("text inputs: {ti:?}");
    click_button(&mut h, "Start forwarding");
    let ok = settle_for(&mut h, 8, |h| forward_port_from_toast(h).is_some() || has_c(h, "Port forward failed"));
    let port2 = forward_port_from_toast(&h);
    eprintln!("dialog forward: ok={ok} port={port2:?}");
    if let Some(p) = port2 {
        eprintln!("curl 127.0.0.1:{p} => {:?}", curl(p));
    }
    // explicit local port
    let row = h.get_all_by_label("alertmanager-1").next().unwrap();
    row.click_secondary();
    frames(&mut h, 3);
    click_button(&mut h, "Port forward");
    settle_for(&mut h, 6, |h| has_c(h, "Port forward to pod"));
    frames(&mut h, 4);
    let inputs: Vec<_> = h.query_all_by_role(Role::TextInput).collect();
    let n = inputs.len();
    eprintln!("dialog inputs: {n}");
    // local port input is the last text input
    h.query_all_by_role(Role::TextInput).last().unwrap().set_text_value_accesskit("18555");
    frames(&mut h, 3);
    click_button(&mut h, "Start forwarding");
    settle_for(&mut h, 8, |h| has_c(h, "Forwarding localhost:18555") || has_c(h, "Port forward failed"));
    eprintln!("explicit port toasts: {:?}", labels(&h).into_iter().filter(|x| x.1.starts_with("Forward") || x.1.starts_with("Port forward")).collect::<Vec<_>>());
    eprintln!("curl 18555 => {:?}", curl(18555));
    // port in use
    let row = h.get_all_by_label("alertmanager-1").next().unwrap();
    row.click_secondary();
    frames(&mut h, 3);
    click_button(&mut h, "Port forward");
    settle_for(&mut h, 6, |h| has_c(h, "Port forward to pod"));
    frames(&mut h, 4);
    h.query_all_by_role(Role::TextInput).last().unwrap().set_text_value_accesskit("18555");
    frames(&mut h, 3);
    click_button(&mut h, "Start forwarding");
    frames(&mut h, 20);
    std::thread::sleep(Duration::from_millis(500));
    frames(&mut h, 5);
    eprintln!("port-in-use toasts: {:?}", labels(&h).into_iter().filter(|x| x.1.starts_with("Forward") || x.1.starts_with("Port forward")).collect::<Vec<_>>());
    shot(&mut h, "pf_in_use");
    // ---- forwards tab
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    frames(&mut h, 3);
    h.query_all_by_role(Role::TextInput).last().unwrap().type_text("port forwards");
    frames(&mut h, 4);
    h.key_press(egui::Key::Enter);
    frames(&mut h, 6);
    shot(&mut h, "pf_tab");
    let d: Vec<_> = dock_labels(&h).into_iter().filter(|x| x.1.contains("localhost") || x.1.contains("connections") || x.1.contains("→") || x.1.contains("shop")).collect();
    eprintln!("forwards tab: {d:?}");
    // stop all
    let mut stops = 0;
    loop {
        let found = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l == "Stop forwarding"));
        match found {
            Some(n) => n.click(),
            None => break,
        }
        frames(&mut h, 4);
        stops += 1;
        if stops > 6 { break; }
    }
    eprintln!("stopped {stops}");
    shot(&mut h, "pf_tab_stopped");
    eprintln!("after stops: empty state shown: {}", has(&h, "No active port forwards"));
    if let Some(p) = port {
        eprintln!("curl after stop {p} => {:?}", curl(p));
    }
    eprintln!("status bar mentions forwards: {}", has_c(&h, "port forward"));
}

fn palette_open(h: &mut H, query: &str) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    frames(h, 3);
    h.query_all_by_role(Role::TextInput).last().expect("palette input").type_text(query);
    frames(h, 4);
}

fn palette_items(h: &H) -> Vec<String> {
    // palette rows are Buttons with title labels; the text input is the last text input
    labels(h).into_iter().filter(|x| x.0 == Role::Button).map(|x| x.1).collect()
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t11_palette() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    for (q, expect_view) in [("po", "pods"), ("deploy", "deployments.apps"), ("svc", "services"), ("cm", "configmaps"), ("ns", "namespaces"), ("sts", "statefulsets.apps"), ("ing", "ingresses.networking.k8s.io"), ("pvc", "persistentvolumeclaims"), ("no", "nodes"), ("ev", "events"), ("cj", "cronjobs.batch"), ("hpa", "horizontalpodautoscalers.autoscaling"), ("crd", "customresourcedefinitions.apiextensions.k8s.io"), ("secret", "secrets"), ("ds", "daemonsets.apps"), ("rs", "replicasets.apps"), ("job", "jobs.batch"), ("pv", "persistentvolumes")] {
        palette_open(&mut h, q);
        let first = palette_items(&h);
        let top: Vec<_> = first.iter().skip_while(|l| !l.contains("Search or jump")).skip(1).take(4).cloned().collect();
        h.key_press(egui::Key::Enter);
        frames(&mut h, 6);
        let v = h.state().view_id();
        let mark = if v == expect_view { "ok" } else { "MISMATCH" };
        eprintln!("palette '{q}' -> view {v} (expected {expect_view}) {mark}; first buttons after search: {top:?}");
        if v != expect_view {
            finding(format!("palette alias '{q}' went to {v}, expected {expect_view}"));
        }
        go_view(&mut h, "pods");
    }
    // custom resource kind by name
    palette_open(&mut h, "database");
    shot(&mut h, "palette_database");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 8);
    eprintln!("palette 'database' -> view {}", h.state().view_id());
    shot(&mut h, "palette_database_view");
    // object by name (pods table loaded)
    go_view(&mut h, "pods");
    settle(&mut h, "pods", pods_ready);
    palette_open(&mut h, "alertmanager-1");
    shot(&mut h, "palette_object");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 10);
    eprintln!("palette object -> view {} detail {}", h.state().view_id(), detail_title(&h));
    // object from a different kind (deployment web) when deployments table loaded
    go_view(&mut h, "deployments.apps");
    settle(&mut h, "deploys", |h| has(h, "qa-web"));
    go_view(&mut h, "pods");
    palette_open(&mut h, "qa-web");
    shot(&mut h, "palette_qaweb");
    let items = palette_items(&h);
    eprintln!("palette 'qa-web' items: {:?}", items.iter().filter(|l| l.contains("qa-web")).collect::<Vec<_>>());
    h.key_press(egui::Key::Enter);
    frames(&mut h, 10);
    eprintln!("-> view {} detail {} ns_btn={:?}", h.state().view_id(), detail_title(&h), ns_button(&h).accesskit_node().label());
    // namespace
    palette_open(&mut h, "payments");
    shot(&mut h, "palette_ns");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 8);
    eprintln!("palette 'payments' -> view {} ns_btn={:?} filter={:?}", h.state().view_id(), ns_button(&h).accesskit_node().label(), h.state().ns_filter);
    palette_open(&mut h, "all namespaces");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 6);
    eprintln!("palette 'all namespaces' -> filter={:?}", h.state().ns_filter);
    // no matches
    palette_open(&mut h, "zzzxqqq");
    eprintln!("no-match palette shows message: {}", has(&h, "No matches"));
    shot(&mut h, "palette_nomatch");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 3);
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    eprintln!("palette closed by Esc: {}", h.state().palette.is_none());
    // actions
    palette_open(&mut h, "theme");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 5);
    eprintln!("theme toggled -> dark={}", h.state().pal.dark);
    palette_open(&mut h, "theme");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 5);
    palette_open(&mut h, "sidebar");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 5);
    eprintln!("sidebar_open={}", h.state().sidebar_open);
    palette_open(&mut h, "sidebar");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 5);
    // cluster switch
    palette_open(&mut h, "scale");
    shot(&mut h, "palette_scale");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 10);
    eprintln!("after switching to scale: active={:?}", h.state().active);
    let ok = settle_for(&mut h, 15, |h| has_c(h, "Name") && !has(h, "Loading…"));
    frames(&mut h, 10);
    eprintln!("scale pods table loaded ok={ok} pill={}", count_pill(&h));
    shot(&mut h, "scale_pods");
    palette_open(&mut h, "demo");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 10);
    eprintln!("back to demo: active={:?}", h.state().active);
    settle_for(&mut h, 10, pods_ready);
    shot(&mut h, "back_to_demo");
}

fn press(h: &mut H, k: egui::Key) {
    h.key_press(k);
    frames(h, 4);
}

/// Name of the table row that is highlighted, from the detail panel after Enter.
fn enter_and_title(h: &mut H) -> String {
    press(h, egui::Key::Enter);
    frames(h, 4);
    detail_title(h)
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t12_keyboard() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "configmaps");
    settle(&mut h, "table", |h| has(h, "api-config"));
    wait_table(&mut h);
    let snap = h.state().session().unwrap().table(&builtin_key("configmaps")).snapshot();
    // expected order: Name asc using the app's own compare
    let col = snap.columns.iter().find(|c| c.source == skiff_core::ColSource::Name).unwrap();
    let mut rows: Vec<_> = snap.rows.iter().cloned().collect();
    rows.sort_by(|a, b| skiff_core::compare(a, b, col));
    let names: Vec<String> = rows.iter().map(|r| format!("{}/{}", r.namespace, r.name)).collect();
    eprintln!("n rows {} first {} last {}", names.len(), names[0], names[names.len() - 1]);
    let title_of = |h: &H| detail_labels(h).into_iter().find(|x| x.0 == Role::Label).map(|x| x.1).unwrap_or_default();
    let ns_of = |h: &H| detail_labels(h).into_iter().filter(|x| x.0 == Role::Label).nth(1).map(|x| x.1).unwrap_or_default();

    // End with nothing selected
    press(&mut h, egui::Key::End);
    press(&mut h, egui::Key::Enter);
    eprintln!("End (no selection) then Enter -> {} / {} (expected last: {})", title_of(&h), ns_of(&h), names[names.len() - 1]);
    shot(&mut h, "kb_end");
    // Escape closes detail
    press(&mut h, egui::Key::Escape);
    eprintln!("Esc closes detail: {}", detail_labels(&h).is_empty());
    // Home
    press(&mut h, egui::Key::Home);
    press(&mut h, egui::Key::Enter);
    eprintln!("Home then Enter -> {} (expected {})", title_of(&h), names[0]);
    // j / k
    press(&mut h, egui::Key::J);
    eprintln!("j (detail open, follows) -> {} (expected {})", title_of(&h), names[1]);
    press(&mut h, egui::Key::J);
    press(&mut h, egui::Key::K);
    eprintln!("j,k -> {} (expected {})", title_of(&h), names[1]);
    press(&mut h, egui::Key::ArrowDown);
    eprintln!("ArrowDown -> {} (expected {})", title_of(&h), names[2]);
    press(&mut h, egui::Key::ArrowUp);
    press(&mut h, egui::Key::ArrowUp);
    eprintln!("Up Up -> {} (expected {})", title_of(&h), names[0]);
    press(&mut h, egui::Key::ArrowUp);
    eprintln!("Up at top stays -> {} (expected {})", title_of(&h), names[0]);
    press(&mut h, egui::Key::PageDown);
    eprintln!("PageDown -> {} (expected {})", title_of(&h), names[20.min(names.len() - 1)]);
    press(&mut h, egui::Key::PageDown);
    eprintln!("PageDown x2 -> {} (expected {})", title_of(&h), names[(40).min(names.len() - 1)]);
    press(&mut h, egui::Key::PageUp);
    eprintln!("PageUp -> {} (expected {})", title_of(&h), names[(20).min(names.len() - 1)]);
    press(&mut h, egui::Key::End);
    eprintln!("End -> {} (expected {})", title_of(&h), names[names.len() - 1]);
    shot(&mut h, "kb_end_selected");
    press(&mut h, egui::Key::ArrowDown);
    eprintln!("Down at bottom stays -> {}", title_of(&h));
    press(&mut h, egui::Key::Home);
    eprintln!("Home -> {} (expected {})", title_of(&h), names[0]);
    // '/' focus filter, typing, Esc
    press(&mut h, egui::Key::Escape);
    h.key_press(egui::Key::Slash);
    frames(&mut h, 3);
    let focused = h.query_all_by_role(Role::TextInput).any(|n| n.is_focused());
    eprintln!("'/' focuses filter: {focused}");
    h.query_all_by_role(Role::TextInput).next().unwrap().type_text("kube");
    frames(&mut h, 5);
    eprintln!("after typing 'kube': pill={}", count_pill(&h));
    // arrow keys while typing should not move selection
    press(&mut h, egui::Key::Escape);
    eprintln!("after first Esc: any text input focused: {} pill={}", h.query_all_by_role(Role::TextInput).any(|n| n.is_focused()), count_pill(&h));
    press(&mut h, egui::Key::Escape);
    eprintln!("after second Esc: pill={}", count_pill(&h));
    shot(&mut h, "kb_after_esc");
    // j/k typed into filter must not trigger navigation
    h.key_press(egui::Key::Slash);
    frames(&mut h, 3);
    h.query_all_by_role(Role::TextInput).next().unwrap().type_text("jk/");
    frames(&mut h, 4);
    eprintln!("typing 'jk/' into filter -> pill={} detail={}", count_pill(&h), title_of(&h));
    eprintln!("filter value: {:?}", h.query_all_by_role(Role::TextInput).next().and_then(|n| n.value()));
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t13_scale_cluster() {
    if !setup() {
        return;
    }
    let t0 = Instant::now();
    let mut h = harness("scale", "pods");
    let ok = settle_for(&mut h, 60, |h| count_pill(h).replace(',', "").parse::<usize>().map_or(false, |n| n > 5000));
    eprintln!("scale pods: ok={ok} in {:?} pill={}", t0.elapsed(), count_pill(&h));
    shot(&mut h, "scale_pods_full");
    let mut max = Duration::ZERO;
    let mut sum = Duration::ZERO;
    for _ in 0..30 {
        h.step();
        let d = h.state().last_ui;
        max = max.max(d);
        sum += d;
    }
    eprintln!("scale pods steady-state ui time: avg {:?} max {:?}", sum / 30, max);
    // sort
    for col in ["Status", "Node", "Age", "Name"] {
        let t = Instant::now();
        click_button(&mut h, col);
        frames(&mut h, 6);
        eprintln!("sort by {col}: {:?} (incl. 6 frames) ui={:?}", t.elapsed(), h.state().last_ui);
    }
    // filter
    let t = Instant::now();
    set_filter(&mut h, "crash");
    eprintln!("filter 'crash': {:?} pill={}", t.elapsed(), count_pill(&h));
    let kc = kubectl("scale", &["get", "pods", "-A", "--no-headers"]);
    eprintln!("   kubectl total pods {} with crash {}", kc.lines().count(), kc.lines().filter(|l| l.to_lowercase().contains("crash")).count());
    shot(&mut h, "scale_filter_crash");
    h.get_all_by_role(Role::TextInput).next().unwrap().set_text_value_accesskit("");
    frames(&mut h, 6);
    // other big tables
    for v in ["services", "configmaps", "secrets", "nodes", "events"] {
        let t = Instant::now();
        go_view(&mut h, v);
        let ok = wait_table(&mut h);
        frames(&mut h, 8);
        eprintln!("scale {v}: ok={ok} in {:?} pill={} ui={:?}", t.elapsed(), count_pill(&h), h.state().last_ui);
        shot(&mut h, &format!("scale_{v}"));
    }
    // ns filter on scale
    go_view(&mut h, "pods");
    wait_table(&mut h);
    // switch back to demo via the switcher UI
    let ctxbtn = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| clean(&l) == "scale")).unwrap();
    ctxbtn.click();
    frames(&mut h, 4);
    shot(&mut h, "ctx_switcher");
    let items: Vec<_> = labels(&h).into_iter().filter(|x| x.0 == Role::Button && (x.1.contains("demo") || x.1.contains("scale") || x.1.contains("Disconnect"))).collect();
    eprintln!("switcher items: {items:?}");
    let demo = h.query_all_by_role(Role::Button).filter(|n| n.accesskit_node().label().map_or(false, |l| clean(&l) == "demo")).last().unwrap();
    demo.click();
    frames(&mut h, 8);
    let ok = settle_for(&mut h, 10, pods_ready);
    eprintln!("back on demo via switcher: active={:?} pods_ready={ok} pill={}", h.state().active, count_pill(&h));
    shot(&mut h, "demo_after_scale");
    // switch to scale again: instant (session kept)?
    let t = Instant::now();
    h.state_mut().switch_context("scale");
    let ok = settle_for(&mut h, 20, |h| count_pill(h).replace(',', "").parse::<usize>().map_or(false, |n| n > 5000));
    eprintln!("second switch to scale shows data after {:?} ok={ok}", t.elapsed());
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t14_unreachable_cluster() {
    let dead = PathBuf::from("/tmp/claude-0/-home-user/93e278fc-dd5c-59a7-8890-d5bc3d4b2292/scratchpad/qa/dead-kubeconfig");
    if !dead.exists() {
        return;
    }
    unsafe { std::env::set_var("KUBECONFIG", &dead) };
    let t0 = Instant::now();
    let opts = AppOptions { context: Some("dead".into()), view: Some("pods".into()), light: false, namespace: None, ..Default::default() };
    let mut h = Harness::builder()
        .with_size([1440.0, 900.0])
        .with_pixels_per_point(1.0)
        .wgpu()
        .build_eframe(move |cc| SkiffApp::headless(&cc.egui_ctx, opts));
    let ok = settle_for(&mut h, 40, |h| has(h, "Couldn't load this view") || has_c(h, "Reconnect"));
    eprintln!("dead cluster: failure visible after {:?} ok={ok}", t0.elapsed());
    frames(&mut h, 5);
    shot(&mut h, "dead_pods");
    let st = h.state().session().map(|s| format!("{:?}", s.status()));
    eprintln!("session status: {st:?}");
    let l: Vec<_> = labels(&h).into_iter().filter(|x| x.1.contains("Couldn") || x.1.contains("refused") || x.1.contains("connect") || x.1.contains("Reconnect") || x.1.contains("Retry") || x.1.contains("error") || x.1.contains("Connecting")).collect();
    eprintln!("labels: {l:?}");
    // other views while dead
    for v in ["nodes", "deployments.apps"] {
        go_view(&mut h, v);
        frames(&mut h, 10);
        eprintln!("view {v} while dead: couldn't-load={} loading={}", has(&h, "Couldn't load this view"), has(&h, "Loading…"));
    }
    go_view(&mut h, "pods");
    frames(&mut h, 5);
    // overview
    h.state_mut().go(View::Overview);
    frames(&mut h, 10);
    shot(&mut h, "dead_overview");
    let l: Vec<_> = labels(&h).into_iter().map(|x| x.1).filter(|l| l.len() > 3).take(40).collect();
    eprintln!("overview labels while dead: {l:?}");
    // namespace picker while dead
    ns_button(&h).click();
    frames(&mut h, 4);
    shot(&mut h, "dead_nspicker");
    h.key_press(egui::Key::Escape);
    frames(&mut h, 3);
    // Retry button
    let retry = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l == "Retry"));
    if let Some(n) = retry {
        n.click();
        frames(&mut h, 5);
        eprintln!("clicked Retry; status now {:?}", h.state().session().map(|s| format!("{:?}", s.status())));
    }
    // recover: switch to demo via palette
    unsafe { std::env::set_var("KUBECONFIG", kubeconfig_with_dead()) };
    palette_open(&mut h, "demo");
    shot(&mut h, "dead_palette");
    h.key_press(egui::Key::Enter);
    frames(&mut h, 6);
    eprintln!("active after switching away: {:?}", h.state().active);
    let ok = settle_for(&mut h, 15, pods_ready);
    eprintln!("recovered demo pods: {ok}");
    {
        let s = h.state().session().unwrap().clone();
        let snap = s.table(&skiff_core::kinds::pods()).snapshot();
        eprintln!("demo after dead: status={:?} table state={:?} err={:?} rows={}", s.status(), snap.state, snap.error, snap.rows.len());
        shot(&mut h, "dead_then_demo");
    }
    // go back to the dead one
    h.state_mut().switch_context("dead");
    frames(&mut h, 10);
    eprintln!("dead again immediately shows: failed={} couldn't={}", has_c(&h, "Reconnect"), has(&h, "Couldn't load this view"));
}

fn kubeconfig_with_dead() -> PathBuf {
    PathBuf::from("/tmp/claude-0/-home-user/93e278fc-dd5c-59a7-8890-d5bc3d4b2292/scratchpad/qa/dead-kubeconfig")
}

#[test]
#[ignore = "exploratory QA flow; see the note at the top of this file"]
fn t15_layouts() {
    if !setup() {
        return;
    }
    for (name, size, light) in [("small", [1000.0f32, 640.0f32], false), ("large", [2200.0, 1300.0], false), ("light", [1440.0, 900.0], true)] {
        let mut h = harness_sized("demo", "pods", size, light);
        settle(&mut h, "pods", pods_ready);
        frames(&mut h, 8);
        shot(&mut h, &format!("lay_{name}_pods"));
        eprintln!("{name}: pods headers {:?}", table_snapshot(&h).0);
        open_obj(&mut h, "pods", "incidents", &{
            let k = kubectl("demo", &["get", "pods", "-n", "incidents", "-l", "app=crashy", "-o", "name"]);
            k.lines().next().unwrap_or("").trim_start_matches("pod/").to_string()
        });
        frames(&mut h, 6);
        shot(&mut h, &format!("lay_{name}_pod_detail"));
        eprintln!("{name}: headers with detail {:?}", table_snapshot(&h).0);
        click_tip(&mut h, "Logs");
        settle_for(&mut h, 8, |h| has_c(h, "streaming"));
        frames(&mut h, 6);
        shot(&mut h, &format!("lay_{name}_logs"));
        tab(&mut h, "YAML");
        shot(&mut h, &format!("lay_{name}_yaml"));
        go_view(&mut h, "nodes");
        wait_table(&mut h);
        frames(&mut h, 6);
        shot(&mut h, &format!("lay_{name}_nodes"));
        h.state_mut().go(View::Overview);
        frames(&mut h, 8);
        shot(&mut h, &format!("lay_{name}_overview"));
        go_view(&mut h, "events");
        wait_table(&mut h);
        frames(&mut h, 6);
        shot(&mut h, &format!("lay_{name}_events"));
        go_view(&mut h, "customresourcedefinitions.apiextensions.k8s.io");
        wait_table(&mut h);
        frames(&mut h, 6);
        shot(&mut h, &format!("lay_{name}_crds"));
        palette_open(&mut h, "dep");
        shot(&mut h, &format!("lay_{name}_palette"));
        h.key_press(egui::Key::Escape);
        frames(&mut h, 3);
        click_button_prefix(&mut h, "\u{e2c7}");
        frames(&mut h, 4);
        shot(&mut h, &format!("lay_{name}_nspicker"));
    }
}

fn click_button_prefix(h: &mut H, prefix: &str) {
    let n = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().map_or(false, |l| l.starts_with(prefix))).expect("button");
    n.click();
}
