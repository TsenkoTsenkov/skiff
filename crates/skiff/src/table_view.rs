//! The live resource table: filtering, sorting and virtualized rendering.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};
use egui_material_icons::icons;
use skiff_core::{
    compare, fmt_age, fmt_bytes, fmt_cpu, ColKind, ColSource, Column, LoadState, Metrics, ResourceKey, Row,
    Snapshot, Tone,
};

use crate::theme::{Palette, ROW_HEIGHT};
use crate::widgets;

/// Per-resource-type view settings the user can change.
#[derive(Clone, Debug, Default)]
pub struct TableUi {
    pub filter: String,
    /// (column title, descending). `None` uses the view's default.
    pub sort: Option<(String, bool)>,
    /// uid of the selected row.
    pub selected: Option<Box<str>>,
    /// User overrides of default column visibility, by title.
    pub col_visible: HashMap<String, bool>,
    /// Widths the user dragged columns to, by title. A column without one
    /// gets its default width (fitted to its content; the name column takes
    /// the space left over).
    pub widths: HashMap<String, f32>,
    /// Set to scroll the selected row into view on the next frame.
    pub scroll_to_selected: bool,
    /// uid of the row the open context menu was opened on.
    pub menu_uid: Option<Box<str>>,
    /// Default widths fitted to the content, by title, and how complete the
    /// data was when they were measured.
    fitted: HashMap<String, f32>,
    fitted_for: Fit,
}

impl TableUi {
    /// Back to the default columns and widths.
    pub fn reset_columns(&mut self) {
        self.col_visible.clear();
        self.widths.clear();
        self.fitted.clear();
        self.fitted_for = Fit::None;
    }
}

/// How much of a table the fitted widths were measured on. They are measured
/// on the first rows to arrive, again once the table is complete, and then
/// left alone: columns that keep changing width under the pointer are worse
/// than columns that are a little too narrow for a row that arrived later.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Fit {
    #[default]
    None,
    Partial,
    Complete,
}

/// A column as displayed: either from the snapshot or computed in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DCol {
    Real(usize),
    PodCpu,
    PodMem,
}

impl DCol {
    pub fn title<'a>(&self, snap: &'a Snapshot) -> &'a str {
        match self {
            DCol::Real(i) => &snap.columns[*i].title,
            DCol::PodCpu => "CPU",
            DCol::PodMem => "Memory",
        }
    }
}

fn col_visible(tui: &TableUi, col: &Column) -> bool {
    tui.col_visible.get(&*col.title).copied().unwrap_or(col.visible)
}

/// Narrowest a column can be dragged to.
pub const MIN_COL_WIDTH: f32 = 40.0;
/// Widest a column can be dragged to.
pub const MAX_COL_WIDTH: f32 = 1600.0;
/// Narrowest the flexible (name) column ever gets.
const MIN_FLEX_WIDTH: f32 = 140.0;
/// Room the flexible column keeps (up to what its content needs) before
/// less important columns are left out to make it: names are what a table
/// is read by.
const COMFORT_FLEX_WIDTH: f32 = 280.0;
/// Widest a fitted default gets (a user can drag it wider).
const MAX_FIT_WIDTH: f32 = 340.0;

/// Fallback width of a column before its content has been measured, and
/// whether it absorbs leftover space.
fn preferred_width(col: &Column) -> (f32, bool) {
    let title = col.title.to_ascii_lowercase();
    let w = match (col.source, col.kind) {
        (ColSource::Name, _) => return (200.0, true),
        (ColSource::Namespace, _) => 120.0,
        (ColSource::Age, _) => 56.0,
        (ColSource::Owner, _) => 170.0,
        (_, ColKind::Time) => 80.0,
        (_, ColKind::Number) => 70.0,
        (_, ColKind::Status) => {
            if title == "ready" || title == "type" {
                72.0
            } else {
                160.0
            }
        }
        _ => match title.as_str() {
            "message" => return (240.0, true),
            "node" => 150.0,
            "object" => 220.0,
            "ip" | "cluster-ip" | "external-ip" | "internal-ip" => 104.0,
            "restarts" | "up-to-date" | "roles" | "version" | "completions" | "desired" | "current" => 84.0,
            "images" | "selector" | "containers" | "port(s)" | "hosts" => 180.0,
            _ => 120.0,
        },
    };
    (w, false)
}

/// Whether a column takes the space the others leave (unless the user has
/// given it a width).
fn is_flexible(c: &DCol, snap: &Snapshot) -> bool {
    match c {
        DCol::Real(i) => preferred_width(&snap.columns[*i]).1,
        _ => false,
    }
}

/// Columns whose cells are drawn specially, at a fixed width.
fn fixed_width(key: &ResourceKey, c: &DCol, snap: &Snapshot) -> Option<f32> {
    match c {
        DCol::PodCpu | DCol::PodMem => Some(76.0),
        // Usage bar and a percentage.
        DCol::Real(i) if key.is("", "nodes") && matches!(&*snap.columns[*i].title, "CPU" | "Memory") => Some(96.0),
        DCol::Real(i) if snap.columns[*i].source == ColSource::Age => Some(64.0),
        _ => None,
    }
}

/// The width a column has when the user has not dragged it.
fn default_width(key: &ResourceKey, c: &DCol, snap: &Snapshot, tui: &TableUi) -> f32 {
    if let Some(w) = fixed_width(key, c, snap) {
        return w;
    }
    let title = c.title(snap);
    match (tui.fitted.get(title), c) {
        (Some(w), _) => *w,
        (None, DCol::Real(i)) => preferred_width(&snap.columns[*i]).0,
        (None, _) => 76.0,
    }
}

/// How readily a column is dropped when the table is narrow (0 = never).
fn droppability(c: &DCol, snap: &Snapshot) -> u8 {
    match c {
        DCol::PodCpu | DCol::PodMem => 6,
        DCol::Real(i) => {
            let col = &snap.columns[*i];
            match (col.source, col.kind) {
                (ColSource::Name, _) => 0,
                // Names repeat across namespaces, so keep the namespace
                // longer than the age or readiness counts.
                (ColSource::Namespace, _) => 2,
                (ColSource::Age, _) => 3,
                (ColSource::Owner, _) => 7,
                (_, ColKind::Status) if col.title.eq_ignore_ascii_case("ready") => 4,
                (_, ColKind::Status) => 1,
                _ if col.title.eq_ignore_ascii_case("message") => 0,
                _ if col.title.eq_ignore_ascii_case("object") => 2,
                _ => 5,
            }
        }
    }
}

/// The columns ticked in the column chooser, before any are dropped for
/// lack of space. Never empty for a table that has columns.
pub fn chosen_columns(key: &ResourceKey, snap: &Snapshot, tui: &TableUi, metrics: &Metrics) -> Vec<DCol> {
    let mut out = Vec::with_capacity(snap.columns.len() + 2);
    let pod_metrics = key.is("", "pods") && metrics.available;
    let shown = |title: &str| tui.col_visible.get(title).copied().unwrap_or(true);
    for (i, col) in snap.columns.iter().enumerate() {
        if pod_metrics && col.source == ColSource::Age {
            if shown("CPU") {
                out.push(DCol::PodCpu);
            }
            if shown("Memory") {
                out.push(DCol::PodMem);
            }
        }
        if col_visible(tui, col) {
            out.push(DCol::Real(i));
        }
    }
    if out.is_empty() && !snap.columns.is_empty() {
        // A remembered choice can hide everything (the chooser itself keeps
        // the last column ticked): a table always shows something.
        let name = snap.columns.iter().position(|c| c.source == ColSource::Name);
        out.push(DCol::Real(name.unwrap_or(0)));
    }
    out
}

/// A column as laid out: what it shows and how wide it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Laid {
    pub col: DCol,
    pub width: f32,
    /// Takes the space the other columns leave.
    pub flexible: bool,
    /// How narrow a drag may make it. For the flexible column this is the
    /// room the layout keeps for it: any narrower and the layout would
    /// leave another column out to give it back.
    pub floor: f32,
}

/// Lays the chosen columns out in `available` points (column gaps of `gap`
/// included).
///
/// Columns keep the width the user gave them, or else their default width;
/// the flexible column (the name) takes what is left. When that does not
/// fit (the window is narrow, the detail panel is open), the flexible column
/// shrinks to its minimum first, then the least important columns are left
/// out, and only then are the remaining ones squeezed. A table never runs
/// off the right edge, and a column the user widened is never the reason
/// another one disappears while they drag (see `max_drag_width`).
pub fn layout_columns(
    key: &ResourceKey,
    snap: &Snapshot,
    tui: &TableUi,
    metrics: &Metrics,
    available: f32,
    gap: f32,
) -> Vec<Laid> {
    let mut cols: Vec<Laid> = chosen_columns(key, snap, tui, metrics)
        .into_iter()
        .map(|c| {
            let user = tui.widths.get(c.title(snap)).copied();
            let flexible = user.is_none() && is_flexible(&c, snap);
            let width = user.unwrap_or_else(|| default_width(key, &c, snap, tui));
            // What the flexible column needs to be readable: its content,
            // within limits.
            let floor = if flexible {
                let fit = tui.fitted.get(c.title(snap)).copied().unwrap_or(MIN_FLEX_WIDTH);
                fit.clamp(MIN_FLEX_WIDTH, COMFORT_FLEX_WIDTH)
            } else {
                MIN_COL_WIDTH
            };
            Laid { col: c, width, flexible, floor }
        })
        .collect();
    let comfort = |l: &Laid| -> f32 { l.floor };
    let need = |cols: &[Laid]| -> f32 {
        cols.iter().map(|l| if l.flexible { comfort(l) } else { l.width } + gap).sum::<f32>() - gap
    };
    while need(&cols) > available {
        let victim = cols
            .iter()
            .enumerate()
            .filter(|(_, l)| droppability(&l.col, snap) > 0)
            .max_by_key(|(i, l)| (droppability(&l.col, snap), *i))
            .map(|(i, _)| i);
        match victim {
            Some(i) if cols.len() > 1 => {
                cols.remove(i);
            }
            _ => break,
        }
    }
    let fixed: f32 = cols.iter().filter(|l| !l.flexible).map(|l| l.width + gap).sum();
    let flex_count = cols.iter().filter(|l| l.flexible).count();
    let gaps_of_flex = gap * flex_count as f32;
    if flex_count > 0 {
        let share = ((available - fixed - gaps_of_flex + gap) / flex_count as f32).floor();
        for l in cols.iter_mut().filter(|l| l.flexible) {
            l.width = share.max(MIN_FLEX_WIDTH).max(MIN_COL_WIDTH);
        }
    }
    // Still too wide (only columns that are never dropped are left, or the
    // user made them wide): squeeze everything proportionally.
    let total: f32 = cols.iter().map(|l| l.width + gap).sum::<f32>() - gap;
    if total > available && total > 0.0 {
        let room = (available - gap * cols.len().saturating_sub(1) as f32).max(0.0);
        let sum: f32 = cols.iter().map(|l| l.width).sum();
        for l in &mut cols {
            l.width = (l.width * room / sum).floor().max(MIN_COL_WIDTH.min(l.width));
        }
    }
    cols
}

/// How wide column `index` of `cols` may be dragged without anything else
/// having to make room: the flexible column gives up space down to its
/// minimum, and nothing is ever dropped because of a drag.
pub fn max_drag_width(cols: &[Laid], index: usize, available: f32, gap: f32) -> f32 {
    let used: f32 = cols
        .iter()
        .enumerate()
        .map(|(i, l)| if l.flexible && i != index { l.floor.min(l.width) } else { l.width } + gap)
        .sum::<f32>()
        - gap;
    let w = cols.get(index).map(|l| l.width).unwrap_or(0.0);
    (w + (available - used).max(0.0)).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
}

/// Widths to store when the boundary after column `index` is dragged by
/// `delta` points from where it was when the drag started (`start`).
///
/// The boundary works like a splitter: what the column gains is taken from
/// the columns to its right (the next one first, each down to its minimum),
/// and what it gives up goes to the next column. The boundary therefore
/// stays under the pointer, columns to the left never move, and the table
/// keeps its total width. Only the last column, with nothing to its right,
/// grows into whatever room the flexible column can give.
/// Flexible columns are not given a width: they take what is left, which
/// comes out right because the total does not change.
pub fn drag_widths(start: &[Laid], index: usize, delta: f32, available: f32, gap: f32) -> Vec<(usize, f32)> {
    let Some(this) = start.get(index) else { return Vec::new() };
    let min_of = |l: &Laid| l.floor.min(l.width);
    let mut out = Vec::new();
    let put = |out: &mut Vec<(usize, f32)>, i: usize, w: f32| {
        if !start[i].flexible {
            out.push((i, w.round()));
        }
    };
    if index + 1 == start.len() {
        let max = max_drag_width(start, index, available, gap);
        out.push((index, (this.width + delta).clamp(MIN_COL_WIDTH.min(this.width), max).round()));
        return out;
    }
    if delta >= 0.0 {
        let mut need = delta;
        for (j, l) in start.iter().enumerate().skip(index + 1) {
            if need <= 0.0 {
                break;
            }
            let take = need.min((l.width - min_of(l)).max(0.0));
            if take > 0.0 {
                put(&mut out, j, l.width - take);
                need -= take;
            }
        }
        put(&mut out, index, this.width + delta - need);
    } else {
        let give = (-delta).min((this.width - min_of(this)).max(0.0));
        put(&mut out, index, this.width - give);
        put(&mut out, index + 1, start[index + 1].width + give);
    }
    out
}

/// Measures default widths from the content, the first time a table has
/// rows and again once it has loaded completely.
pub fn fit_widths(ui: &Ui, key: &ResourceKey, cache: &ViewCache, tui: &mut TableUi) {
    let snap = &cache.snap;
    if snap.rows.is_empty() {
        return;
    }
    let complete = matches!(snap.state, LoadState::Live | LoadState::Polling);
    let want = if complete { Fit::Complete } else { Fit::Partial };
    if tui.fitted_for == want || tui.fitted_for == Fit::Complete {
        return;
    }
    tui.fitted_for = want;
    tui.fitted.clear();
    // The longest values among a sample of rows (spread over the table so
    // one namespace's naming does not decide alone); measuring every row of
    // a 5,000-pod table is not worth it for a default.
    const SAMPLE: usize = 400;
    let n = snap.rows.len();
    let step = (n / SAMPLE).max(1);
    let body = egui::FontId::proportional(13.0);
    let header = egui::FontId::proportional(12.0);
    for col in snap.columns.iter() {
        if col.source == ColSource::Age {
            continue;
        }
        let mut longest: Vec<&str> = Vec::new();
        let mut longest_chars = 0usize;
        for row in snap.rows.iter().step_by(step) {
            let text = row.text(col);
            let chars = text.chars().count();
            if chars > longest_chars {
                longest_chars = chars;
                longest.clear();
            }
            if chars + 2 >= longest_chars && longest.len() < 6 && !text.is_empty() {
                longest.push(text);
            }
        }
        let measure = |text: &str, font: &egui::FontId| {
            ui.painter().layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE).size().x
        };
        let content = longest.iter().map(|t| measure(t, &body)).fold(0.0, f32::max);
        // Room for the sort arrow next to the title.
        let title = measure(&col.title, &header) + 20.0;
        let w = (content + 16.0).max(title).clamp(MIN_COL_WIDTH + 8.0, MAX_FIT_WIDTH);
        tui.fitted.insert(col.title.to_string(), w.ceil());
    }
    let _ = key;
}

/// Filtered + sorted row order, cached across frames.
pub struct ViewCache {
    key: Option<ResourceKey>,
    pub snap: Arc<Snapshot>,
    filter: String,
    sort: Option<(String, bool)>,
    ns: BTreeSet<String>,
    metrics_gen: u64,
    pub order: Vec<u32>,
    computed: Instant,
    cost: Duration,
}

impl Default for ViewCache {
    fn default() -> Self {
        ViewCache {
            key: None,
            snap: Arc::new(Snapshot::empty(LoadState::Connecting)),
            filter: String::new(),
            sort: None,
            ns: BTreeSet::new(),
            metrics_gen: 0,
            order: Vec::new(),
            computed: Instant::now(),
            cost: Duration::ZERO,
        }
    }
}

/// One word of the quick filter.
///
/// A plain word matches anywhere in the row. `column:value` matches only in
/// that column (`node:aks-1`, `status:crash`, `ns:kube`; a column title
/// can be shortened to any prefix, `ns` stands for the namespace and
/// `label` for the labels). A leading `!` (or `-`) keeps the rows that do
/// not match, and `column:=value` wants the whole value (so that
/// `node:=n1` does not also find `n10`). A word whose prefix names no column is matched as plain text,
/// so `http://…` still finds what it looks like.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub field: Option<Field>,
    pub text: String,
    pub negate: bool,
    /// `column:=value`: the whole value, not a part of it.
    pub exact: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Namespace,
    Labels,
    Column(usize),
}

/// Splits the filter into terms against the table's columns.
pub fn parse_filter(filter: &str, snap: &Snapshot) -> Vec<Term> {
    filter
        .to_lowercase()
        .split_whitespace()
        .filter_map(|word| {
            // `!` excludes. So does `-` in front of a column filter
            // (`-ns:shop`); in front of plain text it is part of the text,
            // so `-prod` still finds `api-prod`.
            let column_filter =
                |w: &str| w.split_once(':').is_some_and(|(key, _)| !key.is_empty() && resolve_field(key, snap).is_some());
            let (negate, word) = match word.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => match word.strip_prefix('-').filter(|rest| column_filter(rest)) {
                    Some(rest) => (true, rest),
                    None => (false, word),
                },
            };
            if word.is_empty() {
                return None;
            }
            if let Some((key, value)) = word.split_once(':') {
                if !key.is_empty() {
                    if let Some(field) = resolve_field(key, snap) {
                        let (exact, value) = match value.strip_prefix('=') {
                            Some(v) => (true, v),
                            None => (false, value),
                        };
                        return Some(Term { field: Some(field), text: value.to_string(), negate, exact });
                    }
                }
            }
            Some(Term { field: None, text: word.to_string(), negate, exact: false })
        })
        .collect()
}

fn resolve_field(key: &str, snap: &Snapshot) -> Option<Field> {
    match key {
        "ns" | "namespace" => return Some(Field::Namespace),
        "name" => return Some(Field::Name),
        "label" | "labels" | "l" => return Some(Field::Labels),
        _ => {}
    }
    let squash = |t: &str| t.to_lowercase().replace([' ', '-', '_'], "");
    let key_sq = squash(key);
    let pick = |c: &Column| match c.source {
        ColSource::Name => Field::Name,
        ColSource::Namespace => Field::Namespace,
        _ => Field::Column(snap.columns.iter().position(|x| std::ptr::eq(x, c)).unwrap_or(0)),
    };
    // Exact title first, then a title that starts with the key.
    let usable = |c: &&Column| c.source != ColSource::Age;
    if let Some(c) = snap.columns.iter().filter(usable).find(|c| squash(&c.title) == key_sq) {
        return Some(pick(c));
    }
    snap.columns.iter().filter(usable).find(|c| squash(&c.title).starts_with(&key_sq)).map(pick)
}

fn term_matches(row: &Row, snap: &Snapshot, term: &Term) -> bool {
    let test = |hay: &str| {
        if term.exact {
            hay.eq_ignore_ascii_case(&term.text) || (!hay.is_ascii() && hay.to_lowercase() == term.text)
        } else {
            contains_lower(hay, &term.text)
        }
    };
    let hit = match term.field {
        None => memfind(row.search.as_bytes(), term.text.as_bytes()),
        Some(Field::Name) => test(&row.name),
        Some(Field::Namespace) => test(&row.namespace),
        // Exact for labels means one whole `key=value` pair.
        Some(Field::Labels) if term.exact => row.labels.split(' ').any(|p| p.eq_ignore_ascii_case(&term.text)),
        Some(Field::Labels) => test(&row.labels),
        Some(Field::Column(i)) => snap.columns.get(i).is_some_and(|c| test(row.text(c))),
    };
    hit != term.negate
}

/// Case-insensitive substring test against an already lower-cased needle.
fn contains_lower(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.bytes().any(|b| b.is_ascii_uppercase()) || !hay.is_ascii() {
        memfind(hay.to_lowercase().as_bytes(), needle.as_bytes())
    } else {
        memfind(hay.as_bytes(), needle.as_bytes())
    }
}

fn matches(row: &Row, snap: &Snapshot, terms: &[Term]) -> bool {
    terms.iter().all(|t| term_matches(row, snap, t))
}

fn memfind(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        match hay[i..=last].iter().position(|b| *b == first) {
            None => return false,
            Some(p) => {
                i += p;
                if &hay[i..i + needle.len()] == needle {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}

/// Resolves the effective sort to a column and direction.
pub fn effective_sort(snap: &Snapshot, tui: &TableUi) -> (String, bool) {
    if let Some((title, desc)) = &tui.sort {
        return (title.clone(), *desc);
    }
    if let Some((idx, desc)) = snap.default_sort {
        return (snap.columns[idx].title.to_string(), desc);
    }
    ("Name".to_string(), false)
}

impl ViewCache {
    /// Brings the cache up to date. Returns how long to wait before calling
    /// again when a recompute was deferred (large tables under churn).
    pub fn update(
        &mut self,
        key: &ResourceKey,
        latest: Arc<Snapshot>,
        tui: &TableUi,
        ns_filter: &BTreeSet<String>,
        metrics: &Metrics,
    ) -> Option<Duration> {
        let (sort_title, _) = effective_sort(&latest, tui);
        // CPU / Memory show live usage (pods: absolute, nodes: share of
        // allocatable), so that is what they sort by.
        let sorting_by_metric = metrics.available
            && (key.is("", "pods") || key.is("", "nodes"))
            && matches!(sort_title.as_str(), "CPU" | "Memory");
        let inputs_changed = self.key.as_ref() != Some(key)
            || self.filter != tui.filter
            || self.sort != tui.sort
            || &self.ns != ns_filter
            || (sorting_by_metric && self.metrics_gen != metrics.generation);
        let data_changed = self.snap.generation != latest.generation || !Arc::ptr_eq(&self.snap, &latest);
        if !inputs_changed && !data_changed {
            return None;
        }
        if !inputs_changed {
            // Spend at most ~1/8 of the time re-sorting when updates stream in.
            let min_gap = self.cost * 8;
            let since = self.computed.elapsed();
            if since < min_gap {
                return Some(min_gap - since);
            }
        }

        let started = Instant::now();
        let terms = parse_filter(&tui.filter, &latest);
        let filter_ns = key.namespaced && !ns_filter.is_empty();
        let mut order: Vec<u32> = Vec::with_capacity(latest.rows.len());
        for (i, row) in latest.rows.iter().enumerate() {
            if filter_ns && !ns_filter.contains(&*row.namespace) {
                continue;
            }
            if !terms.is_empty() && !matches(row, &latest, &terms) {
                continue;
            }
            order.push(i as u32);
        }

        let (title, desc) = effective_sort(&latest, tui);
        let rows = &latest.rows;
        use std::cmp::Ordering;
        if sorting_by_metric {
            let pick = |u: skiff_core::Usage| if title == "CPU" { u.cpu } else { u.memory };
            let capacity = latest.columns.iter().find(|c| c.title.eq_ignore_ascii_case(&title));
            let nodes = key.is("", "nodes");
            // Rows without a sample get NaN and sink to the bottom.
            let keys: Vec<f64> = rows
                .iter()
                .map(|r| {
                    if nodes {
                        let cap = capacity.and_then(|c| r.cell(c)).map(|c| c.key).unwrap_or(f64::NAN);
                        match metrics.node(&r.name) {
                            Some(u) if cap > 0.0 => pick(u) / cap,
                            _ => f64::NAN,
                        }
                    } else {
                        metrics.pod(&r.namespace, &r.name).map(pick).unwrap_or(f64::NAN)
                    }
                })
                .collect();
            order.sort_by(|a, b| {
                let (ka, kb) = (keys[*a as usize], keys[*b as usize]);
                match (ka.is_nan(), kb.is_nan()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => {
                        let o = ka.partial_cmp(&kb).unwrap_or(Ordering::Equal);
                        if desc { o.reverse() } else { o }
                    }
                }
                .then_with(|| compare(&rows[*a as usize], &rows[*b as usize], &latest.columns[0]))
            });
        } else if let Some(col) = latest.columns.iter().find(|c| c.title.eq_ignore_ascii_case(&title)) {
            order.sort_by(|a, b| {
                let (ra, rb) = (&rows[*a as usize], &rows[*b as usize]);
                // Missing values go last in either direction.
                match (ra.is_blank(col), rb.is_blank(col)) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => {
                        let o = compare(ra, rb, col);
                        if desc { o.reverse() } else { o }
                    }
                }
            });
        }

        self.key = Some(key.clone());
        self.snap = latest;
        self.filter = tui.filter.clone();
        self.sort = tui.sort.clone();
        self.ns = ns_filter.clone();
        self.metrics_gen = metrics.generation;
        self.order = order;
        self.cost = started.elapsed();
        self.computed = Instant::now();
        None
    }

    pub fn row(&self, display_index: usize) -> Option<&Arc<Row>> {
        self.order.get(display_index).and_then(|i| self.snap.rows.get(*i as usize))
    }

    /// Display index of the row with this uid.
    pub fn position(&self, uid: &str) -> Option<usize> {
        self.order.iter().position(|i| &*self.snap.rows[*i as usize].uid == uid)
    }

    pub fn last_cost(&self) -> Duration {
        self.cost
    }
}

/// How long a relative time stays accurate as displayed: seconds are shown
/// for the first ten minutes, then minutes, then hours.
pub fn age_tick(youngest: Option<i64>) -> Duration {
    match youngest {
        Some(age) if age < 600 => Duration::from_secs(1),
        Some(age) if age < 3 * 3600 => Duration::from_secs(20),
        _ => Duration::from_secs(60),
    }
}

/// What the user did in the table this frame.
#[derive(Default)]
pub struct TableOutput {
    pub clicked: Option<Arc<Row>>,
    pub menu: Option<(RowAction, Arc<Row>)>,
    /// (kind, namespace, name) of an owner link that was clicked.
    pub open_owner: Option<(String, String, String)>,
    /// Youngest age (seconds) drawn this frame; decides how soon the table
    /// needs repainting to keep relative times current.
    pub youngest: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    Logs,
    Shell,
    /// A shell in the user's own terminal app.
    ShellExternal,
    Edit,
    Delete,
    CopyName,
    Restart,
    Scale,
    Forward,
    Cordon,
    Uncordon,
    Suspend,
    Resume,
    /// The pods table, narrowed to the pods on this node / in this namespace.
    ShowPods,
}

impl RowAction {
    /// Whether the action changes the cluster (or, for a shell and a port
    /// forward, opens a way to): these are not offered on a cluster that
    /// is read-only in Skiff.
    pub fn mutates(self) -> bool {
        !matches!(self, RowAction::Logs | RowAction::CopyName | RowAction::ShowPods)
    }
}

/// Which actions make sense for a resource type.
pub fn actions_for(key: &ResourceKey) -> Vec<(RowAction, egui_material_icons::MaterialIcon, &'static str)> {
    let mut v = Vec::new();
    let workload = key.group.as_ref() == "apps"
        && matches!(key.plural.as_ref(), "deployments" | "statefulsets" | "daemonsets" | "replicasets");
    let batch = key.group.as_ref() == "batch" && key.plural.as_ref() == "jobs";
    if key.is("", "pods") || workload || batch {
        v.push((RowAction::Logs, icons::ICON_SUBJECT, "Logs"));
    }
    if key.is("", "pods") {
        v.push((RowAction::Shell, icons::ICON_TERMINAL, "Shell"));
        v.push((RowAction::ShellExternal, icons::ICON_OPEN_IN_NEW, "Shell in terminal app"));
    }
    if key.is("", "pods") || key.is("", "services") {
        v.push((RowAction::Forward, icons::ICON_SWAP_HORIZ, "Port forward"));
    }
    if key.group.as_ref() == "apps" && matches!(key.plural.as_ref(), "deployments" | "statefulsets" | "replicasets") {
        v.push((RowAction::Scale, icons::ICON_UNFOLD_MORE, "Scale"));
    }
    if key.group.as_ref() == "apps" && matches!(key.plural.as_ref(), "deployments" | "statefulsets" | "daemonsets") {
        v.push((RowAction::Restart, icons::ICON_RESTART_ALT, "Restart"));
    }
    if key.is("", "nodes") || key.is("", "namespaces") {
        v.push((RowAction::ShowPods, icons::ICON_DEPLOYED_CODE, "Show pods"));
    }
    if key.is("", "nodes") {
        v.push((RowAction::Cordon, icons::ICON_BLOCK, "Cordon"));
        v.push((RowAction::Uncordon, icons::ICON_CHECK_CIRCLE, "Uncordon"));
    }
    if key.is("batch", "cronjobs") {
        v.push((RowAction::Suspend, icons::ICON_PAUSE, "Suspend"));
        v.push((RowAction::Resume, icons::ICON_PLAY_ARROW, "Resume"));
    }
    v.push((RowAction::Edit, icons::ICON_EDIT, "Edit YAML"));
    v.push((RowAction::CopyName, icons::ICON_CONTENT_COPY, "Copy name"));
    v.push((RowAction::Delete, icons::ICON_DELETE, "Delete"));
    v
}

fn cell_text(ui: &mut Ui, text: &str, color: egui::Color32) {
    ui.add(egui::Label::new(RichText::new(text).color(color)).truncate());
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    pal: &Palette,
    key: &ResourceKey,
    cache: &ViewCache,
    tui: &mut TableUi,
    laid: &[Laid],
    metrics: &Metrics,
    now: i64,
    // Why actions that change the cluster are disabled, when they are.
    locked: Option<&str>,
) -> TableOutput {
    let snap = &cache.snap;
    let mut out = TableOutput::default();
    if laid.is_empty() {
        // (A row without cells is not something the table widget can draw.)
        return out;
    }
    let cols: Vec<DCol> = laid.iter().map(|l| l.col).collect();
    let cols = &cols[..];
    let (sort_title, sort_desc) = effective_sort(snap, tui);
    let mut clicked_header: Option<String> = None;
    let selected_uid = tui.selected.clone();
    let menu_items = actions_for(key);
    let is_nodes = key.is("", "nodes");
    let available = ui.available_width();
    let gap = ui.spacing().item_spacing.x;
    let table_left = ui.cursor().left();
    let table_top = ui.cursor().top();

    ui.spacing_mut().item_spacing.y = 0.0;
    // No elastic overscroll: it is out of place in a data table, and makes
    // the table coast away from a row that was just scrolled to.
    ui.spacing_mut().scroll.kinetic.rubber_band = false;
    // The context menu belongs to a row position; rows can move under it.
    let mut menu_uid = tui.menu_uid.take();
    let mut menu_open = false;
    // Widths are decided here, every frame (`layout_columns`), and given to
    // the table as exact sizes; the table widget's own resizing (which keeps
    // its widths in its own state and fights a flexible column) is not used.
    // Resizing is done with the handles drawn after the table.
    let mut builder = TableBuilder::new(ui)
        .id_salt(("resource-table", key.id()))
        .striped(false)
        .resizable(false)
        .sense(Sense::click())
        .auto_shrink([false, false])
        .cell_layout(Layout::left_to_right(Align::Center));
    for l in laid {
        builder = builder.column(TableColumn::exact(l.width).clip(true).resizable(false));
    }
    if tui.scroll_to_selected {
        tui.scroll_to_selected = false;
        if let Some(pos) = selected_uid.as_deref().and_then(|uid| cache.position(uid)) {
            builder = builder.scroll_to_row(pos, None);
        }
    }
    let mut header_bottom = table_top;

    let scroll = builder
        .header(28.0, |mut header| {
            for c in cols {
                header.col(|ui| {
                    header_bottom = header_bottom.max(ui.max_rect().bottom());
                    let title = c.title(snap);
                    let active = title.eq_ignore_ascii_case(&sort_title);
                    let rich = RichText::new(title).size(12.0).color(if active { pal.text } else { pal.muted });
                    let clicked = if active {
                        let arrow = if sort_desc { icons::ICON_ARROW_DOWNWARD } else { icons::ICON_ARROW_UPWARD };
                        let arrow = widgets::icon(arrow, 12.0).color(pal.accent);
                        ui.add(egui::Button::new((rich, arrow)).frame(false).gap(3.0)).clicked()
                    } else {
                        ui.add(egui::Button::new(rich).frame(false)).clicked()
                    };
                    if clicked {
                        clicked_header = Some(title.to_string());
                    }
                    if ui.min_rect().width() > ui.max_rect().width() {
                        ui.response().on_hover_text(title);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, cache.order.len(), |mut table_row| {
                let Some(row) = cache.row(table_row.index()) else { return };
                let selected = selected_uid.as_deref() == Some(&*row.uid);
                table_row.set_selected(selected);
                let base = if row.deleting { pal.muted } else { pal.text };
                for (ci, c) in cols.iter().enumerate() {
                    table_row.col(|ui| {
                        if ci == 0 && row.tone >= Tone::Warn {
                            // Health marker on the row's leading edge.
                            let r = ui.max_rect();
                            let bar = egui::Rect::from_min_max(
                                egui::pos2(r.left() - 4.0, r.top() + 5.0),
                                egui::pos2(r.left() - 1.5, r.bottom() - 5.0),
                            );
                            ui.painter().rect_filled(bar, 1.5, pal.tone(row.tone));
                        }
                        match c {
                            DCol::PodCpu | DCol::PodMem => match metrics.pod(&row.namespace, &row.name) {
                                Some(u) => {
                                    let text = if *c == DCol::PodCpu { fmt_cpu(u.cpu) } else { fmt_bytes(u.memory) };
                                    cell_text(ui, &text, base);
                                }
                                None => cell_text(ui, "—", pal.faint),
                            },
                            DCol::Real(i) => {
                                let col = &snap.columns[*i];
                                match col.source {
                                    ColSource::Name => cell_text(ui, &row.name, base),
                                    ColSource::Namespace => cell_text(ui, &row.namespace, pal.muted),
                                    ColSource::Age => {
                                        let text = if row.created > 0 {
                                            let age = now - row.created;
                                            out.youngest = Some(out.youngest.map_or(age, |y| y.min(age)));
                                            fmt_age(age)
                                        } else {
                                            "—".into()
                                        };
                                        cell_text(ui, &text, pal.muted);
                                    }
                                    ColSource::Owner => match &row.owner {
                                        Some(o) => {
                                            let label = RichText::new(&*o.1).color(pal.accent);
                                            let resp = ui
                                                .add(egui::Label::new(label).truncate().sense(Sense::click()))
                                                .on_hover_text(format!("{} {}", o.0, o.1));
                                            if resp.clicked() {
                                                out.open_owner =
                                                    Some((o.0.to_string(), row.namespace.to_string(), o.1.to_string()));
                                            }
                                        }
                                        None => cell_text(ui, "—", pal.faint),
                                    },
                                    ColSource::Cell(ci) => {
                                        let Some(cell) = row.cells.get(ci) else { return };
                                        if cell.text.is_empty() {
                                            if col.kind == ColKind::Time && !cell.key.is_nan() {
                                                let age = now - cell.key as i64;
                                                out.youngest = Some(out.youngest.map_or(age, |y| y.min(age)));
                                                cell_text(ui, &fmt_age(age), pal.muted);
                                            } else {
                                                cell_text(ui, "—", pal.faint);
                                            }
                                            return;
                                        }
                                        if is_nodes && matches!(&*col.title, "CPU" | "Memory") {
                                            node_usage_cell(ui, pal, row, &col.title, cell.key, &cell.text, metrics);
                                            return;
                                        }
                                        let color = match (col.kind, cell.tone) {
                                            (ColKind::Status, Tone::Normal) => base,
                                            (ColKind::Status, t) => pal.tone(t),
                                            (_, Tone::Muted) => pal.muted,
                                            _ => base,
                                        };
                                        cell_text(ui, &cell.text, color);
                                    }
                                }
                            }
                        }
                    });
                }
                let resp = table_row.response();
                if resp.clicked() {
                    out.clicked = Some(row.clone());
                }
                if resp.secondary_clicked() {
                    menu_uid = Some(row.uid.clone());
                }
                resp.context_menu(|ui| {
                    menu_open = true;
                    // Opened on another object that has since moved away
                    // from this position (rows were added or removed above
                    // it): never act on whatever took its place.
                    if *menu_uid.get_or_insert_with(|| row.uid.clone()) != row.uid {
                        ui.close();
                        return;
                    }
                    ui.set_min_width(170.0);
                    ui.set_max_width(260.0);
                    ui.add(egui::Label::new(RichText::new(&*row.name).color(pal.muted).size(12.0)).truncate());
                    ui.separator();
                    for (action, ic, label) in &menu_items {
                        if matches!(action, RowAction::Delete) {
                            ui.separator();
                        }
                        let color = if matches!(action, RowAction::Delete) { pal.err } else { pal.text };
                        let atoms = (widgets::icon(*ic, 15.0).color(color), RichText::new(*label).color(color));
                        let reason = locked.filter(|_| action.mutates());
                        let item = ui.add_enabled(reason.is_none(), egui::Button::new(atoms).frame(false));
                        if let Some(reason) = reason {
                            item.on_disabled_hover_text(reason);
                        } else if item.clicked() {
                            out.menu = Some((*action, row.clone()));
                            ui.close();
                        }
                    }
                });
            });
        });

    let _ = scroll;
    tui.menu_uid = menu_uid.filter(|_| menu_open);

    // Column resize handles: on the boundary after each column, over the
    // header (where a drag cannot be mistaken for a click on a row).
    resize_handles(ui, pal, snap, tui, laid, table_left, table_top, header_bottom, available, gap);

    if let Some(title) = clicked_header {
        tui.sort = Some(if title.eq_ignore_ascii_case(&sort_title) {
            (title, !sort_desc)
        } else {
            (title, false)
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn resize_handles(
    ui: &mut Ui,
    pal: &Palette,
    snap: &Snapshot,
    tui: &mut TableUi,
    laid: &[Laid],
    left: f32,
    top: f32,
    header_bottom: f32,
    available: f32,
    gap: f32,
) {
    let mut x = left;
    let grab = 4.0;
    for (i, l) in laid.iter().enumerate() {
        x += l.width;
        let boundary = x + gap / 2.0;
        x += gap;
        // Nothing to drag after the last column when it reaches the edge.
        if i + 1 == laid.len() && boundary >= left + available - 2.0 {
            break;
        }
        let title = l.col.title(snap).to_string();
        let rect = egui::Rect::from_min_max(egui::pos2(boundary - grab, top), egui::pos2(boundary + grab, header_bottom));
        let id = ui.scope_id().with(("col-resize", &title));
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let resp = resp.on_hover_text_at_pointer("Drag to resize · double-click for the default width");
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
        }
        if resp.double_clicked() {
            tui.widths.remove(&title);
        } else if resp.drag_started() {
            // The layout as it is on screen is where the drag starts from;
            // each frame of the drag is worked out from it afresh.
            ui.data_mut(|d| d.insert_temp(id, std::sync::Arc::new(laid.to_vec())));
        } else if resp.dragged() {
            let start: std::sync::Arc<Vec<Laid>> =
                ui.data(|d| d.get_temp(id)).unwrap_or_else(|| std::sync::Arc::new(laid.to_vec()));
            let moved = ui.input(|inp| match (inp.pointer.press_origin(), inp.pointer.latest_pos()) {
                (Some(a), Some(b)) => b.x - a.x,
                _ => 0.0,
            });
            // (Only while the columns are the ones the drag started with.)
            if start.len() == laid.len() && start.iter().zip(laid).all(|(a, b)| a.col == b.col) {
                for (j, w) in drag_widths(&start, i, moved, available, gap) {
                    tui.widths.insert(start[j].col.title(snap).to_string(), w);
                }
            }
        }
        let color = if resp.dragged() {
            pal.accent
        } else if resp.hovered() {
            pal.muted
        } else {
            pal.border
        };
        let y = (header_bottom - top) * 0.25;
        ui.painter().line_segment(
            [egui::pos2(boundary, top + y), egui::pos2(boundary, header_bottom - y)],
            egui::Stroke::new(1.0, color),
        );
    }
}

fn node_usage_cell(ui: &mut Ui, pal: &Palette, row: &Row, title: &str, capacity: f64, text: &str, metrics: &Metrics) {
    match metrics.node(&row.name) {
        Some(u) if capacity > 0.0 => {
            let used = if title == "CPU" { u.cpu } else { u.memory };
            let frac = (used / capacity) as f32;
            widgets::usage_bar(ui, frac, 34.0, pal);
            let label = format!("{:.0}%", frac * 100.0);
            ui.add(egui::Label::new(RichText::new(label).color(pal.text)).truncate())
                .on_hover_text(if title == "CPU" {
                    format!("{} of {} cores", fmt_cpu(used), text)
                } else {
                    format!("{} of {}", fmt_bytes(used), text)
                });
        }
        _ => cell_text(ui, text, pal.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use skiff_core::Cell;

    fn col(title: &str, source: ColSource, kind: ColKind) -> Column {
        Column { title: title.into(), source, kind, visible: true }
    }

    /// A pods-like table: Name, Namespace, Status, Node, Age.
    fn pods_snapshot(rows: &[(&str, &str, &str, &str)]) -> Snapshot {
        let columns: Vec<Column> = vec![
            col("Name", ColSource::Name, ColKind::Text),
            col("Namespace", ColSource::Namespace, ColKind::Text),
            col("Status", ColSource::Cell(0), ColKind::Status),
            col("Node", ColSource::Cell(1), ColKind::Text),
            col("Age", ColSource::Age, ColKind::Time),
        ];
        let rows = rows
            .iter()
            .enumerate()
            .map(|(i, (name, ns, status, node))| {
                let search = format!("{name} {ns} {status} {node} app={name}").to_lowercase();
                Arc::new(Row {
                    uid: format!("u{i}").into(),
                    name: (*name).into(),
                    namespace: (*ns).into(),
                    created: 0,
                    deleting: false,
                    tone: Tone::Normal,
                    rv: "1".into(),
                    cells: vec![Cell::text(*status), Cell::text(*node)].into_boxed_slice(),
                    owner: None,
                    labels: format!("app={name}").into(),
                    search: search.into(),
                })
            })
            .collect();
        Snapshot { generation: 1, columns: Arc::from(columns), rows, state: LoadState::Live, error: None, error_code: 0, default_sort: None }
    }

    fn pods_key() -> ResourceKey {
        skiff_core::kinds::pods()
    }

    fn filtered(snap: &Snapshot, filter: &str) -> Vec<String> {
        let terms = parse_filter(filter, snap);
        snap.rows.iter().filter(|r| matches(r, snap, &terms)).map(|r| r.name.to_string()).collect()
    }

    #[test]
    fn column_filters() {
        let snap = pods_snapshot(&[
            ("web-1", "shop", "Running", "node-1"),
            ("web-2", "shop", "CrashLoopBackOff", "node-10"),
            ("db-0", "data", "Running", "node-1"),
        ]);
        assert_eq!(filtered(&snap, "node:node-1"), ["web-1", "web-2", "db-0"], "substring by default");
        assert_eq!(filtered(&snap, "node:=node-1"), ["web-1", "db-0"], "= wants the whole value");
        assert_eq!(filtered(&snap, "NODE:=NODE-10"), ["web-2"], "case does not matter");
        assert_eq!(filtered(&snap, "ns:data"), ["db-0"]);
        assert_eq!(filtered(&snap, "st:crash"), ["web-2"], "a prefix of the title is enough");
        assert_eq!(filtered(&snap, "!running"), ["web-2"]);
        assert_eq!(filtered(&snap, "-ns:shop"), ["db-0"]);
        // A leading hyphen on plain text is text (a fragment of a name).
        assert_eq!(filtered(&snap, "-2"), ["web-2"]);
        assert_eq!(filtered(&snap, "-0"), ["db-0"]);
        assert_eq!(filtered(&snap, "!-2"), ["web-1", "db-0"]);
        assert_eq!(filtered(&snap, "-bogus:x"), Vec::<String>::new());
        assert_eq!(filtered(&snap, "web node:=node-1"), ["web-1"], "terms combine");
        assert_eq!(filtered(&snap, "label:=app=db-0"), ["db-0"]);
        // A prefix that names no column is plain text.
        assert_eq!(filtered(&snap, "bogus:x"), Vec::<String>::new());
        assert_eq!(parse_filter("http://x", &snap)[0].field, None);
        // Age is not text, so `age:` is not a column filter.
        assert_eq!(parse_filter("age:1", &snap)[0].field, None);
    }

    #[test]
    fn layout_fills_the_width_with_the_name_column() {
        let snap = pods_snapshot(&[("a", "b", "Running", "n")]);
        let tui = TableUi::default();
        let laid = layout_columns(&pods_key(), &snap, &tui, &Metrics::default(), 1200.0, 8.0);
        assert_eq!(laid.len(), 5);
        let total: f32 = laid.iter().map(|l| l.width).sum::<f32>() + 8.0 * 4.0;
        assert!((total - 1200.0).abs() <= 4.0, "fills the width: {total}");
        assert!(laid[0].flexible);
    }

    #[test]
    fn a_dragged_width_is_kept_and_the_name_column_gives_way() {
        let snap = pods_snapshot(&[("a", "b", "Running", "n")]);
        let mut tui = TableUi::default();
        tui.widths.insert("Status".into(), 400.0);
        let laid = layout_columns(&pods_key(), &snap, &tui, &Metrics::default(), 1000.0, 8.0);
        let status = laid.iter().find(|l| l.col == DCol::Real(2)).unwrap();
        assert_eq!(status.width, 400.0);
        let total: f32 = laid.iter().map(|l| l.width).sum::<f32>() + 8.0 * (laid.len() - 1) as f32;
        assert!(total <= 1000.5, "never wider than the space: {total}");

        // The user can size the name column too; it then keeps that width.
        tui.widths.insert("Name".into(), 180.0);
        let laid = layout_columns(&pods_key(), &snap, &tui, &Metrics::default(), 1400.0, 8.0);
        assert_eq!(laid[0].width, 180.0);
        assert!(!laid[0].flexible);
    }

    #[test]
    fn narrow_tables_drop_columns_instead_of_overflowing() {
        let snap = pods_snapshot(&[("a", "b", "Running", "n")]);
        let tui = TableUi::default();
        for width in [200.0, 320.0, 480.0, 640.0, 900.0] {
            let laid = layout_columns(&pods_key(), &snap, &tui, &Metrics::default(), width, 8.0);
            let total: f32 = laid.iter().map(|l| l.width).sum::<f32>() + 8.0 * (laid.len() - 1) as f32;
            assert!(total <= width + 0.5, "{width}: {total}");
            assert!(laid.iter().any(|l| l.col == DCol::Real(0)), "{width}: the name stays");
        }
    }

    #[test]
    fn dragging_never_pushes_columns_out() {
        let snap = pods_snapshot(&[("a", "b", "Running", "n")]);
        let mut tui = TableUi::default();
        let laid = layout_columns(&pods_key(), &snap, &tui, &Metrics::default(), 900.0, 8.0);
        let before = laid.len();
        let max = max_drag_width(&laid, 2, 900.0, 8.0);
        tui.widths.insert("Status".into(), max);
        let laid = layout_columns(&pods_key(), &snap, &tui, &Metrics::default(), 900.0, 8.0);
        assert_eq!(laid.len(), before, "the widest drag still fits every column");
        let total: f32 = laid.iter().map(|l| l.width).sum::<f32>() + 8.0 * (laid.len() - 1) as f32;
        assert!(total <= 900.5, "{total}");
    }

    #[test]
    fn a_dragged_boundary_follows_the_pointer() {
        let snap = pods_snapshot(&[("a", "b", "Running", "n")]);
        let mut tui = TableUi::default();
        let key = pods_key();
        let start = layout_columns(&key, &snap, &tui, &Metrics::default(), 1100.0, 8.0);
        let edge = |laid: &[Laid], i: usize| laid[..=i].iter().map(|l| l.width + 8.0).sum::<f32>();
        for (index, delta) in [(1usize, 60.0f32), (1, -30.0), (0, -50.0), (0, 40.0), (2, 25.0)] {
            tui.widths.clear();
            for (j, w) in drag_widths(&start, index, delta, 1100.0, 8.0) {
                tui.widths.insert(start[j].col.title(&snap).to_string(), w);
            }
            let after = layout_columns(&key, &snap, &tui, &Metrics::default(), 1100.0, 8.0);
            assert_eq!(after.len(), start.len());
            let moved = edge(&after, index) - edge(&start, index);
            assert!((moved - delta).abs() < 1.5, "boundary {index} by {delta}: moved {moved}");
            for k in 0..index {
                assert!((edge(&after, k) - edge(&start, k)).abs() < 1.5, "boundary {k} left of the drag stays");
            }
            let total = edge(&after, after.len() - 1) - 8.0;
            assert!(total <= 1100.5, "{total}");
        }
    }

    #[test]
    fn narrowing_the_name_column_never_costs_another_column() {
        // Long names: the name column wants its full comfortable width.
        let snap = pods_snapshot(&[("a-pod-with-quite-a-long-name-0123456789-abcdef", "b", "Running", "n")]);
        let mut tui = TableUi::default();
        tui.fitted.insert("Name".into(), 400.0);
        let key = pods_key();
        // Just enough room for every column with the name at that width
        // and a little to spare.
        let roomy = layout_columns(&key, &snap, &tui, &Metrics::default(), 2000.0, 8.0);
        let fixed: f32 = roomy.iter().filter(|l| !l.flexible).map(|l| l.width + 8.0).sum();
        let available = fixed + COMFORT_FLEX_WIDTH + 30.0;
        let start = layout_columns(&key, &snap, &tui, &Metrics::default(), available, 8.0);
        assert_eq!(start.len(), roomy.len());
        assert!(start[0].flexible && (start[0].width - (COMFORT_FLEX_WIDTH + 30.0)).abs() < 1.5, "{:?}", start[0]);
        // Dragged far to the left, the name gives the 30 it can spare and
        // no more: every column is still there.
        for (j, w) in drag_widths(&start, 0, -200.0, available, 8.0) {
            tui.widths.insert(start[j].col.title(&snap).to_string(), w);
        }
        let after = layout_columns(&key, &snap, &tui, &Metrics::default(), available, 8.0);
        assert_eq!(after.len(), start.len(), "no column was left out");
        assert!((after[0].width - COMFORT_FLEX_WIDTH).abs() < 1.5, "{:?}", after[0]);
    }

    #[test]
    fn substring_search() {
        assert!(memfind(b"hello world", b"lo w"));
        assert!(memfind(b"hello", b""));
        assert!(!memfind(b"hello", b"world"));
        assert!(!memfind(b"hi", b"hello"));
        assert!(memfind(b"aab", b"ab"));
    }
}
