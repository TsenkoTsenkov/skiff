//! The Settings window: appearance, clusters, updates, the terminal app,
//! the config files and "About".
//!
//! It only draws and reports: [`show`] is given a [`Model`] of what is in
//! effect and returns the [`Action`]s the user asked for; the app carries
//! them out (saving to `config.json` at once) and the next frame shows
//! the result.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2};
use egui_material_icons::{icons, MaterialIcon};
use skiff_core::{Contexts, KubeconfigSources};

use crate::config::{self, Channel, Theme};
use crate::pages::{short_path, KubeconfigReport};
use crate::theme::{self, Palette};
use crate::update;
use crate::widgets;

/// The steps the zoom shortcuts move through.
pub const ZOOM_STEPS: [f32; 12] = [0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The next zoom step above (`up`) or below `zoom`.
pub fn zoom_step(zoom: f32, up: bool) -> f32 {
    let next = if up {
        ZOOM_STEPS.iter().copied().find(|z| *z > zoom + 0.005)
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|z| *z < zoom - 0.005)
    };
    next.unwrap_or(zoom).clamp(config::ZOOM_MIN, config::ZOOM_MAX)
}

/// "125%".
pub fn zoom_percent(zoom: f32) -> String {
    format!("{}%", (zoom * 100.0).round() as i32)
}

/// Where the third-party licences are published.
pub const THIRD_PARTY_FILE: &str = "THIRD-PARTY-NOTICES.md";

/// The project's page on GitHub.
pub fn repository_url() -> String {
    format!("https://github.com/{}", update::repo())
}

/// The commit this build was made from, shortened, when it is known.
pub fn commit() -> Option<String> {
    let commit = option_env!("SKIFF_COMMIT").map(str::trim).filter(|c| !c.is_empty())?;
    let is_hash = commit.len() > 7 && commit.bytes().all(|b| b.is_ascii_hexdigit());
    Some(if is_hash { commit[..7].to_string() } else { commit.to_string() })
}

/// What a bug report should say about this copy of Skiff.
pub fn version_info(channel: Channel) -> String {
    let mut first = format!("Skiff {}", update::current_version());
    let mut details = Vec::new();
    match update::build_number() {
        0 => details.push("development build".to_string()),
        n => details.push(format!("build {n}")),
    }
    if let Some(commit) = commit() {
        details.push(commit);
    }
    first.push_str(&format!(" ({})", details.join(", ")));
    format!(
        "{first}\nTarget: {}\nSystem: {} {}\nUpdate channel: {}\n",
        env!("SKIFF_TARGET"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        channel.name(),
    )
}

/// Creates `path` holding `starter` unless a file is already there.
/// Returns whether it was created.
pub fn ensure_file(path: &Path, starter: &str) -> Result<bool, String> {
    use std::io::Write;
    if path.exists() {
        return Ok(false);
    }
    let fail = |e: std::io::Error| format!("{} could not be created ({e}).", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    // (Never over a file that appeared in the meantime.)
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(starter.as_bytes()).map(|()| true).map_err(fail),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(fail(e)),
    }
}

/// Opens a file with the app the system uses for it (a text editor, for
/// the config files).
pub fn open_in_default_app(path: &Path) -> Result<(), String> {
    let mut command;
    #[cfg(target_os = "macos")]
    {
        command = std::process::Command::new("/usr/bin/open");
        // As text: a .json file may otherwise open in a browser.
        command.arg("-t").arg(path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command = std::process::Command::new("explorer.exe");
        command.arg(path).creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        command = std::process::Command::new("xdg-open");
        command.arg(path);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{} could not be opened ({e}). Its path is copied instead.", path.display()))
}

// ---------------------------------------------------------------------
// The model

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Section {
    #[default]
    Appearance,
    Clusters,
    Updates,
    Terminal,
    Files,
    About,
}

impl Section {
    pub const ALL: [Section; 6] =
        [Section::Appearance, Section::Clusters, Section::Updates, Section::Terminal, Section::Files, Section::About];

    pub fn title(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Clusters => "Clusters",
            Section::Updates => "Updates",
            Section::Terminal => "Terminal",
            Section::Files => "Files",
            Section::About => "About",
        }
    }

    fn icon(self) -> MaterialIcon {
        match self {
            Section::Appearance => icons::ICON_PALETTE,
            Section::Clusters => icons::ICON_HUB,
            Section::Updates => icons::ICON_SYSTEM_UPDATE_ALT,
            Section::Terminal => icons::ICON_TERMINAL,
            Section::Files => icons::ICON_DESCRIPTION,
            Section::About => icons::ICON_INFO,
        }
    }
}

/// Which rule decided the kubeconfig files in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KubeSource {
    /// `--kubeconfig`, for this run.
    CommandLine,
    /// `kubeconfigs` in config.json.
    ConfigFile,
    /// `$KUBECONFIG`.
    Environment,
    /// `~/.kube/config`.
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileState {
    /// Read; it holds this many contexts.
    Used(usize),
    /// Looked for, not there.
    Missing,
    /// A pattern or folder with nothing in it.
    NoMatch,
    /// There, but it cannot be used: why.
    Problem(String),
}

/// One kubeconfig location: a file, or an entry of the config file's
/// list (which may be a folder or a pattern).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeFile {
    /// As it is shown: the entry as written, or the path with `~`.
    pub label: String,
    pub state: FileState,
    /// The config-file entry to take off the list, where that is possible.
    pub remove: Option<String>,
}

/// What the kubeconfig files in effect look like on disk, and where the
/// clusters' credential plugins are. Costs file-system lookups, so the
/// app builds it once per kubeconfig load (see [`State::kube`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kube {
    pub files: Vec<KubeFile>,
    /// Credential plugin command → where it was found on `PATH`.
    pub plugins: BTreeMap<String, Option<PathBuf>>,
}

/// Builds the [`Kube`] model from the last kubeconfig load.
pub fn kube_model(sources: &KubeconfigSources, report: &KubeconfigReport, contexts: &Contexts, removable: bool) -> Kube {
    let state_of = |path: &Path| {
        if let Some(problem) = report.problems.iter().find(|p| p.path == path) {
            FileState::Problem(problem.message.clone())
        } else if report.files.iter().any(|f| f == path) {
            FileState::Used(contexts.items.iter().filter(|c| c.file.as_deref() == Some(path)).count())
        } else {
            FileState::Missing
        }
    };
    let files = match sources.entries() {
        Some(entries) => entries
            .iter()
            .map(|entry| {
                let paths = KubeconfigSources::explicit([entry.clone()]).resolve();
                let state = match paths.as_slice() {
                    [] => FileState::NoMatch,
                    [one] => state_of(one),
                    many => {
                        let states: Vec<FileState> = many.iter().map(|p| state_of(p)).collect();
                        let broken = states.iter().filter(|s| matches!(s, FileState::Problem(_))).count();
                        let total: usize = states.iter().map(|s| if let FileState::Used(n) = s { *n } else { 0 }).sum();
                        match broken {
                            0 => FileState::Used(total),
                            n => FileState::Problem(format!("{n} of {} can't be used", widgets::plural(many.len(), "file"))),
                        }
                    }
                };
                KubeFile { label: entry.clone(), state, remove: removable.then(|| entry.clone()) }
            })
            .collect(),
        None => sources
            .resolve()
            .iter()
            .map(|path| KubeFile { label: short_path(path), state: state_of(path), remove: None })
            .collect(),
    };
    let mut plugins = BTreeMap::new();
    for command in contexts.items.iter().filter_map(|c| c.exec_command.as_deref()) {
        if !plugins.contains_key(command) {
            plugins.insert(command.to_string(), skiff_core::find_executable(command));
        }
    }
    Kube { files, plugins }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterRow {
    pub name: String,
    /// The one that is open.
    pub active: bool,
    pub read_only: bool,
    pub confirm: bool,
    /// The credential plugin its kubeconfig entry runs (`aws`, `gke-gcloud-auth-plugin`).
    pub plugin: Option<String>,
}

/// Everything the window shows, as it is in effect now.
pub struct Model {
    pub theme: Theme,
    /// What "System" resolves to now, if the system says.
    pub system_dark: Option<bool>,
    pub zoom: f32,
    pub kube_source: KubeSource,
    pub kube: Arc<Kube>,
    pub clusters: Vec<ClusterRow>,
    /// The file dialog for "Add file" is up.
    pub picking: bool,
    pub updates: config::Updates,
    pub update_state: update::State,
    /// `SKIFF_NO_UPDATE_CHECK` is set.
    pub updates_off_by_env: bool,
    pub terminal: Option<String>,
    /// `$SKIFF_TERMINAL`, which wins over the setting.
    pub terminal_env: Option<String>,
    pub config_path: Option<PathBuf>,
    pub keys_path: Option<PathBuf>,
    pub mac: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Close,
    SetTheme(Theme),
    SetZoom(f32),
    AddKubeconfig,
    RemoveKubeconfig(String),
    /// Back to `$KUBECONFIG` / `~/.kube/config`.
    ResetKubeconfigs,
    ReloadKubeconfig,
    SetReadOnly(String, bool),
    SetConfirm(String, bool),
    SetUpdateCheck(bool),
    SetChannel(Channel),
    CheckForUpdates,
    SetTerminal(Option<String>),
    OpenConfigFile,
    OpenKeysFile,
    ShowShortcuts,
    OpenUrl(String),
    CopyVersionInfo,
}

/// The window's own state.
pub struct State {
    pub section: Section,
    /// The terminal field's text, and the setting it was filled from.
    terminal: String,
    terminal_from: Option<Option<String>>,
    /// Terminal apps to suggest: found on this computer where that can
    /// be told.
    suggestions: Vec<&'static str>,
    /// The app icon, once it is needed.
    icon: Option<egui::TextureHandle>,
    /// The kubeconfig model and the load it belongs to.
    pub kube: Option<(u64, Arc<Kube>)>,
}

impl State {
    pub fn new(section: Section) -> Self {
        State { section, terminal: String::new(), terminal_from: None, suggestions: terminal_suggestions(), icon: None, kube: None }
    }
}

/// Names the terminal field offers with one click.
fn terminal_suggestions() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec!["Terminal", "iTerm", "Ghostty", "WezTerm", "kitty", "Alacritty"]
    } else if cfg!(windows) {
        Vec::new()
    } else {
        const KNOWN: [&str; 10] =
            ["ghostty", "kitty", "wezterm", "alacritty", "foot", "gnome-terminal", "ptyxis", "konsole", "xfce4-terminal", "xterm"];
        KNOWN.into_iter().filter(|name| crate::shell_env::which(name).is_some()).collect()
    }
}

static ICON_PNG: &[u8] = include_bytes!("../assets/icons/skiff-128.png");

fn app_icon(ctx: &egui::Context, slot: &mut Option<egui::TextureHandle>) -> Option<egui::TextureHandle> {
    if slot.is_none() {
        let icon = eframe::icon_data::from_png_bytes(ICON_PNG).ok()?;
        let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        *slot = Some(ctx.load_texture("skiff-icon", image, egui::TextureOptions::LINEAR));
    }
    slot.clone()
}

// ---------------------------------------------------------------------
// Drawing

const NAV_WIDTH: f32 = 172.0;
const WIDTH: f32 = 780.0;
const HEIGHT: f32 = 600.0;

fn nav_item(ui: &mut Ui, pal: &Palette, section: Section, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
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
        let ic = section.icon();
        p.text(
            egui::pos2(rect.left() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            ic.codepoint,
            FontId::new(16.0, ic.font_family()),
            if selected { pal.accent } else { pal.faint },
        );
        p.text(
            egui::pos2(rect.left() + 34.0, rect.center().y),
            Align2::LEFT_CENTER,
            section.title(),
            FontId::proportional(13.0),
            if selected || resp.hovered() { pal.text } else { pal.muted },
        );
    }
    widgets::focus_ring(ui, &resp, 6, pal);
    let label = section.title().to_string();
    resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::Tab, true, selected, &label));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// One setting: its name and what it does on the left, the control on
/// the right.
fn setting(ui: &mut Ui, pal: &Palette, title: &str, detail: &str, control_width: f32, control: impl FnOnce(&mut Ui)) {
    let full = ui.available_width();
    let text_width = (full - control_width - 12.0).max(80.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        ui.allocate_ui_with_layout(Vec2::new(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(text_width);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).color(pal.text));
            if !detail.is_empty() {
                ui.add(egui::Label::new(RichText::new(detail).color(pal.muted).size(12.0)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
    ui.add_space(14.0);
}

fn note(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(pal.muted).size(12.0)).wrap());
}

/// A bordered box that groups rows.
fn card(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(pal.bg)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// Draws the window and returns what the user asked for.
pub fn show(ctx: &egui::Context, pal: &Palette, state: &mut State, model: &Model) -> Vec<Action> {
    let mut actions = Vec::new();
    let screen = ctx.content_rect();
    let width = WIDTH.min(screen.width() - 48.0).max(420.0);
    let height = HEIGHT.min(screen.height() - 64.0).max(260.0);
    let frame = Frame::new()
        .fill(pal.raised)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::ZERO);
    let resp = egui::Modal::new(egui::Id::unique("settings")).frame(frame).show(ctx, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.set_width(width);
        ui.horizontal_top(|ui| {
            // The sections.
            let (nav_rect, _) = ui.allocate_exact_size(Vec2::new(NAV_WIDTH, height), Sense::hover());
            let corners = CornerRadius { nw: 10, sw: 10, ne: 0, se: 0 };
            ui.painter().rect_filled(nav_rect.shrink(1.0), corners, pal.panel);
            ui.painter().vline(nav_rect.right(), nav_rect.y_range(), Stroke::new(1.0, pal.border));
            let column = |rect: egui::Rect| egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min));
            // (Placed in rectangles that are already allocated.)
            ui.scope_builder(column(nav_rect.shrink2(Vec2::new(8.0, 12.0))), |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 1.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(widgets::semibold("Settings", 15.0).color(pal.text));
                });
                ui.add_space(10.0);
                for section in Section::ALL {
                    if nav_item(ui, pal, section, state.section == section).clicked() {
                        state.section = section;
                    }
                }
            });

            // The section that is open.
            let (body_rect, _) = ui.allocate_exact_size(Vec2::new(width - NAV_WIDTH, height), Sense::hover());
            ui.scope_builder(column(body_rect), |ui| {
                ui.set_clip_rect(body_rect.intersect(ui.clip_rect()));
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
                Frame::new().inner_margin(Margin { left: 22, right: 12, top: 12, bottom: 0 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(widgets::semibold(state.section.title(), 17.0).color(pal.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, icons::ICON_CLOSE, "Close settings").clicked() {
                                actions.push(Action::Close);
                            }
                        });
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt(state.section).auto_shrink([false, false]).show(ui, |ui| {
                    Frame::new().inner_margin(Margin { left: 22, right: 22, top: 6, bottom: 18 }).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match state.section {
                            Section::Appearance => appearance(ui, pal, model, &mut actions),
                            Section::Clusters => clusters(ui, pal, model, &mut actions),
                            Section::Updates => updates(ui, pal, model, &mut actions),
                            Section::Terminal => terminal(ui, pal, state, model, &mut actions),
                            Section::Files => files(ui, pal, model, &mut actions),
                            Section::About => about(ui, pal, state, model, &mut actions),
                        }
                    });
                });
            });
        });
    });
    if resp.should_close() {
        actions.push(Action::Close);
    }
    actions
}

fn appearance(ui: &mut Ui, pal: &Palette, model: &Model, actions: &mut Vec<Action>) {
    let system = match model.system_dark {
        Some(true) => "System follows your computer, which is set to dark now.",
        Some(false) => "System follows your computer, which is set to light now.",
        None => "System follows your computer's light or dark mode.",
    };
    setting(ui, pal, "Theme", system, 236.0, |ui| {
        let at = Theme::ALL.iter().position(|t| *t == model.theme).unwrap_or(0);
        if let Some(i) = widgets::segmented(ui, pal, &["Dark", "Light", "System"], at, 72.0) {
            actions.push(Action::SetTheme(Theme::ALL[i]));
        }
    });
    let cmd = if model.mac { "Cmd" } else { "Ctrl" };
    let detail = format!("The size of everything in the window. {cmd} + and {cmd} − change it from anywhere, {cmd} 0 goes back to 100%.");
    setting(ui, pal, "Zoom", &detail, 236.0, |ui| {
        let reset = ui.add_enabled(
            (model.zoom - 1.0).abs() > 0.005,
            egui::Button::new("Reset").min_size(Vec2::new(0.0, 26.0)),
        );
        if reset.on_hover_text("Back to 100%").clicked() {
            actions.push(Action::SetZoom(1.0));
        }
        let more = zoom_step(model.zoom, true);
        let plus = ui.add_enabled(more > model.zoom, egui::Button::new(widgets::icon(icons::ICON_ADD, 16.0)).min_size(Vec2::new(28.0, 26.0)));
        if widgets::named(plus, "Zoom in").clicked() {
            actions.push(Action::SetZoom(more));
        }
        ui.allocate_ui_with_layout(Vec2::new(46.0, 26.0), Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
            ui.label(widgets::semibold(zoom_percent(model.zoom), 13.0).color(pal.text));
        });
        let less = zoom_step(model.zoom, false);
        let minus = ui.add_enabled(less < model.zoom, egui::Button::new(widgets::icon(icons::ICON_REMOVE, 16.0)).min_size(Vec2::new(28.0, 26.0)));
        if widgets::named(minus, "Zoom out").clicked() {
            actions.push(Action::SetZoom(less));
        }
    });
}

fn clusters(ui: &mut Ui, pal: &Palette, model: &Model, actions: &mut Vec<Action>) {
    widgets::caption(ui, "Kubeconfig files", pal);
    let source = match model.kube_source {
        KubeSource::CommandLine => "Given with --kubeconfig, for this run only.",
        KubeSource::ConfigFile => "The list in config.json. $KUBECONFIG is not read while there is one.",
        KubeSource::Environment => "From $KUBECONFIG, as kubectl reads it.",
        KubeSource::Default => "The place kubectl looks in. $KUBECONFIG is not set.",
    };
    note(ui, pal, source);
    ui.add_space(4.0);
    card(ui, pal, |ui| {
        if model.kube.files.is_empty() {
            ui.label(RichText::new("No kubeconfig location is known.").color(pal.muted));
        }
        for (i, file) in model.kube.files.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            kube_file_row(ui, pal, file, actions);
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let add = ui
            .add_enabled(!model.picking, egui::Button::new((widgets::icon(icons::ICON_FILE_OPEN, 16.0), "Add file…")).min_size(Vec2::new(0.0, 28.0)))
            .on_hover_text("Add a kubeconfig file or folder to the list in config.json. You can also drop one onto the window.");
        if add.clicked() {
            actions.push(Action::AddKubeconfig);
        }
        if widgets::action_button(ui, icons::ICON_SYNC, "Reload")
            .on_hover_text("Read the kubeconfig files again. Skiff also does this when you come back to its window and a file has changed.")
            .clicked()
        {
            actions.push(Action::ReloadKubeconfig);
        }
        if model.kube_source == KubeSource::ConfigFile {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::link(ui, pal, icons::ICON_RESTART_ALT, "Use kubectl's locations")
                    .on_hover_text("Removes the list from config.json: Skiff reads $KUBECONFIG, or ~/.kube/config, again.")
                    .clicked()
                {
                    actions.push(Action::ResetKubeconfigs);
                }
            });
        }
    });
    if model.kube_source != KubeSource::ConfigFile && model.kube_source != KubeSource::CommandLine {
        ui.add_space(2.0);
        note(ui, pal, "Adding a file starts a list in config.json with these locations and the new one.");
    }

    ui.add_space(18.0);
    widgets::caption(ui, "Protection", pal);
    note(
        ui,
        pal,
        "Read-only: Skiff changes nothing in the cluster. Ask first: restart, scale, cordon, suspend and applying YAML are confirmed. Deleting always asks.",
    );
    ui.add_space(4.0);
    if model.clusters.is_empty() {
        card(ui, pal, |ui| {
            ui.label(RichText::new("No clusters yet. Add a kubeconfig file to see its clusters.").color(pal.muted));
        });
        return;
    }
    const TOGGLE: f32 = 86.0;
    card(ui, pal, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.horizontal(|ui| {
            ui.set_height(20.0);
            ui.label(RichText::new("Cluster").color(pal.faint).size(12.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for title in ["Ask first", "Read-only"] {
                    ui.allocate_ui_with_layout(Vec2::new(TOGGLE, 20.0), Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
                        ui.label(RichText::new(title).color(pal.faint).size(12.0));
                    });
                }
            });
        });
        for row in &model.clusters {
            ui.separator();
            ui.horizontal(|ui| {
                let toggles = 2.0 * TOGGLE;
                let name_width = (ui.available_width() - toggles - 8.0).max(80.0);
                ui.allocate_ui_with_layout(Vec2::new(name_width, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(name_width);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let name = if row.active { widgets::semibold(&row.name, 13.0) } else { RichText::new(&row.name) };
                        ui.add(egui::Label::new(name.color(pal.text)).truncate());
                        if row.active {
                            ui.label(RichText::new("open").color(pal.faint).size(11.5));
                        }
                    });
                    if let Some(plugin) = &row.plugin {
                        plugin_line(ui, pal, plugin, model.kube.plugins.get(plugin).and_then(|p| p.as_deref()));
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // (Nothing to ask about while nothing can be changed.)
                    let ask = toggle_cell(ui, pal, TOGGLE, row.confirm, !row.read_only, &format!("Ask before changes to {}", row.name));
                    if ask {
                        actions.push(Action::SetConfirm(row.name.clone(), !row.confirm));
                    }
                    if toggle_cell(ui, pal, TOGGLE, row.read_only, true, &format!("{} is read-only", row.name)) {
                        actions.push(Action::SetReadOnly(row.name.clone(), !row.read_only));
                    }
                });
            });
        }
    });
}

/// A check box centred in a table cell. Returns whether it was clicked.
fn toggle_cell(ui: &mut Ui, pal: &Palette, width: f32, on: bool, enabled: bool, name: &str) -> bool {
    let mut clicked = false;
    ui.allocate_ui_with_layout(Vec2::new(width, 26.0), Layout::centered_and_justified(egui::Direction::LeftToRight), |ui| {
        let (ic, color) = if on { (icons::ICON_CHECK_BOX, pal.accent) } else { (icons::ICON_CHECK_BOX_OUTLINE_BLANK, pal.muted) };
        let button = egui::Button::new(widgets::icon(ic, 18.0).color(color)).frame_when_inactive(false).min_size(Vec2::new(28.0, 26.0));
        let resp = ui.add_enabled(enabled, button);
        let label = name.to_string();
        resp.widget_info(move || egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, on, &label));
        clicked = resp.on_hover_text(name).clicked();
    });
    clicked
}

fn plugin_line(ui: &mut Ui, pal: &Palette, plugin: &str, found: Option<&Path>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        match found {
            Some(path) => {
                ui.label(widgets::icon(icons::ICON_KEY, 13.0).color(pal.faint));
                ui.add(egui::Label::new(RichText::new(format!("Signs in with {plugin}")).color(pal.muted).size(12.0)).truncate())
                    .on_hover_text(format!("The credential plugin this cluster's kubeconfig entry runs.\nFound at {}", path.display()));
            }
            None => {
                ui.label(widgets::icon(icons::ICON_WARNING, 13.0).color(pal.warn));
                ui.add(
                    egui::Label::new(RichText::new(format!("Signs in with {plugin}, which was not found")).color(pal.warn).size(12.0))
                        .truncate(),
                )
                .on_hover_text(format!(
                    "This cluster's kubeconfig entry runs {plugin} to sign in, and no program of that name is on your PATH. \
                     Install it, or start Skiff from a terminal where it works."
                ));
            }
        }
    });
}

/// `text` in the monospace font, shortened from the front (`…/kube/config`)
/// until it is at most `width` wide.
fn elide_front(ui: &Ui, text: &str, width: f32) -> String {
    let wide = |t: &str| ui.painter().layout_no_wrap(t.to_owned(), theme::mono(12.5), Color32::WHITE).size().x;
    if wide(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // The longest tail that fits (widths grow with length: binary search).
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let tail: String = chars[chars.len() - mid..].iter().collect();
        if wide(&format!("…{tail}")) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("…{}", chars[chars.len() - lo..].iter().collect::<String>())
}

fn kube_file_row(ui: &mut Ui, pal: &Palette, file: &KubeFile, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.set_height(26.0);
        let (ic, color, status, tip) = match &file.state {
            FileState::Used(n) => (icons::ICON_CHECK_CIRCLE, pal.ok, widgets::plural(*n, "context"), None),
            FileState::Missing => (icons::ICON_REMOVE, pal.faint, "not found".to_string(), None),
            FileState::NoMatch => (icons::ICON_REMOVE, pal.faint, "no files".to_string(), None),
            FileState::Problem(why) => (icons::ICON_WARNING, pal.warn, "can't be used".to_string(), Some(why.clone())),
        };
        ui.label(widgets::icon(ic, 15.0).color(color));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(entry) = &file.remove {
                let remove = egui::Button::new(widgets::icon(icons::ICON_CLOSE, 15.0).color(pal.muted))
                    .frame_when_inactive(false)
                    .min_size(Vec2::new(26.0, 24.0));
                if widgets::named(ui.add(remove), &format!("Remove {entry} from the list")).clicked() {
                    actions.push(Action::RemoveKubeconfig(entry.clone()));
                }
            }
            let text_color = if matches!(file.state, FileState::Problem(_)) { pal.warn } else { pal.muted };
            let status = ui.label(RichText::new(status).color(text_color).size(12.0));
            if let Some(tip) = tip {
                status.on_hover_text(tip);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                // (Shortened from the front: the file name is what tells
                // the entries apart.)
                let shown = elide_front(ui, &file.label, ui.available_width() - 4.0);
                let label = ui.add(egui::Label::new(RichText::new(&shown).font(theme::mono(12.5)).color(pal.text)));
                if shown != file.label {
                    label.on_hover_text(&file.label);
                }
            });
        });
    });
}

fn updates(ui: &mut Ui, pal: &Palette, model: &Model, actions: &mut Vec<Action>) {
    let check_detail = if model.updates_off_by_env {
        "Turned off by SKIFF_NO_UPDATE_CHECK in the environment, whatever is chosen here."
    } else {
        "Skiff asks GitHub for the newest release when it starts and every few hours. Nothing about you or your clusters is sent."
    };
    setting(ui, pal, "Check for updates automatically", check_detail, 60.0, |ui| {
        if toggle_cell(ui, pal, 40.0, model.updates.check, true, "Check for updates automatically") {
            actions.push(Action::SetUpdateCheck(!model.updates.check));
        }
    });
    setting(
        ui,
        pal,
        "Channel",
        "Stable offers released versions. Nightly offers every build of the main branch, which may be rough.",
        170.0,
        |ui| {
            let at = usize::from(model.updates.channel == Channel::Nightly);
            if let Some(i) = widgets::segmented(ui, pal, &["Stable", "Nightly"], at, 76.0) {
                actions.push(Action::SetChannel(if i == 1 { Channel::Nightly } else { Channel::Stable }));
            }
        },
    );
    ui.separator();
    ui.add_space(8.0);
    let build = match update::build_number() {
        0 => "a development build".to_string(),
        n => format!("build {n}"),
    };
    use update::State as S;
    let status = match &model.update_state {
        S::Idle => format!("You have Skiff {}, {build}.", update::current_version()),
        S::Checking => "Checking…".to_string(),
        S::UpToDate => format!("Skiff {} ({build}) is the newest version.", update::current_version()),
        S::Available(r) | S::DownloadFailed(r, _) => {
            format!("{} is available. Use the button in the top bar to get it.", r.title)
        }
        S::Downloading(r) => format!("Downloading {}…", r.title),
        S::Ready { release, .. } => format!("{} is downloaded. Restart Skiff to use it.", release.title),
        S::Failed(e) => format!("The check did not work: {e}"),
    };
    setting(ui, pal, "This version", &status, 120.0, |ui| {
        let busy = matches!(model.update_state, S::Checking | S::Downloading(_));
        if ui.add_enabled(!busy, egui::Button::new("Check now").min_size(Vec2::new(0.0, 28.0))).clicked() {
            actions.push(Action::CheckForUpdates);
        }
        if busy {
            ui.spinner();
        }
    });
}

fn terminal(ui: &mut Ui, pal: &Palette, state: &mut State, model: &Model, actions: &mut Vec<Action>) {
    if cfg!(windows) {
        note(
            ui,
            pal,
            "\"Open in terminal app\" uses Windows Terminal when it is installed, and the Command Prompt otherwise. There is nothing to choose here.",
        );
        return;
    }
    // (Re)fill the field when the setting changed behind it.
    if state.terminal_from.as_ref() != Some(&model.terminal) {
        state.terminal = model.terminal.clone().unwrap_or_default();
        state.terminal_from = Some(model.terminal.clone());
    }
    let detail = if cfg!(target_os = "macos") {
        "The app that \"Open in terminal app\" starts for shells and logs. Leave it empty and Skiff uses iTerm when it is installed, Terminal otherwise."
    } else {
        "The program that \"Open in terminal app\" starts for shells and logs: a name or a path. Leave it empty and Skiff uses the first one it finds."
    };
    setting(ui, pal, "Terminal app", detail, 220.0, |ui| {
        let edit = ui.add(egui::TextEdit::singleline(&mut state.terminal).hint_text("Automatic").desired_width(204.0));
        let label = "Terminal app";
        edit.widget_info(move || egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
        // Saved when the field is left (Enter included), not per key.
        if edit.lost_focus() {
            let wanted = Some(state.terminal.trim().to_string()).filter(|t| !t.is_empty());
            if wanted != model.terminal {
                actions.push(Action::SetTerminal(wanted));
            }
        }
    });
    if !state.suggestions.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            let lead = if cfg!(target_os = "macos") { "Known to work" } else { "Found here" };
            ui.label(RichText::new(lead).color(pal.muted).size(12.0));
            let automatic = model.terminal.is_none();
            if ui.add(egui::Button::selectable(automatic, "Automatic")).clicked() && !automatic {
                actions.push(Action::SetTerminal(None));
            }
            for name in &state.suggestions {
                let on = model.terminal.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(name));
                if ui.add(egui::Button::selectable(on, *name)).clicked() && !on {
                    actions.push(Action::SetTerminal(Some(name.to_string())));
                }
            }
        });
    }
    if let Some(env) = &model.terminal_env {
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::ICON_INFO, 15.0).color(pal.info));
            ui.add(
                egui::Label::new(RichText::new(format!("SKIFF_TERMINAL is set to {env} and wins over this setting.")).color(pal.muted).size(12.0))
                    .wrap(),
            );
        });
    }
}

fn file_row(ui: &mut Ui, pal: &Palette, name: &str, detail: &str, path: Option<&Path>, button: &str) -> bool {
    let mut clicked = false;
    card(ui, pal, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(widgets::semibold(name, 13.0).color(pal.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let open = ui.add_enabled(
                    path.is_some(),
                    egui::Button::new((widgets::icon(icons::ICON_OPEN_IN_NEW, 15.0), button)).min_size(Vec2::new(0.0, 26.0)),
                );
                clicked = open.clicked();
            });
        });
        note(ui, pal, detail);
        let shown = path.map(short_path).unwrap_or_else(|| "No location is known for it on this computer.".to_string());
        ui.add(egui::Label::new(RichText::new(shown).font(theme::mono(12.0)).color(pal.muted)).wrap().selectable(true));
        ui.add_space(2.0);
    });
    ui.add_space(8.0);
    clicked
}

fn files(ui: &mut Ui, pal: &Palette, model: &Model, actions: &mut Vec<Action>) {
    if file_row(
        ui,
        pal,
        "config.json",
        "Everything in this window. A change made here is saved to it at once; a change made in the file is picked up when you come back to Skiff.",
        model.config_path.as_deref(),
        "Open config file",
    ) {
        actions.push(Action::OpenConfigFile);
    }
    if file_row(
        ui,
        pal,
        "keys.json",
        "The keys for moving around. Opening it creates the file with the standard keys, ready to change.",
        model.keys_path.as_deref(),
        "Open keys file",
    ) {
        actions.push(Action::OpenKeysFile);
    }
    if widgets::link(ui, pal, icons::ICON_KEYBOARD, "Show keyboard shortcuts").clicked() {
        actions.push(Action::ShowShortcuts);
    }

    ui.add_space(16.0);
    widgets::caption(ui, "What config.json understands", pal);
    note(
        ui,
        pal,
        "Every key is optional. A mistake in one key loses only that key, and Skiff says which. A file that is not valid JSON is not used at all.",
    );
    ui.add_space(4.0);
    card(ui, pal, |ui| {
        for (i, key) in config::KEYS.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add(egui::Label::new(RichText::new(key.key).font(theme::mono(12.0)).color(pal.text)).selectable(true));
                ui.label(RichText::new(key.kind).color(pal.faint).size(12.0));
            });
            ui.add(egui::Label::new(RichText::new(key.doc).color(pal.muted).size(12.0)).wrap());
            ui.add_space(2.0);
        }
    });
}

fn about(ui: &mut Ui, pal: &Palette, state: &mut State, model: &Model, actions: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        match app_icon(ui.ctx(), &mut state.icon) {
            Some(texture) => {
                ui.add(egui::Image::new(&texture).fit_to_exact_size(Vec2::splat(72.0)).alt_text("The Skiff icon"));
            }
            None => {
                ui.label(widgets::icon(icons::ICON_SAILING, 56.0).color(pal.accent));
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add_space(8.0);
            ui.label(widgets::semibold("Skiff", 22.0).color(pal.text));
            ui.label(RichText::new("A fast, native Kubernetes cluster browser").color(pal.muted));
            ui.label(RichText::new("No telemetry, no account, no web view.").color(pal.faint).size(12.0));
        });
    });
    ui.add_space(16.0);
    card(ui, pal, |ui| {
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        let build = match update::build_number() {
            0 => "development build".to_string(),
            n => n.to_string(),
        };
        let mono = |ui: &mut Ui, text: String| {
            ui.add(egui::Label::new(RichText::new(text).font(theme::mono(12.5)).color(pal.text)).selectable(true));
        };
        widgets::kv(ui, "Version", pal, |ui| mono(ui, update::current_version().to_string()));
        widgets::kv(ui, "Build", pal, |ui| mono(ui, build));
        widgets::kv(ui, "Commit", pal, |ui| mono(ui, commit().unwrap_or_else(|| "not recorded".to_string())));
        widgets::kv(ui, "Target", pal, |ui| mono(ui, env!("SKIFF_TARGET").to_string()));
        widgets::kv(ui, "Update channel", pal, |ui| mono(ui, model.updates.channel.name().to_string()));
        widgets::kv(ui, "Licence", pal, |ui| mono(ui, "MIT".to_string()));
        ui.add_space(2.0);
    });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if widgets::action_button(ui, icons::ICON_CONTENT_COPY, "Copy version info")
            .on_hover_text("Version, build, commit and system, to paste into a bug report")
            .clicked()
        {
            actions.push(Action::CopyVersionInfo);
        }
    });
    ui.add_space(14.0);
    let repo = repository_url();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(18.0, 6.0);
        for (ic, label, url) in [
            (icons::ICON_CODE, "Repository", repo.clone()),
            (icons::ICON_NEW_RELEASES, "Releases", format!("{repo}/releases")),
            (icons::ICON_BUG_REPORT, "Report a problem", format!("{repo}/issues")),
            (icons::ICON_GAVEL, "Third-party licences", format!("{repo}/blob/main/{THIRD_PARTY_FILE}")),
        ] {
            if widgets::link(ui, pal, ic, label).on_hover_text(&url).clicked() {
                actions.push(Action::OpenUrl(url));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use skiff_core::KubeconfigProblem;

    #[test]
    fn zoom_steps_move_one_notch_and_stop_at_the_ends() {
        assert_eq!(zoom_step(1.0, true), 1.1);
        assert_eq!(zoom_step(1.0, false), 0.9);
        assert_eq!(zoom_step(1.25, true), 1.5);
        // From a value between two steps (typed into the file).
        assert_eq!(zoom_step(1.2, true), 1.25);
        assert_eq!(zoom_step(1.2, false), 1.1);
        assert_eq!(zoom_step(3.0, true), 3.0);
        assert_eq!(zoom_step(0.5, false), 0.5);
        assert_eq!(zoom_percent(1.25), "125%");
        assert_eq!(zoom_percent(0.67), "67%");
        assert!(ZOOM_STEPS.iter().all(|z| (config::ZOOM_MIN..=config::ZOOM_MAX).contains(z)));
    }

    #[test]
    fn version_info_names_version_target_and_system() {
        let text = version_info(Channel::Nightly);
        assert!(text.contains("Update channel: nightly"));
        assert!(text.starts_with(&format!("Skiff {}", env!("CARGO_PKG_VERSION"))), "{text}");
        assert!(text.contains(env!("SKIFF_TARGET")));
        assert!(text.contains(std::env::consts::OS));
    }

    #[test]
    fn a_starter_file_is_only_written_where_there_is_none() {
        let dir = std::env::temp_dir().join(format!("skiff-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("keys.json");
        assert_eq!(ensure_file(&path, "{}\n"), Ok(true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        std::fs::write(&path, "mine").unwrap();
        assert_eq!(ensure_file(&path, "{}\n"), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kubeconfig_files_are_reported_as_found_missing_or_broken() {
        let dir = std::env::temp_dir().join(format!("skiff-settings-kube-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("more")).unwrap();
        let good = dir.join("good.yaml");
        let bad = dir.join("bad.yaml");
        let gone = dir.join("gone.yaml");
        std::fs::write(&good, "x").unwrap();
        std::fs::write(&bad, "x").unwrap();
        let entries = [
            good.to_string_lossy().into_owned(),
            bad.to_string_lossy().into_owned(),
            gone.to_string_lossy().into_owned(),
            format!("{}/more/*.yaml", dir.display()),
        ];
        let sources = KubeconfigSources::explicit(entries.clone());
        let report = KubeconfigReport {
            searched: vec![good.clone(), bad.clone(), gone.clone()],
            files: vec![good.clone()],
            problems: vec![KubeconfigProblem { path: bad.clone(), message: "not YAML".into() }],
        };
        let mut contexts = Contexts::default();
        for name in ["a", "b"] {
            contexts.items.push(skiff_core::ContextInfo {
                name: name.into(),
                cluster: String::new(),
                user: String::new(),
                namespace: None,
                server: String::new(),
                insecure: false,
                proxy: None,
                exec_command: Some("skiff-test-no-such-plugin".into()),
                file: Some(good.clone()),
            });
        }
        let kube = kube_model(&sources, &report, &contexts, true);
        let states: Vec<&FileState> = kube.files.iter().map(|f| &f.state).collect();
        assert_eq!(
            states,
            [&FileState::Used(2), &FileState::Problem("not YAML".into()), &FileState::Missing, &FileState::NoMatch]
        );
        assert_eq!(kube.files[0].remove.as_deref(), Some(entries[0].as_str()));
        assert_eq!(kube.plugins.get("skiff-test-no-such-plugin"), Some(&None));
        // The default locations cannot be taken off a list that is not there.
        let kube = kube_model(&sources, &report, &contexts, false);
        assert!(kube.files.iter().all(|f| f.remove.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
