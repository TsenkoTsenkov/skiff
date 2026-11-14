//! Cluster overview: health at a glance and what needs attention.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui};
use egui_material_icons::icons;
use skiff_core::kinds;
use skiff_core::{fmt_age, fmt_bytes, fmt_cpu, now_unix, ColKind, LoadState, ResourceKey, Row, Session, Snapshot, Tone};

use crate::theme::Palette;
use crate::widgets;

pub enum Action {
    Go(ResourceKey),
    Open { key: ResourceKey, ns: String, name: String },
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

pub fn show(ui: &mut Ui, pal: &Palette, session: &Arc<Session>, ns: &BTreeSet<String>) -> Option<Action> {
    let mut action = None;
    let (k_nodes, k_pods, k_events) = (kinds::nodes(), kinds::pods(), kinds::events());
    let k_deploy = kinds::builtin("apps", "deployments").unwrap().key();
    let nodes = session.table(&k_nodes).snapshot();
    let pods = session.table(&k_pods).snapshot();
    let deploys = session.table(&k_deploy).snapshot();
    let events = session.table(&k_events).snapshot();
    let metrics = session.metrics();
    let now = now_unix();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(widgets::semibold("Overview", 17.0).color(pal.text));
            ui.label(RichText::new(&session.context).color(pal.muted));
            if !ns.is_empty() {
                widgets::pill(ui, &format!("{} selected", widgets::plural(ns.len(), "namespace")), pal.info, pal);
            }
        });
        ui.add_space(10.0);

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
            let sub = if node_bad == 0 { "all ready".to_string() } else { format!("{node_bad} not ready / cordoned") };
            let tone = if node_bad == 0 { Tone::Ok } else { Tone::Warn };
            if card(ui, pal, "Nodes", &nodes.rows.len().to_string(), &sub, tone, loading(&nodes)) {
                action = Some(Action::Go(k_nodes.clone()));
            }
            let (sub, tone) = match (pod_err, pod_warn) {
                (0, 0) => ("all healthy".to_string(), Tone::Ok),
                (0, w) => (format!("{w} pending / starting"), Tone::Warn),
                (e, 0) => (format!("{e} failing"), Tone::Err),
                (e, w) => (format!("{e} failing, {w} pending"), Tone::Err),
            };
            if card(ui, pal, "Pods", &widgets::thousands(pod_rows.len()), &sub, tone, loading(&pods)) {
                action = Some(Action::Go(k_pods.clone()));
            }
            let sub = if dep_bad == 0 { "all available".to_string() } else { format!("{dep_bad} degraded") };
            let tone = if dep_bad == 0 { Tone::Ok } else { Tone::Warn };
            if card(ui, pal, "Deployments", &widgets::thousands(dep_rows.len()), &sub, tone, loading(&deploys)) {
                action = Some(Action::Go(k_deploy.clone()));
            }
            let sub = if warn_events.is_empty() { "no warnings".to_string() } else { "in the last hour".to_string() };
            let tone = if warn_events.is_empty() { Tone::Ok } else { Tone::Warn };
            if card(ui, pal, "Warning events", &widgets::thousands(warn_events.len()), &sub, tone, loading(&events)) {
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

        // What's broken right now.
        let mut problems: Vec<(&ResourceKey, &Snapshot, &Arc<Row>)> = Vec::new();
        problems.extend(nodes.rows.iter().filter(|r| r.tone >= Tone::Warn).map(|r| (&k_nodes, &*nodes, r)));
        problems.extend(dep_rows.iter().filter(|r| r.tone >= Tone::Warn).map(|r| (&k_deploy, &*deploys, *r)));
        problems.extend(pod_rows.iter().filter(|r| r.tone >= Tone::Warn).map(|r| (&k_pods, &*pods, *r)));
        problems.sort_by(|a, b| b.2.tone.cmp(&a.2.tone).then_with(|| a.2.name.cmp(&b.2.name)));

        ui.add_space(16.0);
        widgets::caption(ui, &format!("Needs attention · {}", problems.len()), pal);
        ui.add_space(4.0);
        if problems.is_empty() {
            ui.horizontal(|ui| {
                ui.label(widgets::icon(icons::ICON_CHECK_CIRCLE, 16.0).color(pal.ok));
                ui.label(RichText::new("Nothing is failing in the selected scope.").color(pal.muted));
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
