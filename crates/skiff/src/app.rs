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
}

/// Modal dialogs.
pub enum Dialog {
    Delete { key: ResourceKey, ns: String, name: String, force: bool },
    Scale { key: ResourceKey, ns: String, name: String, replicas: String },
    Forward(ForwardDialog),
    /// Create or update objects from a YAML manifest.
    Create { text: String, job: Option<Job<Vec<String>>>, error: Option<String> },
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
}

fn kind_icon(key: &ResourceKey) -> MaterialIcon {
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

    pub fn switch_context(&mut self, name: &str) {
        if self.active.as_deref() == Some(name) {
            return;
        }
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
        if let Some(s) = self.sessions.remove(name) {
            s.shutdown();
        }
        self.dock.close_for_context(name);
        if self.active.as_deref() == Some(name) {
            self.active = None;
            self.detail = None;
        }
    }

    /// (rows shown in the current table, whether it is fully loaded).
    pub fn table_stats(&self) -> (usize, bool) {
        let live = matches!(self.cache.snap.state, LoadState::Live | LoadState::Polling);
        (self.cache.order.len(), live)
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
        if self.view != view {
            self.detail = None;
            self.view = view;
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, tone: Tone) {
        let secs = if tone == Tone::Err { 8 } else { 4 };
        self.toasts.push(Toast { text: text.into(), tone, until: Instant::now() + Duration::from_secs(secs) });
    }

    pub fn set_theme(&mut self, ctx: &egui::Context, dark: bool) {
        self.pal = if dark { Palette::dark() } else { Palette::light() };
        theme::apply(ctx, &self.pal);
    }

    fn poll(&mut self) {
        let mut toasts = Vec::new();
        let sessions = &self.sessions;
        self.pending.retain(|p| match p {
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
            self.toast(lines.join("\n"), Tone::Ok);
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
        self.detail = Some(Detail::open(&session, key, ns, name));
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
                self.dock.open_logs(&session, format!("{name}"), job);
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
                let current = self
                    .cache
                    .snap
                    .find(ns, name)
                    .and_then(|r| {
                        let col = self.cache.snap.columns.iter().find(|c| c.title.eq_ignore_ascii_case("ready"))?;
                        let text = r.text(col);
                        text.split_once('/').map(|(_, total)| total.trim().to_string())
                    })
                    .unwrap_or_else(|| "1".to_string());
                self.dialog = Some(Dialog::Scale {
                    key: key.clone(),
                    ns: ns.to_string(),
                    name: name.to_string(),
                    replicas: current,
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
        if !already {
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
        self.dialog = Some(Dialog::Create { text: template, job: None, error: None });
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

        // Keep relative ages ticking without spinning the CPU.
        ctx.request_repaint_after(Duration::from_secs(1));
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
        let open_palette = KeyboardShortcut::new(Modifiers::COMMAND, Key::K);
        if ctx.input_mut(|i| i.consume_shortcut(&open_palette)) {
            self.palette = match self.palette {
                Some(_) => None,
                None => Some(PaletteState::default()),
            };
        }
        let busy = self.palette.is_some() || self.dialog.is_some() || self.terminal_focused || ctx.text_edit_focused();
        if busy {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Slash)) {
            self.focus_filter = true;
            // Don't let the "/" itself land in the filter box.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == "/")));
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.detail.as_ref().is_some_and(|d| d.editing()) {
                // Leave edit mode to the detail panel.
            } else if self.detail.is_some() {
                self.detail = None;
            } else if let View::Resource(key) = &self.view {
                if let Some(t) = self.tables.get_mut(&key.id()) {
                    t.filter.clear();
                }
            }
        }
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
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
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
                egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
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
                        let msg = match snap.state {
                            LoadState::Failed => snap.error.as_deref().unwrap_or("failed to list namespaces"),
                            _ => "Loading…",
                        };
                        ui.label(RichText::new(msg).color(pal.muted));
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
            let has_current = matches!(&self.view, View::Resource(k) if keys.iter().any(|x| *x == k));
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
                            let text = format!("{forwards} port forward(s)");
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
        let store = session.table(key);
        let latest = store.snapshot();
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
                        for col in snap.columns.iter() {
                            let mut on = tui.col_visible.get(&*col.title).copied().unwrap_or(col.visible);
                            if ui.checkbox(&mut on, &*col.title).changed() {
                                tui.col_visible.insert(col.title.to_string(), on);
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
                    let msg = snap.error.as_deref().unwrap_or("request failed");
                    widgets::empty_state(ui, icons::ICON_ERROR, "Couldn't load this view", msg, &pal);
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
        let typing = ctx.text_edit_focused() || self.palette.is_some() || self.dialog.is_some() || self.terminal_focused;
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
                let next = match cur {
                    Some(c) => (c as i64).saturating_add(step).clamp(0, shown as i64 - 1) as usize,
                    None => 0,
                };
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

        let mut to_open: Option<Arc<Row>> = None;
        if let Some(row) = out.clicked {
            tui.selected = Some(row.uid.clone());
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
        let mut actions = Vec::new();
        Panel::right("detail")
            .resizable(true)
            .default_size(520.0)
            .size_range(360.0..=1100.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin { left: 14, right: 10, top: 10, bottom: 8 }))
            .show(ui, |ui| {
                actions = detail.ui(ui, &pal, &session);
            });
        let ctx = ui.ctx().clone();
        for a in actions {
            let Some(d) = &self.detail else { break };
            let (key, ns, name) = (d.key.clone(), d.ns.clone(), d.name.clone());
            match a {
                DetailAction::Close => self.detail = None,
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
        let Some(dialog) = &mut self.dialog else { return };
        let Some(session) = self.active.as_ref().and_then(|c| self.sessions.get(c)).cloned() else {
            self.dialog = None;
            return;
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
                        if ui.add(del).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            push = Some(Pending::Simple {
                                job: session.delete_object(key, ns, name, *force),
                                ok: format!("Deleted {name}"),
                            });
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                Dialog::Scale { key, ns, name, replicas } => {
                    ui.label(widgets::semibold(format!("Scale {name}"), 15.0));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Replicas");
                        let edit = ui.add(egui::TextEdit::singleline(replicas).hint_text("count").desired_width(70.0));
                        if !edit.has_focus() && !edit.lost_focus() {
                            edit.request_focus();
                        }
                        if ui.small_button("−").clicked() {
                            let n: i32 = replicas.trim().parse().unwrap_or(1);
                            *replicas = (n - 1).max(0).to_string();
                        }
                        if ui.small_button("+").clicked() {
                            let n: i32 = replicas.trim().parse().unwrap_or(0);
                            *replicas = (n + 1).to_string();
                        }
                    });
                    ui.add_space(10.0);
                    let parsed: Option<i32> = replicas.trim().parse().ok().filter(|n| *n >= 0);
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(parsed.is_some(), egui::Button::new("Scale")).clicked()
                            || (parsed.is_some() && ui.input(|i| i.key_pressed(Key::Enter)));
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
                Dialog::Create { text, job, error } => {
                    ui.set_width(640.0);
                    ui.label(widgets::semibold("Create or update from YAML", 15.0));
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
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
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
                                .desired_rows(18)
                                .layouter(&mut layouter),
                        );
                    });
                    if let Some(err) = error {
                        ui.add_space(4.0);
                        ui.add(egui::Label::new(RichText::new(&*err).color(pal.err)).wrap().selectable(true));
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
                        if ui.button("Cancel").clicked() {
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
        if close || resp.should_close() {
            self.dialog = None;
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        let pal = self.pal;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -40.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    for t in self.toasts.iter().rev() {
                        let color = if t.tone == Tone::Normal { pal.accent } else { pal.tone(t.tone) };
                        Frame::new()
                            .fill(pal.raised)
                            .stroke(Stroke::new(1.0, pal.border))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.set_max_width(420.0);
                                ui.horizontal(|ui| {
                                    widgets::dot(ui, color);
                                    ui.add(egui::Label::new(RichText::new(&t.text).color(pal.text)).wrap());
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl eframe::App for SkiffApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        SkiffApp::ui(self, ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        SkiffApp::save(self, storage);
    }
}
