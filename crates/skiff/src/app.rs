//! Application state and the top-level layout.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Panel,
    PopupCloseBehavior, Response, RichText, Sense, Stroke, Ui, Vec2,
};
use egui_material_icons::{icons, MaterialIcon};
use serde::{Deserialize, Serialize};
use skiff_core::kinds;
use skiff_core::{
    load_contexts, Category, ConnStatus, Contexts, Core, Job, LoadState, Metrics, Notifier, PortForward,
    ResourceKey, Row, Session, Tone, BUILTINS,
};

use crate::detail::{Detail, DetailAction};
use crate::dock::Dock;
use crate::overview;
use crate::palette::{self, Command, PaletteState};
use crate::table_view::{self, RowAction, TableUi, ViewCache};
use crate::theme::{self, Palette, TOP_BAR_HEIGHT};
use crate::widgets;

static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Call first thing in `main` so startup tracing measures from process start.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

/// With `SKIFF_TRACE_STARTUP=1`, prints when the first frame and the first
/// fully loaded table happened; `=exit` also quits once loaded.
struct StartupTrace {
    mode: Option<String>,
    first_frame: bool,
    loaded: bool,
}

impl StartupTrace {
    fn from_env() -> Self {
        StartupTrace { mode: std::env::var("SKIFF_TRACE_STARTUP").ok(), first_frame: false, loaded: false }
    }

    fn since_start() -> Duration {
        PROCESS_START.get().map(|t| t.elapsed()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Overview,
    Resource(ResourceKey),
}

#[derive(Clone, Debug, Default)]
pub struct AppOptions {
    /// Context to open; defaults to the last used or the kubeconfig's current.
    pub context: Option<String>,
    pub light: bool,
    /// Initial view, as a resource id such as `pods` or `deployments.apps`.
    pub view: Option<String>,
    /// Start with the namespace filter set to this namespace.
    pub namespace: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    light: bool,
    context: Option<String>,
    namespaces: HashMap<String, Vec<String>>,
    sidebar_closed: bool,
}

pub struct Toast {
    pub text: String,
    pub tone: Tone,
    until: Instant,
}

enum Pending {
    Simple { job: Job<()>, ok: String },
    Forward { job: Job<Arc<PortForward>>, context: String },
    /// A manifest apply whose dialog was closed before it finished. The
    /// manifest is kept so that a failure can hand it back, on the cluster
    /// it was meant for.
    /// `told`: the user has been told that it failed (it may be waiting for
    /// them to come back to that cluster).
    Apply { job: Job<Vec<String>>, text: String, context: String, told: bool },
}

/// Modal dialogs.
pub enum Dialog {
    Delete { key: ResourceKey, ns: String, name: String, force: bool },
    /// `current` fetches the object so the field starts at its real size.
    Scale {
        key: ResourceKey,
        ns: String,
        name: String,
        replicas: String,
        current: Option<Job<skiff_core::ObjectData>>,
        /// Why the current size could not be read, if it could not.
        error: Option<String>,
        /// The field has been given focus (done once the size is known, so
        /// the caret ends up after it).
        focused: bool,
    },
    Forward(ForwardDialog),
    /// Create or update objects from a YAML manifest. `pristine` is the
    /// text the dialog opened with: untouched, it can be dismissed freely.
    /// `context` is the cluster it applies to (the one it was opened on).
    Create { text: String, pristine: String, context: String, job: Option<Job<Vec<String>>>, error: Option<String> },
}

pub struct ForwardDialog {
    pub ns: String,
    pub name: String,
    pub service: bool,
    /// Known ports as (label, port), once loaded.
    pub ports: Option<Result<Vec<(String, u16)>, String>>,
    job: Option<Job<Vec<(String, u16)>>>,
    pub remote: String,
    pub local: String,
}

pub struct SkiffApp {
    core: Core,
    pub pal: Palette,
    pub contexts: Contexts,
    contexts_error: Option<String>,
    sessions: HashMap<String, Arc<Session>>,
    pub active: Option<String>,
    pub view: View,
    pub ns_filter: BTreeSet<String>,
    saved_ns: HashMap<String, Vec<String>>,
    ns_search: String,
    tables: HashMap<String, TableUi>,
    cache: ViewCache,
    pub detail: Option<Detail>,
    pub dock: Dock,
    pub palette: Option<PaletteState>,
    toasts: Vec<Toast>,
    pending: Vec<Pending>,
    pub dialog: Option<Dialog>,
    pub sidebar_open: bool,
    nav_open: HashMap<String, bool>,
    focus_filter: bool,
    terminal_focused: bool,
    /// Time spent building the previous frame's UI.
    pub last_ui: Duration,
    empty_metrics: Arc<Metrics>,
    trace: StartupTrace,
    /// How soon the next repaint is needed if nothing else happens.
    tick: Duration,
    /// Frames the current dialog has been shown for (0 = just opened).
    dialog_frames: u32,
    /// Whether a dialog, the palette or a menu was open when this frame
    /// began.
    overlay_at_start: bool,
    /// `overlay_at_start` of the previous frame.
    overlay_before: bool,
    /// A shell had the keyboard when the current overlay opened.
    shell_had_keyboard: bool,
    /// A mouse button was used during the previous frame.
    pointer_used: bool,
    /// (context, resource id) pairs whose cluster-wide list was refused.
    forbidden: std::collections::HashSet<(String, String)>,
}

pub fn kind_icon(key: &ResourceKey) -> MaterialIcon {
    match (&*key.group, &*key.plural) {
        ("", "nodes") => icons::ICON_DNS,
        ("", "namespaces") => icons::ICON_FOLDER,
        ("", "events") => icons::ICON_NOTIFICATIONS,
        ("", "pods") => icons::ICON_DEPLOYED_CODE,
        ("apps", "deployments") => icons::ICON_LAYERS,
        ("apps", "daemonsets") => icons::ICON_HUB,
        ("apps", "statefulsets") => icons::ICON_STORAGE,
        ("apps", "replicasets") => icons::ICON_CONTENT_COPY,
        ("batch", "jobs") => icons::ICON_BOLT,
        ("batch", "cronjobs") => icons::ICON_SCHEDULE,
        ("", "configmaps") => icons::ICON_DESCRIPTION,
        ("", "secrets") => icons::ICON_KEY,
        ("autoscaling", _) => icons::ICON_SPEED,
        ("policy", _) => icons::ICON_SHIELD,
        ("", "resourcequotas") => icons::ICON_TOLL,
        ("", "limitranges") => icons::ICON_TUNE,
        ("scheduling.k8s.io", _) => icons::ICON_LABEL,
        ("coordination.k8s.io", _) => icons::ICON_LINK,
        ("", "services") => icons::ICON_LAN,
        ("networking.k8s.io", "ingresses") => icons::ICON_PUBLIC,
        ("networking.k8s.io", "ingressclasses") => icons::ICON_ROUTER,
        ("networking.k8s.io", "networkpolicies") => icons::ICON_SECURITY,
        ("discovery.k8s.io", _) => icons::ICON_ACCOUNT_TREE,
        ("", "persistentvolumeclaims") => icons::ICON_INVENTORY_2,
        ("", "persistentvolumes") => icons::ICON_SD_STORAGE,
        ("storage.k8s.io", _) => icons::ICON_STORAGE,
        ("", "serviceaccounts") => icons::ICON_VIEW_AGENDA,
        ("rbac.authorization.k8s.io", "roles" | "clusterroles") => icons::ICON_LOCK,
        ("rbac.authorization.k8s.io", _) => icons::ICON_LINK,
        ("apiextensions.k8s.io", _) => icons::ICON_EXTENSION,
        _ => icons::ICON_DATA_OBJECT,
    }
}

/// Human title for a resource type.
pub fn kind_title(key: &ResourceKey) -> String {
    match kinds::builtin(&key.group, &key.plural) {
        Some(b) if b.category == Category::Custom => "Custom Resource Definitions".to_string(),
        Some(b) => b.title.to_string(),
        None => key.kind.to_string(),
    }
}

fn nav_row(ui: &mut Ui, pal: &Palette, ic: MaterialIcon, label: &str, selected: bool, indent: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let bg = if selected {
            pal.selected
        } else if resp.hovered() {
            pal.hover
        } else {
            Color32::TRANSPARENT
        };
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(6), bg);
        let fg = if selected || resp.hovered() { pal.text } else { pal.muted };
        let icon_color = if selected { pal.accent } else { pal.faint };
        p.text(
            egui::pos2(rect.left() + 10.0 + indent, rect.center().y),
            Align2::LEFT_CENTER,
            ic.codepoint,
            FontId::new(16.0, ic.font_family()),
            icon_color,
        );
        let text_rect = rect.with_min_x(rect.left() + 34.0 + indent).shrink2(Vec2::new(4.0, 0.0));
        p.with_clip_rect(text_rect).text(
            egui::pos2(text_rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.0),
            fg,
        );
    }
    let label = label.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl SkiffApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: AppOptions) -> Self {
        let saved: Saved = cc
            .storage
            .and_then(|s| eframe::get_value(s, "skiff"))
            .unwrap_or_default();
        Self::build(&cc.egui_ctx, opts, saved)
    }

    /// Construct without persisted settings (tests, headless rendering).
    pub fn headless(ctx: &egui::Context, opts: AppOptions) -> Self {
        Self::build(ctx, opts, Saved::default())
    }

    fn build(ctx: &egui::Context, opts: AppOptions, saved: Saved) -> Self {
        theme::install_fonts(ctx);
        let pal = if opts.light || saved.light { Palette::light() } else { Palette::dark() };
        theme::apply(ctx, &pal);
        let repaint = ctx.clone();
        let core = Core::new(Notifier::new(move || repaint.request_repaint())).expect("failed to start I/O runtime");
        let (contexts, contexts_error) = match load_contexts() {
            Ok(c) => (c, None),
            Err(e) => (Contexts::default(), Some(e)),
        };
        let mut app = SkiffApp {
            core,
            pal,
            contexts,
            contexts_error,
            sessions: HashMap::new(),
            active: None,
            view: View::Overview,
            ns_filter: BTreeSet::new(),
            saved_ns: saved.namespaces,
            ns_search: String::new(),
            tables: HashMap::new(),
            cache: ViewCache::default(),
            detail: None,
            dock: Dock::default(),
            palette: None,
            toasts: Vec::new(),
            pending: Vec::new(),
            dialog: None,
            sidebar_open: !saved.sidebar_closed,
            nav_open: HashMap::new(),
            focus_filter: false,
            terminal_focused: false,
            last_ui: Duration::ZERO,
            empty_metrics: Arc::new(Metrics::default()),
            trace: StartupTrace::from_env(),
            tick: Duration::from_secs(30),
            dialog_frames: 0,
            overlay_at_start: false,
            overlay_before: false,
            shell_had_keyboard: false,
            pointer_used: false,
            forbidden: std::collections::HashSet::new(),
        };
        let known = |name: &String| app.contexts.items.iter().any(|c| &c.name == name);
        let initial = opts
            .context
            .or(saved.context.filter(known))
            .or(app.contexts.current.clone().filter(known))
            .or(app.contexts.items.first().map(|c| c.name.clone()));
        if let Some(c) = initial {
            app.switch_context(&c);
        }
        if let Some(ns) = opts.namespace {
            app.ns_filter = BTreeSet::from([ns]);
        }
        if let Some(id) = opts.view {
            if let Some(b) = BUILTINS.iter().find(|b| b.key().id() == id || b.plural == id) {
                app.view = View::Resource(b.key());
            }
        }
        app
    }

    pub fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.remember_namespaces();
        let saved = Saved {
            light: !self.pal.dark,
            context: self.active.clone(),
            namespaces: self.saved_ns.clone(),
            sidebar_closed: !self.sidebar_open,
        };
        eframe::set_value(storage, "skiff", &saved);
    }

    pub fn session(&self) -> Option<&Arc<Session>> {
        self.active.as_ref().and_then(|c| self.sessions.get(c))
    }

    fn remember_namespaces(&mut self) {
        if let Some(ctx) = &self.active {
            self.saved_ns.insert(ctx.clone(), self.ns_filter.iter().cloned().collect());
        }
    }

    /// True (with a toast) when the detail panel holds unsaved YAML edits
    /// that the requested navigation would throw away.
    fn blocked_by_edit(&mut self) -> bool {
        if self.detail.as_ref().is_some_and(|d| d.has_unsaved_edit()) {
            self.toast("Apply or cancel your YAML changes first", Tone::Warn);
            return true;
        }
        false
    }

    pub fn switch_context(&mut self, name: &str) {
        if self.active.as_deref() == Some(name) || self.blocked_by_edit() {
            return;
        }
        // A dialog belongs to the cluster it was opened for.
        self.dialog = None;
        self.remember_namespaces();
        if !self.sessions.contains_key(name) {
            self.sessions.insert(name.to_string(), self.core.connect(name));
        }
        self.active = Some(name.to_string());
        self.ns_filter = self.saved_ns.get(name).map(|v| v.iter().cloned().collect()).unwrap_or_default();
        self.detail = None;
        self.cache = ViewCache::default();
    }

    pub fn disconnect(&mut self, name: &str) {
        if self.active.as_deref() == Some(name) && self.blocked_by_edit() {
            return;
        }
        if let Some(s) = self.sessions.remove(name) {
            s.shutdown();
        }
        self.dock.close_for_context(name);
        if self.active.as_deref() == Some(name) {
            self.remember_namespaces();
            self.active = None;
            self.detail = None;
            self.dialog = None;
        }
    }

    /// (rows shown in the current table, whether it is fully loaded).
    pub fn table_stats(&self) -> (usize, bool) {
        let live = matches!(self.cache.snap.state, LoadState::Live | LoadState::Polling);
        (self.cache.order.len(), live)
    }

    /// Name and namespace of the row at a display position (for tests and
    /// diagnostics).
    pub fn row_at(&self, index: usize) -> Option<(String, String)> {
        self.cache.row(index).map(|r| (r.name.to_string(), r.namespace.to_string()))
    }

    /// Name and namespace of the highlighted row of the current table.
    pub fn selected_row(&self) -> Option<(String, String)> {
        let View::Resource(key) = &self.view else { return None };
        let uid = self.tables.get(&key.id())?.selected.as_deref()?;
        self.row_at(self.cache.position(uid)?)
    }

    /// Time the last filter + sort pass took.
    pub fn last_table_cost(&self) -> Duration {
        self.cache.last_cost()
    }

    /// Identifier of the current view (`overview` or a resource id).
    pub fn view_id(&self) -> String {
        match &self.view {
            View::Overview => "overview".to_string(),
            View::Resource(k) => k.id(),
        }
    }

    pub fn go(&mut self, view: View) {
        if self.view != view && !self.blocked_by_edit() {
            self.detail = None;
            self.view = view;
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, tone: Tone) {
        let secs = if tone == Tone::Err { 8 } else { 4 };
        let (text, until) = (text.into(), Instant::now() + Duration::from_secs(secs));
        // Saying the same thing again just keeps it on screen longer.
        match self.toasts.iter_mut().find(|t| t.text == text) {
            Some(existing) => existing.until = until,
            None => self.toasts.push(Toast { text, tone, until }),
        }
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, dark: bool) {
        self.pal = if dark { Palette::dark() } else { Palette::light() };
        theme::apply(ctx, &self.pal);
    }

    fn poll(&mut self) {
        let mut toasts = Vec::new();
        let sessions = &self.sessions;
        // A background apply that failed gets its dialog back, manifest and
        // error included: on the cluster it was meant for (never on another
        // one, where "Apply" would send it to the wrong place), and once no
        // dialog, palette or menu is in the way.
        let dialog_free = self.dialog.is_none() && self.palette.is_none() && !self.overlay_at_start;
        let active = self.active.clone();
        let mut reopen: Option<(String, String, String)> = None;
        self.pending.retain_mut(|p| match p {
            Pending::Simple { job, ok } => match job.take() {
                None => true,
                Some(Ok(())) => {
                    toasts.push((ok.clone(), Tone::Ok));
                    false
                }
                Some(Err(e)) => {
                    toasts.push((e, Tone::Err));
                    false
                }
            },
            Pending::Apply { job, text, context, told } => {
                let elsewhere = active.as_deref() != Some(context.as_str());
                let failed = job.peek(|r| matches!(r, Some(Err(_))));
                if failed && !sessions.contains_key(context) {
                    // That cluster was disconnected: there is no place for
                    // the dialog to come back to. Say what happened.
                    if let Some(Err(e)) = job.take() {
                        let lines: Vec<String> = e.lines().map(str::to_string).collect();
                        toasts.push((format!("Applying to {context} failed:\n{}", apply_summary(&lines)), Tone::Err));
                    }
                    return false;
                }
                if failed && (elsewhere || !dialog_free || reopen.is_some()) {
                    // Wait until the user is back on that cluster and free.
                    if elsewhere && !*told {
                        *told = true;
                        let msg = format!("Applying the manifest to {context} failed. Switch back to {context} for the report.");
                        toasts.push((msg, Tone::Err));
                    }
                    return true;
                }
                match job.take() {
                    None => true,
                    Some(Ok(lines)) => {
                        let summary = apply_summary(&lines);
                        toasts.push((if elsewhere { format!("{context}:\n{summary}") } else { summary }, Tone::Ok));
                        false
                    }
                    Some(Err(e)) => {
                        if !*told {
                            toasts.push(("Applying the manifest failed".to_string(), Tone::Err));
                        }
                        reopen = Some((std::mem::take(text), context.clone(), e));
                        false
                    }
                }
            }
            Pending::Forward { job, context } => match job.take() {
                None => true,
                Some(Ok(fwd)) => {
                    toasts.push((
                        format!("Forwarding localhost:{} → {}:{}", fwd.local_port, fwd.label, fwd.remote_port),
                        Tone::Ok,
                    ));
                    if let Some(s) = sessions.get(context) {
                        s.track_forward(fwd);
                    }
                    false
                }
                Some(Err(e)) => {
                    toasts.push((format!("Port forward failed: {e}"), Tone::Err));
                    false
                }
            },
        });
        for (t, tone) in toasts {
            self.toast(t, tone);
        }
        if let Some((text, context, error)) = reopen {
            self.dialog = Some(Dialog::Create { text, pristine: String::new(), context, job: None, error: Some(error) });
        }
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);

        let mut applied: Option<Vec<String>> = None;
        if let Some(Dialog::Create { job, error, .. }) = &mut self.dialog {
            if let Some(res) = job.as_ref().and_then(|j| j.take()) {
                *job = None;
                match res {
                    Ok(lines) => applied = Some(lines),
                    Err(e) => *error = Some(e),
                }
            }
        }
        if let Some(lines) = applied {
            self.dialog = None;
            self.toast(apply_summary(&lines), Tone::Ok);
        }
        if let Some(Dialog::Scale { replicas, current, error, .. }) = &mut self.dialog {
            if let Some(res) = current.as_ref().and_then(|j| j.take()) {
                *current = None;
                match res {
                    Ok(obj) => {
                        if replicas.is_empty() {
                            if let Some(n) = obj.value.pointer("/spec/replicas").and_then(|v| v.as_i64()) {
                                *replicas = n.to_string();
                            }
                        }
                    }
                    Err(e) => *error = Some(format!("Couldn't read the current size: {e}")),
                }
            }
        }
        if let Some(Dialog::Forward(d)) = &mut self.dialog {
            if let Some(job) = &d.job {
                if let Some(res) = job.take() {
                    if let Ok(ports) = &res {
                        if d.remote.is_empty() {
                            if let Some((_, p)) = ports.first() {
                                d.remote = p.to_string();
                            }
                        }
                    }
                    d.ports = Some(res);
                    d.job = None;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    fn open_detail(&mut self, key: &ResourceKey, row: &Row) {
        let Some(session) = self.session().cloned() else { return };
        if let Some(d) = &self.detail {
            if d.is_for(key, &row.namespace, &row.name) {
                return;
            }
        }
        if self.blocked_by_edit() {
            return;
        }
        let tab = self.detail.as_ref().map(|d| d.tab);
        let mut detail = Detail::open(&session, key, &row.namespace, &row.name);
        if let Some(tab) = tab {
            detail.tab = tab;
        }
        self.detail = Some(detail);
    }

    /// Navigates to an object by kind and name (owner links, palette).
    pub fn open_object(&mut self, key: &ResourceKey, ns: &str, name: &str) {
        let Some(session) = self.session().cloned() else { return };
        // Jumping to the object that is already open keeps its panel (and
        // any edit in it) as it is.
        let same = self.detail.as_ref().is_some_and(|d| d.is_for(key, ns, name));
        if !same && self.blocked_by_edit() {
            return;
        }
        self.view = View::Resource(key.clone());
        let tui = self.tables.entry(key.id()).or_default();
        tui.filter.clear();
        tui.selected = session
            .table(key)
            .snapshot()
            .find(ns, name)
            .map(|r| r.uid.clone());
        tui.scroll_to_selected = true;
        if key.namespaced && !self.ns_filter.is_empty() && !self.ns_filter.contains(ns) {
            self.ns_filter.insert(ns.to_string());
        }
        if !same {
            self.detail = Some(Detail::open(&session, key, ns, name));
        }
    }

    fn open_by_kind(&mut self, kind: &str, ns: &str, name: &str) {
        let key = kinds::builtin_by_kind(kind).map(|b| b.key()).or_else(|| {
            self.session()
                .and_then(|s| s.discovered().iter().find(|d| &*d.key.kind == kind).map(|d| d.key.clone()))
        });
        match key {
            Some(key) => {
                let ns = if key.namespaced { ns } else { "" };
                self.open_object(&key, ns, name);
            }
            None => self.toast(format!("Don't know how to open {kind}"), Tone::Warn),
        }
    }

    pub fn run_row_action(&mut self, ctx: &egui::Context, action: RowAction, key: &ResourceKey, ns: &str, name: &str) {
        let Some(session) = self.session().cloned() else { return };
        match action {
            RowAction::Logs => {
                let job = session.log_targets(key, ns, name);
                self.dock.open_logs(&session, name.to_string(), job);
            }
            RowAction::Shell => {
                let job = session.pod_containers(ns, name);
                self.dock.open_shell(&session, ns, name, job);
            }
            RowAction::Edit => {
                self.open_object_if_needed(key, ns, name);
                if let Some(d) = &mut self.detail {
                    d.start_edit();
                }
            }
            RowAction::Delete => {
                self.dialog = Some(Dialog::Delete {
                    key: key.clone(),
                    ns: ns.to_string(),
                    name: name.to_string(),
                    force: false,
                });
            }
            RowAction::CopyName => {
                ctx.copy_text(name.to_string());
                self.toast(format!("Copied {name}"), Tone::Normal);
            }
            RowAction::Restart => {
                self.pending.push(Pending::Simple {
                    job: session.restart(key, ns, name),
                    ok: format!("Restarting {name}"),
                });
            }
            RowAction::Scale => {
                // Start from the object's actual spec.replicas rather than
                // guessing from a table column; empty until it arrives.
                self.dialog = Some(Dialog::Scale {
                    key: key.clone(),
                    ns: ns.to_string(),
                    name: name.to_string(),
                    replicas: String::new(),
                    current: Some(session.get_object(key, ns, name)),
                    error: None,
                    focused: false,
                });
            }
            RowAction::Forward => {
                let service = key.is("", "services");
                let job = Some(session.forwardable_ports(service, ns, name));
                self.dialog = Some(Dialog::Forward(ForwardDialog {
                    ns: ns.to_string(),
                    name: name.to_string(),
                    service,
                    ports: None,
                    job,
                    remote: String::new(),
                    local: String::new(),
                }));
            }
            RowAction::Cordon | RowAction::Uncordon => {
                let on = action == RowAction::Cordon;
                self.pending.push(Pending::Simple {
                    job: session.cordon(name, on),
                    ok: format!("{} {name}", if on { "Cordoned" } else { "Uncordoned" }),
                });
            }
            RowAction::Suspend | RowAction::Resume => {
                let on = action == RowAction::Suspend;
                self.pending.push(Pending::Simple {
                    job: session.suspend_cronjob(key, ns, name, on),
                    ok: format!("{} {name}", if on { "Suspended" } else { "Resumed" }),
                });
            }
        }
    }

    fn open_object_if_needed(&mut self, key: &ResourceKey, ns: &str, name: &str) {
        let already = self.detail.as_ref().is_some_and(|d| d.is_for(key, ns, name));
        if !already && !self.blocked_by_edit() {
            if let Some(session) = self.session().cloned() {
                self.detail = Some(Detail::open(&session, key, ns, name));
            }
        }
    }

    pub fn open_create_dialog(&mut self) {
        let ns = self.ns_filter.iter().next().cloned().unwrap_or_else(|| "default".to_string());
        let template = format!(
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: example\n  namespace: {ns}\ndata:\n  key: value\n"
        );
        let Some(context) = self.active.clone() else { return };
        self.dialog = Some(Dialog::Create { pristine: template.clone(), text: template, context, job: None, error: None });
    }

    pub fn run_command(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::Go(view) => self.go(view),
            Command::Context(name) => self.switch_context(&name),
            Command::Namespace(ns) => {
                self.ns_filter.clear();
                if let Some(ns) = ns {
                    self.ns_filter.insert(ns);
                }
            }
            Command::Object { key, ns, name } => self.open_object(&key, &ns, &name),
            Command::ToggleTheme => self.set_theme(ctx, !self.pal.dark),
            Command::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Command::PortForwards => self.dock.show_forwards(),
            Command::CreateResource => self.open_create_dialog(),
            Command::Reconnect => {
                if let Some(s) = self.session() {
                    s.reconnect();
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Layout
    // ------------------------------------------------------------------

    pub fn ui(&mut self, ui: &mut Ui) {
        let started = Instant::now();
        let ctx = ui.ctx().clone();
        self.tick = Duration::from_secs(30);
        // Sampled before anything is drawn: a key that closes a menu this
        // frame was meant for that menu, not for the table underneath.
        self.overlay_at_start = self.overlay_open(&ctx);
        // The palette, a dialog or a menu takes the keyboard while it is
        // open. If the keyboard was in a shell before, it goes back there
        // afterwards (unless the overlay was left by clicking somewhere,
        // which says where the user wants to be).
        if self.overlay_at_start && !self.overlay_before {
            self.shell_had_keyboard = self.terminal_focused;
        } else if !self.overlay_at_start && self.overlay_before {
            if self.shell_had_keyboard && !self.pointer_used {
                self.dock.focus_shell();
            }
            self.shell_had_keyboard = false;
        }
        self.overlay_before = self.overlay_at_start;
        self.poll();
        self.shortcuts(&ctx);

        self.top_bar(ui);
        self.status_bar(ui);
        self.sidebar(ui);

        // Bottom dock (logs, shells, port forwards).
        let sessions = self.sessions.clone();
        let dock_out = self.dock.ui(ui, &self.pal, &sessions);
        self.terminal_focused = dock_out.terminal_focused;
        for (text, tone) in dock_out.toasts {
            self.toast(text, tone);
        }

        self.detail_panel(ui);

        let pal = self.pal;
        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin { left: 14, right: 8, top: 10, bottom: 0 }))
            .show(ui, |ui| match self.view.clone() {
                View::Overview => self.overview(ui),
                View::Resource(key) => self.resource_view(ui, &key),
            });

        self.palette_ui(&ctx);
        self.dialog_ui(&ctx);
        self.toasts_ui(&ctx);

        // Background changes wake us through the notifier. Otherwise repaint
        // only as often as the relative times on screen can change, so an
        // idle window costs nothing.
        ctx.request_repaint_after(self.tick);
        self.pointer_used = ctx.input(|i| i.pointer.any_down() || i.pointer.any_pressed() || i.pointer.any_released());
        self.last_ui = started.elapsed();
        self.trace_startup(&ctx);
    }

    fn trace_startup(&mut self, ctx: &egui::Context) {
        if self.trace.mode.is_none() || self.trace.loaded {
            return;
        }
        if !self.trace.first_frame {
            self.trace.first_frame = true;
            eprintln!("startup: first frame built at {:?} ({:?} of UI work)", StartupTrace::since_start(), self.last_ui);
        }
        let (rows, live) = self.table_stats();
        if live && rows > 0 {
            self.trace.loaded = true;
            eprintln!("startup: {} rows live at {:?}", rows, StartupTrace::since_start());
            if self.trace.mode.as_deref() == Some("exit") {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // In a shell, Ctrl+K belongs to the shell ("delete to the end of the
        // line"); the palette is on Ctrl+Shift+K there. (On macOS it is
        // Cmd+K everywhere: the shell never sees Cmd.)
        let in_shell = self.terminal_focused && ctx.os() != egui::os::OperatingSystem::Mac;
        let open_palette = if in_shell {
            KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::K)
        } else {
            KeyboardShortcut::new(Modifiers::COMMAND, Key::K)
        };
        if ctx.input_mut(|i| i.consume_shortcut(&open_palette)) && self.dialog.is_none() {
            self.palette = match self.palette {
                Some(_) => None,
                None => Some(PaletteState::default()),
            };
        }
        // Keys belong to whatever is on top: a text field, the terminal, a
        // dialog, the palette, or an open menu / popup.
        let busy = self.overlay_open(ctx) || self.terminal_focused || ctx.text_edit_focused();
        if busy {
            return;
        }
        // Only where a filter box is actually drawn.
        if ctx.input(|i| i.key_pressed(Key::Slash)) && matches!(self.view, View::Resource(_)) && self.session().is_some() {
            self.focus_filter = true;
            // Don't let the "/" itself land in the filter box.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == "/")));
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if let Some(d) = self.detail.as_mut().filter(|d| d.editing()) {
                // Esc backs out of an untouched edit; typed changes are
                // only discarded with the Cancel button.
                if !d.has_unsaved_edit() {
                    d.cancel_edit();
                }
            } else if self.detail.is_some() {
                self.detail = None;
            } else if let View::Resource(key) = &self.view {
                if let Some(t) = self.tables.get_mut(&key.id()) {
                    t.filter.clear();
                }
            }
        }
    }

    /// Whether a dialog, the palette or any menu / popup is open.
    fn overlay_open(&self, ctx: &egui::Context) -> bool {
        self.palette.is_some() || self.dialog.is_some() || egui::Popup::is_any_open(ctx)
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        Panel::top("top-bar")
            .exact_size(TOP_BAR_HEIGHT)
            .resizable(false)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if widgets::icon_button(ui, icons::ICON_MENU, "Toggle sidebar").clicked() {
                        self.sidebar_open = !self.sidebar_open;
                    }
                    self.context_switcher(ui);
                    self.namespace_picker(ui);
                    ui.add_space(8.0);

                    let hint = if cfg!(target_os = "macos") { "Cmd K" } else { "Ctrl K" };
                    let search = egui::Button::new((
                        widgets::icon(icons::ICON_SEARCH, 15.0).color(pal.muted),
                        RichText::new("Search or jump to…").color(pal.muted),
                    ))
                    .right_text(RichText::new(hint).color(pal.faint).size(11.5))
                    .min_size(Vec2::new(300.0, 28.0));
                    if ui.add(search).clicked() {
                        self.palette = Some(PaletteState::default());
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let theme_icon = if pal.dark { icons::ICON_LIGHT_MODE } else { icons::ICON_DARK_MODE };
                        if widgets::icon_button(ui, theme_icon, "Toggle light / dark theme").clicked() {
                            let dark = !self.pal.dark;
                            self.set_theme(ui.ctx(), dark);
                        }
                        if widgets::icon_button(ui, icons::ICON_ADD, "Create resource from YAML").clicked() {
                            self.open_create_dialog();
                        }
                        self.connection_badge(ui);
                    });
                });
            });
    }

    fn connection_badge(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let Some(session) = self.session().cloned() else { return };
        match &*session.status() {
            ConnStatus::Connecting => {
                ui.label(RichText::new("Connecting…").color(pal.muted));
                ui.spinner();
            }
            ConnStatus::Connected { version } => {
                ui.label(RichText::new(version).color(pal.muted).size(12.0))
                    .on_hover_text("Kubernetes API server version");
                widgets::dot(ui, pal.ok);
            }
            ConnStatus::Failed(err) => {
                let resp = ui
                    .add(egui::Button::new((
                        widgets::icon(icons::ICON_REFRESH, 15.0).color(pal.err),
                        RichText::new("Reconnect").color(pal.err),
                    )))
                    .on_hover_text(err);
                if resp.clicked() {
                    session.reconnect();
                }
            }
        }
    }

    fn context_switcher(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let name = self.active.clone().unwrap_or_else(|| "No cluster".to_string());
        let resp = ui
            .add(egui::Button::new((
                widgets::icon(icons::ICON_HUB, 15.0).color(pal.accent),
                widgets::semibold(name, 13.0),
                widgets::icon(icons::ICON_EXPAND_MORE, 15.0).color(pal.muted),
            )))
            .on_hover_text("Switch cluster");
        let mut switch_to = None;
        let mut disconnect = None;
        egui::Popup::menu(&resp)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(280.0);
                widgets::caption(ui, "Clusters", &pal);
                if let Some(err) = &self.contexts_error {
                    ui.label(RichText::new(err).color(pal.err));
                }
                if self.contexts.items.is_empty() && self.contexts_error.is_none() {
                    ui.label(RichText::new("No contexts in kubeconfig").color(pal.muted));
                }
                egui::ScrollArea::vertical().max_height(360.0).min_scrolled_height(360.0).show(ui, |ui| {
                    for c in &self.contexts.items {
                        let is_active = self.active.as_deref() == Some(c.name.as_str());
                        let connected = self.sessions.get(&c.name).map(|s| s.status());
                        ui.horizontal(|ui| {
                            let dot_color = match connected.as_deref() {
                                Some(ConnStatus::Connected { .. }) => pal.ok,
                                Some(ConnStatus::Connecting) => pal.warn,
                                Some(ConnStatus::Failed(_)) => pal.err,
                                None => pal.faint,
                            };
                            widgets::dot(ui, dot_color);
                            let w = ui.available_width() - if connected.is_some() { 30.0 } else { 0.0 };
                            let r = ui
                                .add_sized([w, 26.0], egui::Button::selectable(is_active, &c.name))
                                .on_hover_text(&c.server);
                            if r.clicked() {
                                switch_to = Some(c.name.clone());
                                ui.close();
                            }
                            if connected.is_some()
                                && widgets::icon_button(ui, icons::ICON_POWER_SETTINGS_NEW, "Disconnect").clicked()
                            {
                                disconnect = Some(c.name.clone());
                            }
                        });
                    }
                });
            });
        if let Some(c) = switch_to {
            self.switch_context(&c);
        }
        if let Some(c) = disconnect {
            self.disconnect(&c);
        }
    }

    fn namespace_picker(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let Some(session) = self.session().cloned() else { return };
        let summary = match self.ns_filter.len() {
            0 => "All namespaces".to_string(),
            1 => self.ns_filter.iter().next().cloned().unwrap_or_default(),
            n => format!("{n} namespaces"),
        };
        let resp = ui
            .add(egui::Button::new((
                widgets::icon(icons::ICON_FOLDER, 15.0).color(if self.ns_filter.is_empty() { pal.muted } else { pal.accent }),
                RichText::new(summary),
                widgets::icon(icons::ICON_EXPAND_MORE, 15.0).color(pal.muted),
            )))
            .on_hover_text("Filter by namespace");
        let opened = resp.clicked();
        egui::Popup::menu(&resp)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(260.0);
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.ns_search)
                        .hint_text("Find namespace…")
                        .desired_width(f32::INFINITY),
                );
                if opened {
                    edit.request_focus();
                }
                if ui
                    .add_sized(
                        [ui.available_width(), 26.0],
                        egui::Button::selectable(self.ns_filter.is_empty(), "All namespaces"),
                    )
                    .clicked()
                {
                    self.ns_filter.clear();
                    ui.close();
                }
                ui.separator();
                let snap = session.table(&kinds::namespaces()).snapshot();
                let mut names: Vec<&str> = snap.rows.iter().map(|r| &*r.name).collect();
                names.sort_unstable();
                let needle = self.ns_search.to_lowercase();
                egui::ScrollArea::vertical().max_height(340.0).min_scrolled_height(340.0).show(ui, |ui| {
                    for name in names.into_iter().filter(|n| needle.is_empty() || n.contains(&needle)) {
                        let mut on = self.ns_filter.contains(name);
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                self.ns_filter.insert(name.to_string());
                            } else {
                                self.ns_filter.remove(name);
                            }
                        }
                    }
                    if snap.rows.is_empty() {
                        match snap.state {
                            LoadState::Failed => {
                                // Not allowed to list namespaces: let the
                                // user name the one they work in.
                                ui.add(
                                    egui::Label::new(
                                        RichText::new("Namespaces can't be listed with this account. Type a namespace name above to use it.")
                                            .color(pal.muted)
                                            .size(12.0),
                                    )
                                    .wrap(),
                                );
                                let typed = self.ns_search.trim().to_string();
                                if !typed.is_empty() && ui.button(format!("Use namespace \"{typed}\"")).clicked() {
                                    self.ns_filter = BTreeSet::from([typed]);
                                    self.ns_search.clear();
                                    ui.close();
                                }
                                for name in self.ns_filter.clone() {
                                    let mut on = true;
                                    if ui.checkbox(&mut on, &name).changed() {
                                        self.ns_filter.remove(&name);
                                    }
                                }
                            }
                            _ => {
                                ui.label(RichText::new("Loading…").color(pal.muted));
                            }
                        }
                    }
                });
            });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let mut go: Option<View> = None;
        let discovered = self.session().map(|s| s.discovered());
        let mut open = self.sidebar_open;
        Panel::left("sidebar")
            .resizable(true)
            .default_size(214.0)
            .size_range(168.0..=340.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin { left: 8, right: 4, top: 8, bottom: 0 }))
            .show_collapsible(ui, &mut open, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if nav_row(ui, &pal, icons::ICON_DASHBOARD, "Overview", self.view == View::Overview, 0.0).clicked() {
                        go = Some(View::Overview);
                    }
                    for cat in Category::ALL {
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.add_space(10.0);
                            widgets::caption(ui, cat.title(), &pal);
                        });
                        ui.add_space(3.0);
                        for b in BUILTINS.iter().filter(|b| b.category == cat) {
                            let key = b.key();
                            let selected = matches!(&self.view, View::Resource(k) if k == &key);
                            let title = if cat == Category::Custom { "Definitions" } else { b.title };
                            if nav_row(ui, &pal, kind_icon(&key), title, selected, 0.0).clicked() {
                                go = Some(View::Resource(key));
                            }
                        }
                        if cat == Category::Custom {
                            if let Some(found) = &discovered {
                                self.custom_groups(ui, found, &mut go);
                            }
                        }
                    }
                    ui.add_space(12.0);
                });
            });
        self.sidebar_open = open;
        if let Some(v) = go {
            self.go(v);
        }
    }

    /// Everything discovery found that has no fixed place, grouped by API group.
    fn custom_groups(&mut self, ui: &mut Ui, found: &[skiff_core::Discovered], go: &mut Option<View>) {
        let pal = self.pal;
        let mut groups: Vec<(&str, Vec<&ResourceKey>)> = Vec::new();
        for d in found {
            if kinds::builtin(&d.key.group, &d.key.plural).is_some() {
                continue;
            }
            let g: &str = if d.key.group.is_empty() { "core" } else { &d.key.group };
            match groups.last_mut() {
                Some((name, list)) if *name == g => list.push(&d.key),
                _ => groups.push((g, vec![&d.key])),
            }
        }
        for (group, keys) in groups {
            let has_current = matches!(&self.view, View::Resource(k) if keys.contains(&k));
            let open = *self.nav_open.get(group).unwrap_or(&false) || has_current;
            let chevron = if open { icons::ICON_EXPAND_MORE } else { icons::ICON_CHEVRON_RIGHT };
            if nav_row(ui, &pal, chevron, group, false, 0.0).clicked() {
                self.nav_open.insert(group.to_string(), !open);
            }
            if open {
                for key in keys {
                    let selected = matches!(&self.view, View::Resource(k) if k == key);
                    if nav_row(ui, &pal, kind_icon(key), &key.kind, selected, 12.0).clicked() {
                        *go = Some(View::Resource(key.clone()));
                    }
                }
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let session = self.session().cloned();
        Panel::bottom("status-bar")
            .exact_size(24.0)
            .resizable(false)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let small = |t: String| RichText::new(t).size(11.5).color(pal.muted);
                    if let Some(s) = &session {
                        let n = s.active_watches();
                        ui.label(small(format!("{n} live watch{}", if n == 1 { "" } else { "es" })))
                            .on_hover_text("Only resource types you have opened are watched; idle ones stop after 3 minutes.");
                        let forwards = s.forwards().len();
                        if forwards > 0 {
                            let text = widgets::plural(forwards, "port forward");
                            let ic = widgets::icon(icons::ICON_SWAP_HORIZ, 13.0).color(pal.muted);
                            if ui.add(egui::Button::new((ic, small(text))).frame(false)).clicked() {
                                self.dock.show_forwards();
                            }
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(small(format!("frame {:.1} ms", self.last_ui.as_secs_f32() * 1000.0)))
                            .on_hover_text("CPU time spent building the last frame");
                        if !self.dock.is_empty() {
                            let ic = widgets::icon(icons::ICON_TERMINAL, 13.0).color(pal.muted);
                            if ui.add(egui::Button::new((ic, small("Panel".to_string()))).frame(false)).clicked() {
                                self.dock.open = !self.dock.open;
                            }
                        }
                    });
                });
            });
    }

    fn overview(&mut self, ui: &mut Ui) {
        let Some(session) = self.session().cloned() else {
            self.no_cluster(ui);
            return;
        };
        self.tick = self.tick.min(Duration::from_secs(5));
        if let Some(action) = overview::show(ui, &self.pal, &session, &self.ns_filter) {
            match action {
                overview::Action::Go(key) => self.go(View::Resource(key)),
                overview::Action::Open { key, ns, name } => self.open_object(&key, &ns, &name),
            }
        }
    }

    fn no_cluster(&mut self, ui: &mut Ui) {
        let detail = self
            .contexts_error
            .clone()
            .unwrap_or_else(|| "Pick a cluster from the switcher in the top bar.".to_string());
        widgets::empty_state(ui, icons::ICON_HUB, "No cluster selected", &detail, &self.pal);
    }

    fn resource_view(&mut self, ui: &mut Ui, key: &ResourceKey) {
        let pal = self.pal;
        let Some(session) = self.session().cloned() else {
            self.no_cluster(ui);
            return;
        };
        let ctx = ui.ctx().clone();
        let mut store = session.table(key);
        let mut latest = store.snapshot();
        // Accounts confined to a namespace may not list cluster-wide. With a
        // single namespace selected, watch just that namespace instead.
        // Remember the refusal: the cluster-wide table keeps retrying in the
        // background and briefly reports other states while it does.
        let mark = (session.context.clone(), key.id());
        if latest.state == LoadState::Failed && latest.error_code == 403 && key.namespaced {
            self.forbidden.insert(mark.clone());
        } else if matches!(latest.state, LoadState::Live | LoadState::Polling) {
            self.forbidden.remove(&mark);
        }
        let forbidden = self.forbidden.contains(&mark);
        if forbidden && self.ns_filter.len() == 1 {
            if let Some(ns) = self.ns_filter.iter().next() {
                store = session.table_in(key, Some(ns));
                latest = store.snapshot();
            }
        }
        let metrics = if key.is("", "pods") || key.is("", "nodes") {
            session.metrics()
        } else {
            self.empty_metrics.clone()
        };
        let tui = self.tables.entry(key.id()).or_default();
        if let Some(wait) = self.cache.update(key, latest, tui, &self.ns_filter, &metrics) {
            ctx.request_repaint_after(wait);
        }
        let snap = self.cache.snap.clone();
        let total = snap.rows.len();
        let shown = self.cache.order.len();

        // Toolbar.
        ui.horizontal(|ui| {
            ui.label(widgets::semibold(kind_title(key), 17.0).color(pal.text));
            if !key.group.is_empty() && kinds::builtin(&key.group, &key.plural).is_none() {
                ui.label(RichText::new(&*key.group).color(pal.faint).size(12.0));
            }
            let count = if shown == total {
                widgets::thousands(total)
            } else {
                format!("{} of {}", widgets::thousands(shown), widgets::thousands(total))
            };
            widgets::pill(ui, &count, pal.muted, &pal);
            match snap.state {
                LoadState::Connecting | LoadState::Loading => {
                    ui.spinner();
                }
                LoadState::Reconnecting => {
                    widgets::pill(ui, "reconnecting — data may be stale", pal.warn, &pal)
                        .on_hover_text(snap.error.as_deref().unwrap_or(""));
                }
                LoadState::Polling => {
                    widgets::pill(ui, "polling", pal.info, &pal)
                        .on_hover_text("This API does not support watch; refreshed every 10 seconds.");
                }
                LoadState::Failed if total > 0 => {
                    widgets::pill(ui, "update failed — data may be stale", pal.err, &pal)
                        .on_hover_text(snap.error.as_deref().unwrap_or(""));
                }
                LoadState::Live | LoadState::Failed => {}
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if widgets::icon_button(ui, icons::ICON_REFRESH, "Reload from the API server").clicked() {
                    store.refresh();
                }
                let cols_btn = widgets::icon_button(ui, icons::ICON_VIEW_COLUMN, "Choose columns");
                egui::Popup::menu(&cols_btn)
                    .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        ui.set_min_width(180.0);
                        widgets::caption(ui, "Columns", &pal);
                        // The last column standing cannot be switched off.
                        let last_one = table_view::chosen_columns(key, &snap, tui, &metrics).len() <= 1;
                        for col in snap.columns.iter() {
                            let mut on = tui.col_visible.get(&*col.title).copied().unwrap_or(col.visible);
                            let locked = on && last_one;
                            if ui.add_enabled(!locked, egui::Checkbox::new(&mut on, &*col.title)).changed() {
                                tui.col_visible.insert(col.title.to_string(), on);
                            }
                        }
                        if key.is("", "pods") && metrics.available {
                            for title in ["CPU", "Memory"] {
                                let mut on = tui.col_visible.get(title).copied().unwrap_or(true);
                                let locked = on && last_one;
                                if ui.add_enabled(!locked, egui::Checkbox::new(&mut on, title)).changed() {
                                    tui.col_visible.insert(title.to_string(), on);
                                }
                            }
                        }
                    });
                let id = egui::Id::unique("table-filter");
                let filter = widgets::search_box(ui, &mut tui.filter, "Filter  ( / )", 240.0, id);
                if self.focus_filter {
                    self.focus_filter = false;
                    filter.request_focus();
                }
            });
        });
        ui.add_space(6.0);

        if shown == 0 {
            match snap.state {
                LoadState::Failed => {
                    let mut msg = snap.error.as_deref().unwrap_or("request failed").to_string();
                    if snap.error_code == 403 && key.namespaced && self.ns_filter.len() != 1 {
                        msg.push_str("\n\nIf your account is limited to a namespace, select that one namespace in the picker at the top.");
                    }
                    widgets::empty_state(ui, icons::ICON_ERROR, "Couldn't load this view", &msg, &pal);
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        if ui.button("Retry").clicked() {
                            if matches!(&*session.status(), ConnStatus::Failed(_)) {
                                session.reconnect();
                            } else {
                                store.refresh();
                            }
                        }
                    });
                }
                LoadState::Connecting | LoadState::Loading => {
                    widgets::empty_state(ui, icons::ICON_SYNC, "Loading…", "", &pal);
                }
                _ => {
                    let detail = if total > 0 {
                        "Nothing matches the current filter or namespace selection."
                    } else {
                        "There are no objects of this type."
                    };
                    widgets::empty_state(ui, icons::ICON_SEARCH, "Nothing here", detail, &pal);
                }
            }
            return;
        }

        // Keep the highlighted row in step with the open detail panel (it
        // may have been opened before this table finished loading).
        if tui.selected.is_none() {
            if let Some(d) = self.detail.as_ref().filter(|d| &d.key == key) {
                if let Some(row) = snap.find(&d.ns, &d.name) {
                    tui.selected = Some(row.uid.clone());
                    tui.scroll_to_selected = true;
                }
            }
        }

        // Keyboard navigation.
        let overlay = self.overlay_at_start
            || self.palette.is_some()
            || self.dialog.is_some()
            || egui::Popup::is_any_open(&ctx);
        // While unsaved YAML edits are open the selection stays put: moving
        // it would be refused anyway, one toast per key press.
        let editing = self.detail.as_ref().is_some_and(|d| d.has_unsaved_edit());
        let typing = ctx.text_edit_focused() || overlay || self.terminal_focused || editing;
        let mut open_selected = false;
        if !typing {
            let step: Option<i64> = ctx.input(|i| {
                if i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::J) {
                    Some(1)
                } else if i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::K) {
                    Some(-1)
                } else if i.key_pressed(Key::PageDown) {
                    Some(20)
                } else if i.key_pressed(Key::PageUp) {
                    Some(-20)
                } else if i.key_pressed(Key::Home) {
                    Some(i64::MIN / 2)
                } else if i.key_pressed(Key::End) {
                    Some(i64::MAX / 2)
                } else {
                    None
                }
            });
            if let Some(step) = step {
                let cur = tui.selected.as_deref().and_then(|uid| self.cache.position(uid));
                // With nothing selected, moving down starts at the top and
                // moving up (or End) starts from the bottom.
                let base = cur.map(|c| c as i64).unwrap_or(if step < 0 { shown as i64 } else { -1 });
                let next = base.saturating_add(step).clamp(0, shown as i64 - 1) as usize;
                if let Some(row) = self.cache.row(next) {
                    tui.selected = Some(row.uid.clone());
                    tui.scroll_to_selected = true;
                    open_selected = self.detail.is_some();
                }
            }
            if ctx.input(|i| i.key_pressed(Key::Enter)) {
                open_selected = true;
            }
        }

        let cols = table_view::display_columns(key, &snap, tui, &metrics, ui.available_width());
        let now = skiff_core::now_unix();
        let out = table_view::show(ui, &pal, key, &self.cache, tui, &cols, &metrics, now);
        self.tick = self.tick.min(table_view::age_tick(out.youngest));

        let mut to_open: Option<Arc<Row>> = None;
        if let Some(row) = out.clicked {
            // With unsaved YAML edits the panel stays on its object (and
            // says why); the highlight must stay with it.
            let refused = editing && !self.detail.as_ref().is_some_and(|d| d.is_for(key, &row.namespace, &row.name));
            if !refused {
                tui.selected = Some(row.uid.clone());
                // Opening the panel narrows the table; keep the row in view.
                tui.scroll_to_selected = self.detail.is_none();
            }
            to_open = Some(row);
        } else if open_selected {
            to_open = tui
                .selected
                .as_deref()
                .and_then(|uid| self.cache.position(uid))
                .and_then(|p| self.cache.row(p).cloned());
        }
        if let Some(row) = to_open {
            self.open_detail(key, &row);
        }
        if let Some((action, row)) = out.menu {
            self.run_row_action(&ctx, action, key, &row.namespace, &row.name);
        }
        if let Some((kind, ns, name)) = out.open_owner {
            self.open_by_kind(&kind, &ns, &name);
        }
    }

    fn detail_panel(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let Some(session) = self.session().cloned() else { return };
        let Some(detail) = &mut self.detail else { return };
        // Refresh when the watched row reports a newer version.
        let current_rv = match &self.view {
            View::Resource(k) if k == &detail.key => self
                .cache
                .snap
                .find(&detail.ns, &detail.name)
                .map(|r| r.rv.to_string()),
            _ => None,
        };
        detail.poll(&session, current_rv.as_deref());
        let listed = matches!(&self.view, View::Resource(k) if k == &detail.key)
            && self.cache.snap.state == LoadState::Live;
        detail.set_gone(listed && current_rv.is_none());
        self.tick = self.tick.min(Duration::from_secs(5));
        // (A shell with the keyboard counts: Ctrl+S typed there is for the
        // remote program, not a request to apply an open YAML edit.)
        let overlay = self.overlay_at_start
            || self.palette.is_some()
            || self.dialog.is_some()
            || self.terminal_focused
            || egui::Popup::is_any_open(ui.ctx());
        let mut actions = Vec::new();
        // Leave the table usable in small windows.
        let max_width = (ui.available_width() - 340.0).clamp(300.0, 1100.0);
        Panel::right("detail")
            .resizable(true)
            .default_size(520.0f32.min(max_width))
            .size_range(300.0f32.min(max_width)..=max_width)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin { left: 14, right: 10, top: 10, bottom: 8 }))
            .show(ui, |ui| {
                actions = detail.ui(ui, &pal, &session, overlay);
            });
        let ctx = ui.ctx().clone();
        for a in actions {
            let Some(d) = &self.detail else { break };
            let (key, ns, name) = (d.key.clone(), d.ns.clone(), d.name.clone());
            match a {
                DetailAction::Close => {
                    if !self.blocked_by_edit() {
                        self.detail = None;
                    }
                }
                DetailAction::Row(action) => self.run_row_action(&ctx, action, &key, &ns, &name),
                DetailAction::Open { kind, ns, name } => self.open_by_kind(&kind, &ns, &name),
                DetailAction::Toast(text, tone) => self.toast(text, tone),
                DetailAction::LogsOf { container } => {
                    let targets = vec![skiff_core::LogTarget { namespace: ns.clone(), pod: name.clone(), container: container.clone() }];
                    self.dock.open_logs(&session, format!("{name} · {container}"), Job::done(Ok(targets)));
                }
                DetailAction::ShellIn { container } => {
                    self.dock.open_shell_in(&session, &ns, &name, &container);
                }
                DetailAction::ForwardPort { port } => {
                    let service = key.is("", "services");
                    let job = if service {
                        session.forward_service(&ns, &name, port, None)
                    } else {
                        session.forward_pod(&ns, &name, port, None)
                    };
                    self.pending.push(Pending::Forward { job, context: session.context.clone() });
                }
            }
        }
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.palette else { return };
        let session = self.active.as_ref().and_then(|c| self.sessions.get(c)).cloned();
        match palette::show(ctx, &self.pal, state, &self.contexts, self.active.as_deref(), session.as_ref()) {
            palette::Outcome::Open => {}
            palette::Outcome::Close => self.palette = None,
            palette::Outcome::Run(cmd) => {
                self.palette = None;
                self.run_command(ctx, cmd);
            }
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let Some(dialog) = &mut self.dialog else {
            self.dialog_frames = 0;
            return;
        };
        let Some(session) = self.active.as_ref().and_then(|c| self.sessions.get(c)).cloned() else {
            self.dialog = None;
            return;
        };
        // A manifest is only ever applied to the cluster it was written for.
        if matches!(dialog, Dialog::Create { context, .. } if *context != session.context) {
            self.dialog = None;
            return;
        }
        // Keys that opened the dialog must not also act inside it, so
        // default focus is given once and Enter is left to the focused
        // widget rather than read globally.
        let fresh = self.dialog_frames == 0;
        self.dialog_frames = self.dialog_frames.saturating_add(1);
        // A dialog holding typed text or a running request is only closed
        // by its own buttons, not by Esc or a click outside.
        let sticky = match dialog {
            Dialog::Create { job, text, pristine, .. } => job.is_some() || text != pristine,
            _ => false,
        };
        let mut close = false;
        let mut push: Option<Pending> = None;
        let resp = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            match dialog {
                Dialog::Delete { key, ns, name, force } => {
                    ui.label(widgets::semibold(format!("Delete {}?", key.kind), 15.0));
                    ui.add_space(4.0);
                    let target = if ns.is_empty() { name.clone() } else { format!("{ns}/{name}") };
                    ui.label(RichText::new(target).font(theme::mono(12.5)).color(pal.text));
                    ui.add_space(6.0);
                    ui.checkbox(force, "Force (grace period 0)");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let del = egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(pal.err);
                        if ui.add(del).clicked() {
                            push = Some(Pending::Simple {
                                job: session.delete_object(key, ns, name, *force),
                                ok: format!("Deleted {name}"),
                            });
                            close = true;
                        }
                        // Cancel is the default: a key held a moment too
                        // long (or repeated) must never confirm a deletion.
                        let cancel = ui.button("Cancel");
                        if fresh {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Scale { key, ns, name, replicas, current, error, focused } => {
                    ui.label(widgets::semibold(format!("Scale {name}"), 15.0));
                    ui.add_space(6.0);
                    let mut submitted = false;
                    ui.horizontal(|ui| {
                        ui.label("Replicas");
                        // The field is usable once the current size is in
                        // it (or could not be read): a caret placed in the
                        // empty field would end up in front of the number.
                        let ready = current.is_none();
                        let field = egui::TextEdit::singleline(replicas).hint_text("count").desired_width(70.0);
                        let edit = ui.add_enabled(ready, field);
                        if ready && !*focused {
                            *focused = true;
                            edit.request_focus();
                        }
                        // Enter confirms, but only a fresh key press: a key
                        // still held from opening the dialog (auto-repeat)
                        // must not send the prefilled number off unseen.
                        let (enter, repeated) = ui.input(|i| {
                            let pressed = |repeat: bool| {
                                i.events.iter().any(|e| {
                                    matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, repeat: r, .. } if *r == repeat)
                                })
                            };
                            (pressed(false), pressed(true))
                        });
                        submitted = edit.lost_focus() && enter;
                        if edit.lost_focus() && repeated && !enter {
                            edit.request_focus();
                        }
                        if current.is_some() {
                            ui.spinner();
                        }
                        // Stepping needs a number to step from.
                        let known: Option<i32> = replicas.trim().parse().ok();
                        if ui.add_enabled(known.is_some(), egui::Button::new("−").small()).clicked() {
                            *replicas = (known.unwrap_or(1) - 1).max(0).to_string();
                        }
                        if ui.add_enabled(known.is_some(), egui::Button::new("+").small()).clicked() {
                            *replicas = (known.unwrap_or(0) + 1).to_string();
                        }
                    });
                    if let Some(err) = error {
                        ui.add(egui::Label::new(RichText::new(&*err).color(pal.warn).size(12.0)).wrap());
                    }
                    ui.add_space(10.0);
                    let parsed: Option<i32> = replicas.trim().parse().ok().filter(|n| *n >= 0);
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(parsed.is_some(), egui::Button::new("Scale")).clicked()
                            || (parsed.is_some() && submitted);
                        if let (true, Some(n)) = (go, parsed) {
                            push = Some(Pending::Simple {
                                job: session.scale(key, ns, name, n),
                                ok: format!("Scaled {name} to {n}"),
                            });
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Create { text, job, error, context, .. } => {
                    // Never wider than the window, whatever is pasted.
                    ui.set_width((ui.ctx().content_rect().width() - 64.0).clamp(280.0, 640.0));
                    ui.horizontal(|ui| {
                        ui.label(widgets::semibold("Create or update from YAML", 15.0));
                        ui.label(RichText::new(format!("on {context}")).color(pal.muted));
                    });
                    ui.label(
                        RichText::new("Multiple documents separated by --- are applied in order. Existing objects are replaced.")
                            .color(pal.muted)
                            .size(12.0),
                    );
                    ui.add_space(6.0);
                    let theme = if pal.dark {
                        egui_extras::syntax_highlighting::CodeTheme::dark(12.5)
                    } else {
                        egui_extras::syntax_highlighting::CodeTheme::light(12.5)
                    };
                    // The dialog must fit the window whatever it holds, or
                        // its buttons end up out of reach.
                    let room = (ui.ctx().content_rect().height() - 260.0).max(120.0);
                    let editor_height = room.min(420.0);
                    // (Long lines scroll sideways instead of widening the
                    // dialog until its buttons leave the window.)
                    let editor = egui::ScrollArea::both().id_salt("create-yaml").max_height(editor_height).auto_shrink([false, true]);
                    editor.show(ui, |ui| {
                        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                            let job = egui_extras::syntax_highlighting::highlight(
                                ui.ctx(),
                                ui.style(),
                                &theme,
                                buf.as_str(),
                                "yaml",
                            );
                            ui.fonts_mut(|f| f.layout_job(job))
                        };
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .code_editor()
                                .hint_text("apiVersion: …")
                                .desired_width(f32::INFINITY)
                                .desired_rows(((editor_height / 17.0) as usize).clamp(4, 18))
                                .layouter(&mut layouter),
                        );
                    });
                    if let Some(err) = error {
                        ui.add_space(4.0);
                        // One line per document: a long manifest must not
                        // push the buttons below the window.
                        egui::ScrollArea::vertical().id_salt("create-error").max_height(110.0).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(&*err).color(pal.err)).wrap().selectable(true));
                        });
                    }
                    ui.add_space(10.0);
                    let busy = job.is_some();
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(!busy, egui::Button::new("Apply")).clicked()
                            || (!busy && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)));
                        if go {
                            *error = None;
                            *job = Some(session.apply_manifest(text.clone()));
                        }
                        // Always a way out, even when the server has stopped
                        // answering: a running request keeps going and
                        // reports its result as a notification.
                        let label = if busy { "Close" } else { "Cancel" };
                        if ui.button(label).clicked() {
                            if let Some(running) = job.take() {
                                push = Some(Pending::Apply {
                                    job: running,
                                    text: std::mem::take(text),
                                    context: context.clone(),
                                    told: false,
                                });
                            }
                            close = true;
                        }
                        if busy {
                            ui.spinner();
                        }
                    });
                }
                Dialog::Forward(d) => {
                    let what = if d.service { "service" } else { "pod" };
                    ui.label(widgets::semibold(format!("Port forward to {what} {}", d.name), 15.0));
                    ui.label(RichText::new(format!("namespace {}", d.ns)).color(pal.muted).size(12.0));
                    ui.add_space(6.0);
                    match &d.ports {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("Looking up ports…").color(pal.muted));
                            });
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e).color(pal.err));
                        }
                        Some(Ok(ports)) if !ports.is_empty() => {
                            ui.horizontal_wrapped(|ui| {
                                for (label, port) in ports {
                                    let text = if label.is_empty() { port.to_string() } else { format!("{label} · {port}") };
                                    if ui.add(egui::Button::selectable(d.remote == port.to_string(), text)).clicked() {
                                        d.remote = port.to_string();
                                    }
                                }
                            });
                        }
                        Some(Ok(_)) => {
                            ui.label(RichText::new("No declared ports; enter one manually.").color(pal.muted));
                        }
                    }
                    ui.add_space(6.0);
                    egui::Grid::new("forward-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Remote port");
                        ui.add(egui::TextEdit::singleline(&mut d.remote).hint_text("port").desired_width(90.0));
                        ui.end_row();
                        ui.label("Local port");
                        ui.add(egui::TextEdit::singleline(&mut d.local).hint_text("auto").desired_width(90.0));
                        ui.end_row();
                    });
                    ui.add_space(10.0);
                    let remote: Option<u16> = d.remote.trim().parse().ok();
                    let local: Option<Option<u16>> = if d.local.trim().is_empty() {
                        Some(None)
                    } else {
                        d.local.trim().parse().ok().map(Some)
                    };
                    ui.horizontal(|ui| {
                        let ok = remote.is_some() && local.is_some();
                        let go = ui.add_enabled(ok, egui::Button::new("Start forwarding")).clicked();
                        if let (true, Some(remote), Some(local)) = (go, remote, local) {
                            let job = if d.service {
                                session.forward_service(&d.ns, &d.name, remote, local)
                            } else {
                                session.forward_pod(&d.ns, &d.name, remote, local)
                            };
                            push = Some(Pending::Forward { job, context: session.context.clone() });
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if let Some(p) = push {
            self.pending.push(p);
        }
        if close || (resp.should_close() && !sticky) {
            self.dialog = None;
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let pal = self.pal;
        // Lay the texts out first: the stack is as wide as its widest
        // notification and each one is centred in it.
        const TEXT_MAX: f32 = 400.0;
        let font = egui::TextStyle::Body.resolve(&ctx.global_style());
        let galleys: Vec<_> = self
            .toasts
            .iter()
            .map(|t| ctx.fonts_mut(|f| f.layout(t.text.clone(), font.clone(), pal.text, TEXT_MAX)))
            .collect();
        // Text + dot (8) + gap (6) + the frame's margins (2 x 12).
        let widest = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 8.0 + 6.0 + 24.0;
        egui::Area::new(egui::Id::unique("toasts"))
            // Bottom centre, above the status bar: the one place with no
            // controls under it (the detail panel's and the tables' buttons
            // are along the top and the right).
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -34.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_width(widest);
                for (t, galley) in self.toasts.iter().zip(galleys) {
                    let color = if t.tone == Tone::Normal { pal.accent } else { pal.tone(t.tone) };
                    let width = galley.size().x + 8.0 + 6.0 + 24.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(((widest - width) / 2.0).max(0.0));
                        Frame::new()
                            .fill(pal.raised)
                            .stroke(Stroke::new(1.0, pal.border))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    widgets::dot(ui, color);
                                    ui.add(egui::Label::new(galley));
                                });
                            });
                    });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

/// What a successful apply did, short enough for a notification however
/// many documents the manifest had.
fn apply_summary(lines: &[String]) -> String {
    const SHOWN: usize = 6;
    if lines.len() <= SHOWN {
        return lines.join("\n");
    }
    let mut text = lines[..SHOWN].join("\n");
    text.push_str(&format!("\n… and {} more", lines.len() - SHOWN));
    text
}

impl eframe::App for SkiffApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        SkiffApp::ui(self, ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        SkiffApp::save(self, storage);
    }
}
