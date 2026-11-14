//! Per-kind presentation tweaks layered over the server's Table output.

use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::BTreeMap;

use crate::cells::{fmt_bytes, fmt_cpu, parse_quantity, parse_time, Cell, ColKind};
use crate::kinds::ResourceKey;

/// Receives the full object (when `include_object` is set) and may fill or
/// replace cells by column title.
pub type Enricher = fn(obj: &RawValue, created: i64, set: &mut dyn FnMut(&str, Cell));

#[derive(Clone, Copy, Default)]
pub struct Tweaks {
    /// Ask the server to embed full objects instead of metadata only.
    pub include_object: bool,
    /// "Wide" server columns to show by default.
    pub show_wide: &'static [&'static str],
    /// Default server columns to hide by default.
    pub hide: &'static [&'static str],
    /// Server columns whose values are points in time.
    pub time_cols: &'static [&'static str],
    /// Add a "Controlled By" column from owner references.
    pub owner_col: bool,
    /// The object name is not interesting (events).
    pub hide_name: bool,
    /// (column title, descending).
    pub default_sort: Option<(&'static str, bool)>,
    pub enrich: Option<Enricher>,
    /// Extra columns filled in by the enricher.
    pub extra: &'static [(&'static str, ColKind)],
}

pub fn tweaks_for(key: &ResourceKey) -> Tweaks {
    match (&*key.group, &*key.plural) {
        ("", "pods") => Tweaks {
            show_wide: &["IP", "Node"],
            owner_col: true,
            ..Default::default()
        },
        ("", "nodes") => Tweaks {
            include_object: true,
            show_wide: &["Internal-IP"],
            enrich: Some(enrich_node),
            extra: &[
                ("Taints", ColKind::Number),
                ("CPU", ColKind::Number),
                ("Memory", ColKind::Number),
                ("Pods", ColKind::Number),
            ],
            ..Default::default()
        },
        ("", "events") | ("events.k8s.io", "events") => Tweaks {
            include_object: true,
            hide_name: true,
            show_wide: &["Count"],
            time_cols: &["Last Seen", "First Seen"],
            default_sort: Some(("Last Seen", false)),
            enrich: Some(enrich_event),
            ..Default::default()
        },
        ("apps", "replicasets") | ("batch", "jobs") => Tweaks { owner_col: true, ..Default::default() },
        ("apiextensions.k8s.io", "customresourcedefinitions") => Tweaks {
            show_wide: &["Group", "Kind"],
            hide: &["Created At"],
            ..Default::default()
        },
        _ => Tweaks::default(),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct EventLite {
    #[serde(default)]
    last_timestamp: Option<String>,
    #[serde(default)]
    first_timestamp: Option<String>,
    #[serde(default)]
    event_time: Option<String>,
    #[serde(default)]
    series: Option<EventSeries>,
    #[serde(default)]
    deprecated_last_timestamp: Option<String>,
    #[serde(default)]
    deprecated_first_timestamp: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct EventSeries {
    #[serde(default)]
    last_observed_time: Option<String>,
}

fn first_time(candidates: &[&Option<String>]) -> i64 {
    candidates
        .iter()
        .filter_map(|c| c.as_deref())
        .map(parse_time)
        .find(|t| *t > 0)
        .unwrap_or(0)
}

fn enrich_event(obj: &RawValue, created: i64, set: &mut dyn FnMut(&str, Cell)) {
    let ev: EventLite = serde_json::from_str(obj.get()).unwrap_or_default();
    let series_last = ev.series.and_then(|s| s.last_observed_time);
    let mut last = first_time(&[&series_last, &ev.last_timestamp, &ev.deprecated_last_timestamp, &ev.event_time]);
    if last == 0 {
        last = created;
    }
    let mut first = first_time(&[&ev.first_timestamp, &ev.deprecated_first_timestamp, &ev.event_time]);
    if first == 0 {
        first = created;
    }
    set("Last Seen", Cell::time(last));
    set("First Seen", Cell::time(first));
}

#[derive(Deserialize, Default)]
struct NodeLite {
    #[serde(default)]
    spec: NodeSpecLite,
    #[serde(default)]
    status: NodeStatusLite,
}

#[derive(Deserialize, Default)]
struct NodeSpecLite {
    #[serde(default)]
    taints: Option<Vec<serde::de::IgnoredAny>>,
}

#[derive(Deserialize, Default)]
struct NodeStatusLite {
    #[serde(default)]
    allocatable: BTreeMap<String, String>,
}

fn enrich_node(obj: &RawValue, _created: i64, set: &mut dyn FnMut(&str, Cell)) {
    let node: NodeLite = serde_json::from_str(obj.get()).unwrap_or_default();
    let taints = node.spec.taints.map(|t| t.len()).unwrap_or(0);
    set(
        "Taints",
        if taints == 0 { Cell::empty() } else { Cell::num(taints as f64, taints.to_string()) },
    );
    let q = |k: &str| node.status.allocatable.get(k).and_then(|v| parse_quantity(v));
    match q("cpu") {
        Some(c) => set("CPU", Cell::num(c, fmt_cpu(c))),
        None => set("CPU", Cell::empty()),
    }
    match q("memory") {
        Some(m) => set("Memory", Cell::num(m, fmt_bytes(m))),
        None => set("Memory", Cell::empty()),
    }
    match q("pods") {
        Some(p) => set("Pods", Cell::num(p, format!("{p:.0}"))),
        None => set("Pods", Cell::empty()),
    }
}
