//! The detail side panel: summary, YAML (view/edit) and events of an object.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use egui::{Align, CornerRadius, Frame, Key, Layout, Margin, Modifiers, RichText, Stroke, Ui};
use egui_extras::syntax_highlighting::{highlight, CodeTheme};
use egui_material_icons::icons;
use serde_json::Value;
use skiff_core::cells::{parse_time, status_tone};
use skiff_core::kinds;
use skiff_core::ops::{containers_of, pod_selector_of};
use skiff_core::{fmt_age, now_unix, ColSource, Job, ListQuery, ObjectData, ResourceKey, Session, Snapshot, Tone};

use crate::table_view::{actions_for, RowAction};
use crate::theme::{self, Palette};
use crate::widgets;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Yaml,
    Events,
}

pub enum DetailAction {
    Close,
    Row(RowAction),
    Open { kind: String, ns: String, name: String },
    Toast(String, Tone),
    LogsOf { container: String },
    ShellIn { container: String },
    ForwardPort { port: u16 },
}

struct EditState {
    text: String,
    job: Option<Job<ObjectData>>,
    error: Option<String>,
}

pub struct Detail {
    pub key: ResourceKey,
    pub ns: String,
    pub name: String,
    pub tab: DetailTab,
    obj: Option<ObjectData>,
    obj_error: Option<String>,
    obj_job: Option<Job<ObjectData>>,
    /// Row resourceVersion the last fetch was triggered for.
    fetched_for: Option<String>,
    fetched_at: Instant,
    events: Option<Arc<Snapshot>>,
    events_job: Option<Job<Arc<Snapshot>>>,
    events_at: Option<Instant>,
    pods: Option<Arc<Snapshot>>,
    pods_job: Option<Job<Arc<Snapshot>>>,
    pods_at: Option<Instant>,
    edit: Option<EditState>,
    want_edit: bool,
    revealed: HashSet<String>,
    /// The live table no longer lists this object.
    gone: bool,
    /// Highlighted YAML for (resourceVersion, dark theme), so switching
    /// tabs or repainting does not re-run the highlighter.
    yaml_job: Option<(String, bool, egui::text::LayoutJob)>,
}

const REFRESH: Duration = Duration::from_secs(5);

fn s<'a>(v: &'a Value, path: &str) -> &'a str {
    v.pointer(path).and_then(|x| x.as_str()).unwrap_or("")
}

fn int(v: &Value, path: &str) -> Option<i64> {
    v.pointer(path).and_then(|x| x.as_i64())
}

fn arr<'a>(v: &'a Value, path: &str) -> &'a [Value] {
    v.pointer(path).and_then(|x| x.as_array()).map(|a| a.as_slice()).unwrap_or(&[])
}

fn age_of(ts: &str) -> String {
    let t = parse_time(ts);
    if t > 0 {
        fmt_age(now_unix() - t)
    } else {
        String::new()
    }
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

impl Detail {
    pub fn open(session: &Arc<Session>, key: &ResourceKey, ns: &str, name: &str) -> Self {
        Detail {
            key: key.clone(),
            ns: ns.to_string(),
            name: name.to_string(),
            tab: DetailTab::Overview,
            obj: None,
            obj_error: None,
            obj_job: Some(session.get_object(key, ns, name)),
            fetched_for: None,
            fetched_at: Instant::now(),
            events: None,
            events_job: None,
            events_at: None,
            pods: None,
            pods_job: None,
            pods_at: None,
            edit: None,
            want_edit: false,
            revealed: HashSet::new(),
            gone: false,
            yaml_job: None,
        }
    }

    pub fn set_gone(&mut self, gone: bool) {
        self.gone = gone;
    }

    pub fn is_for(&self, key: &ResourceKey, ns: &str, name: &str) -> bool {
        &self.key == key && self.ns == ns && self.name == name
    }

    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// Whether the YAML editor holds changes that have not been applied.
    pub fn has_unsaved_edit(&self) -> bool {
        match (&self.edit, &self.obj) {
            (Some(edit), Some(obj)) => edit.text != obj.yaml,
            _ => false,
        }
    }

    pub fn start_edit(&mut self) {
        self.tab = DetailTab::Yaml;
        if self.edit.is_some() {
            // Already editing: keep what the user typed.
            return;
        }
        match &self.obj {
            Some(obj) => self.edit = Some(EditState { text: obj.yaml.clone(), job: None, error: None }),
            None => self.want_edit = true,
        }
    }

    fn related_pods_query(&self, obj: &Value) -> Option<(Option<String>, ListQuery)> {
        if self.key.is("", "nodes") {
            return Some((
                None,
                ListQuery { field_selector: Some(format!("spec.nodeName={}", self.name)), ..Default::default() },
            ));
        }
        let workload = matches!(
            (&*self.key.group, &*self.key.plural),
            ("apps", "deployments" | "statefulsets" | "daemonsets" | "replicasets") | ("batch", "jobs") | ("", "services")
        );
        if !workload {
            return None;
        }
        let selector = pod_selector_of(obj)?;
        Some((Some(self.ns.clone()), ListQuery { label_selector: Some(selector), ..Default::default() }))
    }

    /// Advances background fetches. `current_rv` is the resourceVersion the
    /// live table currently shows for this object, if it is in view.
    pub fn poll(&mut self, session: &Arc<Session>, current_rv: Option<&str>) {
        if let Some(job) = &self.obj_job {
            if let Some(res) = job.take() {
                self.obj_job = None;
                match res {
                    Ok(obj) => {
                        self.obj_error = None;
                        if self.want_edit {
                            self.want_edit = false;
                            self.edit = Some(EditState { text: obj.yaml.clone(), job: None, error: None });
                        }
                        self.obj = Some(obj);
                        // Related data may have changed with the object.
                        self.pods_at = None;
                    }
                    Err(e) => self.obj_error = Some(e),
                }
            }
        }
        // Follow the live table: refetch when the row moved to a new version.
        if let Some(rv) = current_rv {
            let stale = self.fetched_for.as_deref() != Some(rv)
                && self.obj.as_ref().map(|o| o.rv.as_str()) != Some(rv);
            if stale && self.obj_job.is_none() && self.fetched_at.elapsed() > Duration::from_millis(250) {
                self.fetched_for = Some(rv.to_string());
                self.fetched_at = Instant::now();
                self.obj_job = Some(session.get_object(&self.key, &self.ns, &self.name));
            }
        }

        if let Some(job) = &self.events_job {
            if let Some(res) = job.take() {
                self.events_job = None;
                if let Ok(snap) = res {
                    self.events = Some(snap);
                }
            }
        }
        let events_due = self.events_at.is_none_or(|t| t.elapsed() > REFRESH);
        if events_due && self.events_job.is_none() && !self.key.is("", "events") {
            // Match on the UID so a recreated object of the same name does
            // not inherit its predecessor's events. (Node events are
            // recorded against the node name instead.)
            if let Some(obj) = &self.obj {
                let uid = s(&obj.value, "/metadata/uid");
                let uid = if self.key.is("", "nodes") { None } else { Some(uid) };
                self.events_at = Some(Instant::now());
                self.events_job = Some(session.events_for(&self.ns, &self.key.kind, &self.name, uid));
            }
        }

        if let Some(job) = &self.pods_job {
            if let Some(res) = job.take() {
                self.pods_job = None;
                if let Ok(snap) = res {
                    self.pods = Some(snap);
                }
            }
        }
        let pods_due = self.pods_at.is_none_or(|t| t.elapsed() > REFRESH);
        if pods_due && self.pods_job.is_none() && self.tab == DetailTab::Overview {
            if let Some(obj) = &self.obj {
                if let Some((ns, query)) = self.related_pods_query(&obj.value) {
                    self.pods_at = Some(Instant::now());
                    self.pods_job = Some(session.list(&kinds::pods(), ns.as_deref(), query));
                }
            }
        }

        if let Some(edit) = &mut self.edit {
            if let Some(job) = &edit.job {
                if let Some(res) = job.take() {
                    edit.job = None;
                    match res {
                        Ok(obj) => {
                            self.obj = Some(obj);
                            self.edit = None;
                        }
                        Err(e) => edit.error = Some(e),
                    }
                }
            }
        }
    }

    /// `overlay_open`: a dialog, the palette or a menu is on top, so keys
    /// pressed this frame are not meant for the panel.
    pub fn ui(&mut self, ui: &mut Ui, pal: &Palette, session: &Arc<Session>, overlay_open: bool) -> Vec<DetailAction> {
        let mut actions = Vec::new();

        // Header.
        ui.horizontal(|ui| {
            ui.label(RichText::new(&*self.key.kind).color(pal.muted).size(12.0));
            if !self.ns.is_empty() {
                ui.label(RichText::new(format!("·  {}", self.ns)).color(pal.faint).size(12.0));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, icons::ICON_CLOSE, "Close (Esc)").clicked() {
                    actions.push(DetailAction::Close);
                }
            });
        });
        ui.add(egui::Label::new(widgets::semibold(&self.name, 16.0).color(pal.text)).wrap().selectable(true));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // Offer only the half of a toggle that applies right now.
            let flag = |path: &str| {
                self.obj
                    .as_ref()
                    .map(|o| o.value.pointer(path).and_then(|v| v.as_bool()).unwrap_or(false))
            };
            let (cordoned, suspended) = (flag("/spec/unschedulable"), flag("/spec/suspend"));
            for (action, ic, label) in actions_for(&self.key) {
                let redundant = match action {
                    RowAction::Edit => self.edit.is_some(),
                    RowAction::Cordon => cordoned == Some(true),
                    RowAction::Uncordon => cordoned == Some(false),
                    RowAction::Suspend => suspended == Some(true),
                    RowAction::Resume => suspended == Some(false),
                    _ => false,
                };
                if redundant {
                    continue;
                }
                let color = if action == RowAction::Delete { pal.err } else { pal.text };
                let btn = egui::Button::new(widgets::icon(ic, 16.0).color(color))
                    .frame_when_inactive(false)
                    .min_size(egui::vec2(28.0, 26.0));
                if widgets::named(ui.add(btn), label).clicked() {
                    actions.push(DetailAction::Row(action));
                }
            }
        });
        ui.add_space(6.0);

        // Tabs.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let event_count = self.events.as_ref().map(|e| e.rows.len()).unwrap_or(0);
            let warn = self.events.as_ref().is_some_and(|e| e.rows.iter().any(|r| r.tone >= Tone::Warn));
            let events_label = if event_count > 0 { format!("Events  {event_count}") } else { "Events".to_string() };
            for (tab, label) in [
                (DetailTab::Overview, "Overview".to_string()),
                (DetailTab::Yaml, "YAML".to_string()),
                (DetailTab::Events, events_label),
            ] {
                let selected = self.tab == tab;
                let mut text = RichText::new(label);
                if tab == DetailTab::Events && warn && !selected {
                    text = text.color(pal.warn);
                }
                if ui.add(egui::Button::selectable(selected, text)).clicked() {
                    self.tab = tab;
                }
            }
            if self.obj_job.is_some() && self.obj.is_some() {
                ui.spinner();
            }
        });
        ui.add_space(2.0);
        ui.separator();

        let Some(obj) = &self.obj else {
            match &self.obj_error {
                Some(e) => widgets::empty_state(ui, icons::ICON_ERROR, "Couldn't load object", e, pal),
                None => widgets::empty_state(ui, icons::ICON_SYNC, "Loading…", "", pal),
            }
            return actions;
        };
        if self.gone {
            Frame::new()
                .fill(pal.tint(pal.warn, 0.12))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.label(RichText::new("This object no longer exists. Showing its last known state.").color(pal.warn));
                });
        } else if let Some(e) = &self.obj_error {
            ui.label(RichText::new(format!("Refresh failed: {e}")).color(pal.warn).size(12.0));
        }

        match self.tab {
            DetailTab::Overview => {
                let value = obj.value.clone();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    self.overview(ui, pal, &value, &mut actions);
                    ui.add_space(16.0);
                });
            }
            DetailTab::Yaml => self.yaml_tab(ui, pal, session, overlay_open, &mut actions),
            DetailTab::Events => self.events_tab(ui, pal),
        }
        actions
    }

    // ------------------------------------------------------------------
    // YAML
    // ------------------------------------------------------------------

    fn yaml_tab(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        session: &Arc<Session>,
        overlay_open: bool,
        actions: &mut Vec<DetailAction>,
    ) {
        // Highlighting is linear in the document and runs on the UI thread:
        // fine for ordinary objects, too slow to redo on every keystroke or
        // at all for very large ones.
        const HIGHLIGHT_VIEW_MAX: usize = 300_000;
        const HIGHLIGHT_EDIT_MAX: usize = 60_000;
        let theme = if pal.dark { CodeTheme::dark(12.5) } else { CodeTheme::light(12.5) };
        let Some(obj) = &self.obj else { return };
        if let Some(edit) = &mut self.edit {
            let saving = edit.job.is_some();
            let mut apply = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                let label = (widgets::icon(icons::ICON_SAVE, 15.0), "Apply");
                if ui.add_enabled(!saving, egui::Button::new(label)).on_hover_text("Ctrl+S").clicked() {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if saving {
                    ui.spinner();
                }
                if edit.text != obj.yaml {
                    ui.label(RichText::new("modified").color(pal.warn).size(12.0));
                }
            });
            if let Some(err) = &edit.error {
                Frame::new()
                    .fill(pal.tint(pal.err, 0.12))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(err).color(pal.err)).wrap().selectable(true));
                    });
            }
            if !overlay_open && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) {
                apply = true;
            }
            // Esc leaves edit mode only when nothing would be lost and the
            // key was not meant for a dialog or menu on top.
            if !overlay_open && edit.text == obj.yaml && ui.input(|i| i.key_pressed(Key::Escape)) {
                cancel = true;
            }
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                    let job = if text.as_str().len() <= HIGHLIGHT_EDIT_MAX {
                        highlight(ui.ctx(), ui.style(), &theme, text.as_str(), "yaml")
                    } else {
                        egui::text::LayoutJob::simple(
                            text.as_str().to_owned(),
                            theme::mono(12.5),
                            pal.text,
                            f32::INFINITY,
                        )
                    };
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                ui.add(
                    egui::TextEdit::multiline(&mut edit.text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(30)
                        .frame(egui::Frame::NONE)
                        .layouter(&mut layouter),
                );
            });
            if apply && !saving {
                edit.error = None;
                edit.job = Some(session.replace_object(&self.key, &self.ns, &self.name, edit.text.clone()));
            }
            if cancel {
                self.edit = None;
            }
        } else {
            ui.horizontal(|ui| {
                if widgets::action_button(ui, icons::ICON_EDIT, "Edit").clicked() {
                    actions.push(DetailAction::Row(RowAction::Edit));
                }
                if widgets::action_button(ui, icons::ICON_CONTENT_COPY, "Copy").clicked() {
                    ui.ctx().copy_text(obj.yaml.clone());
                    actions.push(DetailAction::Toast("YAML copied".into(), Tone::Normal));
                }
                ui.label(RichText::new("managedFields hidden").color(pal.faint).size(11.5));
            });
            let cached = self.yaml_job.as_ref().is_some_and(|(rv, dark, _)| *rv == obj.rv && *dark == pal.dark);
            if !cached {
                let job = if obj.yaml.len() <= HIGHLIGHT_VIEW_MAX {
                    highlight(ui.ctx(), ui.style(), &theme, &obj.yaml, "yaml")
                } else {
                    egui::text::LayoutJob::simple(obj.yaml.clone(), theme::mono(12.5), pal.text, f32::INFINITY)
                };
                self.yaml_job = Some((obj.rv.clone(), pal.dark, job));
            }
            let job = self.yaml_job.as_ref().map(|(_, _, job)| job.clone()).unwrap_or_default();
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                ui.add(egui::Label::new(job).selectable(true).extend());
            });
        }
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    fn events_tab(&mut self, ui: &mut Ui, pal: &Palette) {
        let Some(events) = &self.events else {
            widgets::empty_state(ui, icons::ICON_SYNC, "Loading events…", "", pal);
            return;
        };
        if events.rows.is_empty() {
            widgets::empty_state(
                ui,
                icons::ICON_NOTIFICATIONS,
                "No events",
                "Kubernetes keeps events for about an hour.",
                pal,
            );
            return;
        }
        let col = |title: &str| events.column(title).map(|i| &events.columns[i]);
        let (Some(c_type), Some(c_reason), Some(c_msg), Some(c_seen)) =
            (col("Type"), col("Reason"), col("Message"), col("Last Seen"))
        else {
            return;
        };
        let c_count = col("Count");
        let mut rows: Vec<_> = events.rows.iter().collect();
        rows.sort_by(|a, b| skiff_core::compare(a, b, c_seen));
        let now = now_unix();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for r in rows {
                let tone = status_tone(r.text(c_type)).max(r.cell(c_reason).map(|c| c.tone).unwrap_or(Tone::Normal));
                let color = if tone >= Tone::Warn { pal.tone(tone) } else { pal.faint };
                ui.horizontal(|ui| {
                    widgets::dot(ui, color);
                    ui.label(widgets::semibold(r.text(c_reason), 13.0).color(pal.text));
                    let count = c_count.map(|c| r.text(c)).unwrap_or("");
                    if !count.is_empty() && count != "1" {
                        ui.label(RichText::new(format!("×{count}")).color(pal.muted).size(12.0));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let seen = r.cell(c_seen).map(|c| c.key).unwrap_or(f64::NAN);
                        if !seen.is_nan() {
                            ui.label(RichText::new(format!("{} ago", fmt_age(now - seen as i64))).color(pal.muted).size(12.0));
                        }
                    });
                });
                ui.indent(("event", &*r.uid), |ui| {
                    ui.add(egui::Label::new(RichText::new(r.text(c_msg)).color(pal.muted)).wrap().selectable(true));
                });
                ui.add_space(6.0);
            }
        });
    }

    // ------------------------------------------------------------------
    // Overview
    // ------------------------------------------------------------------

    fn overview(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        self.metadata(ui, pal, v, actions);
        match (&*self.key.group, &*self.key.plural) {
            ("", "pods") => self.pod(ui, pal, v, actions),
            ("apps", "deployments" | "statefulsets" | "daemonsets" | "replicasets") => self.workload(ui, pal, v),
            ("batch", "jobs") => self.job(ui, pal, v),
            ("batch", "cronjobs") => self.cronjob(ui, pal, v),
            ("", "services") => self.service(ui, pal, v, actions),
            ("networking.k8s.io", "ingresses") => self.ingress(ui, pal, v, actions),
            ("", "nodes") => self.node(ui, pal, v),
            ("", "configmaps") => self.configmap(ui, pal, v, actions),
            ("", "secrets") => self.secret(ui, pal, v, actions),
            ("", "persistentvolumeclaims") => self.pvc(ui, pal, v, actions),
            ("", "events") => self.event(ui, pal, v, actions),
            _ => {}
        }
        conditions(ui, pal, v);
        self.related_pods(ui, pal, actions);

        // Kinds without a tailored view (most config kinds and every custom
        // resource) get all of their fields: scalars as rows, the rest as
        // collapsible trees.
        if !self.has_tailored_view() {
            let Some(fields) = v.as_object() else { return };
            let rest = || fields.iter().filter(|(k, _)| !matches!(k.as_str(), "apiVersion" | "kind" | "metadata"));
            for (k, val) in rest() {
                if !val.is_object() && !val.is_array() {
                    widgets::kv_text(ui, k, &scalar_text(val), pal);
                }
            }
            for (k, val) in rest() {
                let empty = val.as_object().is_some_and(|o| o.is_empty()) || val.as_array().is_some_and(|a| a.is_empty());
                if (val.is_object() || val.is_array()) && !empty {
                    widgets::section(ui, k, pal);
                    json_tree(ui, pal, val, &format!("/{k}"), 0);
                }
            }
        }
    }

    fn has_tailored_view(&self) -> bool {
        matches!(
            (&*self.key.group, &*self.key.plural),
            ("", "pods" | "services" | "nodes" | "configmaps" | "secrets" | "persistentvolumeclaims" | "events")
                | ("apps", "deployments" | "statefulsets" | "daemonsets" | "replicasets")
                | ("batch", "jobs" | "cronjobs")
                | ("networking.k8s.io", "ingresses")
        )
    }

    fn metadata(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        let created = s(v, "/metadata/creationTimestamp");
        if !created.is_empty() {
            widgets::kv(ui, "Created", pal, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} ago", age_of(created)));
                    ui.label(RichText::new(created).color(pal.faint).size(12.0));
                });
            });
        }
        if let Some(del) = v.pointer("/metadata/deletionTimestamp").and_then(|d| d.as_str()) {
            widgets::kv(ui, "Deleting", pal, |ui| {
                ui.label(RichText::new(format!("since {} ago", age_of(del))).color(pal.warn));
            });
        }
        for owner in arr(v, "/metadata/ownerReferences") {
            let (kind, name) = (s(owner, "/kind").to_string(), s(owner, "/name").to_string());
            widgets::kv(ui, "Controlled by", pal, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&kind).color(pal.muted));
                    if ui.link(&name).clicked() {
                        actions.push(DetailAction::Open { kind: kind.clone(), ns: self.ns.clone(), name: name.clone() });
                    }
                });
            });
        }
        if let Some(labels) = v.pointer("/metadata/labels").and_then(|l| l.as_object()) {
            if !labels.is_empty() {
                widgets::kv(ui, "Labels", pal, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        for (k, val) in labels {
                            let text = format!("{k}={}", val.as_str().unwrap_or(""));
                            if widgets::chip(ui, &text, pal).on_hover_text("Click to copy").clicked() {
                                ui.ctx().copy_text(text.clone());
                                actions.push(DetailAction::Toast(format!("Copied {text}"), Tone::Normal));
                            }
                        }
                    });
                });
            }
        }
        if let Some(ann) = v.pointer("/metadata/annotations").and_then(|l| l.as_object()) {
            if !ann.is_empty() {
                widgets::kv(ui, "Annotations", pal, |ui| {
                    egui::CollapsingHeader::new(RichText::new(widgets::plural(ann.len(), "annotation")).color(pal.muted))
                        .id_salt("annotations")
                        .show(ui, |ui| {
                            for (k, val) in ann {
                                ui.label(RichText::new(k).color(pal.muted).size(12.0));
                                let text = val.as_str().unwrap_or("");
                                let shown: String = if text.len() > 400 { format!("{}…", &text[..text.floor_char_boundary(400)]) } else { text.to_string() };
                                ui.add(egui::Label::new(RichText::new(shown).font(theme::mono(11.5))).wrap().selectable(true));
                                ui.add_space(4.0);
                            }
                        });
                });
            }
        }
        let finalizers: Vec<&str> = arr(v, "/metadata/finalizers").iter().filter_map(|f| f.as_str()).collect();
        if !finalizers.is_empty() {
            widgets::kv_text(ui, "Finalizers", &finalizers.join(", "), pal);
        }
    }

    fn pod(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        let phase = s(v, "/status/phase");
        widgets::kv(ui, "Phase", pal, |ui| {
            widgets::pill(ui, phase, pal.tone(status_tone(phase)), pal);
        });
        let node = s(v, "/spec/nodeName").to_string();
        if !node.is_empty() {
            widgets::kv(ui, "Node", pal, |ui| {
                if ui.link(&node).clicked() {
                    actions.push(DetailAction::Open { kind: "Node".into(), ns: String::new(), name: node.clone() });
                }
            });
        }
        widgets::kv_text(ui, "Pod IP", s(v, "/status/podIP"), pal);
        widgets::kv_text(ui, "Host IP", s(v, "/status/hostIP"), pal);
        widgets::kv_text(ui, "QoS class", s(v, "/status/qosClass"), pal);
        widgets::kv_text(ui, "Service account", s(v, "/spec/serviceAccountName"), pal);
        widgets::kv_text(ui, "Priority class", s(v, "/spec/priorityClassName"), pal);
        if !s(v, "/status/message").is_empty() {
            widgets::kv_text(ui, "Message", s(v, "/status/message"), pal);
        }

        for (title, spec_path, status_path) in [
            ("Containers", "/spec/containers", "/status/containerStatuses"),
            ("Init containers", "/spec/initContainers", "/status/initContainerStatuses"),
        ] {
            let specs = arr(v, spec_path);
            if specs.is_empty() {
                continue;
            }
            widgets::section(ui, title, pal);
            for c in specs {
                let name = s(c, "/name");
                let status = arr(v, status_path).iter().find(|st| s(st, "/name") == name);
                self.container_card(ui, pal, c, status, actions);
            }
        }

        let volumes = arr(v, "/spec/volumes");
        if !volumes.is_empty() {
            widgets::section(ui, "Volumes", pal);
            for vol in volumes {
                let name = s(vol, "/name");
                let (kind, source) = vol
                    .as_object()
                    .and_then(|o| o.iter().find(|(k, _)| k.as_str() != "name"))
                    .map(|(k, val)| {
                        let src = val
                            .get("claimName")
                            .or(val.get("secretName"))
                            .or(val.get("name"))
                            .or(val.get("path"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        (k.as_str(), src)
                    })
                    .unwrap_or(("", ""));
                ui.horizontal(|ui| {
                    ui.label(name);
                    ui.label(RichText::new(kind).color(pal.muted).size(12.0));
                    if !source.is_empty() {
                        let target_kind = match kind {
                            "persistentVolumeClaim" => Some("PersistentVolumeClaim"),
                            "secret" => Some("Secret"),
                            "configMap" => Some("ConfigMap"),
                            _ => None,
                        };
                        match target_kind {
                            Some(k) => {
                                if ui.link(source).clicked() {
                                    actions.push(DetailAction::Open {
                                        kind: k.into(),
                                        ns: self.ns.clone(),
                                        name: source.to_string(),
                                    });
                                }
                            }
                            None => {
                                ui.label(RichText::new(source).color(pal.faint).size(12.0));
                            }
                        }
                    }
                });
            }
        }
    }

    fn container_card(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        spec: &Value,
        status: Option<&Value>,
        actions: &mut Vec<DetailAction>,
    ) {
        let name = s(spec, "/name").to_string();
        let (state_text, state_tone, state_detail) = match status.and_then(|st| st.get("state")).and_then(|x| x.as_object()) {
            Some(state) => {
                if let Some(r) = state.get("running") {
                    ("Running".to_string(), Tone::Ok, format!("started {} ago", age_of(s(r, "/startedAt"))))
                } else if let Some(w) = state.get("waiting") {
                    let reason = s(w, "/reason");
                    let reason = if reason.is_empty() { "Waiting" } else { reason };
                    (reason.to_string(), status_tone(reason).max(Tone::Warn), s(w, "/message").to_string())
                } else if let Some(t) = state.get("terminated") {
                    let reason = s(t, "/reason");
                    let code = int(t, "/exitCode").unwrap_or(0);
                    let tone = if code == 0 { Tone::Muted } else { Tone::Err };
                    (
                        if reason.is_empty() { "Terminated".to_string() } else { reason.to_string() },
                        tone,
                        format!("exit code {code}, finished {} ago", age_of(s(t, "/finishedAt"))),
                    )
                } else {
                    ("Unknown".to_string(), Tone::Muted, String::new())
                }
            }
            None => ("Not started".to_string(), Tone::Muted, String::new()),
        };
        Frame::new()
            .fill(pal.raised)
            .stroke(Stroke::new(1.0, pal.border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::dot(ui, pal.tone(state_tone));
                    ui.label(widgets::semibold(&name, 13.0).color(pal.text));
                    widgets::pill(ui, &state_text, pal.tone(state_tone), pal);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, icons::ICON_TERMINAL, "Open a shell in this container").clicked() {
                            actions.push(DetailAction::ShellIn { container: name.clone() });
                        }
                        if widgets::icon_button(ui, icons::ICON_SUBJECT, "Logs of this container").clicked() {
                            actions.push(DetailAction::LogsOf { container: name.clone() });
                        }
                    });
                });
                ui.add(
                    egui::Label::new(RichText::new(s(spec, "/image")).font(theme::mono(12.0)).color(pal.muted))
                        .wrap()
                        .selectable(true),
                );
                if !state_detail.is_empty() {
                    ui.add(egui::Label::new(RichText::new(&state_detail).color(pal.muted).size(12.0)).wrap());
                }
                if let Some(st) = status {
                    let restarts = int(st, "/restartCount").unwrap_or(0);
                    if restarts > 0 {
                        let last = st.pointer("/lastState/terminated");
                        let why = last
                            .map(|t| {
                                format!(
                                    " — last: {} (exit {}) {} ago",
                                    s(t, "/reason"),
                                    int(t, "/exitCode").unwrap_or(0),
                                    age_of(s(t, "/finishedAt"))
                                )
                            })
                            .unwrap_or_default();
                        ui.label(RichText::new(format!("{restarts} restarts{why}")).color(pal.warn).size(12.0));
                    }
                }
                let ports = arr(spec, "/ports");
                if !ports.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Ports").color(pal.muted).size(12.0));
                        for p in ports {
                            let Some(port) = int(p, "/containerPort") else { continue };
                            let pname = s(p, "/name");
                            let proto = s(p, "/protocol");
                            let label = if pname.is_empty() {
                                format!("{port}/{}", if proto.is_empty() { "TCP" } else { proto })
                            } else {
                                format!("{pname} · {port}")
                            };
                            let btn = egui::Button::new((
                                widgets::icon(icons::ICON_SWAP_HORIZ, 13.0),
                                RichText::new(label).size(12.0),
                            ))
                            .small();
                            if ui.add(btn).on_hover_text("Forward this port to localhost").clicked() {
                                actions.push(DetailAction::ForwardPort { port: port as u16 });
                            }
                        }
                    });
                }
                let res = |kind: &str| {
                    let cpu = s(spec, &format!("/resources/{kind}/cpu"));
                    let mem = s(spec, &format!("/resources/{kind}/memory"));
                    match (cpu.is_empty(), mem.is_empty()) {
                        (true, true) => String::new(),
                        _ => format!("{kind}: cpu {}, mem {}", if cpu.is_empty() { "—" } else { cpu }, if mem.is_empty() { "—" } else { mem }),
                    }
                };
                let (req, lim) = (res("requests"), res("limits"));
                if !req.is_empty() || !lim.is_empty() {
                    let text = [req, lim].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("   ");
                    ui.label(RichText::new(text).color(pal.muted).size(12.0));
                }
                let env = arr(spec, "/env");
                if !env.is_empty() {
                    egui::CollapsingHeader::new(RichText::new(widgets::plural(env.len(), "environment variable")).color(pal.muted).size(12.0))
                        .id_salt(("env", &name))
                        .show(ui, |ui| {
                            for e in env {
                                let value = match e.get("valueFrom") {
                                    Some(from) => {
                                        if let Some(r) = from.get("secretKeyRef") {
                                            format!("secret {}/{}", s(r, "/name"), s(r, "/key"))
                                        } else if let Some(r) = from.get("configMapKeyRef") {
                                            format!("configmap {}/{}", s(r, "/name"), s(r, "/key"))
                                        } else if let Some(r) = from.get("fieldRef") {
                                            format!("field {}", s(r, "/fieldPath"))
                                        } else {
                                            "(reference)".to_string()
                                        }
                                    }
                                    None => s(e, "/value").to_string(),
                                };
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(s(e, "/name")).font(theme::mono(11.5)).color(pal.text));
                                    ui.add(
                                        egui::Label::new(RichText::new(value).font(theme::mono(11.5)).color(pal.muted))
                                            .wrap()
                                            .selectable(true),
                                    );
                                });
                            }
                        });
                }
                let mounts = arr(spec, "/volumeMounts");
                if !mounts.is_empty() {
                    egui::CollapsingHeader::new(RichText::new(widgets::plural(mounts.len(), "mount")).color(pal.muted).size(12.0))
                        .id_salt(("mounts", &name))
                        .show(ui, |ui| {
                            for m in mounts {
                                ui.label(
                                    RichText::new(format!("{}  ←  {}", s(m, "/mountPath"), s(m, "/name")))
                                        .font(theme::mono(11.5))
                                        .color(pal.muted),
                                );
                            }
                        });
                }
            });
        ui.add_space(4.0);
    }

    fn workload(&mut self, ui: &mut Ui, pal: &Palette, v: &Value) {
        let desired = int(v, "/spec/replicas").or(int(v, "/status/desiredNumberScheduled"));
        let ready = int(v, "/status/readyReplicas").or(int(v, "/status/numberReady")).unwrap_or(0);
        let updated = int(v, "/status/updatedReplicas").or(int(v, "/status/updatedNumberScheduled"));
        let available = int(v, "/status/availableReplicas").or(int(v, "/status/numberAvailable"));
        if let Some(d) = desired {
            widgets::kv(ui, "Replicas", pal, |ui| {
                ui.horizontal(|ui| {
                    let tone = if ready < d { Tone::Warn } else { Tone::Ok };
                    widgets::pill(ui, &format!("{ready}/{d} ready"), pal.tone(tone), pal);
                    if let Some(u) = updated {
                        ui.label(RichText::new(format!("{u} up to date")).color(pal.muted));
                    }
                    if let Some(a) = available {
                        ui.label(RichText::new(format!("{a} available")).color(pal.muted));
                    }
                });
            });
        }
        let strategy = s(v, "/spec/strategy/type");
        let strategy = if strategy.is_empty() { s(v, "/spec/updateStrategy/type") } else { strategy };
        widgets::kv_text(ui, "Strategy", strategy, pal);
        if let Some(sel) = pod_selector_of(v) {
            widgets::kv_text(ui, "Selector", &sel, pal);
        }
        let template = v.pointer("/spec/template").cloned().unwrap_or(Value::Null);
        let containers = containers_of(&template);
        if !containers.is_empty() {
            widgets::kv(ui, "Images", pal, |ui| {
                for c in &containers {
                    ui.add(
                        egui::Label::new(RichText::new(&c.image).font(theme::mono(12.0)).color(pal.text))
                            .wrap()
                            .selectable(true),
                    );
                }
            });
        }
    }

    fn job(&mut self, ui: &mut Ui, pal: &Palette, v: &Value) {
        let completions = int(v, "/spec/completions").unwrap_or(1);
        let succeeded = int(v, "/status/succeeded").unwrap_or(0);
        let failed = int(v, "/status/failed").unwrap_or(0);
        let active = int(v, "/status/active").unwrap_or(0);
        widgets::kv(ui, "Progress", pal, |ui| {
            ui.horizontal(|ui| {
                let tone = if failed > 0 { Tone::Err } else if succeeded >= completions { Tone::Ok } else { Tone::Warn };
                widgets::pill(ui, &format!("{succeeded}/{completions} succeeded"), pal.tone(tone), pal);
                if active > 0 {
                    ui.label(RichText::new(format!("{active} active")).color(pal.muted));
                }
                if failed > 0 {
                    ui.label(RichText::new(format!("{failed} failed")).color(pal.err));
                }
            });
        });
        widgets::kv_text(ui, "Started", &age_of(s(v, "/status/startTime")), pal);
        widgets::kv_text(ui, "Completed", &age_of(s(v, "/status/completionTime")), pal);
        if let Some(b) = int(v, "/spec/backoffLimit") {
            widgets::kv_text(ui, "Backoff limit", &b.to_string(), pal);
        }
    }

    fn cronjob(&mut self, ui: &mut Ui, pal: &Palette, v: &Value) {
        widgets::kv(ui, "Schedule", pal, |ui| {
            ui.label(RichText::new(s(v, "/spec/schedule")).font(theme::mono(12.5)));
        });
        let suspended = v.pointer("/spec/suspend").and_then(|x| x.as_bool()).unwrap_or(false);
        widgets::kv(ui, "State", pal, |ui| {
            if suspended {
                widgets::pill(ui, "Suspended", pal.warn, pal);
            } else {
                widgets::pill(ui, "Active", pal.ok, pal);
            }
        });
        widgets::kv_text(ui, "Concurrency", s(v, "/spec/concurrencyPolicy"), pal);
        let last = age_of(s(v, "/status/lastScheduleTime"));
        if !last.is_empty() {
            widgets::kv_text(ui, "Last schedule", &format!("{last} ago"), pal);
        }
        let ok = age_of(s(v, "/status/lastSuccessfulTime"));
        if !ok.is_empty() {
            widgets::kv_text(ui, "Last success", &format!("{ok} ago"), pal);
        }
        widgets::kv_text(ui, "Active jobs", &arr(v, "/status/active").len().to_string(), pal);
    }

    fn service(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        widgets::kv_text(ui, "Type", s(v, "/spec/type"), pal);
        widgets::kv_text(ui, "Cluster IP", s(v, "/spec/clusterIP"), pal);
        let ext: Vec<String> = arr(v, "/status/loadBalancer/ingress")
            .iter()
            .map(|i| {
                let ip = s(i, "/ip");
                if ip.is_empty() { s(i, "/hostname").to_string() } else { ip.to_string() }
            })
            .collect();
        if !ext.is_empty() {
            widgets::kv_text(ui, "External", &ext.join(", "), pal);
        }
        widgets::kv_text(ui, "External name", s(v, "/spec/externalName"), pal);
        if let Some(sel) = pod_selector_of(v) {
            widgets::kv_text(ui, "Selector", &sel, pal);
        }
        let ports = arr(v, "/spec/ports");
        if !ports.is_empty() {
            widgets::section(ui, "Ports", pal);
            for p in ports {
                let Some(port) = int(p, "/port") else { continue };
                ui.horizontal(|ui| {
                    let target = p.get("targetPort").map(scalar_text).unwrap_or_default();
                    let name = s(p, "/name");
                    let mut text = format!("{port}");
                    if !target.is_empty() && target != port.to_string() {
                        text.push_str(&format!(" → {target}"));
                    }
                    text.push_str(&format!("/{}", if s(p, "/protocol").is_empty() { "TCP" } else { s(p, "/protocol") }));
                    ui.label(RichText::new(text).font(theme::mono(12.5)));
                    if !name.is_empty() {
                        ui.label(RichText::new(name).color(pal.muted));
                    }
                    if let Some(np) = int(p, "/nodePort") {
                        ui.label(RichText::new(format!("nodePort {np}")).color(pal.muted).size(12.0));
                    }
                    let btn = egui::Button::new((
                        widgets::icon(icons::ICON_SWAP_HORIZ, 13.0),
                        RichText::new("Forward").size(12.0),
                    ))
                    .small();
                    if ui.add(btn).on_hover_text("Forward this port to localhost").clicked() {
                        actions.push(DetailAction::ForwardPort { port: port as u16 });
                    }
                });
            }
        }
    }

    fn ingress(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        widgets::kv_text(ui, "Class", s(v, "/spec/ingressClassName"), pal);
        let rules = arr(v, "/spec/rules");
        if !rules.is_empty() {
            widgets::section(ui, "Rules", pal);
            for rule in rules {
                let host = s(rule, "/host");
                for path in arr(rule, "/http/paths") {
                    let svc = s(path, "/backend/service/name").to_string();
                    let port = path
                        .pointer("/backend/service/port/number")
                        .or(path.pointer("/backend/service/port/name"))
                        .map(scalar_text)
                        .unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!("{}{}", if host.is_empty() { "*" } else { host }, s(path, "/path")))
                                .font(theme::mono(12.5)),
                        );
                        ui.label(RichText::new("→").color(pal.faint));
                        if ui.link(format!("{svc}:{port}")).clicked() {
                            actions.push(DetailAction::Open { kind: "Service".into(), ns: self.ns.clone(), name: svc.clone() });
                        }
                    });
                }
            }
        }
        let tls = arr(v, "/spec/tls");
        if !tls.is_empty() {
            widgets::section(ui, "TLS", pal);
            for t in tls {
                let hosts: Vec<&str> = arr(t, "/hosts").iter().filter_map(|h| h.as_str()).collect();
                let secret = s(t, "/secretName").to_string();
                ui.horizontal_wrapped(|ui| {
                    ui.label(hosts.join(", "));
                    if !secret.is_empty() && ui.link(&secret).clicked() {
                        actions.push(DetailAction::Open { kind: "Secret".into(), ns: self.ns.clone(), name: secret.clone() });
                    }
                });
            }
        }
    }

    fn node(&mut self, ui: &mut Ui, pal: &Palette, v: &Value) {
        let info = |k: &str| s(v, &format!("/status/nodeInfo/{k}"));
        for a in arr(v, "/status/addresses") {
            widgets::kv_text(ui, s(a, "/type"), s(a, "/address"), pal);
        }
        let ready = arr(v, "/status/conditions")
            .iter()
            .find(|c| s(c, "/type") == "Ready")
            .map(|c| s(c, "/status"));
        let cordoned = v.pointer("/spec/unschedulable").and_then(|x| x.as_bool()).unwrap_or(false);
        widgets::kv(ui, "Status", pal, |ui| {
            ui.horizontal(|ui| {
                match ready {
                    Some("True") => widgets::pill(ui, "Ready", pal.ok, pal),
                    Some("False") => widgets::pill(ui, "NotReady", pal.err, pal),
                    _ => widgets::pill(ui, "Unknown", pal.warn, pal),
                };
                if cordoned {
                    widgets::pill(ui, "Cordoned", pal.warn, pal);
                }
            });
        });
        widgets::kv_text(ui, "Kubelet", info("kubeletVersion"), pal);
        widgets::kv_text(ui, "OS", &format!("{} ({})", info("osImage"), info("architecture")), pal);
        widgets::kv_text(ui, "Kernel", info("kernelVersion"), pal);
        widgets::kv_text(ui, "Runtime", info("containerRuntimeVersion"), pal);
        widgets::section(ui, "Capacity", pal);
        egui::Grid::new("capacity").num_columns(3).spacing([18.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Resource").color(pal.faint).size(12.0));
            ui.label(RichText::new("Allocatable").color(pal.faint).size(12.0));
            ui.label(RichText::new("Capacity").color(pal.faint).size(12.0));
            ui.end_row();
            if let Some(cap) = v.pointer("/status/capacity").and_then(|c| c.as_object()) {
                for (k, val) in cap {
                    ui.label(k);
                    ui.label(s(v, &format!("/status/allocatable/{}", k.replace('~', "~0").replace('/', "~1"))));
                    ui.label(val.as_str().unwrap_or(""));
                    ui.end_row();
                }
            }
        });
        let taints = arr(v, "/spec/taints");
        if !taints.is_empty() {
            widgets::section(ui, "Taints", pal);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for t in taints {
                    let value = s(t, "/value");
                    let text = if value.is_empty() {
                        format!("{}:{}", s(t, "/key"), s(t, "/effect"))
                    } else {
                        format!("{}={}:{}", s(t, "/key"), value, s(t, "/effect"))
                    };
                    widgets::chip(ui, &text, pal);
                }
            });
        }
    }

    fn configmap(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        let Some(data) = v.get("data").and_then(|d| d.as_object()) else {
            ui.label(RichText::new("No data").color(pal.muted));
            return;
        };
        widgets::section(ui, &format!("Data · {}", widgets::plural(data.len(), "key")), pal);
        for (k, val) in data {
            let text = val.as_str().unwrap_or("");
            data_entry(ui, pal, k, text, true, actions);
        }
    }

    fn secret(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        widgets::kv_text(ui, "Type", s(v, "/type"), pal);
        let Some(data) = v.get("data").and_then(|d| d.as_object()) else {
            ui.label(RichText::new("No data").color(pal.muted));
            return;
        };
        widgets::section(ui, &format!("Data · {}", widgets::plural(data.len(), "key")), pal);
        for (k, val) in data {
            let encoded = val.as_str().unwrap_or("");
            let revealed = self.revealed.contains(k);
            ui.horizontal(|ui| {
                ui.label(widgets::semibold(k, 13.0).color(pal.text));
                ui.label(RichText::new(widgets::plural(widgets::base64_len(encoded), "byte")).color(pal.faint).size(12.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_button(ui, icons::ICON_CONTENT_COPY, "Copy decoded value").clicked() {
                        let decoded = decode_secret(encoded);
                        ui.ctx().copy_text(decoded);
                        actions.push(DetailAction::Toast(format!("Copied {k}"), Tone::Normal));
                    }
                    let ic = if revealed { icons::ICON_VISIBILITY_OFF } else { icons::ICON_VISIBILITY };
                    if widgets::icon_button(ui, ic, if revealed { "Hide" } else { "Reveal" }).clicked() {
                        if revealed {
                            self.revealed.remove(k);
                        } else {
                            self.revealed.insert(k.clone());
                        }
                    }
                });
            });
            if revealed {
                value_block(ui, pal, &decode_secret(encoded));
            } else {
                ui.label(RichText::new("••••••••••••").color(pal.faint));
            }
            ui.add_space(4.0);
        }
    }

    fn pvc(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        let phase = s(v, "/status/phase");
        widgets::kv(ui, "Status", pal, |ui| {
            widgets::pill(ui, phase, pal.tone(status_tone(phase)), pal);
        });
        let volume = s(v, "/spec/volumeName").to_string();
        if !volume.is_empty() {
            widgets::kv(ui, "Volume", pal, |ui| {
                if ui.link(&volume).clicked() {
                    actions.push(DetailAction::Open { kind: "PersistentVolume".into(), ns: String::new(), name: volume.clone() });
                }
            });
        }
        widgets::kv_text(ui, "Storage class", s(v, "/spec/storageClassName"), pal);
        widgets::kv_text(ui, "Requested", s(v, "/spec/resources/requests/storage"), pal);
        widgets::kv_text(ui, "Capacity", s(v, "/status/capacity/storage"), pal);
        let modes: Vec<&str> = arr(v, "/spec/accessModes").iter().filter_map(|m| m.as_str()).collect();
        widgets::kv_text(ui, "Access modes", &modes.join(", "), pal);
    }

    fn event(&mut self, ui: &mut Ui, pal: &Palette, v: &Value, actions: &mut Vec<DetailAction>) {
        let ty = s(v, "/type");
        widgets::kv(ui, "Type", pal, |ui| {
            widgets::pill(ui, ty, pal.tone(status_tone(ty)), pal);
        });
        widgets::kv_text(ui, "Reason", s(v, "/reason"), pal);
        widgets::kv_text(ui, "Message", s(v, "/message"), pal);
        let (kind, name) = (s(v, "/involvedObject/kind").to_string(), s(v, "/involvedObject/name").to_string());
        let ns = s(v, "/involvedObject/namespace").to_string();
        if !name.is_empty() {
            widgets::kv(ui, "Object", pal, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&kind).color(pal.muted));
                    if ui.link(&name).clicked() {
                        actions.push(DetailAction::Open { kind: kind.clone(), ns: ns.clone(), name: name.clone() });
                    }
                });
            });
        }
        if let Some(c) = int(v, "/count") {
            widgets::kv_text(ui, "Count", &c.to_string(), pal);
        }
        let source = s(v, "/source/component");
        let source = if source.is_empty() { s(v, "/reportingComponent") } else { source };
        widgets::kv_text(ui, "Source", source, pal);
    }

    fn related_pods(&mut self, ui: &mut Ui, pal: &Palette, actions: &mut Vec<DetailAction>) {
        let Some(pods) = &self.pods else { return };
        widgets::section(ui, &format!("Pods · {}", pods.rows.len()), pal);
        if pods.rows.is_empty() {
            ui.label(RichText::new("No matching pods").color(pal.muted));
            return;
        }
        let status_col = pods.column("Status").map(|i| &pods.columns[i]);
        let ready_col = pods.column("Ready").map(|i| &pods.columns[i]);
        let name_col = pods.columns.iter().find(|c| c.source == ColSource::Name);
        let mut rows: Vec<_> = pods.rows.iter().collect();
        if let Some(c) = name_col {
            rows.sort_by(|a, b| skiff_core::compare(a, b, c));
        }
        let now = now_unix();
        let show_ns = self.key.is("", "nodes");
        for r in rows.iter().take(200) {
            ui.horizontal(|ui| {
                let tone = status_col.and_then(|c| r.cell(c)).map(|c| c.tone).unwrap_or(Tone::Normal);
                widgets::dot(ui, pal.tone(if tone == Tone::Normal { Tone::Muted } else { tone }));
                if ui.link(&*r.name).clicked() {
                    actions.push(DetailAction::Open { kind: "Pod".into(), ns: r.namespace.to_string(), name: r.name.to_string() });
                }
                if show_ns {
                    ui.label(RichText::new(&*r.namespace).color(pal.faint).size(12.0));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(fmt_age(now - r.created)).color(pal.muted).size(12.0));
                    if let Some(c) = status_col {
                        ui.label(RichText::new(r.text(c)).color(pal.tone(tone)).size(12.0));
                    }
                    if let Some(c) = ready_col {
                        ui.label(RichText::new(r.text(c)).color(pal.muted).size(12.0));
                    }
                });
            });
        }
        if rows.len() > 200 {
            ui.label(RichText::new(format!("… and {} more", rows.len() - 200)).color(pal.muted));
        }
    }
}

fn decode_secret(encoded: &str) -> String {
    match base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => format!("<{} bytes of binary data>", e.as_bytes().len()),
        },
        Err(_) => "<invalid base64>".to_string(),
    }
}

fn value_block(ui: &mut Ui, pal: &Palette, text: &str) {
    Frame::new()
        .fill(pal.bg)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let shown: String = if text.len() > 20_000 {
                format!("{}\n… ({} more bytes)", &text[..text.floor_char_boundary(20_000)], text.len() - 20_000)
            } else {
                text.to_string()
            };
            ui.add(egui::Label::new(RichText::new(shown).font(theme::mono(12.0)).color(pal.text)).wrap().selectable(true));
        });
}

fn data_entry(ui: &mut Ui, pal: &Palette, key: &str, text: &str, open: bool, actions: &mut Vec<DetailAction>) {
    let lines = text.lines().count();
    ui.horizontal(|ui| {
        ui.label(widgets::semibold(key, 13.0).color(pal.text));
        if lines > 1 {
            ui.label(RichText::new(format!("{lines} lines")).color(pal.faint).size(12.0));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::icon_button(ui, icons::ICON_CONTENT_COPY, "Copy value").clicked() {
                ui.ctx().copy_text(text.to_string());
                actions.push(DetailAction::Toast(format!("Copied {key}"), Tone::Normal));
            }
        });
    });
    if lines > 12 {
        egui::CollapsingHeader::new(RichText::new("show").color(pal.muted).size(12.0))
            .id_salt(("data", key))
            .default_open(open && lines < 40)
            .show(ui, |ui| value_block(ui, pal, text));
    } else {
        value_block(ui, pal, text);
    }
    ui.add_space(4.0);
}

/// `status.conditions`, which most built-in and custom resources carry.
fn conditions(ui: &mut Ui, pal: &Palette, v: &Value) {
    let conds = arr(v, "/status/conditions");
    if conds.is_empty() {
        return;
    }
    widgets::section(ui, "Conditions", pal);
    for c in conds {
        let ty = s(c, "/type");
        let status = s(c, "/status");
        // "Ready=True" is good, but "MemoryPressure=True" is bad.
        let negative = ty.ends_with("Pressure") || ty.ends_with("Unavailable") || ty == "Degraded" || ty == "Stalled";
        let good = (status == "True") != negative;
        let tone = match status {
            "Unknown" => Tone::Warn,
            _ if good => Tone::Ok,
            _ => Tone::Err,
        };
        ui.horizontal(|ui| {
            widgets::dot(ui, pal.tone(tone));
            ui.label(ty);
            ui.label(RichText::new(status).color(pal.muted).size(12.0));
            let reason = s(c, "/reason");
            if !reason.is_empty() {
                ui.label(RichText::new(reason).color(pal.muted).size(12.0));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let t = age_of(s(c, "/lastTransitionTime"));
                if !t.is_empty() {
                    ui.label(RichText::new(t).color(pal.faint).size(12.0));
                }
            });
        });
        let msg = s(c, "/message");
        if !msg.is_empty() && !good {
            ui.indent(("cond", ty), |ui| {
                ui.add(egui::Label::new(RichText::new(msg).color(pal.muted).size(12.0)).wrap().selectable(true));
            });
        }
    }
}

/// Collapsible rendering of arbitrary JSON (used for CRD spec/status).
fn json_tree(ui: &mut Ui, pal: &Palette, v: &Value, path: &str, depth: usize) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                json_node(ui, pal, k, child, &format!("{path}/{k}"), depth);
            }
        }
        Value::Array(list) => {
            for (i, child) in list.iter().enumerate() {
                let label = child
                    .get("name")
                    .or(child.get("type"))
                    .and_then(|n| n.as_str())
                    .map(|n| format!("{i} · {n}"))
                    .unwrap_or_else(|| i.to_string());
                json_node(ui, pal, &label, child, &format!("{path}/{i}"), depth);
            }
        }
        other => {
            ui.label(scalar_text(other));
        }
    }
}

fn json_node(ui: &mut Ui, pal: &Palette, key: &str, v: &Value, path: &str, depth: usize) {
    let is_container = matches!(v, Value::Object(m) if !m.is_empty()) || matches!(v, Value::Array(a) if !a.is_empty());
    if is_container {
        let count = match v {
            Value::Array(a) => format!("  [{}]", a.len()),
            _ => String::new(),
        };
        egui::CollapsingHeader::new(RichText::new(format!("{key}{count}")).color(pal.muted))
            .id_salt(path)
            .default_open(depth < 1)
            .show(ui, |ui| json_tree(ui, pal, v, path, depth + 1));
    } else {
        ui.horizontal_top(|ui| {
            ui.label(RichText::new(key).color(pal.muted));
            let (text, color) = match v {
                Value::String(s) => (s.clone(), pal.text),
                Value::Number(n) => (n.to_string(), pal.info),
                Value::Bool(b) => (b.to_string(), if *b { pal.ok } else { pal.warn }),
                Value::Null => ("null".to_string(), pal.faint),
                Value::Object(_) => ("{}".to_string(), pal.faint),
                Value::Array(_) => ("[]".to_string(), pal.faint),
            };
            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap().selectable(true));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_decode() {
        assert_eq!(decode_secret("aGVsbG8="), "hello");
        assert_eq!(decode_secret("!!!"), "<invalid base64>");
        assert!(decode_secret("/w==").contains("binary"));
    }
}
