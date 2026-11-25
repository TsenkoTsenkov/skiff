//! The Metrics tab of the detail panel, and what the app shows about
//! firing alerts.
//!
//! With a Prometheus in the cluster: CPU, memory, network and restarts over
//! a chosen time range. Without one: CPU and memory for as long as Skiff has
//! been watching (metrics-server samples), and one line on how to get more.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Ui};
use egui_material_icons::icons;
use skiff_core::kinds;
use skiff_core::prom::{Chart, ChartTarget, PlotSeries, Unit};
use skiff_core::session::is_critical;
use skiff_core::util::enc;
use skiff_core::{
    fmt_age, now_unix, Alert, AlertKind, AlertTarget, Alerts, Job, Prom, PromError, PromStatus, ResourceKey, Session,
    TimeRange, UsageHistory,
};

use crate::chart;
use crate::theme::Palette;
use crate::widgets;

/// How far back the charts go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Span {
    Hour,
    SixHours,
    Day,
    Week,
}

impl Span {
    pub const ALL: [Span; 4] = [Span::Hour, Span::SixHours, Span::Day, Span::Week];

    pub fn label(self) -> &'static str {
        match self {
            Span::Hour => "1h",
            Span::SixHours => "6h",
            Span::Day => "24h",
            Span::Week => "7d",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Span::Hour => "The last hour",
            Span::SixHours => "The last 6 hours",
            Span::Day => "The last 24 hours",
            Span::Week => "The last 7 days",
        }
    }

    pub fn secs(self) -> u64 {
        match self {
            Span::Hour => 3600,
            Span::SixHours => 6 * 3600,
            Span::Day => 86_400,
            Span::Week => 7 * 86_400,
        }
    }

    /// How often the charts are loaded again while they are on screen:
    /// about once per new point, and never faster than every 30 seconds.
    pub fn refresh(self) -> Duration {
        Duration::from_secs(match self {
            Span::Hour => 30,
            Span::SixHours => 120,
            Span::Day => 300,
            Span::Week => 900,
        })
    }

    /// The range as Grafana writes it.
    fn grafana_from(self) -> &'static str {
        match self {
            Span::Hour => "now-1h",
            Span::SixHours => "now-6h",
            Span::Day => "now-24h",
            Span::Week => "now-7d",
        }
    }
}

/// What the tab is about.
pub struct Subject<'a> {
    /// The cluster's context name (for the config key in the hint).
    pub context: &'a str,
    /// The namespace of the object (empty for nodes): where to look for
    /// Prometheus when the account cannot search the whole cluster.
    pub namespace: &'a str,
    /// `None` while the pods of a workload are still being listed.
    pub target: Option<ChartTarget>,
    /// `clusters.<context>.grafana_url` from the config file.
    pub grafana_url: Option<&'a str>,
}

/// What a set of charts was loaded for.
type Key = (ChartTarget, Span, String);

struct Loaded {
    key: Key,
    range: TimeRange,
    charts: Vec<Chart>,
    at: Instant,
}

/// Charts of one subject are not asked for again sooner than this after
/// the last request, whatever changed: a workload whose pods come and go
/// (a rollout, a crash loop) would otherwise start a new set of queries
/// with every change.
const RELOAD_GAP: Duration = Duration::from_secs(15);

/// The metrics-server history of a subject, as it was at one generation
/// of the metrics (a new one arrives every 15 seconds; a frame is drawn
/// far more often).
struct UsageCache {
    generation: u64,
    target: ChartTarget,
    at: Instant,
    /// When the oldest sample was taken.
    first: Option<i64>,
    cpu: [PlotSeries; 1],
    memory: [PlotSeries; 1],
}

/// The history is read again at least this often, which also tells the
/// session that it is still wanted when no new samples arrive.
const USAGE_CACHE_MAX_AGE: Duration = Duration::from_secs(60);

impl UsageCache {
    fn new(generation: u64, target: &ChartTarget, history: UsageHistory) -> Self {
        let series = |points| [PlotSeries { name: "used".into(), reference: false, points }];
        UsageCache {
            generation,
            target: target.clone(),
            at: Instant::now(),
            first: history.cpu.first().map(|p| p.0 as i64),
            cpu: series(history.cpu),
            memory: series(history.memory),
        }
    }

    fn is_for(&self, generation: u64, target: &ChartTarget) -> bool {
        self.generation == generation && self.target == *target && self.at.elapsed() < USAGE_CACHE_MAX_AGE
    }
}

/// State of the Metrics tab of one object.
pub struct MetricsView {
    span: Span,
    job: Option<(Key, TimeRange, Job<Vec<Chart>>)>,
    loaded: Option<Loaded>,
    /// What the last request was for, and when it was made.
    started: Option<(Key, Instant)>,
    usage: Option<UsageCache>,
}

/// What [`should_load`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    /// Ask for the charts now.
    Now,
    /// What is on screen is current, or its replacement is on the way.
    No,
    /// Something changed, but the last request is too recent: look again
    /// after this long.
    Later(Duration),
}

/// Whether the charts for `key` are to be requested now.
///
/// `loaded` is the key of the charts on screen and their age, `in_flight`
/// the key of a request that has not answered yet, `started` the key of
/// the last request and how long ago it was made. Another object or
/// range is loaded at once. For the same one (a workload stays the same
/// when only its pods changed) nothing starts while a request is in
/// flight, nor within [`RELOAD_GAP`] of the last one.
fn should_load(
    key: &Key,
    loaded: Option<(&Key, Duration)>,
    in_flight: Option<&Key>,
    started: Option<(&Key, Duration)>,
    refresh: Duration,
) -> Load {
    if loaded.is_some_and(|(k, age)| k == key && age < refresh) {
        return Load::No;
    }
    if in_flight.is_some_and(|k| same_subject(k, key)) {
        return Load::No;
    }
    match started {
        Some((k, ago)) if same_subject(k, key) && ago < RELOAD_GAP => Load::Later(RELOAD_GAP - ago),
        _ => Load::Now,
    }
}

/// Bytes of pod names that go into a link to Grafana. The queries travel
/// in the URL, twice (CPU and memory) and percent-encoded, and browsers,
/// proxies and Grafana itself stop accepting URLs at a few kilobytes.
const GRAFANA_POD_BYTES: usize = 1500;

/// `target` as it goes into a Grafana link: a workload keeps as many of
/// its first pods as fit [`GRAFANA_POD_BYTES`] (always at least one).
fn grafana_target(target: &ChartTarget) -> ChartTarget {
    match target {
        ChartTarget::Workload { namespace, pods } => {
            ChartTarget::Workload { namespace: namespace.clone(), pods: pods[..grafana_pods(pods)].to_vec() }
        }
        other => other.clone(),
    }
}

/// How many of a workload's pods fit into a Grafana link.
fn grafana_pods(pods: &[String]) -> usize {
    let mut bytes = 0;
    let fitting = pods
        .iter()
        .take(skiff_core::prom::MAX_WORKLOAD_PODS)
        .take_while(|p| {
            bytes += p.len() + 1;
            bytes <= GRAFANA_POD_BYTES
        })
        .count();
    fitting.max(1).min(pods.len())
}

/// The time range last chosen, so the next object opens with it.
fn remembered_span(ctx: &egui::Context) -> Span {
    ctx.data(|d| d.get_temp(egui::Id::unique("metrics-span")))
        .unwrap_or(Span::Hour)
}

impl MetricsView {
    pub fn new() -> Self {
        MetricsView {
            span: Span::Hour,
            job: None,
            loaded: None,
            started: None,
            usage: None,
        }
    }

    /// The time range in use.
    pub fn span(&self) -> Span {
        self.span
    }

    pub fn ui(&mut self, ui: &mut Ui, pal: &Palette, session: &Arc<Session>, subject: &Subject<'_>) {
        if self.loaded.is_none() && self.job.is_none() {
            self.span = remembered_span(ui.ctx());
        }
        let namespaces: Vec<&str> = [subject.namespace].into_iter().filter(|ns| !ns.is_empty()).collect();
        let status = session.prometheus(&namespaces);
        match &*status {
            PromStatus::Ready(prom) => self.prometheus_ui(ui, pal, session, subject, prom),
            PromStatus::Detecting => {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new("Looking for Prometheus in this cluster…").color(pal.muted));
                });
            }
            PromStatus::Unavailable(error) => self.fallback_ui(ui, pal, session, subject, error),
        }
    }

    /// `scope`: the label matchers of the Prometheus in use (see
    /// `Prom::scope`), for the Grafana link; empty for none.
    fn header(&mut self, ui: &mut Ui, pal: &Palette, subject: &Subject<'_>, with_range: bool, scope: &str) {
        ui.add_space(4.0);
        // (A workload without pods has no queries to hand over.)
        let grafana = match (subject.grafana_url, &subject.target) {
            (_, Some(ChartTarget::Workload { pods, .. })) if pods.is_empty() => None,
            (Some(base), Some(target)) => Some((base, target)),
            _ => None,
        };
        if !with_range && grafana.is_none() {
            // Nothing to choose and nowhere to go: no empty row.
            return;
        }
        ui.horizontal(|ui| {
            if with_range {
                ui.spacing_mut().item_spacing.x = 2.0;
                for span in Span::ALL {
                    let button = egui::Button::selectable(self.span == span, RichText::new(span.label()).size(12.0))
                        .min_size(egui::vec2(34.0, 24.0));
                    if ui.add(button).on_hover_text(span.description()).clicked() && self.span != span {
                        self.span = span;
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(egui::Id::unique("metrics-span"), span));
                    }
                }
            }
            if let Some((base, target)) = grafana {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let button = egui::Button::new((
                        widgets::icon(icons::ICON_OPEN_IN_NEW, 14.0).color(pal.muted),
                        RichText::new("Open in Grafana").size(12.0),
                    ))
                    .min_size(egui::vec2(0.0, 24.0));
                    let mut tip =
                        "Opens Grafana's Explore in your browser with the CPU and memory queries of this object".to_string();
                    if let ChartTarget::Workload { pods, .. } = target {
                        let fitting = grafana_pods(pods);
                        if fitting < pods.len() {
                            // (A link only holds so much.)
                            tip.push_str(&format!(", for the first {} of its {} pods", fitting, widgets::thousands(pods.len())));
                        }
                    }
                    if ui.add(button).on_hover_text(tip).clicked() {
                        // The queries and the link are only put together
                        // when they are asked for, not on every frame.
                        let queries = grafana_target(target).usage_queries_in(scope);
                        if !queries.is_empty() {
                            let url = grafana_explore_url(base, &queries, self.span);
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                    }
                });
            }
        });
    }

    fn prometheus_ui(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        session: &Arc<Session>,
        subject: &Subject<'_>,
        prom: &Prom,
    ) {
        self.header(ui, pal, subject, true, prom.scope());
        let source = format!("Prometheus · {}", prom.source());

        let Some(target) = &subject.target else {
            source_line(ui, pal, &source);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(RichText::new("Finding the pods of this workload…").color(pal.muted));
            });
            return;
        };
        if matches!(target, ChartTarget::Workload { pods, .. } if pods.is_empty()) {
            source_line(ui, pal, &source);
            quiet_note(
                ui,
                pal,
                icons::ICON_INFO,
                "This workload has no pods right now, so there is nothing to chart.",
            );
            return;
        }

        // Load, and load again while the tab stays open.
        let key: Key = (target.clone(), self.span, prom.display());
        if let Some((job_key, range, job)) = &self.job {
            if let Some(result) = job.take() {
                if let Ok(charts) = result {
                    self.loaded = Some(Loaded {
                        key: job_key.clone(),
                        range: *range,
                        charts,
                        at: Instant::now(),
                    });
                }
                self.job = None;
            }
        }
        let decision = should_load(
            &key,
            self.loaded.as_ref().map(|l| (&l.key, l.at.elapsed())),
            self.job.as_ref().map(|(k, ..)| k),
            self.started.as_ref().map(|(k, at)| (k, at.elapsed())),
            self.span.refresh(),
        );
        match decision {
            Load::Now => {
                let range = TimeRange::last(self.span.secs(), now_unix());
                self.job = Some((key.clone(), range, session.charts(prom, target.clone(), range)));
                self.started = Some((key.clone(), Instant::now()));
            }
            Load::Later(wait) => ui.ctx().request_repaint_after(wait),
            Load::No => {}
        }
        ui.ctx().request_repaint_after(self.span.refresh());

        let step = self
            .loaded
            .as_ref()
            .filter(|l| same_subject(&l.key, &key))
            .map(|l| l.range.step);
        match step {
            Some(step) => source_line(ui, pal, &format!("{source} · one point every {}", step_text(step))),
            None => source_line(ui, pal, &source),
        }
        ui.add_space(6.0);

        // (A workload whose pods changed keeps its charts on screen until
        // the ones for the new set of pods have arrived.)
        match self.loaded.as_ref().filter(|l| same_subject(&l.key, &key)) {
            Some(loaded) => {
                let window = (loaded.range.start, loaded.range.end);
                for c in &loaded.charts {
                    let message;
                    let state = match &c.data {
                        Ok(data) if data.series.iter().all(|s| s.points.is_empty()) => {
                            chart::State::Empty(match c.title {
                                "Restarts" => {
                                    "No restart counts in this time range (they come from kube-state-metrics)"
                                }
                                _ => "No data in this time range",
                            })
                        }
                        Ok(data) => chart::State::Data(&data.series),
                        Err(e) => {
                            message = e.to_string();
                            chart::State::Error(&message)
                        }
                    };
                    chart_card(ui, pal, c.title, c.unit, window, state);
                }
                if loaded.charts.iter().any(|c| matches!(&c.data, Ok(d) if d.truncated)) {
                    quiet_note(
                        ui,
                        pal,
                        icons::ICON_INFO,
                        "There are more series than Skiff draws; the charts show the first 200.",
                    );
                }
                if let ChartTarget::Workload { pods, .. } = target {
                    if pods.len() > skiff_core::prom::MAX_WORKLOAD_PODS {
                        quiet_note(
                            ui,
                            pal,
                            icons::ICON_INFO,
                            &format!(
                                "This workload has {} pods; the charts cover the first {}.",
                                widgets::thousands(pods.len()),
                                skiff_core::prom::MAX_WORKLOAD_PODS
                            ),
                        );
                    }
                }
            }
            // Nothing for this object and range yet: the charts' outlines.
            None => {
                let range = TimeRange::last(self.span.secs(), now_unix());
                for panel in target.panels(&range) {
                    chart_card(
                        ui,
                        pal,
                        panel.title,
                        panel.unit,
                        (range.start, range.end),
                        chart::State::Loading,
                    );
                }
            }
        }
    }

    fn fallback_ui(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        session: &Arc<Session>,
        subject: &Subject<'_>,
        error: &PromError,
    ) {
        self.header(ui, pal, subject, false, "");
        // The history is put together once per sample of the metrics (and
        // per object), not once per frame.
        let metrics = session.metrics();
        let has_metrics = metrics.available;
        match &subject.target {
            Some(target) => {
                if !self.usage.as_ref().is_some_and(|u| u.is_for(metrics.generation, target)) {
                    let history = match target {
                        ChartTarget::Pod { namespace, name } => session.pod_usage_history(namespace, &[name.as_str()]),
                        ChartTarget::Workload { namespace, pods } => {
                            let pods: Vec<&str> = pods.iter().map(String::as_str).collect();
                            session.pod_usage_history(namespace, &pods)
                        }
                        ChartTarget::Node { name } => session.node_usage_history(name),
                    };
                    self.usage = Some(UsageCache::new(metrics.generation, target, history));
                }
            }
            None => self.usage = None,
        }
        let usage = self.usage.as_ref();
        let empty = usage.is_none_or(|u| u.cpu[0].points.is_empty() && u.memory[0].points.is_empty());
        if has_metrics || !empty {
            source_line(ui, pal, "metrics-server · collected since you opened this view");
            ui.add_space(6.0);
            let now = now_unix();
            let first = usage.and_then(|u| u.first).unwrap_or(now);
            // At least five minutes wide, so the first samples do not
            // stretch across the whole chart.
            let window = (first.min(now - 300), now);
            let nothing = match &subject.target {
                None => "Finding the pods of this workload…",
                Some(ChartTarget::Node { .. }) => "metrics-server reports no usage for this node",
                Some(ChartTarget::Pod { .. }) => "metrics-server reports no usage for this pod (it may not be running)",
                Some(ChartTarget::Workload { .. }) => "metrics-server reports no usage for these pods",
            };
            for (title, unit, series) in
                [("CPU", Unit::Cores, usage.map(|u| &u.cpu)), ("Memory", Unit::Bytes, usage.map(|u| &u.memory))]
            {
                match series.filter(|s| !s[0].points.is_empty()) {
                    Some(series) => chart_card(ui, pal, title, unit, window, chart::State::Data(series)),
                    None => chart_card(ui, pal, title, unit, window, chart::State::Empty(nothing)),
                }
            }
            // New samples arrive every 15 seconds.
            ui.ctx().request_repaint_after(Duration::from_secs(5));
        }

        let (icon, text) = match error {
            PromError::NotFound => (
                icons::ICON_INFO,
                format!(
                    "{} Skiff found none in this cluster. If your metrics are in a managed Prometheus (Azure Monitor, \
                     Amazon, Google Cloud, Grafana Cloud) or one with an unusual name, connect it below (it is saved \
                     as {} in Skiff's config file).",
                    if has_metrics || !empty {
                        "Longer history, network and restarts need Prometheus."
                    } else {
                        "Charts need Prometheus or metrics-server."
                    },
                    config_key(subject.context)
                ),
            ),
            PromError::Forbidden(_) => (icons::ICON_LOCK, error.to_string()),
            other => (icons::ICON_INFO, other.to_string()),
        };
        ui.add_space(2.0);
        ui.horizontal_top(|ui| {
            ui.label(widgets::icon(icon, 15.0).color(pal.faint));
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(text).color(pal.muted).size(12.0)).wrap());
                let link = egui::Label::new(RichText::new("Look for Prometheus again").color(pal.accent).size(12.0))
                    .sense(egui::Sense::click());
                ui.horizontal(|ui| {
                    let resp = ui.add(link).on_hover_cursor(egui::CursorIcon::PointingHand);
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Look for Prometheus again"));
                    if resp.clicked() {
                        session.retry_prometheus();
                    }
                    ui.label(RichText::new("·").color(pal.faint));
                    crate::prom_connect::link(ui, pal, Some(subject.context), "Connect a Prometheus…");
                });
            });
        });
    }
}

impl Default for MetricsView {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether two sets of charts are about the same thing over the same
/// range: equal, or the same workload with a changed set of pods.
fn same_subject(a: &Key, b: &Key) -> bool {
    let same_target = match (&a.0, &b.0) {
        (ChartTarget::Workload { namespace: x, .. }, ChartTarget::Workload { namespace: y, .. }) => x == y,
        (x, y) => x == y,
    };
    same_target && a.1 == b.1 && a.2 == b.2
}

/// The config key that points Skiff at a Prometheus, with the context's
/// name in it when that is short enough to read.
pub fn config_key(context: &str) -> String {
    let plain = context.len() <= 32
        && context
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    format!("clusters.{}.prometheus", if plain { context } else { "<context>" })
}

/// The distance between two points of a chart, in words: "30 s", "2 h".
pub fn step_text(secs: u32) -> String {
    match secs {
        0..=119 => format!("{secs} s"),
        120..=7199 => format!("{} min", secs / 60),
        7200..=172_799 => format!("{} h", secs / 3600),
        _ => format!("{} days", secs / 86_400),
    }
}

fn source_line(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(RichText::new(text).color(pal.faint).size(11.5));
}

fn quiet_note(ui: &mut Ui, pal: &Palette, icon: egui_material_icons::MaterialIcon, text: &str) {
    ui.add_space(4.0);
    ui.horizontal_top(|ui| {
        ui.label(widgets::icon(icon, 15.0).color(pal.faint));
        ui.add(egui::Label::new(RichText::new(text).color(pal.muted).size(12.0)).wrap());
    });
}

fn chart_card(ui: &mut Ui, pal: &Palette, title: &str, unit: Unit, window: (i64, i64), state: chart::State<'_>) {
    Frame::new()
        .fill(pal.raised)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin {
            left: 10,
            right: 10,
            top: 8,
            bottom: 4,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            chart::show(ui, pal, title, title, unit, window, state);
        });
    ui.add_space(4.0);
}

/// A link into Grafana's Explore for these PromQL queries. `base` is the
/// configured `grafana_url` (no trailing slash needed). Nothing but the
/// queries and the time range is sent: Grafana uses its default data
/// source and the user's own session.
pub fn grafana_explore_url(base: &str, queries: &[String], span: Span) -> String {
    const REF_IDS: [&str; 6] = ["A", "B", "C", "D", "E", "F"];
    let queries: Vec<serde_json::Value> = queries
        .iter()
        .zip(REF_IDS)
        .map(|(expr, id)| serde_json::json!({"refId": id, "expr": expr, "range": true, "instant": false}))
        .collect();
    let panes = serde_json::json!({
        "skiff": {"queries": queries, "range": {"from": span.grafana_from(), "to": "now"}}
    });
    // `enc` escapes everything outside the unreserved set, so quotes,
    // braces and `&` in a query cannot end the parameter.
    format!(
        "{}/explore?schemaVersion=1&panes={}",
        base.trim_end_matches('/'),
        enc(&panes.to_string())
    )
}

// ---------------------------------------------------------------------------
// Alerts

/// The colour of an alert's severity.
pub fn severity_color(severity: &str, pal: &Palette) -> egui::Color32 {
    match severity {
        s if is_critical(s) => pal.err,
        "warning" => pal.warn,
        "info" => pal.info,
        _ => pal.muted,
    }
}

/// The severity as it is shown: the label's own word, or "alert" when
/// the rule sets none.
pub fn severity_text(severity: &str) -> &str {
    if severity.is_empty() {
        "alert"
    } else {
        severity
    }
}

/// How long an alert has been firing, e.g. "47m".
pub fn alert_age(alert: &Alert, now: i64) -> String {
    alert
        .active_since
        .map(|since| fmt_age((now - since).max(0)))
        .unwrap_or_default()
}

/// The resource type an alert target is an object of. `NodeAddress`
/// (a node known by address only) is a node.
pub fn target_key(kind: AlertKind) -> Option<ResourceKey> {
    let builtin = |group: &str, plural: &str| kinds::builtin(group, plural).map(|b| b.key());
    match kind {
        AlertKind::Pod => Some(kinds::pods()),
        AlertKind::Deployment => builtin("apps", "deployments"),
        AlertKind::StatefulSet => builtin("apps", "statefulsets"),
        AlertKind::DaemonSet => builtin("apps", "daemonsets"),
        AlertKind::Job => builtin("batch", "jobs"),
        AlertKind::Pvc => builtin("", "persistentvolumeclaims"),
        AlertKind::Node | AlertKind::NodeAddress => Some(kinds::nodes()),
    }
}

/// Whether an alert's target is this object. `addresses` are a node's
/// addresses (alerts from node-level scrape jobs name the node by address).
pub fn targets(target: &AlertTarget, key: &ResourceKey, namespace: &str, name: &str, addresses: &[&str]) -> bool {
    if target_key(target.kind).as_ref() != Some(key) {
        return false;
    }
    match target.kind {
        AlertKind::Node => target.name == name,
        AlertKind::NodeAddress => target.name == name || addresses.contains(&target.name.as_str()),
        _ => target.namespace.as_deref() == Some(namespace) && target.name == name,
    }
}

/// The firing alerts that concern one object.
pub fn alerts_for<'a>(
    alerts: &'a Alerts,
    key: &ResourceKey,
    namespace: &str,
    name: &str,
    addresses: &[&str],
) -> Vec<&'a Alert> {
    alerts
        .items
        .iter()
        .filter(|a| a.target().is_some_and(|t| targets(&t, key, namespace, name, addresses)))
        .collect()
}

/// Characters of an alert's name shown in a detail panel (the rest on
/// hover).
const MAX_ALERT_NAME: usize = 64;
/// Characters of an alert's description shown in a detail panel.
const MAX_ALERT_TEXT: usize = 400;

/// The alerts of one object, at the top of its detail panel.
pub fn object_alerts(ui: &mut Ui, pal: &Palette, alerts: &[&Alert]) {
    if alerts.is_empty() {
        return;
    }
    let now = now_unix();
    let worst = severity_color(&alerts[0].severity, pal);
    Frame::new()
        .fill(pal.tint(worst, 0.10))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, alert) in alerts.iter().enumerate() {
                if i > 0 {
                    ui.add_space(4.0);
                }
                let color = severity_color(&alert.severity, pal);
                ui.horizontal(|ui| {
                    ui.label(widgets::icon(icons::ICON_NOTIFICATIONS_ACTIVE, 14.0).color(color));
                    let name = widgets::clipped(&alert.name, MAX_ALERT_NAME);
                    let label = ui.label(widgets::semibold(&*name, 13.0).color(pal.text));
                    if *name != alert.name {
                        label.on_hover_text(&alert.name);
                    }
                    ui.label(RichText::new(widgets::clipped(severity_text(&alert.severity), 24)).color(color).size(12.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let age = alert_age(alert, now);
                        if !age.is_empty() {
                            ui.label(RichText::new(format!("firing for {age}")).color(pal.muted).size(12.0));
                        }
                    });
                });
                let text = if alert.description.is_empty() {
                    &alert.summary
                } else {
                    &alert.description
                };
                if !text.is_empty() {
                    // A few lines of it; all of it on hover.
                    let short = widgets::clipped(text, MAX_ALERT_TEXT);
                    let label = ui.add(
                        egui::Label::new(RichText::new(&*short).color(pal.muted).size(12.0))
                            .wrap()
                            .selectable(true),
                    );
                    if *short != **text {
                        label.on_hover_text(text.as_str());
                    }
                }
            }
        });
    ui.add_space(2.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn alert(labels: &[(&str, &str)]) -> Alert {
        let labels: BTreeMap<String, String> = labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Alert {
            name: labels.get("alertname").cloned().unwrap_or_default(),
            severity: labels.get("severity").cloned().unwrap_or_default(),
            state: "firing".into(),
            summary: String::new(),
            description: String::new(),
            labels,
            active_since: Some(1000),
        }
    }

    #[test]
    fn grafana_links_are_encoded_whatever_the_query_holds() {
        let query = r#"sum by(container)(rate(container_cpu_usage_seconds_total{namespace="shop",pod="web-1"}[120s]))"#;
        let url = grafana_explore_url("https://grafana.example.com/", &[query.to_string()], Span::SixHours);
        let (base, params) = url.split_once('?').expect("query string");
        assert_eq!(base, "https://grafana.example.com/explore");
        let (schema, panes) = params.split_once('&').expect("two parameters");
        assert_eq!(schema, "schemaVersion=1");
        let encoded = panes.strip_prefix("panes=").expect("panes");
        // Nothing that could end the parameter or the URL survives unescaped.
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~%".contains(&b)),
            "{encoded}"
        );

        // Decoded, it is the JSON Grafana expects, with the query intact.
        let mut bytes = Vec::new();
        let mut rest = encoded.as_bytes();
        while let Some((&b, tail)) = rest.split_first() {
            if b == b'%' {
                bytes.push(u8::from_str_radix(std::str::from_utf8(&tail[..2]).unwrap(), 16).unwrap());
                rest = &tail[2..];
            } else {
                bytes.push(b);
                rest = tail;
            }
        }
        let panes: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(panes["skiff"]["queries"][0]["expr"], query);
        assert_eq!(panes["skiff"]["queries"][0]["refId"], "A");
        assert_eq!(panes["skiff"]["range"]["from"], "now-6h");
        assert_eq!(panes["skiff"]["range"]["to"], "now");

        // A hostile name stays inside the parameter; a sub-path base works.
        let hostile = "x\"}&orgId=2#frag\u{e9}".to_string();
        let url = grafana_explore_url(
            "https://example.com/grafana",
            &[hostile.clone(), "up".into()],
            Span::Week,
        );
        assert!(
            url.starts_with("https://example.com/grafana/explore?schemaVersion=1&panes=%7B"),
            "{url}"
        );
        assert_eq!(url.matches('&').count(), 1, "{url}");
        assert!(!url.contains('#') && !url.contains('"'), "{url}");
        assert!(url.contains("now-7d") && url.contains("%22refId%22%3A%22B%22"));
    }

    #[test]
    fn spans_refresh_no_faster_than_their_resolution() {
        for span in Span::ALL {
            let range = TimeRange::last(span.secs(), 1_700_000_000);
            assert!(span.refresh().as_secs() >= 30);
            assert!(span.refresh().as_secs() >= u64::from(range.step) / 8, "{span:?}");
            assert_eq!((range.end - range.start) as u64, span.secs());
        }
        assert_eq!(Span::Hour.refresh(), Duration::from_secs(30));
        let steps = Span::ALL.map(|s| step_text(TimeRange::last(s.secs(), 1_700_000_000).step));
        assert_eq!(steps, ["30 s", "5 min", "15 min", "2 h"]);
        assert_eq!(step_text(60), "60 s");
        assert_eq!(step_text(3 * 86_400), "3 days");
        assert_eq!(Span::ALL.map(Span::label), ["1h", "6h", "24h", "7d"]);
    }

    #[test]
    fn charts_survive_a_change_of_pods_but_not_of_object_or_range() {
        let workload = |pods: &[&str]| ChartTarget::Workload {
            namespace: "shop".into(),
            pods: pods.iter().map(|p| p.to_string()).collect(),
        };
        let key = |target: ChartTarget, span: Span| (target, span, "monitoring/prom:9090".to_string());
        assert!(same_subject(
            &key(workload(&["a", "b"]), Span::Hour),
            &key(workload(&["a", "c"]), Span::Hour)
        ));
        assert!(!same_subject(
            &key(workload(&["a"]), Span::Hour),
            &key(workload(&["a"]), Span::Day)
        ));
        let pod = |name: &str| ChartTarget::Pod {
            namespace: "shop".into(),
            name: name.into(),
        };
        assert!(same_subject(&key(pod("a"), Span::Hour), &key(pod("a"), Span::Hour)));
        assert!(!same_subject(&key(pod("a"), Span::Hour), &key(pod("b"), Span::Hour)));
        assert!(!same_subject(
            &key(pod("a"), Span::Hour),
            &key(workload(&["a"]), Span::Hour)
        ));
    }

    #[test]
    fn charts_are_not_asked_for_again_with_every_change_of_pods() {
        let workload = |pods: &[&str]| -> Key {
            let pods = pods.iter().map(|p| p.to_string()).collect();
            (ChartTarget::Workload { namespace: "shop".into(), pods }, Span::Hour, "monitoring/prom:9090".to_string())
        };
        let secs = Duration::from_secs;
        let refresh = Span::Hour.refresh();
        let (ab, ac, ad) = (workload(&["a", "b"]), workload(&["a", "c"]), workload(&["a", "d"]));

        // Nothing yet: now. Then nothing more while that is in flight,
        // whatever the pods do.
        assert_eq!(should_load(&ab, None, None, None, refresh), Load::Now);
        assert_eq!(should_load(&ab, None, Some(&ab), Some((&ab, secs(1))), refresh), Load::No);
        assert_eq!(should_load(&ac, None, Some(&ab), Some((&ab, secs(1))), refresh), Load::No);
        // It answered, for pods that have changed since: not before the
        // gap is over, and then once.
        assert_eq!(should_load(&ac, Some((&ab, secs(0))), None, Some((&ab, secs(2))), refresh), Load::Later(secs(13)));
        assert_eq!(should_load(&ad, Some((&ab, secs(12))), None, Some((&ab, secs(14))), refresh), Load::Later(secs(1)));
        assert_eq!(should_load(&ad, Some((&ab, secs(13))), None, Some((&ab, secs(15))), refresh), Load::Now);
        // Unchanged and fresh: nothing. Stale: the usual refresh.
        assert_eq!(should_load(&ab, Some((&ab, secs(20))), None, Some((&ab, secs(21))), refresh), Load::No);
        assert_eq!(should_load(&ab, Some((&ab, secs(30))), None, Some((&ab, secs(31))), refresh), Load::Now);
        // A request that failed leaves nothing loaded: it is not repeated
        // on the next frame either.
        assert_eq!(should_load(&ab, None, None, Some((&ab, secs(3))), refresh), Load::Later(secs(12)));

        // Another range or object is not held back by any of it, not
        // even by a request in flight.
        let day: Key = (ab.0.clone(), Span::Day, ab.2.clone());
        assert_eq!(should_load(&day, Some((&ab, secs(1))), Some(&ab), Some((&ab, secs(1))), Span::Day.refresh()), Load::Now);
        let pod: Key = (ChartTarget::Pod { namespace: "shop".into(), name: "a".into() }, Span::Hour, ab.2.clone());
        assert_eq!(should_load(&pod, Some((&ab, secs(1))), Some(&ab), Some((&ab, secs(1))), refresh), Load::Now);
    }

    #[test]
    fn a_grafana_link_holds_only_as_many_pods_as_fit() {
        let names = |n: usize, len: usize| -> Vec<String> { (0..n).map(|i| format!("{i:0len$}")).collect() };
        // Ordinary names: all of them (up to what the charts cover).
        assert_eq!(grafana_pods(&names(20, 40)), 20);
        assert_eq!(grafana_pods(&names(500, 5)), skiff_core::prom::MAX_WORKLOAD_PODS);
        // Long ones: what fits the budget, and never none.
        assert_eq!(grafana_pods(&names(100, 253)), GRAFANA_POD_BYTES / 254);
        assert_eq!(grafana_pods(&names(3, 5000)), 1);
        assert_eq!(grafana_pods(&[]), 0);

        let long = ChartTarget::Workload { namespace: "shop".into(), pods: names(100, 253) };
        let ChartTarget::Workload { pods, .. } = grafana_target(&long) else { panic!("a workload") };
        assert_eq!(pods, names(100, 253)[..5]);
        let url = grafana_explore_url("https://grafana.example.com", &grafana_target(&long).usage_queries(), Span::Hour);
        assert!(url.len() < 8000, "{} bytes", url.len());
        let whole = grafana_explore_url("https://grafana.example.com", &long.usage_queries(), Span::Hour);
        assert!(whole.len() > 50_000, "the test's names are long enough to matter: {}", whole.len());
        // Other targets are handed over as they are.
        let pod = ChartTarget::Pod { namespace: "shop".into(), name: "web-1".into() };
        assert_eq!(grafana_target(&pod), pod);
    }

    #[test]
    fn the_config_hint_names_the_context_when_it_is_readable() {
        assert_eq!(config_key("prod-eu"), "clusters.prod-eu.prometheus");
        assert_eq!(
            config_key("arn:aws:eks:eu-west-1:123456789012:cluster/prod"),
            "clusters.<context>.prometheus"
        );
        assert_eq!(config_key("has space"), "clusters.<context>.prometheus");
    }

    #[test]
    fn alerts_find_their_object() {
        let deploy = kinds::builtin("apps", "deployments").unwrap().key();
        let alerts = Alerts {
            loaded: true,
            items: vec![
                alert(&[("alertname", "NodeDown"), ("severity", "critical"), ("node", "node-1")]),
                alert(&[
                    ("alertname", "Mismatch"),
                    ("severity", "warning"),
                    ("namespace", "shop"),
                    ("deployment", "web"),
                    ("pod", "ksm-0"),
                ]),
                alert(&[
                    ("alertname", "Crash"),
                    ("severity", "warning"),
                    ("namespace", "shop"),
                    ("pod", "web-1"),
                ]),
                alert(&[
                    ("alertname", "DiskFull"),
                    ("severity", "warning"),
                    ("job", "node-exporter"),
                    ("instance", "10.0.0.7:9100"),
                ]),
                alert(&[
                    ("alertname", "TargetDown"),
                    ("severity", "warning"),
                    ("namespace", "shop"),
                ]),
            ],
            omitted: 0,
            error: None,
        };
        let names = |found: Vec<&Alert>| found.iter().map(|a| a.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(alerts_for(&alerts, &deploy, "shop", "web", &[])), ["Mismatch"]);
        assert!(
            alerts_for(&alerts, &deploy, "other", "web", &[]).is_empty(),
            "same name, another namespace"
        );
        assert_eq!(
            names(alerts_for(&alerts, &kinds::pods(), "shop", "web-1", &[])),
            ["Crash"]
        );
        assert!(
            alerts_for(&alerts, &kinds::pods(), "shop", "ksm-0", &[]).is_empty(),
            "the reporting pod is not the subject"
        );
        assert_eq!(
            names(alerts_for(&alerts, &kinds::nodes(), "", "node-1", &[])),
            ["NodeDown"]
        );
        // A node known by address only.
        assert_eq!(
            names(alerts_for(
                &alerts,
                &kinds::nodes(),
                "",
                "node-7",
                &["10.0.0.7", "node-7.lan"]
            )),
            ["DiskFull"]
        );
        assert!(alerts_for(&alerts, &kinds::nodes(), "", "node-8", &["10.0.0.8"]).is_empty());

        assert_eq!(severity_text(""), "alert");
        assert_eq!(severity_text("warning"), "warning");
        assert_eq!(alert_age(&alerts.items[0], 1000 + 47 * 60), "47m");
        let pal = Palette::dark();
        assert_eq!(severity_color("critical", &pal), pal.err);
        assert_eq!(severity_color("page", &pal), pal.err);
        assert_eq!(severity_color("warning", &pal), pal.warn);
        assert_eq!(severity_color("info", &pal), pal.info);
        assert_eq!(severity_color("", &pal), pal.muted);
    }
}
