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
    let opts = AppOptions { context: Some(context.to_string()), view: Some(view.to_string()), light };
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
fn t00_dump() {
    if !setup() {
        return;
    }
    let mut h = harness("demo", "pods");
    settle(&mut h, "pods", pods_ready);
    dump(&h);
}

#[test]
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
fn t02_sort_keys() {
    use skiff_core::cells::sort_key;
    for t in ["10.0.0.161", "10.0.0.93", "192.168.1.5", "1.50", "150m", "12Gi", "3 (5m ago)", "31 (55s ago)", "1/1", "0/1", "5d3h", "2m17s", "10.0.0.1,10.0.0.2", "3.7.1", "443/TCP", "80, 443", "1.2.3"] {
        eprintln!("sort_key({t:?}) = {}", sort_key(t));
    }
}

#[test]
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

pub fn tooltip_button(h: &H, tip: &str) -> String {
    // icon buttons carry their tooltip text as accessible label
    let _ = tip;
    String::new()
}

#[test]
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
