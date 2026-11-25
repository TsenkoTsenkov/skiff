//! Cluster overview: health at a glance and what needs attention.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui};
use egui_material_icons::icons;
use skiff_core::kinds;
use skiff_core::{
    fmt_age, fmt_bytes, fmt_cpu, now_unix, Alert, AlertKind, ColKind, LoadState, PromError, PromStatus, ResourceKey, Row, Session,
    Snapshot, Tone,
};

use crate::metrics_view::{alert_age, severity_color, severity_text, target_key};
use crate::theme::Palette;
use crate::widgets;

pub enum Action {
    Go(ResourceKey),
    Open { key: ResourceKey, ns: String, name: String },
    /// Narrow the namespace filter to this one namespace.
    UseNamespace(String),
}

/// The tables the overview is built from. For an account that may not
/// list across the cluster these are the selected namespace's tables.
pub struct Data {
    pub nodes: Arc<Snapshot>,
    pub pods: Arc<Snapshot>,
    pub deploys: Arc<Snapshot>,
    pub events: Arc<Snapshot>,
    /// The account may not list namespaced resources cluster-wide.
    pub limited: bool,
    /// The namespace the kubeconfig context names, if any: offered as the
    /// one to work in when access is limited.
    pub home_namespace: Option<String>,
}

/// The server refused to list this table.
fn denied(snap: &Snapshot) -> bool {
    snap.state == LoadState::Failed && snap.error_code == 403
}

fn in_scope(row: &Row, ns: &BTreeSet<String>) -> bool {
    ns.is_empty() || row.namespace.is_empty() || ns.contains(&*row.namespace)
}

fn card(ui: &mut Ui, pal: &Palette, title: &str, big: &str, sub: &str, tone: Tone, loading: bool) -> bool {
    let resp = Frame::new()
        .fill(pal.raised)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(168.0);
            ui.set_height(74.0);
            ui.vertical(|ui| {
                widgets::caption(ui, title, pal);
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(widgets::semibold(big, 24.0).color(pal.text));
                    if loading {
                        ui.spinner();
                    }
                });
                let color = if tone >= Tone::Warn { pal.tone(tone) } else { pal.muted };
                ui.label(RichText::new(sub).color(color).size(12.0));
            });
        })
        .response;
    let resp = resp.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let label = title.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    resp.clicked()
}

fn loading(snap: &Snapshot) -> bool {
    matches!(snap.state, LoadState::Connecting | LoadState::Loading)
}

/// Why a list has nothing to show, when it failed (no permission to list
/// it, cluster unreachable): an empty list then means "unknown", never
/// "all healthy". With the words and tone for its card.
fn failure(snap: &Snapshot) -> Option<(&'static str, Tone)> {
    if denied(snap) {
        Some(("no access", Tone::Muted))
    } else if snap.state == LoadState::Failed && snap.rows.is_empty() {
        Some(("couldn't load", Tone::Err))
    } else {
        None
    }
}

/// What the "Needs attention" block can say when it lists no problems.
#[derive(Debug, PartialEq)]
enum Verdict<'a> {
    /// These lists failed to load, so their health is not known.
    Unknown(Vec<&'a str>),
    /// Everything the account can read has loaded; `unreadable` names the
    /// lists it may not read (expected for an account limited to a
    /// namespace, and no reason for a warning).
    Clear { unreadable: Vec<&'a str> },
    /// Still loading: nothing to say yet.
    Waiting,
}

fn verdict<'a>(lists: &[(&'a str, &Snapshot)]) -> Verdict<'a> {
    let names = |pick: &dyn Fn(&Snapshot) -> bool| -> Vec<&'a str> {
        lists.iter().filter(|(_, s)| pick(s)).map(|(name, _)| *name).collect()
    };
    let unknown = names(&|s| !denied(s) && failure(s).is_some());
    if !unknown.is_empty() {
        return Verdict::Unknown(unknown);
    }
    let unreadable = names(&|s| denied(s));
    let loaded = |s: &Snapshot| matches!(s.state, LoadState::Live | LoadState::Polling);
    if unreadable.len() < lists.len() && lists.iter().all(|(_, s)| loaded(s) || denied(s)) {
        Verdict::Clear { unreadable }
    } else {
        Verdict::Waiting
    }
}

fn status_text<'a>(snap: &'a Snapshot, row: &'a Row) -> &'a str {
    snap.columns
        .iter()
        .filter(|c| c.kind == ColKind::Status)
        .filter_map(|c| row.cell(c))
        .filter(|c| c.tone >= Tone::Warn)
        .max_by_key(|c| c.tone)
        .map(|c| &*c.text)
        .unwrap_or("")
}

/// The resource type of the overview's deployments card.
pub fn deployments_key() -> ResourceKey {
    kinds::builtin("apps", "deployments").map(|b| b.key()).unwrap_or_else(kinds::pods)
}

/// What an account without cluster-wide access sees until one namespace
/// is selected: why there are no numbers, and the way to get some.
fn limited_access(ui: &mut Ui, pal: &Palette, home: Option<&str>) -> Option<Action> {
    let mut action = None;
    Frame::new()
        .fill(pal.raised)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width((ui.available_width() - 20.0).min(620.0));
            ui.horizontal(|ui| {
                ui.label(widgets::icon(icons::ICON_LOCK, 18.0).color(pal.warn));
                ui.label(widgets::semibold("Limited access", 14.0).color(pal.text));
            });
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "This account can't list resources across the whole cluster. Select the one namespace \
                         you work in with the picker at the top, and the overview shows that namespace.",
                    )
                    .color(pal.muted),
                )
                .wrap(),
            );
            if let Some(ns) = home {
                ui.add_space(8.0);
                if ui
                    .button(format!("Use namespace {ns}"))
                    .on_hover_text("The namespace this context names in your kubeconfig")
                    .clicked()
                {
                    action = Some(Action::UseNamespace(ns.to_string()));
                }
            }
        });
    action
}

/// Alerts shown before "… and N more".
const MAX_ALERTS: usize = 12;
/// Characters of an alert's name shown in its row (the rest on hover).
const MAX_ALERT_NAME: usize = 48;

/// The line under the listed alerts: how many more there are. `more` are
/// in the selected scope and not listed; `omitted` are firing in the
/// cluster but were not kept (`Alerts::omitted`), so which scope they
/// belong to is not known.
fn more_alerts_text(more: usize, omitted: usize, whole_cluster: bool) -> Option<String> {
    match (more, omitted) {
        (0, 0) => None,
        (more, 0) => Some(format!("… and {} more", widgets::thousands(more))),
        (more, omitted) if whole_cluster => Some(format!("… and {} more", widgets::thousands(more + omitted))),
        (0, omitted) => Some(format!("… and {} less severe in the cluster that are not counted here", widgets::thousands(omitted))),
        (more, omitted) => Some(format!(
            "… and {} more, and {} less severe in the cluster that are not counted here",
            widgets::thousands(more),
            widgets::thousands(omitted)
        )),
    }
}

/// The object an alert is about, as something that can be opened. A node
/// that the alert knows by address only is looked up among the nodes.
fn alert_object(alert: &Alert, nodes: &Snapshot) -> Option<(ResourceKey, String, String)> {
    let target = alert.target()?;
    let key = target_key(target.kind)?;
    let name = match target.kind {
        AlertKind::NodeAddress => {
            let known = |r: &&Arc<Row>| *r.name == *target.name || r.cells.iter().any(|c| *c.text == *target.name);
            nodes.rows.iter().find(known)?.name.to_string()
        }
        _ => target.name,
    };
    Some((key, target.namespace.unwrap_or_default(), name))
}

/// Whether an alert belongs to the selected namespaces. Alerts without a
/// namespace (nodes, the cluster itself) always do.
fn alert_in_scope(alert: &Alert, ns: &BTreeSet<String>) -> bool {
    ns.is_empty() || alert.labels.get("namespace").is_none_or(|n| n.is_empty() || ns.contains(n))
}

/// The "Firing alerts" section: what Prometheus says is wrong right now,
/// each alert with a link to the object it is about. Nothing is drawn for
/// a cluster without Prometheus.
fn firing_alerts(ui: &mut Ui, pal: &Palette, session: &Arc<Session>, ns: &BTreeSet<String>, nodes: &Snapshot) -> Option<Action> {
    let namespaces: Vec<&str> = ns.iter().map(String::as_str).collect();
    let status = session.prometheus(&namespaces);
    let prom = match &*status {
        PromStatus::Ready(prom) => prom,
        // No Prometheus is the normal case for many clusters, and looking
        // for it takes a moment: neither is worth a line here.
        PromStatus::Detecting | PromStatus::Unavailable(PromError::NotFound) => return None,
        PromStatus::Unavailable(error) => {
            ui.add_space(16.0);
            widgets::caption(ui, "Firing alerts", pal);
            ui.add_space(4.0);
            ui.horizontal_top(|ui| {
                ui.label(widgets::icon(icons::ICON_INFO, 16.0).color(pal.faint));
                ui.vertical(|ui| {
                    ui.set_max_width((ui.available_width() - 20.0).min(760.0));
                    ui.add(egui::Label::new(RichText::new(error.to_string()).color(pal.muted)).wrap());
                    let link = egui::Label::new(RichText::new("Look for Prometheus again").color(pal.accent).size(12.0))
                        .sense(Sense::click());
                    ui.horizontal(|ui| {
                        let resp = ui.add(link).on_hover_cursor(egui::CursorIcon::PointingHand);
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Look for Prometheus again"));
                        if resp.clicked() {
                            session.retry_prometheus();
                        }
                        ui.label(RichText::new("·").color(pal.faint));
                        crate::prom_connect::link(ui, pal, None, "Change Prometheus…");
                    });
                });
            });
            return None;
        }
    };
    let alerts = session.alerts();
    let shown: Vec<&Alert> = alerts.items.iter().filter(|a| alert_in_scope(a, ns)).collect();
    let source = format!("Prometheus · {}", prom.source());
    let mut action = None;

    ui.add_space(16.0);
    ui.horizontal(|ui| {
        let title = if alerts.loaded && alerts.error.is_none() {
            // (Not every alert was kept: the count is a lower bound.)
            format!("Firing alerts · {}{}", shown.len(), if alerts.omitted > 0 { "+" } else { "" })
        } else {
            "Firing alerts".to_string()
        };
        widgets::caption(ui, &title, pal);
        ui.label(RichText::new(source).color(pal.faint).size(11.5));
    });
    ui.add_space(4.0);
    if let Some(error) = &alerts.error {
        let text = match error {
            // Not a failure: a managed Prometheus keeps its alerts elsewhere.
            PromError::Unsupported(msg) => msg.clone(),
            other => format!("The alerts could not be loaded. {other}"),
        };
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::ICON_INFO, 16.0).color(pal.faint));
            ui.add(egui::Label::new(RichText::new(text).color(pal.muted)).wrap());
        });
        return None;
    }
    if !alerts.loaded {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.label(RichText::new("Loading alerts…").color(pal.muted));
        });
        return None;
    }
    if shown.is_empty() {
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::ICON_CHECK_CIRCLE, 16.0).color(pal.ok));
            let text = if ns.is_empty() { "No alerts are firing." } else { "No alerts are firing in the selected scope." };
            ui.label(RichText::new(text).color(pal.muted));
        });
        return None;
    }

    let now = now_unix();
    Frame::new()
        .fill(pal.raised)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width((ui.available_width() - 20.0).clamp(240.0, 1100.0));
            ui.spacing_mut().item_spacing.y = 1.0;
            for alert in shown.iter().take(MAX_ALERTS) {
                let color = severity_color(&alert.severity, pal);
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(egui::vec2(62.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.set_min_width(62.0);
                        widgets::pill(ui, severity_text(&alert.severity), color, pal);
                    });
                    let name = widgets::clipped(&alert.name, MAX_ALERT_NAME);
                    let label = ui.label(widgets::semibold(&*name, 13.0).color(pal.text));
                    if *name != alert.name {
                        label.on_hover_text(&alert.name);
                    }
                    match alert_object(alert, nodes) {
                        Some((key, namespace, name)) => {
                            ui.label(RichText::new(&*key.kind).color(pal.muted).size(12.0));
                            if ui.link(&name).on_hover_text("Open this object").clicked() {
                                action = Some(Action::Open { key, ns: namespace.clone(), name });
                            }
                            if !namespace.is_empty() {
                                ui.label(RichText::new(namespace).color(pal.faint).size(12.0));
                            }
                        }
                        None => {
                            if let Some(namespace) = alert.labels.get("namespace").filter(|n| !n.is_empty()) {
                                ui.label(RichText::new(namespace).color(pal.faint).size(12.0));
                            }
                        }
                    }
                    // The age at the right edge; the summary takes what is
                    // left between, cut short when it does not fit.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(alert_age(alert, now)).color(pal.muted).size(12.0));
                        let text = if alert.summary.is_empty() { &alert.description } else { &alert.summary };
                        if !text.is_empty() && ui.available_width() > 60.0 {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let summary = ui.add(egui::Label::new(RichText::new(text).color(pal.muted).size(12.0)).truncate());
                                if !alert.description.is_empty() {
                                    summary.on_hover_text(&alert.description);
                                }
                            });
                        }
                    });
                });
            }
            if let Some(more) = more_alerts_text(shown.len().saturating_sub(MAX_ALERTS), alerts.omitted, ns.is_empty()) {
                ui.add_space(4.0);
                ui.label(RichText::new(more).color(pal.muted));
            }
        });
    action
}

pub fn show(ui: &mut Ui, pal: &Palette, session: &Arc<Session>, ns: &BTreeSet<String>, data: &Data) -> Option<Action> {
    let mut action = None;
    let (k_nodes, k_pods, k_events) = (kinds::nodes(), kinds::pods(), kinds::events());
    let k_deploy = deployments_key();
    let (nodes, pods, deploys, events) = (&data.nodes, &data.pods, &data.deploys, &data.events);
    let metrics = session.metrics();
    let now = now_unix();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(widgets::semibold("Overview", 17.0).color(pal.text));
            ui.label(RichText::new(&session.context).color(pal.muted));
            if !ns.is_empty() {
                widgets::pill(ui, &format!("{} selected", widgets::plural(ns.len(), "namespace")), pal.info, pal);
            }
            if data.limited {
                widgets::badge(ui, icons::ICON_LOCK, "limited access", pal.warn, pal).on_hover_text(
                    "This account can't list resources across the whole cluster, so the overview only covers \
                     the selected namespace.",
                );
            }
        });
        ui.add_space(10.0);
        if data.limited && ns.len() != 1 {
            action = limited_access(ui, pal, data.home_namespace.as_deref());
            return;
        }

        // Stat cards.
        let node_bad = nodes.rows.iter().filter(|r| r.tone >= Tone::Warn).count();
        let pod_rows: Vec<&Arc<Row>> = pods.rows.iter().filter(|r| in_scope(r, ns)).collect();
        let pod_err = pod_rows.iter().filter(|r| r.tone == Tone::Err).count();
        let pod_warn = pod_rows.iter().filter(|r| r.tone == Tone::Warn).count();
        let dep_rows: Vec<&Arc<Row>> = deploys.rows.iter().filter(|r| in_scope(r, ns)).collect();
        let dep_bad = dep_rows.iter().filter(|r| r.tone >= Tone::Warn).count();
        let warn_events: Vec<&Arc<Row>> = events
            .rows
            .iter()
            .filter(|r| in_scope(r, ns) && r.tone >= Tone::Warn)
            .collect();

        ui.with_layout(Layout::left_to_right(Align::Min).with_main_wrap(true), |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            // A table that could not be read (no access, or the list
            // failed) has no number to show, and is not "all ready".
            let count = |snap: &Snapshot, n: usize| if failure(snap).is_some() { "—".to_string() } else { widgets::thousands(n) };
            let failed = |snap: &Snapshot| failure(snap).map(|(why, tone)| (why.to_string(), tone));
            let (sub, tone) = if let Some(why) = failed(nodes) {
                why
            } else if node_bad == 0 {
                ("all ready".to_string(), Tone::Ok)
            } else {
                (format!("{node_bad} not ready / cordoned"), Tone::Warn)
            };
            if card(ui, pal, "Nodes", &count(nodes, nodes.rows.len()), &sub, tone, loading(nodes)) {
                action = Some(Action::Go(k_nodes.clone()));
            }
            let (sub, tone) = match (pod_err, pod_warn) {
                (0, 0) => ("all healthy".to_string(), Tone::Ok),
                (0, w) => (format!("{w} pending / starting"), Tone::Warn),
                (e, 0) => (format!("{e} failing"), Tone::Err),
                (e, w) => (format!("{e} failing, {w} pending"), Tone::Err),
            };
            let (sub, tone) = failed(pods).unwrap_or((sub, tone));
            if card(ui, pal, "Pods", &count(pods, pod_rows.len()), &sub, tone, loading(pods)) {
                action = Some(Action::Go(k_pods.clone()));
            }
            let (sub, tone) = if let Some(why) = failed(deploys) {
                why
            } else if dep_bad == 0 {
                ("all available".to_string(), Tone::Ok)
            } else {
                (format!("{dep_bad} degraded"), Tone::Warn)
            };
            if card(ui, pal, "Deployments", &count(deploys, dep_rows.len()), &sub, tone, loading(deploys)) {
                action = Some(Action::Go(k_deploy.clone()));
            }
            let (sub, tone) = if let Some(why) = failed(events) {
                why
            } else if warn_events.is_empty() {
                ("no warnings".to_string(), Tone::Ok)
            } else {
                ("in the last hour".to_string(), Tone::Warn)
            };
            if card(ui, pal, "Warning events", &count(events, warn_events.len()), &sub, tone, loading(events)) {
                action = Some(Action::Go(k_events.clone()));
            }
        });

        // Cluster-wide usage from metrics-server.
        if metrics.available {
            let cpu_col = nodes.column("CPU").map(|i| &nodes.columns[i]);
            let mem_col = nodes.column("Memory").map(|i| &nodes.columns[i]);
            let (mut cpu_cap, mut mem_cap, mut cpu_used, mut mem_used) = (0.0, 0.0, 0.0, 0.0);
            for r in nodes.rows.iter() {
                let key = |c: Option<&skiff_core::Column>| c.and_then(|c| r.cell(c)).map(|c| c.key).filter(|k| !k.is_nan());
                cpu_cap += key(cpu_col).unwrap_or(0.0);
                mem_cap += key(mem_col).unwrap_or(0.0);
                if let Some(u) = metrics.node(&r.name) {
                    cpu_used += u.cpu;
                    mem_used += u.memory;
                }
            }
            if cpu_cap > 0.0 && mem_cap > 0.0 {
                ui.add_space(14.0);
                widgets::caption(ui, "Resource usage", pal);
                ui.add_space(4.0);
                for (label, used, cap, text) in [
                    ("CPU", cpu_used, cpu_cap, format!("{} of {} cores", fmt_cpu(cpu_used), fmt_cpu(cpu_cap))),
                    ("Memory", mem_used, mem_cap, format!("{} of {}", fmt_bytes(mem_used), fmt_bytes(mem_cap))),
                ] {
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(64.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.set_min_width(64.0);
                            ui.label(RichText::new(label).color(pal.muted));
                        });
                        let frac = (used / cap) as f32;
                        widgets::usage_bar(ui, frac, 260.0, pal);
                        ui.label(format!("{:.0}%", frac * 100.0));
                        ui.label(RichText::new(text).color(pal.muted).size(12.0));
                    });
                }
            }
        }

        // What Prometheus says is wrong (when the cluster has one).
        if let Some(open) = firing_alerts(ui, pal, session, ns, nodes) {
            action = Some(open);
        }

        // What's broken right now.
        let mut problems: Vec<(&ResourceKey, &Snapshot, &Arc<Row>)> = Vec::new();
        problems.extend(nodes.rows.iter().filter(|r| r.tone >= Tone::Warn).map(|r| (&k_nodes, &**nodes, r)));
        problems.extend(dep_rows.iter().filter(|r| r.tone >= Tone::Warn).map(|r| (&k_deploy, &**deploys, *r)));
        problems.extend(pod_rows.iter().filter(|r| r.tone >= Tone::Warn).map(|r| (&k_pods, &**pods, *r)));
        problems.sort_by(|a, b| b.2.tone.cmp(&a.2.tone).then_with(|| a.2.name.cmp(&b.2.name)));

        ui.add_space(16.0);
        widgets::caption(ui, &format!("Needs attention · {}", problems.len()), pal);
        ui.add_space(4.0);
        // A list that failed says nothing about health, and one that is
        // still loading does not yet: "nothing is failing" waits for both.
        // A list the account may not read is expected, and no warning.
        let lists: [(&str, &Snapshot); 4] =
            [("nodes", nodes), ("pods", pods), ("deployments", deploys), ("events", events)];
        let verdict = verdict(&lists);
        if let Verdict::Unknown(unknown) = &verdict {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::icon(icons::ICON_ERROR, 16.0).color(pal.warn));
                ui.label(
                    RichText::new(format!(
                        "Couldn't list {}, so this page can't say whether they are healthy.",
                        unknown.join(", ")
                    ))
                    .color(pal.muted),
                );
            });
        } else if let (true, Verdict::Clear { unreadable }) = (problems.is_empty(), &verdict) {
            ui.horizontal(|ui| {
                ui.label(widgets::icon(icons::ICON_CHECK_CIRCLE, 16.0).color(pal.ok));
                ui.label(RichText::new("Nothing is failing in the selected scope.").color(pal.muted));
                if !unreadable.is_empty() {
                    ui.label(
                        RichText::new(format!("(No access to {}.)", unreadable.join(", "))).color(pal.faint).size(12.0),
                    );
                }
            });
        }
        for (key, snap, row) in problems.iter().take(40) {
            ui.horizontal(|ui| {
                widgets::dot(ui, pal.tone(row.tone));
                ui.allocate_ui_with_layout(egui::vec2(84.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_min_width(84.0);
                    ui.label(RichText::new(&*key.kind).color(pal.muted).size(12.0));
                });
                if ui.link(&*row.name).clicked() {
                    action = Some(Action::Open {
                        key: (*key).clone(),
                        ns: row.namespace.to_string(),
                        name: row.name.to_string(),
                    });
                }
                if !row.namespace.is_empty() {
                    ui.label(RichText::new(&*row.namespace).color(pal.faint).size(12.0));
                }
                ui.label(RichText::new(status_text(snap, row)).color(pal.tone(row.tone)).size(12.0));
            });
        }
        if problems.len() > 40 {
            ui.label(RichText::new(format!("… and {} more", problems.len() - 40)).color(pal.muted));
        }

        // Latest warnings.
        if let (Some(c_seen), Some(c_reason), Some(c_obj), Some(c_msg)) = (
            events.column("Last Seen").map(|i| &events.columns[i]),
            events.column("Reason").map(|i| &events.columns[i]),
            events.column("Object").map(|i| &events.columns[i]),
            events.column("Message").map(|i| &events.columns[i]),
        ) {
            let mut recent = warn_events.clone();
            recent.sort_by(|a, b| skiff_core::compare(a, b, c_seen));
            ui.add_space(16.0);
            widgets::caption(ui, "Recent warnings", pal);
            ui.add_space(4.0);
            if recent.is_empty() {
                ui.label(RichText::new("No warning events.").color(pal.muted));
            }
            for row in recent.iter().take(15) {
                ui.horizontal(|ui| {
                    widgets::dot(ui, pal.warn);
                    let seen = row.cell(c_seen).map(|c| c.key).unwrap_or(f64::NAN);
                    ui.allocate_ui_with_layout(egui::vec2(46.0, 18.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.set_min_width(46.0);
                        if !seen.is_nan() {
                            ui.label(RichText::new(fmt_age(now - seen as i64)).color(pal.muted).size(12.0));
                        }
                    });
                    ui.label(widgets::semibold(row.text(c_reason), 13.0).color(pal.text));
                    ui.label(RichText::new(row.text(c_obj)).color(pal.muted).size(12.0));
                    ui.add(egui::Label::new(RichText::new(row.text(c_msg)).color(pal.muted)).truncate());
                });
            }
        }
        ui.add_space(20.0);
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_access_is_not_a_failed_list() {
        let live = Snapshot::empty(LoadState::Live);
        let loading = Snapshot::empty(LoadState::Loading);
        let failed = |code| Snapshot { error_code: code, ..Snapshot::empty(LoadState::Failed) };
        let (denied, broken) = (failed(403), failed(500));
        // An account limited to a namespace can never list nodes: the page
        // still says that what it can read is fine.
        assert_eq!(
            verdict(&[("nodes", &denied), ("pods", &live), ("events", &live)]),
            Verdict::Clear { unreadable: vec!["nodes"] }
        );
        assert_eq!(verdict(&[("nodes", &live), ("pods", &live)]), Verdict::Clear { unreadable: vec![] });
        // A list that failed for another reason is what is not known.
        assert_eq!(
            verdict(&[("nodes", &denied), ("pods", &broken), ("events", &live)]),
            Verdict::Unknown(vec!["pods"])
        );
        assert_eq!(verdict(&[("nodes", &denied), ("pods", &loading)]), Verdict::Waiting);
        // Nothing readable at all is not "nothing is failing".
        assert_eq!(verdict(&[("nodes", &denied), ("pods", &denied)]), Verdict::Waiting);
    }

    #[test]
    fn the_line_under_the_alerts_counts_what_is_not_listed() {
        assert_eq!(more_alerts_text(0, 0, true), None);
        assert_eq!(more_alerts_text(3, 0, false).as_deref(), Some("… and 3 more"));
        // Alerts the cluster has but Skiff did not keep are part of "more"
        // for the whole cluster, and named apart for a selection of
        // namespaces (nobody knows which of them belong to it).
        assert_eq!(more_alerts_text(1988, 50, true).as_deref(), Some("… and 2,038 more"));
        assert_eq!(
            more_alerts_text(2, 50, false).as_deref(),
            Some("… and 2 more, and 50 less severe in the cluster that are not counted here")
        );
        assert_eq!(
            more_alerts_text(0, 50, false).as_deref(),
            Some("… and 50 less severe in the cluster that are not counted here")
        );
        assert_eq!(widgets::clipped("KubePodCrashLooping", 48), "KubePodCrashLooping");
        assert_eq!(widgets::clipped("ééé and more", 3), "ééé…");
    }
}
