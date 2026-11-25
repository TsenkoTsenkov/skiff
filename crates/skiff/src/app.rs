//! Application state and the top-level layout.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Panel,
    PopupCloseBehavior, Response, RichText, Sense, Stroke, Ui, Vec2,
};
use egui_material_icons::{icons, MaterialIcon};
use serde::{Deserialize, Serialize};
use skiff_core::kinds;
use skiff_core::{
    Category, ConnStatus, Contexts, Core, Job, KubeconfigSources, LoadState, LoadedKubeconfig, Metrics, Notifier,
    PortForward, ResourceKey, Row, Session, Snapshot, TableStore, Tone, BUILTINS,
};

use crate::config::{Config, Theme};
use crate::detail::{Detail, DetailAction};
use crate::dock::Dock;
use crate::keymap::{Action, Keymap};
use crate::overview;
use crate::pages::{self, KubeconfigReport};
use crate::palette::{self, Command, PaletteState};
use crate::settings::{self, Section};
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

/// Which pane keyboard navigation (see [`Keymap`]) acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavFocus {
    /// The sidebar: up / down move a cursor over its entries.
    Sidebar,
    /// The table (and the detail panel next to it).
    #[default]
    Main,
}

/// A sidebar entry the keyboard can land on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NavItem {
    View(View),
    /// A collapsible API group of custom resources.
    Group(String),
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
    /// Kubeconfig files from `--kubeconfig`, in the order given. When not
    /// empty they are used instead of the configured or default ones.
    pub kubeconfigs: Vec<std::path::PathBuf>,
}

// (`serde(default)`: state saved by an older version, which lacks the
// newer fields, must still load.)
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    /// The theme as versions before the `theme` setting stored it: used
    /// only while config.json names none.
    light: bool,
    context: Option<String>,
    namespaces: HashMap<String, Vec<String>>,
    sidebar_closed: bool,
    /// The first-run hint about the palette and `?` has done its job.
    hint_seen: bool,
    /// Column choices and widths, by resource id (window state, like the
    /// sidebar: not a setting of config.json).
    columns: HashMap<String, SavedColumns>,
    /// The version whose "What's new" has been shown: a newer one shows
    /// it again, once.
    seen_version: Option<String>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
struct SavedColumns {
    visible: HashMap<String, bool>,
    widths: HashMap<String, f32>,
}

/// How long a secret copied from Skiff stays on the clipboard.
const SECRET_CLIPBOARD_TIME: Duration = Duration::from_secs(60);

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

/// A standing message under the top bar that stays until it is dismissed
/// (a notification, by contrast, goes away by itself): something the user
/// has to know about but that needs no answer now. Raise one with
/// [`SkiffApp::notify`].
#[derive(Clone, Debug)]
pub struct Notice {
    /// Identifies the notice: raising one with the same key replaces it.
    pub key: String,
    pub tone: Tone,
    /// What it is about, in one line.
    pub title: String,
    /// The details, one per line.
    pub lines: Vec<String>,
    /// A button, with what it does.
    pub action: Option<(String, NoticeAction)>,
}

#[derive(Clone, Debug)]
pub enum NoticeAction {
    OpenUrl(String),
}

/// A question asked before something changes a cluster that is set to ask
/// first (`confirm` in the config file). Shown on top of everything else,
/// with Cancel as the default.
pub struct Confirm {
    /// The question: "Restart Deployment?".
    pub title: String,
    /// What it is about: `shop/web`.
    pub object: String,
    /// What will happen, in a sentence.
    pub note: String,
    /// The label of the button that goes ahead.
    pub verb: String,
    /// The cluster it was asked for; it is dropped when that changes.
    pub context: String,
    op: ConfirmOp,
}

enum ConfirmOp {
    Row { action: RowAction, key: ResourceKey, ns: String, name: String },
    Scale { key: ResourceKey, ns: String, name: String, replicas: i32 },
    /// Apply the manifest in the open create dialog.
    ApplyManifest { force: bool },
    /// Apply the YAML edit in the detail panel.
    ApplyEdit,
}

/// What a kubeconfig reload says about itself in a notification.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Announce {
    /// What was found, even if nothing changed (the user asked).
    Always,
    /// Only that contexts were added or removed.
    Changes,
    Never,
}

/// Size and modification time of a kubeconfig file (`None`: not there),
/// to notice that it changed without reading it.
type FileStamp = Option<(SystemTime, u64)>;

fn file_stamps(sources: &KubeconfigSources) -> Vec<(PathBuf, FileStamp)> {
    sources
        .resolve()
        .into_iter()
        .map(|path| {
            let stamp = std::fs::metadata(&path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
            (path, stamp)
        })
        .collect()
}

/// Where the config file says kubeconfigs are read from.
fn config_sources(config: &Config) -> KubeconfigSources {
    match config.kubeconfigs() {
        Some(entries) => KubeconfigSources::explicit(entries.iter().cloned()),
        None => KubeconfigSources::from_env(),
    }
}

/// The `kubeconfigs` list to save when `entry` is added from the UI.
///
/// The first entry starts the list, and from then on only the list is
/// read. So that the clusters shown until now stay, and keep following
/// the environment, the list starts with
/// [`skiff_core::kubeconfig::DEFAULT_ENTRY`] (`"$KUBECONFIG"`), which is
/// resolved on every load to the files `$KUBECONFIG` lists, else
/// `~/.kube/config`: nothing of this computer or this moment is written
/// into the file.
fn kubeconfigs_with_added(listed: Option<&[String]>, entry: &str) -> KubeconfigSources {
    match listed {
        Some(entries) => KubeconfigSources::explicit(entries.iter().cloned()),
        None => KubeconfigSources::from_env(),
    }
    .with_entry(entry)
}

/// The text on the system clipboard, for [`SkiffApp::clipboard_reader`]:
/// `Some("")` when it holds something that is not text (or nothing),
/// `None` when it could not be read at all.
fn system_clipboard_text() -> Option<String> {
    match arboard::Clipboard::new().ok()?.get_text() {
        Ok(text) => Some(text),
        Err(arboard::Error::ContentNotAvailable) => Some(String::new()),
        Err(_) => None,
    }
}

/// Whether a native "open file" dialog can be expected to appear. On
/// Linux it comes from the desktop portal (over D-Bus) or `zenity`; with
/// neither, asking for one does nothing at all.
/// `clusters.<context>.prometheus` of the config file, as the endpoint the
/// session should use instead of searching the cluster.
fn prometheus_endpoint(config: &Config, context: &str) -> Option<skiff_core::PromEndpoint> {
    config.cluster(context).prometheus.map(|p| skiff_core::PromEndpoint {
        namespace: p.namespace,
        service: p.service,
        port: p.port,
        scheme: p.scheme,
        path_prefix: p.path_prefix,
    })
}

fn native_file_dialog_available() -> bool {
    if cfg!(any(target_os = "macos", windows)) {
        return true;
    }
    let session_bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|v| !v.is_empty())
        || std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|dir| Path::new(&dir).join("bus").exists());
    session_bus || skiff_core::find_executable("zenity").is_some()
}

/// "1 context", "3 contexts".
fn contexts_text(n: usize) -> String {
    widgets::plural(n, "context")
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
    /// Name a kubeconfig file by its path (where no native file dialog is
    /// available, or it did not come up).
    AddKubeconfig { path: String, error: Option<String>, focused: bool },
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
    /// Every context of the kubeconfig files. The cluster menu and the
    /// palette show [`SkiffApp::menu_contexts`], which leaves out the ones
    /// hidden in Settings.
    pub contexts: Contexts,
    /// The config file. Read it with [`SkiffApp::config`], change it with
    /// [`SkiffApp::edit_config`] (which saves and applies it).
    config: Config,
    /// What the last kubeconfig load looked at and could not use.
    kube: KubeconfigReport,
    /// The kubeconfig files as they were at the last load, to notice a
    /// change when the window gets the focus back.
    kube_stamps: Vec<(PathBuf, FileStamp)>,
    /// The kubeconfigs came from `--kubeconfig`, not the config file.
    cli_kubeconfigs: bool,
    /// The window had the focus during the previous frame.
    window_focused: bool,
    /// The native "open file" dialog is up: its answer, and when it was
    /// asked for.
    file_pick: Option<(std::sync::mpsc::Receiver<Option<PathBuf>>, Instant)>,
    /// Whether "Add kubeconfig file…" asks the system for its file dialog;
    /// otherwise (no desktop portal, or a headless run) it asks for the
    /// path in a dialog of its own.
    pub native_file_dialog: bool,
    notices: Vec<Notice>,
    /// A question on top of everything else (see [`Confirm`]).
    pub confirm: Option<Confirm>,
    /// Frames the current confirmation has been shown for.
    confirm_frames: u32,
    /// The window title last asked for.
    title: String,
    /// The failed update the user has been told about.
    update_failure_told: Option<String>,
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
    updater: crate::update::Updater,
    /// The user asked for the current update check (so "up to date" is
    /// worth saying).
    update_check_asked: bool,
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
    pub keymap: Keymap,
    pub nav_focus: NavFocus,
    /// The sidebar entry under the keyboard cursor (while it has focus).
    sidebar_cursor: Option<NavItem>,
    /// Scroll the sidebar cursor into view on the next frame.
    sidebar_scroll: bool,
    /// Where the sidebar was drawn last frame.
    sidebar_rect: Option<egui::Rect>,
    ctx: egui::Context,
    /// Built without the user's own files (tests, headless rendering).
    headless: bool,
    /// `--light`: the theme for this run, until one is chosen in the app.
    theme_override: Option<Theme>,
    /// The theme a version before the `theme` setting had stored.
    legacy_light: bool,
    /// The Settings window, while it is open.
    pub settings: Option<settings::State>,
    /// The list of keyboard shortcuts is open.
    pub help_open: bool,
    /// The "What's new" window, while it is open, with what it shows.
    pub whats_new: Option<Vec<crate::whats_new::Release>>,
    /// The first-run hint has been dismissed or acted on.
    hint_seen: bool,
    /// Counts kubeconfig loads, for what is derived from one.
    kube_generation: u64,
    /// Size and time of config.json and keys.json when last read or
    /// written, to notice an edit made outside the app.
    settings_stamps: [FileStamp; 2],
    /// A secret Skiff put on the clipboard is to be cleared at this time.
    secret_clear_at: Option<Instant>,
    /// A keyed hash of that secret (never its text), to tell at that time
    /// whether the clipboard still holds it.
    secret_hash: u64,
    secret_hasher: std::collections::hash_map::RandomState,
    /// Reads the system clipboard's text: `Some("")` when it holds no
    /// text, `None` when it could not be read. With it, a copied secret
    /// is left alone when the clipboard is known to hold something else
    /// by then (copied in another app). Without it, or when the read
    /// fails, nothing is known, and the clipboard is cleared whenever
    /// nothing else was copied in Skiff: a secret is never left behind
    /// because of a clipboard that would not answer.
    pub clipboard_reader: Option<Box<dyn Fn() -> Option<String>>>,
    /// A secret was copied during this frame.
    secret_copied: bool,
    /// How long a copied secret stays on the clipboard.
    pub secret_clipboard_time: Duration,
    /// Whether "Open config file" starts the system's editor (not in
    /// headless runs).
    pub open_files: bool,
}

fn stamp_of(path: Option<&Path>) -> FileStamp {
    let meta = std::fs::metadata(path?).ok()?;
    Some((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()))
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

/// Custom resource types discovery found that have no fixed place in the
/// sidebar, grouped by API group, in sidebar order.
fn custom_group_list(found: &[skiff_core::Discovered]) -> Vec<(String, Vec<ResourceKey>)> {
    let mut groups: Vec<(String, Vec<ResourceKey>)> = Vec::new();
    for d in found {
        if kinds::builtin(&d.key.group, &d.key.plural).is_some() {
            continue;
        }
        let g: &str = if d.key.group.is_empty() { "core" } else { &d.key.group };
        match groups.last_mut() {
            Some((name, list)) if name == g => list.push(d.key.clone()),
            _ => groups.push((g.to_string(), vec![d.key.clone()])),
        }
    }
    groups
}

/// A sidebar row; `cursor` draws the keyboard cursor around it.
fn nav_row(
    ui: &mut Ui,
    pal: &Palette,
    ic: MaterialIcon,
    label: &str,
    selected: bool,
    indent: f32,
    cursor: bool,
) -> Response {
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
        if cursor {
            p.rect_stroke(rect.shrink(0.5), CornerRadius::same(6), Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
        }
        let fg = if selected || resp.hovered() || cursor { pal.text } else { pal.muted };
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
    widgets::focus_ring(ui, &resp, 6, pal);
    let label = label.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One cluster in the switcher: status dot and name, left-aligned, at a
/// fixed width.
#[allow(clippy::too_many_arguments)]
fn cluster_row(
    ui: &mut Ui,
    pal: &Palette,
    width: f32,
    name: &str,
    selected: bool,
    dot: Color32,
    room_for_button: bool,
    // Small icons after the name (read-only, unverified TLS).
    marks: &[(MaterialIcon, Color32)],
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
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
        p.circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, dot);
        let mut right = if room_for_button { 36.0 } else { 8.0 };
        for (ic, color) in marks.iter().rev() {
            p.text(
                egui::pos2(rect.right() - right, rect.center().y),
                Align2::RIGHT_CENTER,
                ic.codepoint,
                FontId::new(14.0, ic.font_family()),
                *color,
            );
            right += 20.0;
        }
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 28.0, rect.top()),
            egui::pos2(rect.right() - right, rect.bottom()),
        );
        let color = if selected { pal.accent } else { pal.text };
        let font = if selected { FontId::new(13.0, theme::semibold()) } else { FontId::proportional(13.0) };
        let mut job = egui::text::LayoutJob::simple_singleline(name.to_owned(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width().max(10.0));
        let galley = ui.painter().layout_job(job);
        p.galley(egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
    }
    widgets::focus_ring(ui, &resp, 6, pal);
    let label = name.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl SkiffApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: AppOptions) -> Self {
        let stored: Option<Saved> = cc.storage.and_then(|s| eframe::get_value(s, "skiff"));
        let existing_user = stored.is_some();
        let saved = stored.unwrap_or_default();
        let seen_version = saved.seen_version.clone();
        let (config, warnings) = Config::load();
        let mut app = Self::build(&cc.egui_ctx, opts, saved, config, warnings);
        // Just updated: say what changed, once.
        let news = crate::whats_new::after_update(
            &crate::whats_new::parse(crate::whats_new::CHANGELOG),
            seen_version.as_deref(),
            existing_user,
            crate::update::current_version(),
        );
        if !news.is_empty() {
            app.whats_new = Some(news);
        }
        app.updater.enable();
        app.native_file_dialog = native_file_dialog_available();
        app.headless = false;
        app.clipboard_reader = Some(Box::new(system_clipboard_text));
        app.open_files = true;
        app.settings_stamps = app.read_settings_stamps();
        let (keymap, error) = Keymap::load();
        app.keymap = keymap;
        if let Some(e) = error {
            app.toast(format!("Key bindings: {e}"), Tone::Warn);
        }
        app
    }

    /// Construct without persisted settings (tests, headless rendering):
    /// nothing is read from, or written to, the user's own files.
    pub fn headless(ctx: &egui::Context, opts: AppOptions) -> Self {
        Self::build(ctx, opts, Saved::default(), Config::default(), Vec::new())
    }

    /// Like [`SkiffApp::headless`], with a config file (as loaded by
    /// `Config::load_from`, with its warnings).
    pub fn headless_with_config(ctx: &egui::Context, opts: AppOptions, config: Config, warnings: Vec<String>) -> Self {
        Self::build(ctx, opts, Saved::default(), config, warnings)
    }

    fn build(ctx: &egui::Context, opts: AppOptions, saved: Saved, config: Config, warnings: Vec<String>) -> Self {
        theme::install_fonts(ctx);
        // The theme: --light for this run, else config.json's, else the
        // one an older version stored, else dark. (What "system" means is
        // known from the first frame on.)
        let theme_override = opts.light.then_some(Theme::Light);
        let choice = theme_override.or(config.theme()).unwrap_or(if saved.light { Theme::Light } else { Theme::Dark });
        let pal = if choice == Theme::Light { Palette::light() } else { Palette::dark() };
        theme::apply(ctx, &pal);
        // Zoom is Skiff's own (saved in config.json), not egui's.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let repaint = ctx.clone();
        let core = Core::new(Notifier::new(move || repaint.request_repaint())).expect("failed to start I/O runtime");
        // Where clusters come from: --kubeconfig, else the config file's
        // list, else what kubectl reads.
        let cli_kubeconfigs = !opts.kubeconfigs.is_empty();
        core.set_kubeconfig_sources(if cli_kubeconfigs {
            KubeconfigSources::explicit(opts.kubeconfigs.iter().map(|p| p.to_string_lossy().into_owned()))
        } else {
            config_sources(&config)
        });
        let mut tables: HashMap<String, TableUi> = HashMap::new();
        for (id, cols) in saved.columns {
            let t = tables.entry(id).or_default();
            t.col_visible = cols.visible;
            t.widths = cols.widths;
        }
        // (`apply_config` sets the log history a new tab loads.)
        let dock = Dock::default();
        let mut app = SkiffApp {
            core,
            pal,
            contexts: Contexts::default(),
            config,
            kube: KubeconfigReport::default(),
            kube_stamps: Vec::new(),
            cli_kubeconfigs,
            window_focused: true,
            file_pick: None,
            native_file_dialog: false,
            notices: Vec::new(),
            confirm: None,
            confirm_frames: 0,
            title: "Skiff".to_string(),
            update_failure_told: None,
            sessions: HashMap::new(),
            active: None,
            view: View::Overview,
            ns_filter: BTreeSet::new(),
            saved_ns: saved.namespaces,
            ns_search: String::new(),
            tables,
            cache: ViewCache::default(),
            detail: None,
            dock,
            updater: Default::default(),
            update_check_asked: false,
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
            keymap: Keymap::default(),
            nav_focus: NavFocus::Main,
            sidebar_cursor: None,
            sidebar_scroll: false,
            sidebar_rect: None,
            ctx: ctx.clone(),
            headless: true,
            theme_override,
            legacy_light: saved.light,
            settings: None,
            help_open: false,
            whats_new: None,
            hint_seen: saved.hint_seen,
            kube_generation: 0,
            settings_stamps: [None, None],
            secret_clear_at: None,
            secret_hash: 0,
            secret_hasher: Default::default(),
            clipboard_reader: None,
            secret_copied: false,
            secret_clipboard_time: SECRET_CLIPBOARD_TIME,
            open_files: false,
        };
        app.apply_config();
        app.settings_stamps = app.read_settings_stamps();
        app.report_config_warnings(&warnings);
        let loaded = app.core.load_kubeconfig();
        app.take_kubeconfig(loaded);
        let known = |name: &String| app.contexts.items.iter().any(|c| &c.name == name);
        let initial = opts
            .context
            .or(saved.context.filter(known))
            .or(app.contexts.current.clone().filter(known))
            .or(app.contexts.items.iter().find(|c| !app.config.cluster(&c.name).hidden).map(|c| c.name.clone()))
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
            hint_seen: self.hint_seen,
            seen_version: Some(crate::update::current_version().to_string()),
            columns: self
                .tables
                .iter()
                .filter(|(_, t)| !t.col_visible.is_empty() || !t.widths.is_empty())
                .map(|(id, t)| (id.clone(), SavedColumns { visible: t.col_visible.clone(), widths: t.widths.clone() }))
                .collect(),
        };
        eframe::set_value(storage, "skiff", &saved);
    }

    /// Whether the cluster menu and the palette offer this context: not
    /// when it is hidden in Settings, unless it is the open one (the menu
    /// never hides where you are).
    fn in_menu(&self, context: &str) -> bool {
        !self.config.cluster(context).hidden || self.active.as_deref() == Some(context)
    }

    /// The contexts the cluster menu and the palette offer.
    pub fn menu_contexts(&self) -> Contexts {
        let mut visible = self.contexts.clone();
        visible.items.retain(|c| self.in_menu(&c.name));
        visible
    }

    pub fn session(&self) -> Option<&Arc<Session>> {
        self.active.as_ref().and_then(|c| self.sessions.get(c))
    }

    // ------------------------------------------------------------------
    // Config file
    // ------------------------------------------------------------------

    /// The config file's settings.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Changes the config file: `change` edits it, then it is saved (a
    /// failure is reported as a notification) and applied to the running
    /// app. This is the one way settings are changed from the UI.
    pub fn edit_config(&mut self, change: impl FnOnce(&mut Config)) {
        change(&mut self.config);
        // (A headless run without a config file has nowhere to save to,
        // and nothing to say about it.)
        if !(self.headless && self.config.path().is_none()) {
            if let Err(e) = self.config.save() {
                self.toast(e, Tone::Err);
            }
        }
        self.settings_stamps = self.read_settings_stamps();
        self.apply_config();
    }

    /// Says which settings of the config file were not understood (and
    /// takes that back when there are none any more).
    fn report_config_warnings(&mut self, warnings: &[String]) {
        if let Some(why) = self.config.unusable() {
            // Only at startup is an unusable file taken as the config
            // (there is no earlier one to keep): the defaults then, but
            // not their "nothing is read-only".
            let lines = vec![
                "Default settings are in use, and every cluster is read-only until the file is fixed.".to_string(),
                "Fix the file and come back to Skiff; it is read again then.".to_string(),
            ];
            self.report_unusable_config(why.to_string(), lines);
            return;
        }
        if warnings.is_empty() {
            self.notices.retain(|n| n.key != "config");
            return;
        }
        let file = self.config.path().map(pages::short_path).unwrap_or_else(|| "the config file".to_string());
        const SHOWN: usize = 5;
        let mut lines: Vec<String> = warnings.iter().take(SHOWN).cloned().collect();
        if warnings.len() > SHOWN {
            lines.push(format!("… and {} more", warnings.len() - SHOWN));
        }
        self.notify(Notice {
            key: "config".into(),
            tone: Tone::Warn,
            title: format!("Some settings in {file} were ignored"),
            lines,
            action: None,
        });
    }

    /// Says that the config file cannot be used at all (`why` follows its
    /// name: "is not valid JSON (...)") and what that means for now.
    fn report_unusable_config(&mut self, why: String, lines: Vec<String>) {
        let file = self.config.path().map(pages::short_path).unwrap_or_else(|| "The config file".to_string());
        self.notify(Notice { key: "config".into(), tone: Tone::Err, title: format!("{file} {why}"), lines, action: None });
    }

    /// Whether Skiff refuses changes to the cluster behind `context`: the
    /// config file says so, or it could not be used when Skiff started,
    /// in which case what it says about protection is not known and
    /// every cluster is held read-only until it can be read.
    fn cluster_read_only(&self, context: &str) -> bool {
        self.config.unusable().is_some() || self.config.cluster(context).read_only
    }

    /// Where the key bindings file is.
    fn keys_path(&self) -> Option<PathBuf> {
        if self.headless {
            // Next to the config file the run was given, never the user's own.
            self.config.path().and_then(Path::parent).map(|dir| dir.join("keys.json"))
        } else {
            crate::config::keymap_path()
        }
    }

    fn read_settings_stamps(&self) -> [FileStamp; 2] {
        [stamp_of(self.config.path()), stamp_of(self.keys_path().as_deref())]
    }

    /// Picks up config.json and keys.json again if they were edited
    /// outside the app (checked when the window gets the focus back).
    /// Returns whether anything was read.
    pub fn reload_settings_if_changed(&mut self) -> bool {
        let now = self.read_settings_stamps();
        let before = std::mem::replace(&mut self.settings_stamps, now);
        let mut reloaded = false;
        if let Some(path) = self.config.path().map(Path::to_path_buf).filter(|_| now[0] != before[0]) {
            let listed = self.config.kubeconfigs().map(<[String]>::to_vec);
            // What the file says now, with its warnings; `Err` when it
            // says nothing usable.
            let read: Result<Vec<String>, Option<String>> = if self.config.is_dirty() {
                // A change made here could not be saved earlier (the file
                // was broken or could not be written). Saving reads the
                // file as it is now and puts that change on top.
                self.config.save().map(|_| self.config.warnings()).map_err(Some)
            } else {
                let (config, warnings) = Config::load_from(&path);
                match config.unusable() {
                    // A file that cannot be read says nothing, and above
                    // all not "this cluster is no longer read-only": the
                    // settings in use stay, kubeconfig list included.
                    Some(why) => {
                        let why = why.to_string();
                        let lines = if self.config.unusable().is_some() {
                            // (Still as broken as when Skiff started.)
                            vec!["Default settings are in use, and every cluster is read-only until the file is fixed."
                                .to_string()]
                        } else {
                            vec!["The settings in use are unchanged. Fix the file and come back to Skiff.".to_string()]
                        };
                        self.report_unusable_config(why, lines);
                        Err(None)
                    }
                    None => {
                        self.config = config;
                        Ok(warnings)
                    }
                }
            };
            match read {
                Ok(warnings) => {
                    self.report_config_warnings(&warnings);
                    if self.config.kubeconfigs() != listed.as_deref() && !self.cli_kubeconfigs {
                        self.core.set_kubeconfig_sources(config_sources(&self.config));
                        self.reload_kubeconfig_saying(Announce::Changes);
                    }
                    self.apply_config();
                    // (Saving may have written the file.)
                    self.settings_stamps[0] = self.read_settings_stamps()[0];
                    self.toast("Settings reloaded from config.json", Tone::Normal);
                    reloaded = true;
                }
                // Said once: the stamp is replaced, so the next try is at
                // the file's next change, not at every return to the window.
                Err(Some(e)) => self.toast(e, Tone::Err),
                Err(None) => {}
            }
        }
        if now[1] != before[1] && !self.headless {
            let (keymap, error) = Keymap::load();
            self.keymap = keymap;
            match error {
                Some(e) => self.toast(format!("Key bindings: {e}"), Tone::Warn),
                None => self.toast("Key bindings reloaded from keys.json", Tone::Normal),
            }
            reloaded = true;
        }
        reloaded
    }

    /// Puts the config file's settings into effect (everything but the
    /// kubeconfig list, which [`SkiffApp::set_kubeconfig_sources`] owns).
    fn apply_config(&mut self) {
        let updates = self.config.updates();
        let channel = match updates.channel {
            crate::config::Channel::Stable => crate::update::Channel::Stable,
            crate::config::Channel::Nightly => crate::update::Channel::Nightly,
        };
        self.updater.configure(updates.check, channel);
        crate::external::set_preferred_terminal(self.config.terminal());
        self.dock.default_tail = self.log_history();
        for (name, session) in &self.sessions {
            session.set_read_only(self.cluster_read_only(name));
            session.set_prometheus_endpoint(prometheus_endpoint(&self.config, name));
        }
        self.ctx.set_zoom_factor(self.zoom());
        self.sync_theme();
    }

    /// Earlier lines a new log tab loads (`None`: all of them).
    pub fn log_history(&self) -> Option<i64> {
        self.config.log_history().map_or(skiff_core::LogOptions::default().tail, |h| h.tail())
    }

    /// Every table back to its default columns and widths.
    pub fn reset_all_columns(&mut self) {
        for t in self.tables.values_mut() {
            t.reset_columns();
        }
    }

    /// Hides a cluster from the cluster menu and the palette (or shows it
    /// again) and saves that in the config file.
    pub fn set_cluster_hidden(&mut self, context: &str, hidden: bool) {
        self.edit_config(|c| c.set_hidden(context, hidden));
    }

    // ------------------------------------------------------------------
    // Appearance
    // ------------------------------------------------------------------

    /// The zoom factor in effect.
    pub fn zoom(&self) -> f32 {
        self.config.zoom().unwrap_or(1.0)
    }

    /// Sets the zoom factor (clamped) and saves it.
    pub fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(crate::config::ZOOM_MIN, crate::config::ZOOM_MAX);
        if (zoom - self.zoom()).abs() > 0.001 {
            self.edit_config(|c| c.set_zoom(zoom));
        }
    }

    /// One zoom step up or down, saying the new size.
    fn zoom_by_step(&mut self, up: bool) {
        let next = settings::zoom_step(self.zoom(), up);
        self.set_zoom(next);
        self.toast(format!("Zoom {}", settings::zoom_percent(self.zoom())), Tone::Normal);
    }

    /// The theme that is chosen: `--light` for this run, else the config
    /// file's, else the one an older version stored, else dark.
    pub fn theme_choice(&self) -> Theme {
        self.theme_override
            .or(self.config.theme())
            .unwrap_or(if self.legacy_light { Theme::Light } else { Theme::Dark })
    }

    /// Chooses the theme and saves it in the config file.
    pub fn choose_theme(&mut self, theme: Theme) {
        self.theme_override = None;
        self.legacy_light = false;
        self.edit_config(|c| c.set_theme(theme));
    }

    /// Makes the palette the one the chosen theme asks for; "system" is
    /// whatever the computer is set to now (dark when it does not say).
    /// Returns whether the palette changed.
    fn sync_theme(&mut self) -> bool {
        let dark = match self.theme_choice() {
            Theme::Dark => true,
            Theme::Light => false,
            Theme::System => self.ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark),
        };
        if dark == self.pal.dark {
            return false;
        }
        self.pal = if dark { Palette::dark() } else { Palette::light() };
        theme::apply(&self.ctx, &self.pal);
        true
    }

    // ------------------------------------------------------------------
    // Settings, shortcuts, clipboard
    // ------------------------------------------------------------------

    /// Opens the Settings window on a section.
    pub fn open_settings(&mut self, section: Section) {
        self.palette = None;
        self.help_open = false;
        self.whats_new = None;
        match &mut self.settings {
            Some(state) => state.section = section,
            None => self.settings = Some(settings::State::new(section)),
        }
    }

    /// Opens the list of keyboard shortcuts.
    pub fn open_help(&mut self) {
        self.palette = None;
        self.settings = None;
        self.whats_new = None;
        self.help_open = true;
        self.hint_seen = true;
    }

    /// Opens "What's new" on this version's changelog entries (and, on the
    /// nightly channel, what is not released yet).
    pub fn open_whats_new(&mut self) {
        use crate::whats_new::{between, parse, CHANGELOG};
        self.palette = None;
        self.settings = None;
        self.help_open = false;
        self.hint_seen = true;
        let all = parse(CHANGELOG);
        let mut shown = Vec::new();
        if self.config.updates().channel == crate::config::Channel::Nightly {
            shown.extend(all.iter().filter(|r| r.name == "Unreleased" && !r.is_empty()).cloned());
        }
        shown.extend(between(&all, None, crate::update::current_version()));
        self.whats_new = Some(shown);
    }

    /// Puts a secret on the clipboard, to be cleared again after
    /// [`SkiffApp::secret_clipboard_time`] unless something else is
    /// copied in Skiff first. `what` names it in the notification.
    ///
    /// (egui cannot read the clipboard back. Where there is a
    /// [`SkiffApp::clipboard_reader`], the clipboard is left alone when
    /// it is found to hold something else by then; where there is none,
    /// or the read fails, it is cleared whenever nothing else was copied
    /// from Skiff in the meantime. The notification says which.)
    pub fn copy_secret(&mut self, ctx: &egui::Context, text: String, what: &str) {
        use std::hash::BuildHasher;
        self.secret_hash = self.secret_hasher.hash_one(&text);
        ctx.copy_text(text);
        self.secret_copied = true;
        let secs = self.secret_clipboard_time.as_secs().max(1);
        let unless = if self.clipboard_reader.is_some() { " if it still holds it" } else { "" };
        self.toast(format!("Copied {what}. The clipboard will be cleared in {secs} s{unless}."), Tone::Normal);
    }

    /// Whether the secret copied last is still what the clipboard holds,
    /// as far as that can be known: `None` without a reader, or when the
    /// clipboard could not be read.
    fn clipboard_holds_secret(&self) -> Option<bool> {
        use std::hash::BuildHasher;
        let text = (self.clipboard_reader.as_ref()?)()?;
        Some(self.secret_hasher.hash_one(&text) == self.secret_hash)
    }

    /// Whether a copied secret is waiting to be cleared from the clipboard.
    pub fn secret_on_clipboard(&self) -> bool {
        self.secret_clear_at.is_some()
    }

    /// Run at the end of every frame: starts, cancels or carries out the
    /// clearing of a copied secret.
    fn guard_clipboard(&mut self, ctx: &egui::Context) {
        // Everything Skiff copies goes out through egui's output, text
        // fields and the terminal included.
        let copied = ctx.output(|o| o.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(_))));
        if std::mem::take(&mut self.secret_copied) {
            self.secret_clear_at = Some(Instant::now() + self.secret_clipboard_time);
        } else if copied {
            // Something else is on the clipboard now: not ours to clear.
            self.secret_clear_at = None;
        }
        if let Some(at) = self.secret_clear_at {
            let left = at.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.secret_clear_at = None;
                // Read once, now, not every frame. Something copied in
                // another app since is not ours to clear (nor to announce);
                // a clipboard that cannot be read is cleared, as the
                // secret may well still be on it.
                if self.clipboard_holds_secret().unwrap_or(true) {
                    ctx.copy_text(String::new());
                    self.toast("Clipboard cleared", Tone::Normal);
                }
            } else {
                // Wake up for it even if nothing else happens.
                self.tick = self.tick.min(left);
                ctx.request_repaint_after(left);
            }
        }
    }

    /// Opens config.json or keys.json in the system's editor, creating it
    /// first when it does not exist: keys.json with the standard keys
    /// spelled out, config.json empty (an example would switch settings
    /// on, and JSON has no comments to put it in).
    pub fn open_settings_file(&mut self, keys: bool) {
        let (path, name, starter) = if keys {
            (self.keys_path(), "keys.json", Keymap::starter_json())
        } else {
            (self.config.path().map(Path::to_path_buf), "config.json", "{}\n".to_string())
        };
        let Some(path) = path else {
            self.toast(format!("No location is known for {name} on this computer."), Tone::Warn);
            return;
        };
        let created = match settings::ensure_file(&path, &starter) {
            Ok(created) => created,
            Err(e) => {
                self.toast(e, Tone::Err);
                return;
            }
        };
        // (Our own new file is not a change to pick up.)
        self.settings_stamps = self.read_settings_stamps();
        if self.open_files {
            if let Err(e) = settings::open_in_default_app(&path) {
                self.ctx.copy_text(path.display().to_string());
                self.toast(e, Tone::Err);
                return;
            }
        }
        let shown = pages::short_path(&path);
        let text = match (created, keys) {
            (true, true) => format!("Created {shown} with the standard keys"),
            (true, false) => format!("Created {shown}"),
            (false, _) => format!("Opened {shown}"),
        };
        self.toast(text, Tone::Normal);
    }

    /// Which rule decided the kubeconfig files in effect.
    fn kube_source(&self) -> settings::KubeSource {
        if self.cli_kubeconfigs {
            settings::KubeSource::CommandLine
        } else if self.config.kubeconfigs().is_some() {
            settings::KubeSource::ConfigFile
        } else if std::env::var_os("KUBECONFIG").is_some_and(|v| !v.is_empty()) {
            settings::KubeSource::Environment
        } else {
            settings::KubeSource::Default
        }
    }

    /// Takes a kubeconfig entry off the list in the config file and
    /// reloads. Clusters that are open stay open while their context
    /// still exists.
    pub fn remove_kubeconfig(&mut self, entry: &str) {
        self.edit_config(|c| c.remove_kubeconfig(entry));
        self.follow_config_kubeconfigs();
    }

    /// Removes the list of kubeconfigs from the config file: back to
    /// `$KUBECONFIG` / `~/.kube/config`.
    pub fn reset_kubeconfigs(&mut self) {
        self.edit_config(|c| c.set_kubeconfigs(None));
        self.follow_config_kubeconfigs();
    }

    fn follow_config_kubeconfigs(&mut self) {
        if !self.cli_kubeconfigs {
            self.core.set_kubeconfig_sources(config_sources(&self.config));
        }
        self.reload_kubeconfig_saying(Announce::Changes);
    }

    fn settings_model(&mut self) -> Option<settings::Model> {
        let generation = self.kube_generation;
        let removable = self.kube_source() == settings::KubeSource::ConfigFile;
        let fresh = self.settings.as_ref()?.kube.as_ref().is_some_and(|(g, _)| *g == generation);
        if !fresh {
            let sources = self.core.kubeconfig_sources();
            let mut kube = settings::kube_model(&sources, &self.kube, &self.contexts, removable);
            // (Not for --kubeconfig, which is for one run, and never the
            // user's own folder in a headless run.)
            if !self.cli_kubeconfigs && !self.headless {
                kube.suggestions = skiff_core::kubeconfig::suggestions(&sources);
            }
            self.settings.as_mut()?.kube = Some((generation, Arc::new(kube)));
        }
        let kube = self.settings.as_ref()?.kube.as_ref()?.1.clone();
        let clusters = self
            .contexts
            .items
            .iter()
            .map(|c| {
                let cluster = self.config.cluster(&c.name);
                let session = self.sessions.get(&c.name);
                let state = match session.map(|s| s.status()).as_deref() {
                    Some(ConnStatus::Connected { version }) => settings::ClusterState::Connected(version.clone()),
                    Some(ConnStatus::Connecting) => settings::ClusterState::Connecting,
                    Some(ConnStatus::Failed(err)) => settings::ClusterState::Failed(
                        session.and_then(|s| s.failure()).map(|info| info.summary).unwrap_or_else(|| err.clone()),
                    ),
                    None => settings::ClusterState::NotConnected,
                };
                settings::ClusterRow {
                    name: c.name.clone(),
                    active: self.active.as_deref() == Some(c.name.as_str()),
                    state,
                    hidden: cluster.hidden,
                    read_only: self.cluster_read_only(&c.name),
                    confirm: cluster.confirm,
                    plugin: c.exec_command.clone(),
                    plugin_missing: c.exec_missing,
                }
            })
            .collect();
        Some(settings::Model {
            theme: self.theme_choice(),
            system_dark: self.ctx.system_theme().map(|t| t == egui::Theme::Dark),
            zoom: self.zoom(),
            log_history: self.log_history(),
            tables_customised: self.tables.values().filter(|t| !t.col_visible.is_empty() || !t.widths.is_empty()).count(),
            kube_source: self.kube_source(),
            kube,
            clusters,
            picking: self.file_pick.is_some(),
            updates: self.config.updates(),
            update_state: self.updater.state(),
            updates_off_by_env: std::env::var_os("SKIFF_NO_UPDATE_CHECK").is_some(),
            terminal: self.config.terminal().map(str::to_string),
            terminal_env: std::env::var("SKIFF_TERMINAL").ok().filter(|t| !t.trim().is_empty()),
            config_path: self.config.path().map(Path::to_path_buf),
            keys_path: self.keys_path(),
            mac: self.ctx.os() == egui::os::OperatingSystem::Mac,
            frame_time: self.last_ui,
            memory: resident_memory(),
            connections: (self.sessions.len(), self.sessions.values().map(|s| s.active_watches()).sum()),
        })
    }

    fn settings_ui(&mut self, ctx: &egui::Context) {
        let Some(model) = self.settings_model() else { return };
        let pal = self.pal;
        let Some(state) = &mut self.settings else { return };
        // (A dialog or question on top of the window has the keyboard
        // and the clicks; closing is for when it is the window's turn.)
        let covered = self.dialog.is_some() || self.confirm.is_some();
        for action in settings::show(ctx, &pal, state, &model) {
            use settings::Action as A;
            match action {
                A::Close => {
                    if !covered {
                        self.settings = None;
                    }
                }
                A::SetTheme(theme) => self.choose_theme(theme),
                A::SetZoom(zoom) => self.set_zoom(zoom),
                A::SetLogHistory(tail) => {
                    self.edit_config(|c| c.set_log_history(crate::config::LogHistory::from_tail(tail)));
                }
                A::ResetColumns => {
                    self.reset_all_columns();
                    self.toast("Every table is back to its default columns", Tone::Normal);
                }
                A::AddKubeconfig => self.ask_for_kubeconfig(ctx),
                A::AddKubeconfigPath(path) => {
                    if let Err(e) = self.add_kubeconfig(&path) {
                        self.toast(e, Tone::Err);
                    }
                }
                A::RemoveKubeconfig(entry) => self.remove_kubeconfig(&entry),
                A::ResetKubeconfigs => self.reset_kubeconfigs(),
                A::ReloadKubeconfig => self.reload_kubeconfig(true),
                A::SetReadOnly(context, on) => self.set_read_only(&context, on),
                A::SetConfirm(context, on) => self.set_confirm(&context, on),
                A::SetHidden(context, on) => self.set_cluster_hidden(&context, on),
                // (Opening closes the window: the cluster is what the
                // user asked to see. An unsaved edit keeps both.)
                A::OpenCluster(context) => {
                    self.switch_context(&context);
                    if self.active.as_deref() == Some(context.as_str()) && !covered {
                        self.settings = None;
                    }
                }
                A::Reconnect(context) => {
                    if let Some(s) = self.sessions.get(&context) {
                        s.reconnect();
                        self.toast(format!("Reconnecting to {context}"), Tone::Normal);
                    }
                }
                A::Disconnect(context) => self.disconnect(&context),
                A::SetUpdateCheck(on) => self.edit_config(|c| c.set_update_check(on)),
                A::SetChannel(channel) => self.edit_config(|c| c.set_update_channel(channel)),
                A::CheckForUpdates => self.run_command(ctx, Command::CheckForUpdates),
                A::SetTerminal(name) => self.edit_config(|c| c.set_terminal(name.as_deref())),
                A::OpenConfigFile => self.open_settings_file(false),
                A::OpenKeysFile => self.open_settings_file(true),
                A::ShowShortcuts => self.open_help(),
                A::WhatsNew => self.open_whats_new(),
                A::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
                A::CopyVersionInfo => {
                    ctx.copy_text(settings::version_info(self.config.updates().channel));
                    self.toast("Copied the version info", Tone::Normal);
                }
            }
        }
    }

    fn whats_new_ui(&mut self, ctx: &egui::Context) {
        let Some(releases) = &self.whats_new else { return };
        if crate::whats_new::show(ctx, &self.pal, releases) {
            self.whats_new = None;
        }
    }

    fn help_ui(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mac = ctx.os() == egui::os::OperatingSystem::Mac;
        // The key that opens the list also closes it.
        let again = ctx.input(|i| self.keymap.pressed(i, Action::Help));
        if crate::help::show(ctx, &self.pal, &self.keymap, mac) || again {
            self.help_open = false;
        }
    }

    /// The first-run pointer at the two things worth knowing: the palette
    /// and the list of shortcuts. Goes away once dismissed or used.
    fn first_run_hint(&mut self, ui: &mut Ui) {
        if self.hint_seen {
            return;
        }
        let pal = self.pal;
        let cmd = if ui.ctx().os() == egui::os::OperatingSystem::Mac { "Cmd+K" } else { "Ctrl+K" };
        let help: Vec<String> = self.keymap.bindings(Action::Help).iter().take(1).map(|b| b.label(false)).collect();
        Frame::new()
            .fill(pal.tint(pal.accent, 0.08))
            .stroke(Stroke::new(1.0, pal.tint(pal.accent, 0.28)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width() - 6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(widgets::icon(icons::ICON_LIGHTBULB, 16.0).color(pal.accent));
                    ui.label(RichText::new("Press").color(pal.text));
                    widgets::keycap(ui, cmd, &pal);
                    ui.label(RichText::new("to search or jump to anything").color(pal.text));
                    if let Some(key) = help.first() {
                        ui.label(RichText::new("and").color(pal.text));
                        widgets::keycap(ui, key, &pal);
                        ui.label(RichText::new("to see every keyboard shortcut.").color(pal.text));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close = egui::Button::new(widgets::icon(icons::ICON_CLOSE, 15.0).color(pal.muted))
                            .frame_when_inactive(false)
                            .min_size(Vec2::new(24.0, 22.0));
                        if widgets::named(ui.add(close), "Dismiss this hint").clicked() {
                            self.hint_seen = true;
                        }
                    });
                });
            });
        ui.add_space(10.0);
    }

    /// Whether the current cluster is read-only in Skiff.
    pub fn read_only(&self) -> bool {
        self.session().is_some_and(|s| s.is_read_only())
    }

    /// Why actions that change the current cluster are disabled, if they
    /// are: the words for their tooltips.
    fn locked_reason(&self) -> Option<String> {
        let session = self.session().filter(|s| s.is_read_only())?;
        Some(format!("{} is read-only in Skiff. Turn that off in the cluster menu to make changes.", session.context))
    }

    /// Whether changes to the current cluster are confirmed first.
    fn asks_first(&self) -> bool {
        self.active.as_ref().is_some_and(|c| self.config.cluster(c).confirm)
    }

    /// Makes a cluster read-only in Skiff (or writable again) and saves
    /// that in the config file.
    pub fn set_read_only(&mut self, context: &str, read_only: bool) {
        if let (false, Some(why)) = (read_only, self.config.unusable()) {
            // Not this switch's to lift: nothing is known about the
            // file's settings, and nothing can be saved into it.
            let file = self.config.path().map(pages::short_path).unwrap_or_else(|| "The config file".to_string());
            self.toast(format!("{file} {why}. Every cluster is read-only until the file is fixed."), Tone::Err);
            return;
        }
        self.edit_config(|c| c.set_read_only(context, read_only));
        if read_only && self.active.as_deref() == Some(context) {
            // Whatever was about to change the cluster is off.
            self.confirm = None;
            if !matches!(self.dialog, Some(Dialog::AddKubeconfig { .. })) {
                self.dialog = None;
            }
        }
        let text = if read_only {
            format!("{context} is now read-only in Skiff")
        } else {
            format!("Changes to {context} are allowed again")
        };
        self.toast(text, Tone::Normal);
    }

    /// Sets whether changes to a cluster are confirmed first, and saves it.
    pub fn set_confirm(&mut self, context: &str, confirm: bool) {
        self.edit_config(|c| c.set_confirm(context, confirm));
    }

    // ------------------------------------------------------------------
    // Kubeconfig
    // ------------------------------------------------------------------

    /// What the last kubeconfig load looked at.
    pub fn kubeconfig_report(&self) -> &KubeconfigReport {
        &self.kube
    }

    /// Takes over a freshly loaded kubeconfig: the context list, what was
    /// searched, and the files' state for change detection.
    fn take_kubeconfig(&mut self, loaded: LoadedKubeconfig) {
        let sources = self.core.kubeconfig_sources();
        self.kube_stamps = file_stamps(&sources);
        self.kube_generation = self.kube_generation.wrapping_add(1);
        // `kubectl` in a terminal must see the same clusters. The default
        // sources are what it reads anyway.
        let in_effect = (!sources.is_default())
            .then(|| std::env::join_paths(&loaded.files).ok())
            .flatten()
            .map(|list| list.to_string_lossy().into_owned());
        crate::external::set_kubeconfig(in_effect);
        self.contexts = loaded.contexts;
        self.kube = KubeconfigReport { searched: loaded.searched, files: loaded.files, problems: loaded.problems };
    }

    /// Reads the kubeconfig files again. Clusters that are open stay open
    /// as long as their context still exists; `announce` says what was
    /// found even when nothing changed (the user asked).
    pub fn reload_kubeconfig(&mut self, announce: bool) {
        self.reload_kubeconfig_saying(if announce { Announce::Always } else { Announce::Changes });
    }

    fn reload_kubeconfig_saying(&mut self, announce: Announce) {
        let before: Vec<String> = self.contexts.items.iter().map(|c| c.name.clone()).collect();
        let loaded = self.core.load_kubeconfig();
        self.take_kubeconfig(loaded);
        let now: Vec<String> = self.contexts.items.iter().map(|c| c.name.clone()).collect();
        let added = now.iter().filter(|n| !before.contains(n)).count();
        let removed = before.iter().filter(|n| !now.contains(n)).count();

        // A cluster whose context is gone cannot reconnect: close it,
        // unless that would throw away an edit.
        let editing = self.detail.as_ref().is_some_and(|d| d.has_unsaved_edit());
        let gone: Vec<String> = self.sessions.keys().filter(|n| !now.contains(n)).cloned().collect();
        for name in gone {
            let active = self.active.as_deref() == Some(name.as_str());
            if active && editing {
                continue;
            }
            self.disconnect(&name);
            if active {
                self.toast(format!("{name} is no longer in your kubeconfig"), Tone::Warn);
            }
        }
        // A failed connection may have been waiting for exactly this.
        for session in self.sessions.values() {
            if session.failure().is_some() {
                session.reconnect();
            }
        }
        // The first clusters to appear: open the kubeconfig's current one.
        if before.is_empty() && self.active.is_none() {
            let known = |name: &String| now.contains(name);
            if let Some(first) = self.contexts.current.clone().filter(known).or_else(|| now.first().cloned()) {
                self.switch_context(&first);
            }
        }

        let problems = self.kube.problems.len();
        let trouble = match problems {
            0 => String::new(),
            n => format!(" ({} can't be used)", widgets::plural(n, "file")),
        };
        if announce == Announce::Always {
            let tone = if problems > 0 { Tone::Warn } else { Tone::Normal };
            self.toast(format!("Kubeconfig reloaded: {}{trouble}", contexts_text(now.len())), tone);
        } else if announce == Announce::Changes && (added > 0 || removed > 0) {
            let mut parts = Vec::new();
            if added > 0 {
                parts.push(format!("{} added", contexts_text(added)));
            }
            if removed > 0 {
                parts.push(format!("{} removed", contexts_text(removed)));
            }
            self.toast(format!("Kubeconfig changed: {}", parts.join(", ")), Tone::Normal);
        }
    }

    /// Reloads the kubeconfig if one of its files was changed, added or
    /// removed since the last load (a `stat` per file; nothing is read
    /// otherwise). Returns whether it reloaded.
    pub fn reload_kubeconfig_if_changed(&mut self) -> bool {
        if file_stamps(&self.core.kubeconfig_sources()) == self.kube_stamps {
            return false;
        }
        self.reload_kubeconfig(false);
        true
    }

    /// Adds a kubeconfig file (or folder) to the ones Skiff reads, saves
    /// that in the config file and loads it. Refuses a file that has no
    /// usable context, saying why.
    pub fn add_kubeconfig(&mut self, path: &str) -> Result<(), String> {
        let entry = path.trim().to_string();
        if entry.is_empty() {
            return Err("Enter the path of a kubeconfig file.".into());
        }
        let alone = skiff_core::kubeconfig::load(&KubeconfigSources::explicit([entry.clone()]));
        if let Some(problem) = alone.problems.first() {
            return Err(format!("{} can't be used: {}", pages::short_path(&problem.path), problem.message));
        }
        if alone.files.is_empty() {
            return Err(format!("There is no file at {entry}."));
        }
        if alone.contexts.items.is_empty() {
            return Err(format!("{entry} has no contexts, so there is no cluster in it to show."));
        }
        // Added to what is read today, so that the clusters from
        // $KUBECONFIG / ~/.kube/config stay.
        let starts_list = self.config.kubeconfigs().is_none();
        let saved = kubeconfigs_with_added(self.config.kubeconfigs(), &entry);
        let in_effect = if self.cli_kubeconfigs { self.core.kubeconfig_sources().with_entry(entry.clone()) } else { saved.clone() };
        self.edit_config(|c| c.set_kubeconfigs(saved.entries()));
        self.core.set_kubeconfig_sources(in_effect);
        self.reload_kubeconfig_saying(Announce::Never);
        let n = alone.contexts.items.len();
        let mut text = format!("Added {} from {}", widgets::plural(n, "cluster"), pages::short_path(Path::new(&entry)));
        if starts_list && self.config.kubeconfigs().is_some() {
            // From now on the list decides, which is easy to miss.
            text.push_str(
                ". Skiff now reads the kubeconfig list in config.json: \"$KUBECONFIG\" (kubectl's own locations, still followed) and this file; see Settings > Clusters.",
            );
        }
        self.toast(text, Tone::Ok);
        Ok(())
    }

    /// Asks for a kubeconfig file: with the system's file dialog (which
    /// runs beside the UI, never blocking it), or by path where there is
    /// none.
    pub fn ask_for_kubeconfig(&mut self, ctx: &egui::Context) {
        if self.file_pick.is_some() {
            return;
        }
        if !self.native_file_dialog {
            self.ask_for_kubeconfig_path();
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new().set_title("Add kubeconfig file");
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        if let Some(dir) = home.map(|h| h.join(".kube")).filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        // Created here, on the UI thread (macOS wants that), and waited
        // for on the I/O runtime.
        let picking = dialog.pick_file();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        self.core.handle().spawn(async move {
            let picked = picking.await.map(|file| file.path().to_path_buf());
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
        self.file_pick = Some((rx, Instant::now()));
    }

    fn ask_for_kubeconfig_path(&mut self) {
        self.confirm = None;
        self.dialog = Some(Dialog::AddKubeconfig { path: String::new(), error: None, focused: false });
    }

    /// Takes the file dialog's answer, once there is one.
    fn poll_file_pick(&mut self) {
        let Some((rx, asked)) = &self.file_pick else { return };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        // "Nothing chosen" faster than anyone can cancel a dialog means
        // that none came up (no desktop portal answering): ask by path.
        let never_shown = answer.is_none() && asked.elapsed() < Duration::from_millis(400);
        self.file_pick = None;
        match answer {
            Some(path) => {
                if let Err(e) = self.add_kubeconfig(&path.to_string_lossy()) {
                    self.toast(e, Tone::Err);
                }
            }
            None if never_shown && self.dialog.is_none() => self.ask_for_kubeconfig_path(),
            None => {}
        }
    }

    // ------------------------------------------------------------------
    // Notices
    // ------------------------------------------------------------------

    /// Shows a standing message under the top bar until it is dismissed.
    pub fn notify(&mut self, notice: Notice) {
        match self.notices.iter_mut().find(|n| n.key == notice.key) {
            Some(existing) => *existing = notice,
            None => self.notices.push(notice),
        }
    }

    /// The standing messages currently shown.
    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }

    /// What the window is called: the app, and the cluster it shows.
    pub fn window_title(&self) -> String {
        match &self.active {
            Some(context) => format!("Skiff — {context}"),
            None => "Skiff".to_string(),
        }
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
        self.confirm = None;
        self.remember_namespaces();
        if !self.sessions.contains_key(name) {
            let session = self.core.connect(name);
            session.set_read_only(self.cluster_read_only(name));
            session.set_prometheus_endpoint(prometheus_endpoint(&self.config, name));
            self.sessions.insert(name.to_string(), session);
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
            self.confirm = None;
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

    /// The quick filter of the current table (for tests and diagnostics).
    pub fn table_filter(&self) -> String {
        let View::Resource(key) = &self.view else { return String::new() };
        self.tables.get(&key.id()).map(|t| t.filter.clone()).unwrap_or_default()
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

    /// Switches to the dark or the light theme (and saves that choice).
    pub fn set_theme(&mut self, _ctx: &egui::Context, dark: bool) {
        self.choose_theme(if dark { Theme::Dark } else { Theme::Light });
    }

    fn poll(&mut self) {
        self.poll_file_pick();
        let mut toasts = Vec::new();
        let sessions = &self.sessions;
        // A background apply that failed gets its dialog back, manifest and
        // error included: on the cluster it was meant for (never on another
        // one, where "Apply" would send it to the wrong place), and once no
        // dialog, palette or menu is in the way.
        let dialog_free =
            self.dialog.is_none() && self.confirm.is_none() && self.palette.is_none() && !self.overlay_at_start;
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

    /// Runs an action on an object, as chosen from a row's menu or the
    /// detail panel: refused on a read-only cluster, and asked about first
    /// on a cluster that is set to confirm changes.
    pub fn run_row_action(&mut self, ctx: &egui::Context, action: RowAction, key: &ResourceKey, ns: &str, name: &str) {
        let Some(session) = self.session().cloned() else { return };
        if let Some(reason) = self.locked_reason().filter(|_| action.mutates()) {
            self.toast(reason, Tone::Warn);
            return;
        }
        let question = match action {
            RowAction::Restart => Some(("Restart", "Its pods are replaced, a few at a time.")),
            RowAction::Cordon => Some(("Cordon", "No new pods will be scheduled on it.")),
            RowAction::Uncordon => Some(("Uncordon", "Pods can be scheduled on it again.")),
            RowAction::Suspend => Some(("Suspend", "No new jobs are started until it is resumed.")),
            RowAction::Resume => Some(("Resume", "Jobs are started on schedule again.")),
            _ => None,
        };
        if let (Some((verb, note)), true) = (question, self.asks_first()) {
            self.confirm = Some(Confirm {
                title: format!("{verb} {}?", key.kind),
                object: if ns.is_empty() { name.to_string() } else { format!("{ns}/{name}") },
                note: note.to_string(),
                verb: verb.to_string(),
                context: session.context.clone(),
                op: ConfirmOp::Row { action, key: key.clone(), ns: ns.to_string(), name: name.to_string() },
            });
            return;
        }
        self.do_row_action(ctx, action, key, ns, name);
    }

    /// [`SkiffApp::run_row_action`] without the questions.
    fn do_row_action(&mut self, ctx: &egui::Context, action: RowAction, key: &ResourceKey, ns: &str, name: &str) {
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
            RowAction::ShellExternal => {
                // kubectl picks the pod's default container.
                let inv = crate::external::exec_invocation(&session.context, ns, name, None);
                match crate::external::open_invocation(&inv) {
                    Ok(app) => self.toast(format!("Opened a shell in {app}"), Tone::Normal),
                    Err(e) => self.toast(e, Tone::Err),
                }
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
            RowAction::ShowPods => self.show_pods_of(key, name),
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

    /// Opens the pods table showing the pods on a node (by an exact match on
    /// the Node column) or in a namespace (by the namespace filter).
    pub fn show_pods_of(&mut self, key: &ResourceKey, name: &str) {
        let pods = kinds::pods();
        if self.blocked_by_edit() {
            return;
        }
        self.detail = None;
        self.view = View::Resource(pods.clone());
        let tui = self.tables.entry(pods.id()).or_default();
        tui.selected = None;
        if key.is("", "nodes") {
            tui.filter = format!("node:={name}");
        } else if key.is("", "namespaces") {
            tui.filter.clear();
            self.ns_filter = BTreeSet::from([name.to_string()]);
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

    /// Goes ahead with what a confirmation was asked for.
    fn run_confirmed(&mut self, ctx: &egui::Context, op: ConfirmOp) {
        let Some(session) = self.session().cloned() else { return };
        match op {
            ConfirmOp::Row { action, key, ns, name } => self.do_row_action(ctx, action, &key, &ns, &name),
            ConfirmOp::Scale { key, ns, name, replicas } => {
                self.pending.push(Pending::Simple {
                    job: session.scale(&key, &ns, &name, replicas),
                    ok: format!("Scaled {name} to {replicas}"),
                });
            }
            ConfirmOp::ApplyManifest { force } => {
                if let Some(Dialog::Create { text, context, job, error, .. }) = &mut self.dialog {
                    if *context == session.context && job.is_none() {
                        *error = None;
                        *job = Some(session.apply_manifest_with(text.clone(), force));
                    }
                }
            }
            ConfirmOp::ApplyEdit => {
                if let Some(detail) = &mut self.detail {
                    detail.apply_edit(&session);
                }
            }
        }
    }

    pub fn open_create_dialog(&mut self) {
        if let Some(reason) = self.locked_reason() {
            self.toast(reason, Tone::Warn);
            return;
        }
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
            Command::CheckForUpdates => {
                self.updater.enable();
                self.updater.check_now(ctx);
                self.update_check_asked = true;
            }
            Command::Reconnect => {
                if let Some(s) = self.session() {
                    s.reconnect();
                }
            }
            Command::ReloadKubeconfig => self.reload_kubeconfig(true),
            Command::AddKubeconfig => self.ask_for_kubeconfig(ctx),
            Command::SetReadOnly(on) => {
                if let Some(context) = self.active.clone() {
                    self.set_read_only(&context, on);
                }
            }
            Command::Settings(section) => self.open_settings(section),
            Command::Shortcuts => self.open_help(),
            Command::WhatsNew => self.open_whats_new(),
            Command::ZoomIn => self.zoom_by_step(true),
            Command::ZoomOut => self.zoom_by_step(false),
            Command::ZoomReset => {
                self.set_zoom(1.0);
                self.toast("Zoom 100%", Tone::Normal);
            }
            Command::OpenConfigFile => self.open_settings_file(false),
            Command::OpenKeysFile => self.open_settings_file(true),
        }
    }

    // ------------------------------------------------------------------
    // Layout
    // ------------------------------------------------------------------

    pub fn ui(&mut self, ui: &mut Ui) {
        let started = Instant::now();
        let ctx = ui.ctx().clone();
        self.tick = Duration::from_secs(30);
        // "System" follows the computer, also when it changes while
        // Skiff runs. (This frame's widgets take the new style at once.)
        if self.sync_theme() {
            ui.set_style(ctx.global_style());
        }
        self.updater.tick(&ctx);
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
        self.window_events(&ctx);
        self.shortcuts(&ctx);

        self.top_bar(ui);
        self.notices_ui(ui);
        self.status_bar(ui);
        // (With no cluster to look at, its list of resource types would
        // lead nowhere.)
        if !self.contexts.items.is_empty() || self.active.is_some() {
            self.sidebar(ui);
        }

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
        self.settings_ui(&ctx);
        self.help_ui(&ctx);
        self.whats_new_ui(&ctx);
        self.dialog_ui(&ctx);
        self.confirm_ui(&ctx);
        self.guard_clipboard(&ctx);
        self.toasts_ui(&ctx);

        // Background changes wake us through the notifier. Otherwise repaint
        // only as often as the relative times on screen can change, so an
        // idle window costs nothing.
        ctx.request_repaint_after(self.tick);
        self.pointer_used = ctx.input(|i| i.pointer.any_down() || i.pointer.any_pressed() || i.pointer.any_released());
        self.last_ui = started.elapsed();
        self.trace_startup(&ctx);
    }

    /// What happens to the window as a whole: its title, coming back to
    /// it, and files dropped onto it.
    fn window_events(&mut self, ctx: &egui::Context) {
        let title = self.window_title();
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        // Back from the terminal, where `aws eks update-kubeconfig` or a
        // login just ran: pick up what changed. Checked only at that
        // moment, and by modification time, so it costs nothing.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.window_focused {
            self.reload_kubeconfig_if_changed();
            self.reload_settings_if_changed();
        }
        self.window_focused = focused;

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        for path in dropped {
            match self.add_kubeconfig(&path.to_string_lossy()) {
                Ok(()) => {
                    if matches!(self.dialog, Some(Dialog::AddKubeconfig { .. })) {
                        self.dialog = None;
                    }
                }
                Err(e) => self.toast(e, Tone::Err),
            }
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let pal = self.pal;
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("file-drop")));
            painter.rect_filled(screen, CornerRadius::ZERO, pal.tint(pal.bg, 0.86));
            painter.rect_stroke(screen.shrink(12.0), CornerRadius::same(12), Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "Drop to add this kubeconfig",
                FontId::new(18.0, theme::semibold()),
                pal.text,
            );
        }
    }

    /// The standing messages, between the top bar and everything else.
    fn notices_ui(&mut self, ui: &mut Ui) {
        if self.notices.is_empty() {
            return;
        }
        let pal = self.pal;
        let mut dismiss: Option<usize> = None;
        let mut act: Option<NoticeAction> = None;
        Panel::top("notices")
            .resizable(false)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin { left: 10, right: 10, top: 0, bottom: 8 }))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for (i, notice) in self.notices.iter().enumerate() {
                    let color = if notice.tone == Tone::Normal { pal.accent } else { pal.tone(notice.tone) };
                    let icon = if notice.tone == Tone::Err { icons::ICON_ERROR } else if notice.tone == Tone::Warn { icons::ICON_WARNING } else { icons::ICON_INFO };
                    Frame::new()
                        .fill(pal.tint(color, 0.10))
                        .stroke(Stroke::new(1.0, pal.tint(color, 0.35)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                ui.label(widgets::icon(icon, 16.0).color(color));
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    let close = egui::Button::new(widgets::icon(icons::ICON_CLOSE, 15.0).color(pal.muted))
                                        .frame_when_inactive(false)
                                        .min_size(Vec2::new(24.0, 22.0));
                                    if widgets::named(ui.add(close), "Dismiss").clicked() {
                                        dismiss = Some(i);
                                    }
                                    if let Some((label, action)) = &notice.action {
                                        if ui.add(egui::Button::new(label.as_str()).min_size(Vec2::new(0.0, 22.0))).clicked() {
                                            act = Some(action.clone());
                                        }
                                    }
                                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.add(egui::Label::new(widgets::semibold(&notice.title, 13.0).color(pal.text)).wrap());
                                        for line in &notice.lines {
                                            ui.add(
                                                egui::Label::new(RichText::new(line).color(pal.muted).size(12.5))
                                                    .wrap()
                                                    .selectable(true),
                                            );
                                        }
                                    });
                                });
                            });
                        });
                }
            });
        if let Some(NoticeAction::OpenUrl(url)) = act {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
        if let Some(i) = dismiss {
            self.notices.remove(i);
        }
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
        if ctx.input_mut(|i| i.consume_shortcut(&open_palette)) && self.dialog.is_none() && self.confirm.is_none() {
            self.palette = match self.palette {
                Some(_) => None,
                None => Some(PaletteState::default()),
            };
            if self.palette.is_some() {
                self.settings = None;
                self.help_open = false;
                self.whats_new = None;
                self.hint_seen = true;
            }
        }
        // Zoom, from anywhere but a shell on Linux and Windows (where
        // Ctrl+- and friends are the remote program's).
        if !in_shell {
            let pressed = |ctx: &egui::Context, key: Key| {
                ctx.input_mut(|i| {
                    i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key))
                        || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key))
                })
            };
            if pressed(ctx, Key::Equals) || pressed(ctx, Key::Plus) {
                self.zoom_by_step(true);
            } else if pressed(ctx, Key::Minus) {
                self.zoom_by_step(false);
            } else if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Num0))) {
                self.set_zoom(1.0);
                self.toast("Zoom 100%", Tone::Normal);
            }
        }
        let open_settings = KeyboardShortcut::new(Modifiers::COMMAND, Key::Comma);
        if ctx.input_mut(|i| i.consume_shortcut(&open_settings)) && self.dialog.is_none() && self.confirm.is_none() {
            match self.settings {
                Some(_) => self.settings = None,
                None => self.open_settings(Section::default()),
            }
        }
        // Under a window, dialog or menu nothing is found in: the find
        // shortcuts are taken, so that the dock below does not open its
        // find box behind it.
        if self.overlay_open(ctx) {
            let shell_find = crate::shell::find_shortcut(ctx);
            ctx.input_mut(|i| {
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::F));
                i.consume_shortcut(&shell_find);
            });
        }
        // Keys belong to whatever is on top: a text field, the terminal, a
        // dialog, the palette, or an open menu / popup.
        let busy = self.overlay_open(ctx) || self.terminal_focused || ctx.text_edit_focused();
        if busy {
            return;
        }
        if ctx.input(|i| self.keymap.pressed(i, Action::Help)) {
            // (Taken, so that the list does not see it as "close".)
            let keymap = &self.keymap;
            ctx.input_mut(|i| i.events.retain(|e| !keymap.is_event(e, Action::Help)));
            self.open_help();
            return;
        }
        // A click outside the sidebar takes the keyboard back to the table.
        if self.nav_focus == NavFocus::Sidebar {
            let elsewhere = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.interact_pos().is_some_and(|p| !self.sidebar_rect.is_some_and(|r| r.contains(p)))
            });
            if elsewhere || !self.sidebar_open {
                self.nav_focus = NavFocus::Main;
            }
        }
        // Cmd/Ctrl+F finds in the table, unless the user is working in the
        // dock (logs, a shell), which then takes it. A keys.json binding
        // on the same keys wins: it is the user's own choice.
        let find = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
        if matches!(self.view, View::Resource(_))
            && self.session().is_some()
            && !self.dock.has_attention()
            && !ctx.input(|i| self.keymap.claims(i, Key::F))
            && ctx.input_mut(|i| i.consume_shortcut(&find))
        {
            self.focus_filter = true;
            self.nav_focus = NavFocus::Main;
        }
        // Only where a filter box is actually drawn.
        if ctx.input(|i| self.keymap.pressed(i, Action::Filter))
            && matches!(self.view, View::Resource(_))
            && self.session().is_some()
        {
            self.focus_filter = true;
            self.nav_focus = NavFocus::Main;
            // Don't let the key itself land in the filter box.
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(_))));
        }
        if self.nav_focus == NavFocus::Sidebar {
            self.sidebar_keys(ctx);
            return;
        }
        if ctx.input(|i| self.keymap.pressed(i, Action::FocusLeft)) {
            self.nav_focus = NavFocus::Sidebar;
            self.sidebar_open = true;
            self.sidebar_cursor = Some(self.current_nav_item());
            self.sidebar_scroll = true;
            return;
        }
        if let Some(d) = self.detail.as_mut() {
            let tabs = d.tabs();
            let at = tabs.iter().position(|t| *t == d.tab).unwrap_or(0);
            let step = ctx.input(|i| {
                if self.keymap.pressed(i, Action::NextTab) {
                    Some(1)
                } else if self.keymap.pressed(i, Action::PrevTab) {
                    Some(tabs.len() - 1)
                } else {
                    None
                }
            });
            // An edit in progress keeps the YAML tab.
            if let Some(step) = step.filter(|_| !d.has_unsaved_edit()) {
                d.tab = tabs[(at + step) % tabs.len()];
            }
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

    /// The sidebar entry for the current view.
    fn current_nav_item(&self) -> NavItem {
        NavItem::View(self.view.clone())
    }

    /// Whether a custom resource group is expanded in the sidebar.
    fn group_open(&self, group: &str, keys: &[ResourceKey]) -> bool {
        let has_current = matches!(&self.view, View::Resource(k) if keys.contains(k));
        *self.nav_open.get(group).unwrap_or(&false) || has_current
    }

    /// Sidebar entries in the order they are drawn.
    fn nav_items(&self) -> Vec<NavItem> {
        let mut items = vec![NavItem::View(View::Overview)];
        for cat in Category::ALL {
            items.extend(BUILTINS.iter().filter(|b| b.category == cat).map(|b| NavItem::View(View::Resource(b.key()))));
            if cat == Category::Custom {
                if let Some(found) = self.session().map(|s| s.discovered()) {
                    for (group, keys) in custom_group_list(&found) {
                        let open = self.group_open(&group, &keys);
                        items.push(NavItem::Group(group));
                        if open {
                            items.extend(keys.into_iter().map(|k| NavItem::View(View::Resource(k))));
                        }
                    }
                }
            }
        }
        items
    }

    /// Keyboard navigation while the sidebar has focus.
    fn sidebar_keys(&mut self, ctx: &egui::Context) {
        let items = self.nav_items();
        if items.is_empty() {
            return;
        }
        let km = &self.keymap;
        let (step, open, back) = ctx.input(|i| {
            let step: Option<i64> = if km.pressed(i, Action::Down) {
                Some(1)
            } else if km.pressed(i, Action::Up) {
                Some(-1)
            } else if km.pressed(i, Action::PageDown) {
                Some(10)
            } else if km.pressed(i, Action::PageUp) {
                Some(-10)
            } else if km.pressed(i, Action::Top) {
                Some(i64::MIN / 2)
            } else if km.pressed(i, Action::Bottom) {
                Some(i64::MAX / 2)
            } else {
                None
            };
            let open = km.pressed(i, Action::Open) || km.pressed(i, Action::FocusRight);
            (step, open, i.key_pressed(Key::Escape))
        });
        let cur = self.sidebar_cursor.clone().unwrap_or_else(|| self.current_nav_item());
        let at = items.iter().position(|it| *it == cur).unwrap_or(0);
        if let Some(step) = step {
            let next = (at as i64).saturating_add(step).clamp(0, items.len() as i64 - 1) as usize;
            self.sidebar_cursor = Some(items[next].clone());
            self.sidebar_scroll = true;
        }
        if open {
            match items.get(at).cloned() {
                Some(NavItem::Group(g)) => {
                    let keys = self
                        .session()
                        .map(|s| custom_group_list(&s.discovered()))
                        .and_then(|gs| gs.into_iter().find(|(n, _)| *n == g))
                        .map(|(_, k)| k)
                        .unwrap_or_default();
                    let open = self.group_open(&g, &keys);
                    self.nav_open.insert(g, !open);
                }
                Some(NavItem::View(v)) => {
                    self.go(v);
                    self.nav_focus = NavFocus::Main;
                }
                None => {}
            }
        } else if back {
            self.nav_focus = NavFocus::Main;
        }
    }

    /// Whether a dialog, the palette or any menu / popup is open.
    fn overlay_open(&self, ctx: &egui::Context) -> bool {
        self.palette.is_some()
            || self.dialog.is_some()
            || self.confirm.is_some()
            || self.settings.is_some()
            || self.help_open
            || self.whats_new.is_some()
            || egui::Popup::is_any_open(ctx)
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        Panel::top("top-bar")
            .exact_size(TOP_BAR_HEIGHT)
            .resizable(false)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(10, 0)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    // (No sidebar on the welcome page, so nothing to toggle.)
                    let has_sidebar = !self.contexts.items.is_empty() || self.active.is_some();
                    if has_sidebar && widgets::icon_button(ui, icons::ICON_MENU, "Toggle sidebar").clicked() {
                        self.sidebar_open = !self.sidebar_open;
                    }
                    self.context_switcher(ui);
                    self.cluster_badges(ui);
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
                        let comma = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
                        if widgets::icon_button(ui, icons::ICON_SETTINGS, &format!("Settings  ({comma} ,)")).clicked() {
                            self.open_settings(Section::default());
                        }
                        let theme_icon = if pal.dark { icons::ICON_LIGHT_MODE } else { icons::ICON_DARK_MODE };
                        if widgets::icon_button(ui, theme_icon, "Toggle light / dark theme").clicked() {
                            let dark = !self.pal.dark;
                            self.set_theme(ui.ctx(), dark);
                        }
                        if self.session().is_some() {
                            let locked = self.locked_reason();
                            let add = ui.add_enabled_ui(locked.is_none(), |ui| {
                                widgets::icon_button(ui, icons::ICON_ADD, "Create resource from YAML")
                            });
                            match locked {
                                Some(reason) => {
                                    add.inner.on_disabled_hover_text(reason);
                                }
                                None if add.inner.clicked() => self.open_create_dialog(),
                                None => {}
                            }
                        }
                        self.update_button(ui);
                        self.connection_badge(ui);
                    });
                });
            });
    }

    fn update_button(&mut self, ui: &mut Ui) {
        use crate::update::{self, State};
        let pal = self.pal;
        let ctx = ui.ctx().clone();
        let state = self.updater.state();
        if self.update_check_asked {
            match &state {
                State::UpToDate => {
                    self.update_check_asked = false;
                    let build = match update::build_number() {
                        0 => String::new(),
                        n => format!(", build {n}"),
                    };
                    self.toast(format!("Skiff is up to date ({}{build})", crate::update::current_version()), Tone::Ok);
                }
                State::Failed(e) => {
                    self.update_check_asked = false;
                    self.toast(format!("Couldn't check for updates: {e}"), Tone::Err);
                }
                State::Available(_) => self.update_check_asked = false,
                _ => {}
            }
        }
        let accent_with = |ic: MaterialIcon, text: &str| {
            egui::Button::new((widgets::icon(ic, 16.0).color(pal.bg), RichText::new(text).color(pal.bg)))
                .fill(pal.accent)
                .min_size(Vec2::new(0.0, 28.0))
        };
        let accent = |text: &str| accent_with(icons::ICON_SYSTEM_UPDATE_ALT, text);
        // A failed update comes with advice ("Move Skiff to Applications
        // to enable updates"): say it where it is seen, once.
        match &state {
            State::DownloadFailed(r, e) => {
                if self.update_failure_told.as_deref() != Some(e.as_str()) {
                    self.update_failure_told = Some(e.clone());
                    self.notify(Notice {
                        key: "update".into(),
                        tone: Tone::Warn,
                        title: format!("{} could not be installed", r.title),
                        lines: vec![e.clone()],
                        action: Some(("Open release page".into(), NoticeAction::OpenUrl(r.page.clone()))),
                    });
                }
            }
            State::Downloading(_) | State::Ready { .. } => {
                self.update_failure_told = None;
                self.notices.retain(|n| n.key != "update");
            }
            _ => {}
        }
        match state {
            State::Available(r) | State::DownloadFailed(r, _) => {
                let failed = match self.updater.state() {
                    State::DownloadFailed(_, e) => Some(e),
                    _ => None,
                };
                let mut tip = format!("{} is available (you have build {}).", r.title, update::build_number());
                let notes = crate::whats_new::notes_summary(&r.notes, 14);
                if !notes.is_empty() {
                    tip.push_str(&format!("\n\n{notes}"));
                }
                if let Some(e) = &failed {
                    tip.push_str(&format!("\n\nLast attempt failed: {e}"));
                }
                if update::can_install() {
                    let label = if failed.is_some() { "Retry update" } else { "Update" };
                    if ui.add(accent(label)).on_hover_text(tip).clicked() {
                        self.updater.download(&ctx, r);
                    }
                } else {
                    // This copy cannot replace itself (Windows, Linux, or
                    // not installed as an app): the release page has the
                    // download.
                    tip.push_str("\n\nOpens the release page, where the new version can be downloaded.");
                    if ui.add(accent_with(icons::ICON_OPEN_IN_NEW, "New version")).on_hover_text(tip).clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(r.page));
                    }
                }
            }
            State::Downloading(_) => {
                ui.label(RichText::new("Downloading update…").color(pal.muted).size(12.0));
                ui.spinner();
            }
            State::Ready { release, .. } => {
                let resp = ui
                    .add(accent("Restart to update"))
                    .on_hover_text(format!("{} is downloaded. Skiff restarts into it.", release.title));
                if resp.clicked() && !self.blocked_by_edit() {
                    match self.updater.apply() {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.toast(e, Tone::Err),
                    }
                }
            }
            State::Idle | State::Checking | State::UpToDate | State::Failed(_) => {}
        }
    }

    fn connection_badge(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let Some(session) = self.session().cloned() else { return };
        match &*session.status() {
            ConnStatus::Connecting => {
                let what = session.connecting_detail().unwrap_or_else(|| "Connecting…".to_string());
                ui.label(RichText::new(what).color(pal.muted));
                ui.spinner();
            }
            ConnStatus::Connected { version } => {
                ui.label(RichText::new(format!("Kubernetes {version}")).color(pal.muted).size(12.0))
                    .on_hover_text("The cluster's Kubernetes version (its API server). Skiff's own version is in Settings, About.");
                widgets::dot(ui, pal.ok);
            }
            ConnStatus::Failed(err) => {
                let info = session.failure();
                let tip = match &info {
                    Some(info) => match &info.hint {
                        Some(hint) => format!("{}\n{hint}", info.summary),
                        None => info.summary.clone(),
                    },
                    None => err.clone(),
                };
                let resp = ui
                    .add(egui::Button::new((
                        widgets::icon(icons::ICON_REFRESH, 15.0).color(pal.err),
                        RichText::new("Reconnect").color(pal.err),
                    )))
                    .on_hover_text(&tip);
                if resp.clicked() {
                    session.reconnect();
                }
                // What went wrong, as far as there is room for it.
                let summary = info.map(|i| i.summary).unwrap_or_else(|| err.clone());
                let room = (ui.available_width() - 16.0).min(300.0);
                if room > 80.0 {
                    let mut job = egui::text::LayoutJob::simple_singleline(summary, FontId::proportional(12.0), pal.err);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
                    ui.add(egui::Label::new(job)).on_hover_text(tip);
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
        let mut read_only: Option<bool> = None;
        let mut ask_first: Option<bool> = None;
        let (mut add_file, mut reload, mut manage) = (false, false, false);
        // (Clusters hidden in Settings are left out.)
        let shown: Vec<&skiff_core::ContextInfo> = self.contexts.items.iter().filter(|c| self.in_menu(&c.name)).collect();
        let hidden = self.contexts.items.len() - shown.len();
        // The longest context name decides the width, within limits. The
        // width is fixed for the whole menu: rows that size themselves from
        // the space left over would make the popup grow on every frame.
        let longest = shown
            .iter()
            .map(|c| {
                ui.painter()
                    .layout_no_wrap(c.name.clone(), FontId::proportional(13.0), pal.text)
                    .size()
                    .x
            })
            .fold(0.0_f32, f32::max);
        let menu_width = (longest + 130.0).clamp(280.0, 520.0);
        egui::Popup::menu(&resp)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(menu_width);
                widgets::caption(ui, "Clusters", &pal);
                if self.contexts.items.is_empty() {
                    ui.label(RichText::new("No clusters yet. Add a kubeconfig file to see its clusters.").color(pal.muted));
                }
                if !self.kube.problems.is_empty() && !self.contexts.items.is_empty() {
                    let text = format!("{} can't be used", widgets::plural(self.kube.problems.len(), "kubeconfig file"));
                    let details: Vec<String> = self.kube.problems.iter().map(|p| p.to_string()).collect();
                    ui.horizontal(|ui| {
                        ui.label(widgets::icon(icons::ICON_WARNING, 14.0).color(pal.warn));
                        ui.label(RichText::new(text).color(pal.warn).size(12.0));
                    })
                    .response
                    .on_hover_text(details.join("\n"));
                }
                egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let row_width = ui.available_width();
                    for c in &shown {
                        let is_active = self.active.as_deref() == Some(c.name.as_str());
                        let session = self.sessions.get(&c.name);
                        let connected = session.map(|s| s.status());
                        let dot_color = match connected.as_deref() {
                            Some(ConnStatus::Connected { .. }) => pal.ok,
                            Some(ConnStatus::Connecting) => pal.warn,
                            Some(ConnStatus::Failed(_)) => pal.err,
                            None => pal.faint,
                        };
                        // The same words as the top bar and the page.
                        let status = match connected.as_deref() {
                            Some(ConnStatus::Connected { .. }) => "Connected".to_string(),
                            Some(ConnStatus::Connecting) => session
                                .and_then(|s| s.connecting_detail())
                                .unwrap_or_else(|| "Connecting…".to_string()),
                            Some(ConnStatus::Failed(err)) => match session.and_then(|s| s.failure()) {
                                Some(info) => match info.hint {
                                    Some(hint) => format!("{}\n{hint}", info.summary),
                                    None => info.summary,
                                },
                                None => err.clone(),
                            },
                            None => "Not connected".to_string(),
                        };
                        let locked = self.cluster_read_only(&c.name);
                        let mut marks = Vec::new();
                        let mut tip = format!("{}\n{}", c.server, status);
                        if c.insecure {
                            marks.push((icons::ICON_GPP_BAD, pal.warn));
                            tip.push_str("\nTLS certificate is not verified");
                        }
                        if locked {
                            marks.push((icons::ICON_LOCK, pal.muted));
                            tip.push_str("\nRead-only in Skiff");
                        }
                        let r = cluster_row(ui, &pal, row_width, &c.name, is_active, dot_color, connected.is_some(), &marks)
                            .on_hover_text(tip);
                        if connected.is_some() {
                            // Drawn over the row's right end (and so
                            // clicked instead of it).
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(r.rect.right() - 32.0, r.rect.top() + 2.0),
                                Vec2::new(28.0, r.rect.height() - 4.0),
                            );
                            let off = ui.put(
                                rect,
                                egui::Button::new(widgets::icon(icons::ICON_POWER_SETTINGS_NEW, 16.0).color(pal.muted))
                                    .frame_when_inactive(false),
                            );
                            if widgets::named(off, "Disconnect").clicked() {
                                disconnect = Some(c.name.clone());
                                continue;
                            }
                        }
                        if r.clicked() {
                            switch_to = Some(c.name.clone());
                            ui.close();
                        }
                    }
                });
                // Protection of the cluster that is open.
                if let Some(active) = &self.active {
                    let cluster = self.config.cluster(active);
                    let locked = self.cluster_read_only(active);
                    ui.separator();
                    widgets::caption(ui, "Protection", &pal);
                    if menu_toggle(ui, &pal, locked, "Read-only", true)
                        .on_hover_text(format!(
                            "Skiff will not change anything in {active}: no edits, deletes or scaling, and no new shells or port forwards. \
                             Shells and port forwards that are already open keep running until you close them. Saved in the config file."
                        ))
                        .clicked()
                    {
                        read_only = Some(!locked);
                    }
                    // (Nothing to ask about while nothing can be changed.)
                    if menu_toggle(ui, &pal, cluster.confirm, "Ask before changes", !locked)
                        .on_hover_text(format!(
                            "Restart, scale, cordon, suspend and applying YAML in {active} ask for confirmation first. Deleting always asks."
                        ))
                        .clicked()
                    {
                        ask_first = Some(!cluster.confirm);
                    }
                }
                ui.separator();
                let item = |ui: &mut Ui, ic: MaterialIcon, label: &str| {
                    let atoms = (widgets::icon(ic, 15.0).color(pal.muted), RichText::new(label).color(pal.text));
                    ui.add(egui::Button::new(atoms).frame(false))
                };
                if item(ui, icons::ICON_FILE_OPEN, "Add kubeconfig file…").clicked() {
                    add_file = true;
                    ui.close();
                }
                if item(ui, icons::ICON_SYNC, "Reload kubeconfig")
                    .on_hover_text("Read the kubeconfig files again. Skiff also does this when you come back to its window and a file has changed.")
                    .clicked()
                {
                    reload = true;
                    ui.close();
                }
                let label = match hidden {
                    0 => "Manage clusters…".to_string(),
                    n => format!("Manage clusters… ({n} hidden)"),
                };
                if item(ui, icons::ICON_SETTINGS, &label)
                    .on_hover_text("Settings, Clusters: kubeconfig files, hiding clusters from this menu, protection")
                    .clicked()
                {
                    manage = true;
                    ui.close();
                }
            });
        if let Some(c) = switch_to {
            self.switch_context(&c);
        }
        if let Some(c) = disconnect {
            self.disconnect(&c);
        }
        if let (Some(on), Some(active)) = (read_only, self.active.clone()) {
            self.set_read_only(&active, on);
        }
        if let (Some(on), Some(active)) = (ask_first, self.active.clone()) {
            self.set_confirm(&active, on);
        }
        if add_file {
            self.ask_for_kubeconfig(ui.ctx());
        }
        if reload {
            self.reload_kubeconfig(true);
        }
        if manage {
            self.open_settings(Section::Clusters);
        }
    }

    /// Standing conditions of the current cluster, next to its name.
    fn cluster_badges(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let Some(active) = self.active.clone() else { return };
        if self.read_only() {
            widgets::badge(ui, icons::ICON_LOCK, "Read-only", pal.info, &pal).on_hover_text(format!(
                "Skiff will not change anything in {active}: editing, deleting, scaling, new shells and new port forwards are off. \
                 Shells and port forwards that are already open keep running until you close them. \
                 Turn this off in the cluster menu."
            ));
        }
        if let Some(info) = self.contexts.items.iter().find(|c| c.name == active).filter(|c| c.insecure) {
            let file = info.file.as_deref().map(pages::short_path).unwrap_or_else(|| "your kubeconfig".to_string());
            widgets::badge(ui, icons::ICON_GPP_BAD, "TLS not verified", pal.warn, &pal).on_hover_text(format!(
                "This context sets insecure-skip-tls-verify, so the server's certificate is not checked: \
                 someone on the network could read or change what is sent, credentials included. \
                 Remove that setting from {file} once the cluster has a certificate your computer trusts."
            ));
        }
        // Firing alerts, once the overview or a Metrics tab has found the
        // cluster's Prometheus (the badge itself never goes looking).
        let alerts = self.session().map(|s| s.alerts()).unwrap_or_default();
        let (critical, warning) = alerts.counts();
        if critical + warning > 0 {
            let color = if critical > 0 { pal.err } else { pal.warn };
            let text = widgets::plural(critical + warning, "alert");
            let mut parts = Vec::new();
            if critical > 0 {
                parts.push(format!("{critical} critical"));
            }
            if warning > 0 {
                parts.push(widgets::plural(warning, "warning"));
            }
            let badge = widgets::badge(ui, icons::ICON_NOTIFICATIONS_ACTIVE, &text, color, &pal)
                .interact(egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(format!("Firing in {active}: {}. Click to see them on the overview.", parts.join(", ")));
            if badge.clicked() {
                self.go(View::Overview);
            }
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
        let cursor = (self.nav_focus == NavFocus::Sidebar).then(|| self.sidebar_cursor.clone()).flatten();
        let scroll = std::mem::take(&mut self.sidebar_scroll);
        let mark = |resp: &Response, item: &NavItem| {
            if scroll && cursor.as_ref() == Some(item) {
                resp.scroll_to_me(None);
            }
        };
        let shown = Panel::left("sidebar")
            .resizable(true)
            .default_size(214.0)
            .size_range(168.0..=340.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin { left: 8, right: 4, top: 8, bottom: 0 }))
            .show_collapsible(ui, &mut open, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    let item = NavItem::View(View::Overview);
                    let r = nav_row(
                        ui,
                        &pal,
                        icons::ICON_DASHBOARD,
                        "Overview",
                        self.view == View::Overview,
                        0.0,
                        cursor.as_ref() == Some(&item),
                    );
                    mark(&r, &item);
                    if r.clicked() {
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
                            let item = NavItem::View(View::Resource(key.clone()));
                            let r = nav_row(
                                ui,
                                &pal,
                                kind_icon(&key),
                                title,
                                selected,
                                0.0,
                                cursor.as_ref() == Some(&item),
                            );
                            mark(&r, &item);
                            if r.clicked() {
                                go = Some(View::Resource(key));
                            }
                        }
                        if cat == Category::Custom {
                            if let Some(found) = &discovered {
                                self.custom_groups(ui, found, &mut go, cursor.as_ref(), scroll);
                            }
                        }
                    }
                    ui.add_space(12.0);
                });
            });
        self.sidebar_rect = shown.map(|s| s.response.rect);
        self.sidebar_open = open;
        if let Some(v) = go {
            self.go(v);
            self.nav_focus = NavFocus::Main;
        }
    }

    /// Everything discovery found that has no fixed place, grouped by API group.
    fn custom_groups(
        &mut self,
        ui: &mut Ui,
        found: &[skiff_core::Discovered],
        go: &mut Option<View>,
        cursor: Option<&NavItem>,
        scroll: bool,
    ) {
        let pal = self.pal;
        let mark = |resp: &Response, item: &NavItem| {
            if scroll && cursor == Some(item) {
                resp.scroll_to_me(None);
            }
        };
        for (group, keys) in custom_group_list(found) {
            let open = self.group_open(&group, &keys);
            let chevron = if open { icons::ICON_EXPAND_MORE } else { icons::ICON_CHEVRON_RIGHT };
            let item = NavItem::Group(group.clone());
            let r = nav_row(ui, &pal, chevron, &group, false, 0.0, cursor == Some(&item));
            mark(&r, &item);
            if r.clicked() {
                self.nav_open.insert(group.clone(), !open);
            }
            if open {
                for key in keys {
                    let selected = matches!(&self.view, View::Resource(k) if k == &key);
                    let item = NavItem::View(View::Resource(key.clone()));
                    let r = nav_row(ui, &pal, kind_icon(&key), &key.kind, selected, 12.0, cursor == Some(&item));
                    mark(&r, &item);
                    if r.clicked() {
                        *go = Some(View::Resource(key));
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
        if self.connection_page(ui, &session, true) {
            return;
        }
        self.tick = self.tick.min(Duration::from_secs(5));
        self.first_run_hint(ui);
        // An account confined to a namespace is refused the cluster-wide
        // lists: the overview then covers the one selected namespace.
        let (_, pods, limited_pods) = self.scoped_table(&session, &kinds::pods());
        let (_, deploys, limited_deploys) = self.scoped_table(&session, &overview::deployments_key());
        let (_, events, limited_events) = self.scoped_table(&session, &kinds::events());
        let data = overview::Data {
            nodes: session.table(&kinds::nodes()).snapshot(),
            pods,
            deploys,
            events,
            limited: limited_pods || limited_deploys || limited_events,
            home_namespace: self
                .contexts
                .items
                .iter()
                .find(|c| c.name == session.context)
                .and_then(|c| c.namespace.clone())
                .filter(|ns| !ns.is_empty()),
        };
        if let Some(action) = overview::show(ui, &self.pal, &session, &self.ns_filter, &data) {
            match action {
                overview::Action::Go(key) => self.go(View::Resource(key)),
                overview::Action::Open { key, ns, name } => self.open_object(&key, &ns, &name),
                overview::Action::UseNamespace(ns) => self.ns_filter = BTreeSet::from([ns]),
            }
        }
    }

    /// The table to show for a resource type, with its latest snapshot.
    ///
    /// Accounts confined to a namespace may not list cluster-wide. With a
    /// single namespace selected, that namespace is watched instead. The
    /// refusal is remembered: the cluster-wide table keeps retrying in the
    /// background and briefly reports other states while it does. The
    /// last value says whether the cluster-wide list is refused.
    fn scoped_table(&mut self, session: &Arc<Session>, key: &ResourceKey) -> (Arc<TableStore>, Arc<Snapshot>, bool) {
        let mut store = session.table(key);
        let mut latest = store.snapshot();
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
        (store, latest, forbidden)
    }

    /// While the cluster cannot be reached, draws the page that says so in
    /// place of the view and returns true. `connecting` also covers the
    /// time before the first answer.
    fn connection_page(&mut self, ui: &mut Ui, session: &Arc<Session>, connecting: bool) -> bool {
        let pal = self.pal;
        if let Some(info) = session.failure() {
            let context = self.contexts.items.iter().find(|c| c.name == session.context);
            let server = context.map(|c| c.server.clone()).unwrap_or_default();
            match pages::connection_failed(ui, &pal, &session.context, &server, &info) {
                Some(pages::FailureAction::Retry) => session.reconnect(),
                Some(pages::FailureAction::CopyDetails) => {
                    let file = context.and_then(|c| c.file.as_deref());
                    let plugin = context.and_then(|c| c.exec_command.as_deref());
                    ui.ctx().copy_text(pages::failure_details(&session.context, &server, file, plugin, &info));
                    self.toast("Copied the connection details", Tone::Normal);
                }
                None => {}
            }
            return true;
        }
        if connecting && matches!(&*session.status(), ConnStatus::Connecting) {
            pages::connecting(ui, &pal, &session.context, session.connecting_detail().as_deref());
            return true;
        }
        false
    }

    fn no_cluster(&mut self, ui: &mut Ui) {
        if self.contexts.items.is_empty() {
            // The first minute: nothing to show yet, so say where Skiff
            // looked and how to go on.
            let picking = self.file_pick.is_some();
            match pages::welcome(ui, &self.pal, &self.kube, picking) {
                Some(pages::WelcomeAction::AddFile) => self.ask_for_kubeconfig(ui.ctx()),
                Some(pages::WelcomeAction::Reload) => self.reload_kubeconfig(true),
                None => {}
            }
            return;
        }
        widgets::empty_state(
            ui,
            icons::ICON_HUB,
            "No cluster selected",
            "Pick a cluster from the switcher in the top bar.",
            &self.pal,
        );
    }

    fn resource_view(&mut self, ui: &mut Ui, key: &ResourceKey) {
        let pal = self.pal;
        let Some(session) = self.session().cloned() else {
            self.no_cluster(ui);
            return;
        };
        if self.connection_page(ui, &session, false) {
            return;
        }
        let ctx = ui.ctx().clone();
        let locked = self.locked_reason();
        let (store, latest, _) = self.scoped_table(&session, key);
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
                        ui.separator();
                        if widgets::action_button(ui, icons::ICON_RESTART_ALT, "Reset columns")
                            .on_hover_text("Default columns and widths")
                            .clicked()
                        {
                            tui.reset_columns();
                        }
                    });
                let id = egui::Id::unique("table-filter");
                let filter = widgets::search_box(ui, &mut tui.filter, "Filter  ( / )", 240.0, id).on_hover_text(
                    "Words match anywhere in a row. Column filters: node:worker-1, status:crash, ns:kube, label:app=web\n\
                     column:=value matches the whole value; !word or !column:value leaves matches out.",
                );
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
                    if matches!(&*session.status(), ConnStatus::Connecting) {
                        pages::connecting(ui, &pal, &session.context, session.connecting_detail().as_deref());
                    } else {
                        widgets::empty_state(ui, icons::ICON_SYNC, "Loading…", "", &pal);
                    }
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
        let typing = ctx.text_edit_focused()
            || overlay
            || self.terminal_focused
            || editing
            || self.nav_focus != NavFocus::Main;
        let mut open_selected = false;
        if !typing {
            let km = &self.keymap;
            let step: Option<i64> = ctx.input(|i| {
                if km.pressed(i, Action::Down) {
                    Some(1)
                } else if km.pressed(i, Action::Up) {
                    Some(-1)
                } else if km.pressed(i, Action::PageDown) {
                    Some(20)
                } else if km.pressed(i, Action::PageUp) {
                    Some(-20)
                } else if km.pressed(i, Action::Top) {
                    Some(i64::MIN / 2)
                } else if km.pressed(i, Action::Bottom) {
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
            if ctx.input(|i| km.pressed(i, Action::Open) || km.pressed(i, Action::FocusRight)) {
                open_selected = true;
            }
        }

        table_view::fit_widths(ui, key, &self.cache, tui);
        let gap = ui.spacing().item_spacing.x;
        let cols = table_view::layout_columns(key, &snap, tui, &metrics, ui.available_width(), gap);
        let now = skiff_core::now_unix();
        let out = table_view::show(ui, &pal, key, &self.cache, tui, &cols, &metrics, now, locked.as_deref());
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
        let locked = self.locked_reason();
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
        detail.locked = locked;
        detail.confirm_apply = self.config.cluster(&session.context).confirm;
        detail.grafana_url = self.config.cluster(&session.context).grafana_url;
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
                DetailAction::CopySecret { text, what } => self.copy_secret(&ctx, text, &what),
                DetailAction::LogsOf { container } => {
                    let targets = vec![skiff_core::LogTarget { namespace: ns.clone(), pod: name.clone(), container: container.clone() }];
                    self.dock.open_logs(&session, format!("{name} · {container}"), Job::done(Ok(targets)));
                }
                DetailAction::ConfirmApply => {
                    self.confirm = Some(Confirm {
                        title: format!("Apply changes to {}?", key.kind),
                        object: if ns.is_empty() { name.clone() } else { format!("{ns}/{name}") },
                        note: "The object is replaced with the YAML in the editor.".to_string(),
                        verb: "Apply".to_string(),
                        context: session.context.clone(),
                        op: ConfirmOp::ApplyEdit,
                    });
                }
                DetailAction::ShellIn { .. } | DetailAction::ForwardPort { .. } if session.is_read_only() => {
                    if let Some(reason) = self.locked_reason() {
                        self.toast(reason, Tone::Warn);
                    }
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
        if self.palette.is_none() {
            return;
        }
        // (Clusters hidden in Settings are not offered.)
        let shown = self.menu_contexts();
        let Some(state) = &mut self.palette else { return };
        let session = self.active.as_ref().and_then(|c| self.sessions.get(c)).cloned();
        match palette::show(ctx, &self.pal, state, &shown, self.active.as_deref(), session.as_ref()) {
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
        if matches!(self.dialog, Some(Dialog::AddKubeconfig { .. })) {
            // (The one dialog that needs no cluster.)
            self.add_kubeconfig_dialog(ctx);
            return;
        }
        let asks = self.asks_first();
        let confirming = self.confirm.is_some();
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
        let mut ask: Option<Confirm> = None;
        let resp = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            match dialog {
                Dialog::AddKubeconfig { .. } => {}
                Dialog::Delete { key, ns, name, force } => {
                    ui.label(widgets::semibold(format!("Delete {}?", key.kind), 15.0));
                    ui.add_space(4.0);
                    let target = if ns.is_empty() { name.clone() } else { format!("{ns}/{name}") };
                    ui.label(RichText::new(target).font(theme::mono(12.5)).color(pal.text));
                    cluster_line(ui, &pal, &session.context);
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
                        let fewer = ui.add_enabled(known.is_some(), egui::Button::new("−").small());
                        if widgets::named(fewer, "One replica fewer").clicked() {
                            *replicas = (known.unwrap_or(1) - 1).max(0).to_string();
                        }
                        let more = ui.add_enabled(known.is_some(), egui::Button::new("+").small());
                        if widgets::named(more, "One replica more").clicked() {
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
                            if asks {
                                ask = Some(Confirm {
                                    title: format!("Scale {} to {}?", key.kind, widgets::plural(n as usize, "replica")),
                                    object: if ns.is_empty() { name.clone() } else { format!("{ns}/{name}") },
                                    note: String::new(),
                                    verb: "Scale".to_string(),
                                    context: session.context.clone(),
                                    op: ConfirmOp::Scale { key: key.clone(), ns: ns.clone(), name: name.clone(), replicas: n },
                                });
                            } else {
                                push = Some(Pending::Simple {
                                    job: session.scale(key, ns, name, n),
                                    ok: format!("Scaled {name} to {n}"),
                                });
                            }
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
                        // (One line in the smallest window: a second
                        // one pushes the buttons out of it.)
                        RichText::new("Documents separated by --- are applied in order. Existing objects are updated, not replaced.")
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
                    // Fields another manager (Helm, Argo CD, a controller)
                    // owns are not taken over silently: say which, and
                    // offer to.
                    let conflict = error.as_deref().is_some_and(skiff_core::ops::is_apply_conflict);
                    if let Some(err) = error {
                        ui.add_space(4.0);
                        // One line per document: a long manifest must not
                        // push the buttons below the window.
                        egui::ScrollArea::vertical().id_salt("create-error").max_height(150.0).show(ui, |ui| {
                            if conflict {
                                apply_conflict_report(ui, &pal, err);
                            } else {
                                ui.add(egui::Label::new(RichText::new(&*err).color(pal.err)).wrap().selectable(true));
                            }
                        });
                    }
                    ui.add_space(10.0);
                    let busy = job.is_some();
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(!busy, egui::Button::new("Apply")).clicked()
                            || (!busy && !confirming && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)));
                        let anyway = conflict
                            && ui
                                .add_enabled(!busy, egui::Button::new(RichText::new("Apply anyway").color(pal.warn)))
                                .on_hover_text(
                                    "Applies the manifest and takes these fields over from their managers \
                                     (kubectl apply --server-side --force-conflicts). The other manager may set them back.",
                                )
                                .clicked();
                        if go || anyway {
                            if asks {
                                let documents = text.lines().filter(|l| l.trim_end() == "---").count() + 1;
                                ask = Some(Confirm {
                                    title: if anyway { "Apply this manifest anyway?" } else { "Apply this manifest?" }.to_string(),
                                    object: widgets::plural(documents, "document"),
                                    note: if anyway {
                                        "Fields owned by other managers are taken over.".to_string()
                                    } else {
                                        "Objects in it are created, or updated where they exist.".to_string()
                                    },
                                    verb: if anyway { "Apply anyway" } else { "Apply" }.to_string(),
                                    context: session.context.clone(),
                                    op: ConfirmOp::ApplyManifest { force: anyway },
                                });
                            } else {
                                *error = None;
                                *job = Some(session.apply_manifest_with(text.clone(), anyway));
                            }
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
        if close || (resp.should_close() && !sticky && !confirming) {
            self.dialog = None;
        }
        if let Some(question) = ask {
            self.confirm = Some(question);
        }
    }

    /// Naming a kubeconfig file by its path.
    fn add_kubeconfig_dialog(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        self.dialog_frames = self.dialog_frames.saturating_add(1);
        let Some(Dialog::AddKubeconfig { path, error, focused }) = &mut self.dialog else { return };
        let (mut add, mut close) = (false, false);
        let resp = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(widgets::semibold("Add kubeconfig file", 15.0));
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    RichText::new("Enter the path of a kubeconfig file, or of a folder of them. You can also drop the file onto this window.")
                        .color(pal.muted)
                        .size(12.0),
                )
                .wrap(),
            );
            ui.add_space(8.0);
            let edit = ui.add(
                egui::TextEdit::singleline(path)
                    .hint_text("~/.kube/config")
                    .font(theme::mono(12.5))
                    .desired_width(f32::INFINITY),
            );
            if !*focused {
                *focused = true;
                edit.request_focus();
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if let Some(err) = error {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(&*err).color(pal.err)).wrap().selectable(true));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let ready = !path.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new("Add")).clicked() || (ready && enter) {
                    add = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if add {
            let entered = path.clone();
            match self.add_kubeconfig(&entered) {
                Ok(()) => close = true,
                Err(e) => {
                    if let Some(Dialog::AddKubeconfig { error, .. }) = &mut self.dialog {
                        *error = Some(e);
                    }
                }
            }
        }
        if (close || resp.should_close()) && matches!(self.dialog, Some(Dialog::AddKubeconfig { .. })) {
            self.dialog = None;
        }
    }

    /// The question asked before a change to a cluster that is set to ask
    /// first. Cancel has the focus: a key held a moment too long never
    /// confirms.
    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let pal = self.pal;
        let Some(confirm) = &self.confirm else {
            self.confirm_frames = 0;
            return;
        };
        if self.active.as_deref() != Some(confirm.context.as_str()) {
            self.confirm = None;
            return;
        }
        let fresh = self.confirm_frames == 0;
        self.confirm_frames = self.confirm_frames.saturating_add(1);
        let (mut go, mut close) = (false, false);
        let resp = egui::Modal::new(egui::Id::unique("confirm")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(widgets::semibold(&confirm.title, 15.0));
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&confirm.object).font(theme::mono(12.5)).color(pal.text)).wrap());
            cluster_line(ui, &pal, &confirm.context);
            if !confirm.note.is_empty() {
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(&confirm.note).color(pal.muted)).wrap());
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let yes = egui::Button::new(RichText::new(&confirm.verb).color(pal.bg)).fill(pal.accent);
                if ui.add(yes).clicked() {
                    go = true;
                }
                let cancel = ui.button("Cancel");
                if fresh {
                    cancel.request_focus();
                }
                if cancel.clicked() {
                    close = true;
                }
            });
        });
        if go {
            if let Some(confirm) = self.confirm.take() {
                self.run_confirmed(ctx, confirm.op);
            }
        } else if close || resp.should_close() {
            self.confirm = None;
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

/// A setting in a menu that is on or off: a check box drawn so that it
/// shows on the menu's background, and its label.
fn menu_toggle(ui: &mut Ui, pal: &Palette, on: bool, label: &str, enabled: bool) -> Response {
    let (ic, color) = if on {
        (icons::ICON_CHECK_BOX, pal.accent)
    } else {
        (icons::ICON_CHECK_BOX_OUTLINE_BLANK, pal.muted)
    };
    let atoms = (widgets::icon(ic, 17.0).color(color), RichText::new(label).color(pal.text));
    let resp = ui.add_enabled(enabled, egui::Button::new(atoms).frame(false));
    let label = label.to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, on, &label));
    resp
}

/// "on cluster <name>": which cluster a dialog's action goes to.
fn cluster_line(ui: &mut Ui, pal: &Palette, context: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(RichText::new("on cluster").color(pal.muted).size(12.5));
        ui.label(widgets::icon(icons::ICON_HUB, 14.0).color(pal.accent));
        ui.add(egui::Label::new(widgets::semibold(context, 13.0).color(pal.text)).truncate());
    });
}

/// One object of an apply report that was left alone because other
/// managers own fields of it: (field, manager) pairs.
struct ConflictedObject {
    object: String,
    fields: Vec<(String, String)>,
}

/// Splits an apply report (see `skiff_core::ops::apply_manifest_with`)
/// into the objects with field conflicts and the remaining lines.
fn apply_conflicts(report: &str) -> (Vec<ConflictedObject>, Vec<String>) {
    let mut objects: Vec<ConflictedObject> = Vec::new();
    let mut rest = Vec::new();
    let mut in_object = false;
    for line in report.lines() {
        if let Some(field) = line.strip_prefix("    ").filter(|_| in_object) {
            // (A manager's name may itself hold brackets; a field path
            // has no " (".)
            let (field, manager) = match field.split_once(" (") {
                Some((field, manager)) => (field.to_string(), manager.strip_suffix(')').unwrap_or(manager).to_string()),
                // ("and 3 more")
                None => (field.to_string(), String::new()),
            };
            if let Some(object) = objects.last_mut() {
                object.fields.push((field, manager));
            }
            continue;
        }
        in_object = skiff_core::ops::is_apply_conflict(line);
        match line.split_once(": not applied") {
            Some((object, _)) if in_object => objects.push(ConflictedObject { object: object.to_string(), fields: Vec::new() }),
            _ => rest.push(line.to_string()),
        }
    }
    (objects, rest)
}

/// An apply report with field conflicts: which fields of which objects
/// belong to which manager, then whatever else the report says.
fn apply_conflict_report(ui: &mut Ui, pal: &Palette, report: &str) {
    let (objects, rest) = apply_conflicts(report);
    Frame::new()
        .fill(pal.tint(pal.warn, 0.10))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::icon(icons::ICON_WARNING, 15.0).color(pal.warn));
                ui.label(widgets::semibold("Not applied: some fields belong to another manager", 13.0).color(pal.text));
            });
            for object in &objects {
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(&object.object).color(pal.muted).size(12.0)).wrap().selectable(true));
                for (field, manager) in &object.fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.add_space(8.0);
                        ui.add(egui::Label::new(RichText::new(field).font(theme::mono(12.0)).color(pal.text)).selectable(true));
                        if !manager.is_empty() {
                            ui.label(RichText::new("managed by").color(pal.muted).size(12.0));
                            ui.label(widgets::semibold(manager, 12.5).color(pal.text));
                        }
                    });
                }
            }
        });
    if !rest.is_empty() {
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(rest.join("\n")).color(pal.muted).size(12.5)).wrap().selectable(true));
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

/// Memory the process holds in RAM (resident set), when the platform says.
pub fn resident_memory() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(target_os = "macos")]
    {
        // `ps` is cheap enough when asked rarely; cached for a few seconds.
        use std::sync::Mutex;
        static CACHE: Mutex<Option<(Instant, Option<u64>)>> = Mutex::new(None);
        let mut cache = CACHE.lock().ok()?;
        if let Some((at, v)) = *cache {
            if at.elapsed() < Duration::from_secs(5) {
                return v;
            }
        }
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .output()
            .ok();
        let v = out
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|t| t.trim().parse::<u64>().ok())
            .map(|kb| kb * 1024);
        *cache = Some((Instant::now(), v));
        v
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_reports_are_split_into_fields_and_managers() {
        let report = "configmaps/a: configured\n\
                      deployments.apps/web: not applied, 2 fields are owned by another manager:\n    \
                      .spec.replicas (helm)\n    .spec.template.spec.containers[name=\"web\"].image (argocd-controller)\n\
                      configmaps/b: not applied, 21 fields are owned by another manager:\n    .data.mode (kubectl (legacy))\n    and 20 more\n\
                      secrets/c: forbidden (403)";
        let (objects, rest) = apply_conflicts(report);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].object, "deployments.apps/web");
        assert_eq!(objects[0].fields[0], (".spec.replicas".to_string(), "helm".to_string()));
        assert_eq!(objects[0].fields[1].1, "argocd-controller");
        assert_eq!(objects[1].fields[0], (".data.mode".to_string(), "kubectl (legacy)".to_string()));
        assert_eq!(objects[1].fields[1], ("and 20 more".to_string(), String::new()));
        assert_eq!(rest, ["configmaps/a: configured", "secrets/c: forbidden (403)"]);
    }

    #[test]
    fn config_decides_where_kubeconfigs_come_from() {
        let (config, _) = Config::from_json(r#"{"kubeconfigs": ["~/a", "/b/*.yaml"]}"#);
        assert_eq!(config_sources(&config).entries(), Some(&["~/a".to_string(), "/b/*.yaml".to_string()][..]));
        assert!(config_sources(&Config::default()).is_default());
    }

    #[test]
    fn the_first_added_kubeconfig_keeps_what_was_read_before() {
        let entries = |s: KubeconfigSources| s.entries().map(<[String]>::to_vec).unwrap_or_default();
        // No list yet: the default by name (resolved at every load, so a
        // $KUBECONFIG that changes later is still followed), not the
        // files it holds today or this user's home directory.
        assert_eq!(entries(kubeconfigs_with_added(None, "/x/staging.yaml")), ["$KUBECONFIG", "/x/staging.yaml"]);
        // A list that exists is added to, and not twice.
        let listed = ["~/a".to_string()];
        assert_eq!(entries(kubeconfigs_with_added(Some(&listed), "/b")), ["~/a", "/b"]);
        assert_eq!(entries(kubeconfigs_with_added(Some(&listed), "~/a")), ["~/a"]);
        // What is saved reads back as the same sources.
        let mut config = Config::default();
        config.set_kubeconfigs(kubeconfigs_with_added(None, "/x/staging.yaml").entries());
        assert_eq!(entries(config_sources(&config)), ["$KUBECONFIG", "/x/staging.yaml"]);
    }

    #[test]
    fn a_changed_or_new_kubeconfig_file_changes_the_stamps() {
        let dir = std::env::temp_dir().join(format!("skiff-stamps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("one.yaml");
        let sources = KubeconfigSources::explicit([file.to_string_lossy().into_owned(), format!("{}/*.conf", dir.display())]);
        let missing = file_stamps(&sources);
        assert_eq!(missing, vec![(file.clone(), None)]);
        std::fs::write(&file, "a").unwrap();
        let present = file_stamps(&sources);
        assert_ne!(present, missing, "a searched file that appears is a change");
        std::fs::write(&file, "ab").unwrap();
        assert_ne!(file_stamps(&sources), present, "new content is a change");
        std::fs::write(dir.join("two.conf"), "x").unwrap();
        assert_eq!(file_stamps(&sources).len(), 2, "a new file matching a wildcard is a change");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
