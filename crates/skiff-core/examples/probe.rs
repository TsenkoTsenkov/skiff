//! Smoke test against a real cluster:
//!   cargo run -p skiff-core --example probe -- <context> [plural[.group]] [watch-seconds]
use std::time::{Duration, Instant};

use skiff_core::{load_contexts, ConnStatus, Core, LoadState, Notifier, BUILTINS};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let contexts = load_contexts().expect("kubeconfig");
    println!("contexts: {:?} (current {:?})", contexts.items.iter().map(|c| &c.name).collect::<Vec<_>>(), contexts.current);
    let context = args.get(1).cloned().or(contexts.current.clone()).expect("no context");
    let wanted = args.get(2).map(String::as_str).unwrap_or("pods");
    let watch_secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);

    let core = Core::new(Notifier::noop()).unwrap();
    let t0 = Instant::now();
    let session = core.connect(&context);
    loop {
        match &*session.status() {
            ConnStatus::Connecting => std::thread::sleep(Duration::from_millis(5)),
            ConnStatus::Connected { version } => {
                println!("connected to {context}: {version} in {:?}", t0.elapsed());
                break;
            }
            ConnStatus::Failed(e) => {
                println!("FAILED: {e}");
                return;
            }
        }
    }

    let key = BUILTINS
        .iter()
        .map(|b| b.key())
        .find(|k| k.id() == wanted || &*k.plural == wanted)
        .or_else(|| {
            // Not a built-in: wait for discovery.
            for _ in 0..500 {
                if let Some(d) = session.discovered().iter().find(|d| d.key.id() == wanted || &*d.key.plural == wanted) {
                    return Some(d.key.clone());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        })
        .expect("unknown resource");

    let t1 = Instant::now();
    let mut first_rows = None;
    let snap = loop {
        let snap = session.table(&key).snapshot();
        if first_rows.is_none() && !snap.rows.is_empty() {
            first_rows = Some((t1.elapsed(), snap.rows.len()));
        }
        match snap.state {
            LoadState::Live | LoadState::Polling => break snap,
            LoadState::Failed => {
                println!("table failed: {:?} (code {})", snap.error, snap.error_code);
                return;
            }
            _ => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    println!(
        "{}: {} rows, state {:?}, first rows after {:?}, complete after {:?}",
        key.id(),
        snap.rows.len(),
        snap.state,
        first_rows.map(|f| f.0),
        t1.elapsed()
    );
    let cols: Vec<String> = snap
        .columns
        .iter()
        .map(|c| format!("{}{}", c.title, if c.visible { "" } else { "*" }))
        .collect();
    println!("columns (* = hidden by default): {}", cols.join(" | "));
    let mut rows = snap.rows.clone();
    if let Some((col, desc)) = snap.default_sort {
        rows.sort_by(|a, b| {
            let o = skiff_core::compare(a, b, &snap.columns[col]);
            if desc { o.reverse() } else { o }
        });
    }
    for r in rows.iter().take(8) {
        let cells: Vec<String> = snap
            .columns
            .iter()
            .map(|c| match c.source {
                skiff_core::ColSource::Age => skiff_core::fmt_age(skiff_core::now_unix() - r.created),
                _ => match r.cell(c) {
                    Some(cell) if cell.text.is_empty() && !cell.key.is_nan() && c.kind == skiff_core::ColKind::Time => {
                        format!("~{}", skiff_core::fmt_age(skiff_core::now_unix() - cell.key as i64))
                    }
                    _ => r.text(c).to_string(),
                },
            })
            .collect();
        println!("  [{:?}] {}", r.tone, cells.join(" | "));
    }
    let bytes: usize = snap
        .rows
        .iter()
        .map(|r| {
            std::mem::size_of::<skiff_core::Row>()
                + r.name.len() + r.namespace.len() + r.uid.len() + r.rv.len() + r.labels.len() + r.search.len()
                + r.cells.iter().map(|c| std::mem::size_of::<skiff_core::Cell>() + c.text.len()).sum::<usize>()
        })
        .sum();
    println!("approx row memory: {:.1} MiB ({} B/row)", bytes as f64 / 1048576.0, bytes / snap.rows.len().max(1));

    let start_gen = snap.generation;
    std::thread::sleep(Duration::from_secs(watch_secs));
    let later = session.table(&key).snapshot();
    println!(
        "after {watch_secs}s: {} rows, {} snapshot updates, state {:?}",
        later.rows.len(),
        later.generation - start_gen,
        later.state
    );
    println!("discovered {} resource types; {} active watches", session.discovered().len(), session.active_watches());
    let m = session.metrics();
    std::thread::sleep(Duration::from_millis(1500));
    let m2 = session.metrics();
    println!("metrics available: {} -> {} ({} pods, {} nodes)", m.available, m2.available, m2.pods.len(), m2.nodes.len());
}
