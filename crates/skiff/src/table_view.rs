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
    /// Set to scroll the selected row into view on the next frame.
    pub scroll_to_selected: bool,
    /// Scroll position and column count of the last frame, to carry the
    /// position over when the set of columns (and so the table's stored
    /// state) changes.
    pub scroll_y: f32,
    pub last_cols: usize,
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

/// Preferred width of a column and whether it absorbs leftover space.
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

fn dcol_width(c: &DCol, snap: &Snapshot) -> (f32, bool) {
    match c {
        DCol::Real(i) => preferred_width(&snap.columns[*i]),
        DCol::PodCpu | DCol::PodMem => (68.0, false),
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

/// Columns to draw, in order. When `available` is too narrow for all of
/// them (e.g. the detail panel is open) the least important ones are left
/// out instead of clipping the table.
pub fn display_columns(key: &ResourceKey, snap: &Snapshot, tui: &TableUi, metrics: &Metrics, available: f32) -> Vec<DCol> {
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
    const GAP: f32 = 8.0;
    let mut total: f32 = out.iter().map(|c| dcol_width(c, snap).0 + GAP).sum();
    while total > available {
        let victim = out
            .iter()
            .enumerate()
            .filter(|(_, c)| droppability(c, snap) > 0)
            .max_by_key(|(i, c)| (droppability(c, snap), *i))
            .map(|(i, _)| i);
        match victim {
            Some(i) => {
                total -= dcol_width(&out[i], snap).0 + GAP;
                out.remove(i);
            }
            None => break,
        }
    }
    out
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

fn matches(row: &Row, terms: &[String]) -> bool {
    terms.iter().all(|t| memfind(row.search.as_bytes(), t.as_bytes()))
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
        let terms: Vec<String> = tui.filter.to_lowercase().split_whitespace().map(str::to_string).collect();
        let filter_ns = key.namespaced && !ns_filter.is_empty();
        let mut order: Vec<u32> = Vec::with_capacity(latest.rows.len());
        for (i, row) in latest.rows.iter().enumerate() {
            if filter_ns && !ns_filter.contains(&*row.namespace) {
                continue;
            }
            if !terms.is_empty() && !matches(row, &terms) {
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
    cols: &[DCol],
    metrics: &Metrics,
    now: i64,
) -> TableOutput {
    let snap = &cache.snap;
    let mut out = TableOutput::default();
    let (sort_title, sort_desc) = effective_sort(snap, tui);
    let mut clicked_header: Option<String> = None;
    let selected_uid = tui.selected.clone();
    let menu_items = actions_for(key);
    let is_nodes = key.is("", "nodes");

    ui.spacing_mut().item_spacing.y = 0.0;
    let mut builder = TableBuilder::new(ui)
        // One stored state per column count, so widths the user dragged
        // survive the detail panel opening and closing (which changes how
        // many columns fit). The scroll position is carried over below.
        .id_salt(("resource-table", key.id(), cols.len()))
        .striped(false)
        .resizable(true)
        .sense(Sense::click())
        .auto_shrink([false, false])
        .cell_layout(Layout::left_to_right(Align::Center));
    for c in cols {
        let (width, flexible) = dcol_width(c, snap);
        builder = builder.column(if flexible {
            // Not resizable: a resizable column keeps its stored width, so
            // it would not give space back when the table gets narrower
            // (detail panel opening) and would push the rest off-screen.
            TableColumn::remainder().at_least(width.min(160.0)).clip(true).resizable(false)
        } else {
            TableColumn::initial(width).at_least(36.0).clip(true)
        });
    }
    if tui.last_cols != cols.len() {
        if tui.last_cols != 0 {
            builder = builder.vertical_scroll_offset(tui.scroll_y);
        }
        tui.last_cols = cols.len();
    }
    if tui.scroll_to_selected {
        tui.scroll_to_selected = false;
        if let Some(pos) = selected_uid.as_deref().and_then(|uid| cache.position(uid)) {
            builder = builder.scroll_to_row(pos, None);
        }
    }

    let scroll = builder
        .header(28.0, |mut header| {
            for c in cols {
                header.col(|ui| {
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
                resp.context_menu(|ui| {
                    ui.set_min_width(170.0);
                    for (action, ic, label) in &menu_items {
                        if matches!(action, RowAction::Delete) {
                            ui.separator();
                        }
                        let color = if matches!(action, RowAction::Delete) { pal.err } else { pal.text };
                        let atoms = (widgets::icon(*ic, 15.0).color(color), RichText::new(*label).color(color));
                        if ui.add(egui::Button::new(atoms).frame(false)).clicked() {
                            out.menu = Some((*action, row.clone()));
                            ui.close();
                        }
                    }
                });
            });
        });

    tui.scroll_y = scroll.state.clamped_offset().y;

    if let Some(title) = clicked_header {
        tui.sort = Some(if title.eq_ignore_ascii_case(&sort_title) {
            (title, !sort_desc)
        } else {
            (title, false)
        });
    }
    out
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

    #[test]
    fn substring_search() {
        assert!(memfind(b"hello world", b"lo w"));
        assert!(memfind(b"hello", b""));
        assert!(!memfind(b"hello", b"world"));
        assert!(!memfind(b"hi", b"hello"));
        assert!(memfind(b"aab", b"ab"));
    }
}
