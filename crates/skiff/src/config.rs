//! The user's configuration file, `config.json`.
//!
//! It lives next to `keys.json` in the config directory (see
//! [`config_dir`]) and holds the few things that are about the user's
//! setup rather than about one window: which kubeconfig files to read, the
//! update channel, the terminal app, and per-cluster protection and
//! integrations. Window state (theme, pane sizes, last view) stays in
//! eframe's own storage. There are no secrets in this file.
//!
//! Loading is lenient: a mistake in one key costs that key, never the
//! file. [`Config::load`] returns what could be understood plus a list of
//! warnings that name the key and say what is wrong. A file that cannot be
//! used at all (not valid JSON, not an object, unreadable) is a different
//! matter: its settings are unknown, protection included, so it is not
//! read as "no settings" but reported by [`Config::unusable`].
//!
//! Saving keeps everything the user wrote that Skiff does not understand:
//! changes are recorded as small edits and applied to the file as it is on
//! disk at that moment, so unknown keys and edits made in an editor while
//! the app was running survive.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 3.0;

// ---------------------------------------------------------------------
// Where the files are

/// The environment values the file locations depend on, so that the rules
/// for every platform can be tested on any of them.
#[derive(Clone, Debug, Default)]
pub struct PathEnv {
    /// Windows rules (`%APPDATA%`) rather than the XDG ones.
    pub windows: bool,
    pub skiff_config: Option<OsString>,
    pub skiff_keymap: Option<OsString>,
    pub xdg_config_home: Option<OsString>,
    pub home: Option<OsString>,
    pub appdata: Option<OsString>,
    pub userprofile: Option<OsString>,
}

impl PathEnv {
    /// The values of this process. Empty variables count as unset.
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        PathEnv {
            windows: cfg!(windows),
            skiff_config: var("SKIFF_CONFIG"),
            skiff_keymap: var("SKIFF_KEYMAP"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            home: var("HOME"),
            appdata: var("APPDATA"),
            userprofile: var("USERPROFILE"),
        }
    }

    fn get(v: &Option<OsString>) -> Option<PathBuf> {
        v.as_ref().filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    /// `$XDG_CONFIG_HOME/skiff`, else `.config/skiff` under the home
    /// directory: the location on Linux and macOS, and where Skiff used to
    /// look on Windows too.
    fn xdg_dir(&self) -> Option<PathBuf> {
        let home = if self.windows { &self.userprofile } else { &self.home };
        // The XDG spec says a relative $XDG_CONFIG_HOME is to be ignored.
        // (Not checked under Windows rules, where it is only the old
        // location and was never checked.)
        Self::get(&self.xdg_config_home)
            .filter(|p| self.windows || p.is_absolute())
            .or_else(|| Self::get(home).map(|h| h.join(".config")))
            .map(|base| base.join("skiff"))
    }
}

/// The directory of `config.json` and `keys.json`: `$XDG_CONFIG_HOME/skiff`
/// or `~/.config/skiff` on Linux and macOS, `%APPDATA%\skiff` on Windows.
pub fn config_dir_in(env: &PathEnv) -> Option<PathBuf> {
    if env.windows {
        PathEnv::get(&env.appdata)
            .or_else(|| PathEnv::get(&env.userprofile).map(|h| h.join("AppData").join("Roaming")))
            .map(|base| base.join("skiff"))
    } else {
        env.xdg_dir()
    }
}

/// Where the files were before the config directory moved: only Windows
/// has one (`%USERPROFILE%\.config\skiff`).
pub fn legacy_config_dir_in(env: &PathEnv) -> Option<PathBuf> {
    if env.windows {
        env.xdg_dir()
    } else {
        None
    }
}

/// `$SKIFF_CONFIG`, else `config.json` in the config directory.
pub fn config_path_in(env: &PathEnv) -> Option<PathBuf> {
    PathEnv::get(&env.skiff_config).or_else(|| config_dir_in(env).map(|d| d.join("config.json")))
}

/// `$SKIFF_KEYMAP`, else `keys.json` in the config directory. A file that
/// is only at the old Windows location is still used, so an upgrade does
/// not silently drop someone's key bindings.
pub fn keymap_path_in(env: &PathEnv, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    if let Some(p) = PathEnv::get(&env.skiff_keymap) {
        return Some(p);
    }
    let current = config_dir_in(env).map(|d| d.join("keys.json"));
    let legacy = legacy_config_dir_in(env).map(|d| d.join("keys.json"));
    match (current, legacy) {
        (Some(current), Some(legacy)) if !exists(&current) && exists(&legacy) => Some(legacy),
        (Some(current), _) => Some(current),
        (None, legacy) => legacy,
    }
}

/// The config directory for this process (see [`config_dir_in`]).
pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(&PathEnv::from_process())
}

/// The config file for this process (see [`config_path_in`]).
pub fn config_path() -> Option<PathBuf> {
    config_path_in(&PathEnv::from_process())
}

/// The keymap file for this process (see [`keymap_path_in`]).
pub fn keymap_path() -> Option<PathBuf> {
    keymap_path_in(&PathEnv::from_process(), &|p| p.is_file())
}

// ---------------------------------------------------------------------
// The schema: documentation and example

/// One key of the file, for the docs and a settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyDoc {
    /// Dotted path; `<context>` stands for a kubeconfig context name.
    pub key: &'static str,
    /// The JSON type, in words.
    pub kind: &'static str,
    /// What applies when the key is absent.
    pub default: &'static str,
    pub doc: &'static str,
}

/// Every key Skiff understands. The loader takes its list of known keys
/// from this table, so a key cannot be accepted without being documented;
/// the tests check the reverse (every documented key is in [`EXAMPLE`] and
/// is read).
pub const KEYS: &[KeyDoc] = &[
    KeyDoc {
        key: "kubeconfigs",
        kind: "list of paths",
        default: "$KUBECONFIG, else ~/.kube/config",
        doc: "Kubeconfig files to read. An entry is a file or a directory (every file in it); \
              it may start with ~ and use * in its last part. When set, nothing else is read.",
    },
    KeyDoc {
        key: "updates.check",
        kind: "true or false",
        default: "true",
        doc: "Look for a new version of Skiff at startup. SKIFF_NO_UPDATE_CHECK=1 also turns it off.",
    },
    KeyDoc {
        key: "updates.channel",
        kind: "\"stable\" or \"nightly\"",
        default: "\"stable\"",
        doc: "Which releases to offer: stable releases only, or every build of the main branch.",
    },
    KeyDoc {
        key: "terminal",
        kind: "text",
        default: "the built-in terminal's \"open in terminal app\" picks one",
        doc: "The terminal app for \"open in terminal\": a name such as iTerm, Ghostty or kitty, \
              or on Linux a program path. $SKIFF_TERMINAL wins over it.",
    },
    KeyDoc {
        key: "theme",
        kind: "\"dark\", \"light\" or \"system\"",
        default: "\"dark\"",
        doc: "The colours of the window. \"system\" follows the light or dark mode of your computer.",
    },
    KeyDoc {
        key: "zoom",
        kind: "number, 0.5 to 3.0",
        default: "1.0",
        doc: "Size of everything in the window. Also changed by the zoom shortcuts.",
    },
    KeyDoc {
        key: "clusters.<context>.read_only",
        kind: "true or false",
        default: "false",
        doc: "Refuse every change in this cluster: no edit, delete, scale or restart, and no new shell \
              or port-forward. Shells and port-forwards that are already open keep running until closed.",
    },
    KeyDoc {
        key: "clusters.<context>.confirm",
        kind: "true or false",
        default: "false",
        doc: "Ask before every change in this cluster, also for the ones that normally go ahead at once.",
    },
    KeyDoc {
        key: "clusters.<context>.prometheus.namespace",
        kind: "text",
        default: "found automatically",
        doc: "Namespace of the Prometheus Service to chart from. Set namespace and service to \
              skip the automatic search.",
    },
    KeyDoc {
        key: "clusters.<context>.prometheus.service",
        kind: "text",
        default: "found automatically",
        doc: "Name of the Prometheus Service. It is reached through the API server, with your \
              cluster credentials.",
    },
    KeyDoc {
        key: "clusters.<context>.prometheus.port",
        kind: "number, 1 to 65535",
        default: "9090",
        doc: "Port of the Prometheus Service.",
    },
    KeyDoc {
        key: "clusters.<context>.prometheus.scheme",
        kind: "\"http\" or \"https\"",
        default: "\"http\"",
        doc: "What Prometheus itself speaks inside the cluster.",
    },
    KeyDoc {
        key: "clusters.<context>.prometheus.path_prefix",
        kind: "text",
        default: "\"\"",
        doc: "Path in front of /api/v1, for example /select/0/prometheus for VictoriaMetrics.",
    },
    KeyDoc {
        key: "clusters.<context>.grafana_url",
        kind: "http or https URL",
        default: "none",
        doc: "Address of your Grafana, for \"Open in Grafana\" links. Opened in the browser; \
              no login is stored.",
    },
];

/// A complete file using every key. It is valid JSON (JSON has no
/// comments; the explanations are in [`KEYS`] / [`documentation`]).
pub const EXAMPLE: &str = r#"{
  "kubeconfigs": ["~/.kube/config", "~/.kube/conf.d/*.yaml"],
  "updates": { "check": true, "channel": "stable" },
  "terminal": "iTerm",
  "theme": "system",
  "zoom": 1.0,
  "clusters": {
    "production": {
      "read_only": true,
      "confirm": true,
      "prometheus": {
        "namespace": "monitoring",
        "service": "prometheus-operated",
        "port": 9090,
        "scheme": "http",
        "path_prefix": ""
      },
      "grafana_url": "https://grafana.example.com"
    }
  }
}
"#;

/// The reference text for the file: where it is, every key with its type,
/// default and meaning, and the example.
pub fn documentation() -> String {
    let mut out = String::from(
        "Skiff reads config.json from $XDG_CONFIG_HOME/skiff or ~/.config/skiff \
         (Linux, macOS) or %APPDATA%\\skiff (Windows). $SKIFF_CONFIG names another file.\n\
         Every key is optional. A mistake in one key loses only that key, and Skiff says which. \
         A file that is not valid JSON is not used at all.\n\n",
    );
    for k in KEYS {
        out.push_str(&format!("{}\n    {}. Default: {}.\n    {}\n\n", k.key, k.kind, k.default, k.doc));
    }
    out.push_str("Example:\n\n");
    out.push_str(EXAMPLE);
    out
}

/// The keys allowed directly under `prefix` (`""` for the top level).
fn known_keys(prefix: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for k in KEYS {
        let rest = if prefix.is_empty() {
            k.key
        } else {
            match k.key.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) {
                Some(r) => r,
                None => continue,
            }
        };
        let first = rest.split('.').next().unwrap_or(rest);
        if !out.contains(&first) {
            out.push(first);
        }
    }
    out
}

// ---------------------------------------------------------------------
// Typed settings

/// Which releases the updater offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Channel {
    /// `vX.Y.Z` releases only.
    #[default]
    Stable,
    /// Every build of the main branch (prereleases) as well.
    Nightly,
}

impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        }
    }

    pub fn from_name(name: &str) -> Option<Channel> {
        match name.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Channel::Stable),
            "nightly" => Some(Channel::Nightly),
            _ => None,
        }
    }
}

/// The colours of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    /// Whichever of the two the computer is set to.
    System,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Dark, Theme::Light, Theme::System];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::System => "system",
        }
    }

    pub fn from_name(name: &str) -> Option<Theme> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            "system" | "auto" => Some(Theme::System),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Updates {
    pub check: bool,
    pub channel: Channel,
}

impl Default for Updates {
    fn default() -> Self {
        Updates { check: true, channel: Channel::Stable }
    }
}

/// `clusters.<context>.prometheus`: where Prometheus is, when the
/// automatic search should be skipped. Plain data, the same shape as
/// `skiff_core::prom::PromEndpoint`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrometheusOverride {
    pub namespace: String,
    pub service: String,
    pub port: u16,
    /// `http` or `https`.
    pub scheme: String,
    pub path_prefix: String,
}

/// The settings of one cluster (one kubeconfig context).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClusterConfig {
    pub read_only: bool,
    pub confirm: bool,
    pub prometheus: Option<PrometheusOverride>,
    /// An `http://` or `https://` address without a trailing slash.
    pub grafana_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Settings {
    kubeconfigs: Option<Vec<String>>,
    updates: Updates,
    terminal: Option<String>,
    theme: Option<Theme>,
    zoom: Option<f32>,
    clusters: BTreeMap<String, ClusterConfig>,
}

/// One change made from the UI: set (or with `None` remove) the value at a
/// path of object keys.
#[derive(Clone, Debug, PartialEq)]
struct Edit {
    path: Vec<String>,
    value: Option<Value>,
}

/// The config file: typed settings, and the changes not yet saved.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Where it was loaded from and will be saved to.
    path: Option<PathBuf>,
    /// The file's content (an object) with the unsaved edits applied.
    raw: Map<String, Value>,
    settings: Settings,
    pending: Vec<Edit>,
    /// Why the file could not be used, when it could not.
    unusable: Option<String>,
}

impl Config {
    /// The user's config file. A missing file is an empty config without
    /// warnings.
    pub fn load() -> (Config, Vec<String>) {
        match config_path() {
            Some(path) => Config::load_from(&path),
            None => (Config::default(), Vec::new()),
        }
    }

    /// The config in `path`, which is also where [`Config::save`] writes.
    pub fn load_from(path: &Path) -> (Config, Vec<String>) {
        let (mut config, warnings) = match std::fs::read_to_string(path) {
            Ok(text) => Config::from_json(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), Vec::new()),
            Err(e) => {
                let why = format!("could not be read ({e})");
                (Config { unusable: Some(why.clone()), ..Config::default() }, vec![format!("{why}; using the defaults")])
            }
        };
        config.path = Some(path.to_path_buf());
        (config, warnings)
    }

    /// The config in `text` and what was wrong with it. Each warning names
    /// the key it is about.
    pub fn from_json(text: &str) -> (Config, Vec<String>) {
        match parse_document(text) {
            Ok(raw) => {
                let mut warnings = Vec::new();
                let settings = interpret(&raw, &mut warnings);
                (Config { path: None, raw, settings, pending: Vec::new(), unusable: None }, warnings)
            }
            Err(e) => (Config { unusable: Some(e.clone()), ..Config::default() }, vec![format!("{e}; using the defaults")]),
        }
    }

    /// Why the file could not be used, if it could not: it is not valid
    /// JSON, not an object, or could not be read (a missing or empty file
    /// is fine). The words follow the file's name: "is not valid JSON
    /// (...)". This config then holds the defaults, which says nothing
    /// about what the user wrote: whoever loaded it must not take
    /// "read_only is off" from it. A successful [`Config::save`] ends it.
    pub fn unusable(&self) -> Option<&str> {
        self.unusable.as_deref()
    }

    /// What is wrong with single keys of the config as it is now.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        interpret(&self.raw, &mut warnings);
        warnings
    }

    /// The file this config belongs to.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The `kubeconfigs` entries as written (`~` and wildcards are
    /// expanded by `skiff_core::kubeconfig`). `None` when the key is
    /// absent or lists nothing: use `$KUBECONFIG` / `~/.kube/config`.
    pub fn kubeconfigs(&self) -> Option<&[String]> {
        self.settings.kubeconfigs.as_deref()
    }

    pub fn updates(&self) -> Updates {
        self.settings.updates
    }

    /// The terminal app to open shells in, if one is named.
    pub fn terminal(&self) -> Option<&str> {
        self.settings.terminal.as_deref()
    }

    /// The theme, if the file names one.
    pub fn theme(&self) -> Option<Theme> {
        self.settings.theme
    }

    /// The zoom factor, within [`ZOOM_MIN`]..=[`ZOOM_MAX`], if set.
    pub fn zoom(&self) -> Option<f32> {
        self.settings.zoom
    }

    /// The settings of the cluster behind kubeconfig context `context`
    /// (the defaults when the file has none).
    pub fn cluster(&self, context: &str) -> ClusterConfig {
        self.settings.clusters.get(context).cloned().unwrap_or_default()
    }

    /// The contexts that have settings in the file.
    pub fn cluster_names(&self) -> impl Iterator<Item = &str> {
        self.settings.clusters.keys().map(String::as_str)
    }

    /// True when a change has been made that [`Config::save`] would write.
    pub fn is_dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    // -- changes from the UI ------------------------------------------

    /// Replaces the `kubeconfigs` list; `None` removes the key (back to
    /// `$KUBECONFIG` / `~/.kube/config`).
    pub fn set_kubeconfigs(&mut self, entries: Option<&[String]>) {
        self.edit(&["kubeconfigs"], entries.map(|e| json!(e)));
    }

    /// Appends one entry to `kubeconfigs` unless it is already there. With
    /// no list yet the entry becomes the whole list, which stops
    /// `$KUBECONFIG` from being read: to keep what was shown, pass the
    /// entries of `KubeconfigSources::with_entry` to
    /// [`Config::set_kubeconfigs`] instead.
    pub fn add_kubeconfig(&mut self, entry: &str) {
        let entry = entry.trim();
        if entry.is_empty() {
            return;
        }
        let mut list = self.settings.kubeconfigs.clone().unwrap_or_default();
        if !list.iter().any(|e| e == entry) {
            list.push(entry.to_string());
            self.set_kubeconfigs(Some(&list));
        }
    }

    /// Takes one entry off the `kubeconfigs` list. Taking the last one
    /// removes the key (back to `$KUBECONFIG` / `~/.kube/config`).
    pub fn remove_kubeconfig(&mut self, entry: &str) {
        let Some(list) = &self.settings.kubeconfigs else { return };
        let kept: Vec<String> = list.iter().filter(|e| e.as_str() != entry).cloned().collect();
        if kept.len() != list.len() {
            self.set_kubeconfigs((!kept.is_empty()).then_some(&kept[..]));
        }
    }

    pub fn set_update_check(&mut self, check: bool) {
        self.edit(&["updates", "check"], Some(json!(check)));
    }

    pub fn set_update_channel(&mut self, channel: Channel) {
        self.edit(&["updates", "channel"], Some(json!(channel.name())));
    }

    /// Names the terminal app; `None` (or an empty name) removes the key.
    pub fn set_terminal(&mut self, name: Option<&str>) {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        self.edit(&["terminal"], name.map(|n| json!(n)));
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.edit(&["theme"], Some(json!(theme.name())));
    }

    /// Sets the zoom factor, clamped to [`ZOOM_MIN`]..=[`ZOOM_MAX`].
    pub fn set_zoom(&mut self, zoom: f32) {
        if !zoom.is_finite() {
            return;
        }
        // Two decimals: enough for a zoom step, and keeps f32 noise
        // (1.100000023841858) out of the file.
        let z = (f64::from(zoom.clamp(ZOOM_MIN, ZOOM_MAX)) * 100.0).round() / 100.0;
        self.edit(&["zoom"], Some(json!(z)));
    }

    pub fn set_read_only(&mut self, context: &str, read_only: bool) {
        self.edit(&["clusters", context, "read_only"], Some(json!(read_only)));
    }

    pub fn set_confirm(&mut self, context: &str, confirm: bool) {
        self.edit(&["clusters", context, "confirm"], Some(json!(confirm)));
    }

    /// Records one change, unless the file already says so.
    fn edit(&mut self, path: &[&str], value: Option<Value>) {
        if lookup(&self.raw, path) == value.as_ref() {
            return;
        }
        let edit = Edit { path: path.iter().map(|s| s.to_string()).collect(), value };
        apply(&mut self.raw, &edit);
        self.pending.push(edit);
        self.settings = interpret(&self.raw, &mut Vec::new());
    }

    // -- saving -------------------------------------------------------

    /// Writes the changes made since loading. Returns whether a file was
    /// written: nothing is when nothing changed.
    ///
    /// The changes are applied to the file as it is on disk now, so
    /// unknown keys, and anything edited by hand in the meantime, are
    /// kept; afterwards this config reflects that file. A file that is not
    /// valid JSON is left alone and reported, since rewriting it would
    /// lose what the user wrote. (Key order is not kept: keys are written
    /// in alphabetical order.)
    pub fn save(&mut self) -> Result<bool, String> {
        if self.pending.is_empty() {
            return Ok(false);
        }
        let Some(path) = self.path.clone() else {
            return Err("No config file location is known (set SKIFF_CONFIG to a file path).".into());
        };
        let on_disk = match std::fs::read_to_string(&path) {
            Ok(text) => Some(
                parse_document(&text)
                    .map_err(|e| format!("Settings not saved: {} {e}. Fix or remove the file.", path.display()))?,
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Settings not saved: {} could not be read ({e}).", path.display())),
        };
        let mut doc = on_disk.clone().unwrap_or_default();
        for edit in &self.pending {
            apply(&mut doc, edit);
        }
        let changed = on_disk.as_ref() != Some(&doc);
        if changed {
            let mut text = serde_json::to_string_pretty(&Value::Object(doc.clone()))
                .map_err(|e| format!("Settings not saved: {e}."))?;
            text.push('\n');
            write_atomic(&path, text.as_bytes())
                .map_err(|e| format!("Settings not saved: {} could not be written ({e}).", path.display()))?;
        }
        self.settings = interpret(&doc, &mut Vec::new());
        self.raw = doc;
        self.pending.clear();
        // The file was read (or is not there): its settings are known.
        self.unusable = None;
        Ok(changed)
    }
}

/// Writes `bytes` to `path` so that a crash leaves either the old file or
/// the new one: a temporary file in the same directory, then a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // A config file that is a symlink (dotfiles in a git checkout) stays
    // one: write the file it points to.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "config.json".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));

    let open = || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&tmp)
    };
    let mut file = match open() {
        Ok(f) => f,
        // Left behind by a run of ours that died mid-write (same pid).
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&tmp)?;
            open()?
        }
        Err(e) => return Err(e),
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let done = written.and_then(|()| std::fs::rename(&tmp, &target));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

// ---------------------------------------------------------------------
// Reading

/// The file as a JSON object. An empty file is an empty object.
fn parse_document(text: &str) -> Result<Map<String, Value>, String> {
    // Windows editors may put a byte-order mark first.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(other) => Err(format!("should be a JSON object ({{ ... }}), not {}", kind_of(&other))),
        Err(e) => Err(format!("is not valid JSON ({e})")),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "true/false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn lookup<'a>(root: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut v = root.get(*first)?;
    for key in rest {
        v = v.as_object()?.get(*key)?;
    }
    Some(v)
}

/// Applies one edit. Anything in the way that is not an object (it was
/// reported as a mistake when loading) is replaced by one.
fn apply(root: &mut Map<String, Value>, edit: &Edit) {
    let Some((last, parents)) = edit.path.split_last() else { return };
    let mut map = root;
    for key in parents {
        if edit.value.is_none() && !map.get(key).is_some_and(Value::is_object) {
            return; // nothing to remove
        }
        let slot = map.entry(key.clone()).or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        map = match slot {
            Value::Object(m) => m,
            _ => return,
        };
    }
    match &edit.value {
        Some(v) => {
            map.insert(last.clone(), v.clone());
        }
        None => {
            map.remove(last);
        }
    }
}

/// Warns about the keys of `obj` that are not known under `prefix`.
fn warn_unknown(obj: &Map<String, Value>, prefix: &str, label: &str, warnings: &mut Vec<String>) {
    let known = known_keys(prefix);
    for key in obj.keys() {
        if !known.contains(&key.as_str()) {
            let full = if label.is_empty() { key.clone() } else { format!("{label}.{key}") };
            warnings.push(format!("{full}: unknown key, ignored (known here: {})", known.join(", ")));
        }
    }
}

fn read_bool(obj: &Map<String, Value>, key: &str, label: &str, warnings: &mut Vec<String>) -> Option<bool> {
    match obj.get(key)? {
        Value::Bool(b) => Some(*b),
        other => {
            warnings.push(format!("{label}: should be true or false, not {}; ignored", kind_of(other)));
            None
        }
    }
}

fn interpret(raw: &Map<String, Value>, warnings: &mut Vec<String>) -> Settings {
    let mut s = Settings::default();
    warn_unknown(raw, "", "", warnings);

    match raw.get("kubeconfigs") {
        None => {}
        Some(Value::String(one)) if !one.trim().is_empty() => s.kubeconfigs = Some(vec![one.clone()]),
        Some(Value::Array(items)) => {
            let mut list = Vec::new();
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::String(p) if !p.trim().is_empty() => list.push(p.clone()),
                    Value::String(_) => warnings.push(format!("kubeconfigs[{i}]: empty path, ignored")),
                    other => warnings
                        .push(format!("kubeconfigs[{i}]: should be a path in quotes, not {}; ignored", kind_of(other))),
                }
            }
            // A list of nothing would hide every cluster; treat it as absent.
            s.kubeconfigs = (!list.is_empty()).then_some(list);
        }
        Some(other) => {
            warnings.push(format!("kubeconfigs: should be a list of paths, not {}; ignored", kind_of(other)))
        }
    }

    match raw.get("updates") {
        None => {}
        Some(Value::Object(u)) => {
            warn_unknown(u, "updates", "updates", warnings);
            if let Some(check) = read_bool(u, "check", "updates.check", warnings) {
                s.updates.check = check;
            }
            match u.get("channel") {
                None => {}
                Some(Value::String(name)) => match Channel::from_name(name) {
                    Some(c) => s.updates.channel = c,
                    None => warnings.push(format!(
                        "updates.channel: should be \"stable\" or \"nightly\", not {name:?}; using \"stable\""
                    )),
                },
                Some(other) => warnings.push(format!(
                    "updates.channel: should be \"stable\" or \"nightly\", not {}; using \"stable\"",
                    kind_of(other)
                )),
            }
        }
        Some(other) => warnings.push(format!(
            "updates: should be an object like {{ \"check\": true, \"channel\": \"stable\" }}, not {}; ignored",
            kind_of(other)
        )),
    }

    match raw.get("terminal") {
        None => {}
        Some(Value::String(t)) if !t.trim().is_empty() => s.terminal = Some(t.trim().to_string()),
        Some(Value::String(_)) => warnings.push("terminal: empty name, ignored".into()),
        Some(other) => {
            warnings.push(format!("terminal: should be the name of a terminal app, not {}; ignored", kind_of(other)))
        }
    }

    match raw.get("theme") {
        None => {}
        Some(Value::String(name)) => match Theme::from_name(name) {
            Some(t) => s.theme = Some(t),
            None => warnings
                .push(format!("theme: should be \"dark\", \"light\" or \"system\", not {name:?}; ignored")),
        },
        Some(other) => warnings.push(format!(
            "theme: should be \"dark\", \"light\" or \"system\", not {}; ignored",
            kind_of(other)
        )),
    }

    match raw.get("zoom") {
        None => {}
        Some(Value::Number(n)) => match n.as_f64().filter(|z| z.is_finite()) {
            Some(z) => {
                let clamped = z.clamp(f64::from(ZOOM_MIN), f64::from(ZOOM_MAX));
                if clamped != z {
                    warnings.push(format!("zoom: {z} is outside {ZOOM_MIN:?} to {ZOOM_MAX:?}; using {clamped:?}"));
                }
                s.zoom = Some(clamped as f32);
            }
            None => warnings.push("zoom: not a usable number; ignored".into()),
        },
        Some(other) => warnings.push(format!(
            "zoom: should be a number from {ZOOM_MIN:?} to {ZOOM_MAX:?}, not {}; ignored",
            kind_of(other)
        )),
    }

    match raw.get("clusters") {
        None => {}
        Some(Value::Object(clusters)) => {
            for (name, value) in clusters {
                let label = format!("clusters.{name}");
                if name.is_empty() {
                    warnings.push("clusters: an entry has an empty context name; ignored".into());
                    continue;
                }
                match value {
                    Value::Object(c) => {
                        s.clusters.insert(name.clone(), interpret_cluster(c, &label, warnings));
                    }
                    other => warnings
                        .push(format!("{label}: should be an object of settings, not {}; ignored", kind_of(other))),
                }
            }
        }
        Some(other) => warnings.push(format!(
            "clusters: should be an object of context names to settings, not {}; ignored",
            kind_of(other)
        )),
    }
    s
}

fn interpret_cluster(c: &Map<String, Value>, label: &str, warnings: &mut Vec<String>) -> ClusterConfig {
    warn_unknown(c, "clusters.<context>", label, warnings);
    let mut out = ClusterConfig {
        read_only: read_bool(c, "read_only", &format!("{label}.read_only"), warnings).unwrap_or(false),
        confirm: read_bool(c, "confirm", &format!("{label}.confirm"), warnings).unwrap_or(false),
        ..Default::default()
    };
    match c.get("prometheus") {
        None => {}
        Some(Value::Object(p)) => out.prometheus = interpret_prometheus(p, &format!("{label}.prometheus"), warnings),
        Some(other) => warnings.push(format!(
            "{label}.prometheus: should be an object with \"namespace\" and \"service\", not {}; ignored",
            kind_of(other)
        )),
    }
    match c.get("grafana_url") {
        None => {}
        Some(Value::String(url)) => match http_url(url) {
            Ok(url) => out.grafana_url = Some(url),
            Err(why) => warnings.push(format!("{label}.grafana_url: {why}; ignored")),
        },
        Some(other) => warnings.push(format!(
            "{label}.grafana_url: should be an address like \"https://grafana.example.com\", not {}; ignored",
            kind_of(other)
        )),
    }
    out
}

/// The override, or `None` with a warning when any part of it is wrong: a
/// half-understood address would query the wrong service and look like
/// "no data".
fn interpret_prometheus(
    p: &Map<String, Value>,
    label: &str,
    warnings: &mut Vec<String>,
) -> Option<PrometheusOverride> {
    warn_unknown(p, "clusters.<context>.prometheus", label, warnings);
    let before = warnings.len();
    let mut name = |key: &str| match p.get(key) {
        Some(Value::String(v)) if !v.trim().is_empty() => Some(v.trim().to_string()),
        Some(Value::String(_)) | None => {
            warnings.push(format!("{label}.{key}: missing; namespace and service are both needed"));
            None
        }
        Some(other) => {
            warnings.push(format!("{label}.{key}: should be a name in quotes, not {}", kind_of(other)));
            None
        }
    };
    let namespace = name("namespace");
    let service = name("service");
    let port = match p.get("port") {
        None => Some(9090),
        Some(v) => match v.as_u64().and_then(|n| u16::try_from(n).ok()).filter(|n| *n != 0) {
            Some(n) => Some(n),
            None => {
                warnings.push(format!("{label}.port: should be a number from 1 to 65535"));
                None
            }
        },
    };
    let scheme = match p.get("scheme") {
        None => Some("http".to_string()),
        Some(Value::String(v)) if matches!(v.to_ascii_lowercase().as_str(), "http" | "https") => {
            Some(v.to_ascii_lowercase())
        }
        Some(_) => {
            warnings.push(format!("{label}.scheme: should be \"http\" or \"https\""));
            None
        }
    };
    let path_prefix = match p.get("path_prefix") {
        None => Some(String::new()),
        Some(Value::String(v)) => Some(v.trim().to_string()),
        Some(other) => {
            warnings.push(format!("{label}.path_prefix: should be a path in quotes, not {}", kind_of(other)));
            None
        }
    };
    if warnings.len() > before {
        warnings.push(format!("{label}: not used because of the above; Prometheus is searched for automatically"));
        return None;
    }
    Some(PrometheusOverride {
        namespace: namespace?,
        service: service?,
        port: port?,
        scheme: scheme?,
        path_prefix: path_prefix?,
    })
}

/// Checks that `url` is a plain `http://` or `https://` address (it is
/// handed to the browser, so nothing else may get through: no other
/// scheme, no spaces or control characters, no login in it). Returns it
/// without surrounding space and trailing slashes.
pub fn http_url(url: &str) -> Result<String, String> {
    const EXAMPLE: &str = "like \"https://grafana.example.com\"";
    let url = url.trim();
    if url.is_empty() {
        return Err(format!("empty; should be an address {EXAMPLE}"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`')) {
        return Err("has a space or another character an address cannot contain".into());
    }
    let lower = url.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return Err(format!("should start with http:// or https://, {EXAMPLE}"));
    };
    let rest = &url[scheme_len..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    if authority.contains('@') {
        return Err("must not contain a user name or password".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, after)) if !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') => {
                (h, after.strip_prefix(':').or(after.is_empty().then_some("")))
            }
            _ => return Err(format!("has no usable host name; should be an address {EXAMPLE}")),
        }
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, Some("")),
        }
    };
    let host_ok = !host.is_empty()
        && !host.starts_with(['-', '.'])
        && host.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'));
    if !host_ok {
        return Err(format!("has no usable host name; should be an address {EXAMPLE}"));
    }
    match port {
        Some("") => {}
        Some(p) if p.parse::<u16>().is_ok_and(|n| n != 0) => {}
        _ => return Err("has a port that is not a number from 1 to 65535".into()),
    }
    Ok(url.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    fn warnings_of(text: &str) -> Vec<String> {
        Config::from_json(text).1
    }

    /// Exactly one warning, which starts with `key` and contains `needle`.
    fn one_warning(text: &str, key: &str, needle: &str) -> Config {
        let (config, warnings) = Config::from_json(text);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with(key), "{warnings:?}");
        assert!(warnings[0].contains(needle), "{warnings:?}");
        config
    }

    /// A fresh directory for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skiff-config-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // -- locations ----------------------------------------------------

    // (Unix path rules: `/xdg` is not absolute for a Windows `Path`.)
    #[cfg(unix)]
    #[test]
    fn unix_paths() {
        let env = PathEnv { home: os("/home/ada"), ..Default::default() };
        assert_eq!(config_dir_in(&env), Some(PathBuf::from("/home/ada/.config/skiff")));
        assert_eq!(config_path_in(&env), Some(PathBuf::from("/home/ada/.config/skiff/config.json")));
        assert_eq!(keymap_path_in(&env, &|_| false), Some(PathBuf::from("/home/ada/.config/skiff/keys.json")));
        assert_eq!(legacy_config_dir_in(&env), None);

        let xdg = PathEnv { xdg_config_home: os("/xdg"), ..env.clone() };
        assert_eq!(config_path_in(&xdg), Some(PathBuf::from("/xdg/skiff/config.json")));
        assert_eq!(keymap_path_in(&xdg, &|_| true), Some(PathBuf::from("/xdg/skiff/keys.json")));

        // Relative and empty XDG values are ignored; APPDATA means nothing here.
        let odd = PathEnv { xdg_config_home: os("relative"), appdata: os("/appdata"), ..env.clone() };
        assert_eq!(config_dir_in(&odd), Some(PathBuf::from("/home/ada/.config/skiff")));
        let empty = PathEnv { xdg_config_home: os(""), ..env.clone() };
        assert_eq!(config_dir_in(&empty), Some(PathBuf::from("/home/ada/.config/skiff")));

        assert_eq!(config_dir_in(&PathEnv::default()), None);
        assert_eq!(config_path_in(&PathEnv::default()), None);
        assert_eq!(keymap_path_in(&PathEnv::default(), &|_| true), None);
    }

    #[test]
    fn overrides_win_everywhere() {
        for windows in [false, true] {
            let env = PathEnv {
                windows,
                home: os("/home/ada"),
                appdata: os("C:\\Users\\ada\\AppData\\Roaming"),
                userprofile: os("C:\\Users\\ada"),
                skiff_config: os("/tmp/my.json"),
                skiff_keymap: os("/tmp/keys.json"),
                ..Default::default()
            };
            assert_eq!(config_path_in(&env), Some(PathBuf::from("/tmp/my.json")));
            // Even when a file exists at the usual places.
            assert_eq!(keymap_path_in(&env, &|_| true), Some(PathBuf::from("/tmp/keys.json")));
        }
        // An override needs no home directory at all.
        let bare = PathEnv { skiff_config: os("/x.json"), ..Default::default() };
        assert_eq!(config_path_in(&bare), Some(PathBuf::from("/x.json")));
    }

    #[test]
    fn windows_paths() {
        let appdata = PathBuf::from("C:\\Users\\ada\\AppData\\Roaming");
        let env = PathEnv {
            windows: true,
            appdata: os("C:\\Users\\ada\\AppData\\Roaming"),
            userprofile: os("C:\\Users\\ada"),
            // Set by Git Bash and the like; not the Windows location.
            home: os("/c/Users/ada"),
            ..Default::default()
        };
        assert_eq!(config_dir_in(&env), Some(appdata.join("skiff")));
        assert_eq!(config_path_in(&env), Some(appdata.join("skiff").join("config.json")));

        let new = appdata.join("skiff").join("keys.json");
        let old = PathBuf::from("C:\\Users\\ada").join(".config").join("skiff").join("keys.json");
        assert_eq!(legacy_config_dir_in(&env), old.parent().map(Path::to_path_buf));
        // Nothing anywhere: the new place.
        assert_eq!(keymap_path_in(&env, &|_| false), Some(new.clone()));
        // Only the old file exists: it is still used.
        assert_eq!(keymap_path_in(&env, &|p| p == old), Some(old.clone()));
        // Both exist: the new place wins.
        assert_eq!(keymap_path_in(&env, &|_| true), Some(new.clone()));

        // The old location honoured XDG_CONFIG_HOME; the new one does not.
        let xdg = PathEnv { xdg_config_home: os("D:\\cfg"), ..env.clone() };
        let old_xdg = PathBuf::from("D:\\cfg").join("skiff").join("keys.json");
        assert_eq!(config_dir_in(&xdg), Some(appdata.join("skiff")));
        assert_eq!(keymap_path_in(&xdg, &|p| p == old_xdg), Some(old_xdg.clone()));

        // Without APPDATA, its usual place under the profile.
        let no_appdata = PathEnv { appdata: None, ..env.clone() };
        assert_eq!(
            config_dir_in(&no_appdata),
            Some(PathBuf::from("C:\\Users\\ada").join("AppData").join("Roaming").join("skiff"))
        );
    }

    // -- schema and docs ----------------------------------------------

    #[test]
    fn example_parses_without_warnings_and_sets_everything() {
        let (config, warnings) = Config::from_json(EXAMPLE);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            config.kubeconfigs(),
            Some(&["~/.kube/config".to_string(), "~/.kube/conf.d/*.yaml".to_string()][..])
        );
        assert_eq!(config.updates(), Updates { check: true, channel: Channel::Stable });
        assert_eq!(config.terminal(), Some("iTerm"));
        assert_eq!(config.zoom(), Some(1.0));
        assert_eq!(config.cluster_names().collect::<Vec<_>>(), ["production"]);
        assert_eq!(
            config.cluster("production"),
            ClusterConfig {
                read_only: true,
                confirm: true,
                prometheus: Some(PrometheusOverride {
                    namespace: "monitoring".into(),
                    service: "prometheus-operated".into(),
                    port: 9090,
                    scheme: "http".into(),
                    path_prefix: String::new(),
                }),
                grafana_url: Some("https://grafana.example.com".into()),
            }
        );
        assert_eq!(config.cluster("other"), ClusterConfig::default());
    }

    /// Every leaf key path of `v`, with cluster names as `<context>`.
    fn leaf_paths(v: &Value, prefix: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    let k = if prefix == "clusters" { "<context>" } else { k.as_str() };
                    let path = if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") };
                    leaf_paths(child, &path, out);
                }
            }
            _ => out.push(prefix.to_string()),
        }
    }

    #[test]
    fn docs_and_example_cover_the_same_keys() {
        let mut in_example = Vec::new();
        leaf_paths(&serde_json::from_str(EXAMPLE).unwrap(), "", &mut in_example);
        in_example.sort();
        let mut documented: Vec<String> = KEYS.iter().map(|k| k.key.to_string()).collect();
        documented.sort();
        assert_eq!(in_example, documented);

        let text = documentation();
        for k in KEYS {
            assert!(text.contains(k.key) && text.contains(k.doc), "{}", k.key);
            assert!(!k.doc.is_empty() && !k.kind.is_empty() && !k.default.is_empty());
        }
        assert!(text.contains(EXAMPLE));
    }

    #[test]
    fn known_keys_come_from_the_docs() {
        assert_eq!(known_keys(""), ["kubeconfigs", "updates", "terminal", "theme", "zoom", "clusters"]);
        assert_eq!(known_keys("updates"), ["check", "channel"]);
        assert_eq!(known_keys("clusters.<context>"), ["read_only", "confirm", "prometheus", "grafana_url"]);
        assert_eq!(
            known_keys("clusters.<context>.prometheus"),
            ["namespace", "service", "port", "scheme", "path_prefix"]
        );
    }

    // -- lenient loading ----------------------------------------------

    #[test]
    fn empty_and_missing_are_defaults() {
        for text in ["", "  \n", "{}", "\u{feff}{}"] {
            let (config, warnings) = Config::from_json(text);
            assert!(warnings.is_empty(), "{text:?}: {warnings:?}");
            assert_eq!(config.kubeconfigs(), None);
            assert_eq!(config.updates(), Updates { check: true, channel: Channel::Stable });
            assert_eq!(config.terminal(), None);
            assert_eq!(config.zoom(), None);
        }
        let dir = temp_dir("missing");
        let (config, warnings) = Config::load_from(&dir.join("config.json"));
        assert!(warnings.is_empty());
        assert_eq!(config.path(), Some(dir.join("config.json").as_path()));
        // A directory where the file should be: a warning, not a failure.
        let (_, warnings) = Config::load_from(&dir);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("could not be read"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_is_one_warning() {
        let w = warnings_of("{ \"zoom\": 1.5, ");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("not valid JSON") && w[0].contains("using the defaults"), "{w:?}");
        let w = warnings_of("[1, 2]");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("JSON object") && w[0].contains("a list"), "{w:?}");
    }

    #[test]
    fn an_unusable_file_is_told_apart_from_one_with_mistakes() {
        // Not JSON, or not an object: nothing is known about the settings.
        for text in ["{ \"clusters\": { \"prod\": { \"read_only\": true } }, }", "[1, 2]", "{"] {
            let (config, _) = Config::from_json(text);
            assert!(config.unusable().is_some(), "{text}");
        }
        assert!(Config::from_json("{ \"zoom\": 1.5, ").0.unusable().unwrap().contains("not valid JSON"));
        // A mistake in a key, an empty file and a missing file are usable.
        for text in ["{ \"zoom\": \"big\", \"nope\": 1 }", "", "{}"] {
            assert_eq!(Config::from_json(text).0.unusable(), None, "{text}");
        }
        let dir = temp_dir("unusable");
        let path = dir.join("config.json");
        assert_eq!(Config::load_from(&path).0.unusable(), None);
        // A file that cannot be read (here: a directory) is unusable too.
        assert!(Config::load_from(&dir).0.unusable().unwrap().contains("could not be read"));

        // Saving ends it once the file is fixed, and the warnings about
        // single keys can be asked for afterwards.
        std::fs::write(&path, "{ oops").unwrap();
        let (mut config, _) = Config::load_from(&path);
        config.set_zoom(2.0);
        assert!(config.save().is_err() && config.unusable().is_some());
        std::fs::write(&path, "{ \"terminal\": 5 }").unwrap();
        assert_eq!(config.save(), Ok(true));
        assert_eq!(config.unusable(), None);
        let warnings = config.warnings();
        assert!(warnings.len() == 1 && warnings[0].starts_with("terminal"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_keys_warn_at_every_level() {
        let (config, w) = Config::from_json(
            r#"{ "zoom": 2, "colour": "red",
                 "updates": { "chanel": "nightly" },
                 "clusters": { "prod": { "readonly": true, "read_only": true,
                                         "prometheus": { "namespace": "m", "service": "p", "tls": true } } } }"#,
        );
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(w.iter().any(|m| m.starts_with("colour: unknown key") && m.contains("kubeconfigs")));
        assert!(w.iter().any(|m| m.starts_with("updates.chanel: unknown key") && m.contains("channel")));
        assert!(w.iter().any(|m| m.starts_with("clusters.prod.readonly: unknown key") && m.contains("read_only")));
        assert!(w.iter().any(|m| m.starts_with("clusters.prod.prometheus.tls: unknown key")));
        // The rest of the file still counts, including the override with an unknown key.
        assert_eq!(config.zoom(), Some(2.0));
        assert!(config.cluster("prod").read_only);
        assert_eq!(config.cluster("prod").prometheus.map(|p| p.service), Some("p".into()));
    }

    #[test]
    fn kubeconfigs_mistakes() {
        let c = one_warning(r#"{ "kubeconfigs": 5 }"#, "kubeconfigs:", "list of paths");
        assert_eq!(c.kubeconfigs(), None);
        let c = one_warning(r#"{ "kubeconfigs": ["a", 5, "b"] }"#, "kubeconfigs[1]:", "a number");
        assert_eq!(c.kubeconfigs(), Some(&["a".to_string(), "b".to_string()][..]));
        let c = one_warning(r#"{ "kubeconfigs": ["a", " "] }"#, "kubeconfigs[1]:", "empty path");
        assert_eq!(c.kubeconfigs().map(<[String]>::len), Some(1));
        one_warning(r#"{ "kubeconfigs": "" }"#, "kubeconfigs:", "list of paths");

        // One path on its own is accepted; an empty list means "not set".
        let (c, w) = Config::from_json(r#"{ "kubeconfigs": "~/k.yaml" }"#);
        assert!(w.is_empty());
        assert_eq!(c.kubeconfigs(), Some(&["~/k.yaml".to_string()][..]));
        let (c, w) = Config::from_json(r#"{ "kubeconfigs": [] }"#);
        assert!(w.is_empty());
        assert_eq!(c.kubeconfigs(), None);
    }

    #[test]
    fn updates_mistakes() {
        let c = one_warning(r#"{ "updates": true }"#, "updates:", "object");
        assert_eq!(c.updates(), Updates::default());
        let c = one_warning(r#"{ "updates": { "check": "no", "channel": "nightly" } }"#, "updates.check:", "true or false");
        assert_eq!(c.updates(), Updates { check: true, channel: Channel::Nightly });
        let c = one_warning(r#"{ "updates": { "check": false, "channel": "beta" } }"#, "updates.channel:", "\"beta\"");
        assert_eq!(c.updates(), Updates { check: false, channel: Channel::Stable });
        one_warning(r#"{ "updates": { "channel": 2 } }"#, "updates.channel:", "a number");

        let (c, w) = Config::from_json(r#"{ "updates": { "channel": "Nightly" } }"#);
        assert!(w.is_empty());
        assert_eq!(c.updates().channel, Channel::Nightly);
        assert_eq!(Channel::from_name(Channel::Nightly.name()), Some(Channel::Nightly));
        assert_eq!(Channel::from_name(Channel::Stable.name()), Some(Channel::Stable));
    }

    #[test]
    fn terminal_mistakes() {
        assert_eq!(one_warning(r#"{ "terminal": 1 }"#, "terminal:", "a number").terminal(), None);
        assert_eq!(one_warning(r#"{ "terminal": " " }"#, "terminal:", "empty").terminal(), None);
        assert_eq!(Config::from_json(r#"{ "terminal": " kitty " }"#).0.terminal(), Some("kitty"));
    }

    #[test]
    fn theme_is_one_of_three() {
        assert_eq!(Config::from_json("{}").0.theme(), None);
        for (name, theme) in [("dark", Theme::Dark), ("Light", Theme::Light), ("system", Theme::System)] {
            assert_eq!(Config::from_json(&format!(r#"{{ "theme": "{name}" }}"#)).0.theme(), Some(theme));
        }
        assert_eq!(one_warning(r#"{ "theme": "pink" }"#, "theme:", "pink").theme(), None);
        assert_eq!(one_warning(r#"{ "theme": true }"#, "theme:", "true/false").theme(), None);
        let mut c = Config::default();
        c.set_theme(Theme::System);
        assert_eq!(c.theme(), Some(Theme::System));
        assert_eq!(c.raw["theme"], json!("system"));
    }

    #[test]
    fn removing_the_last_kubeconfig_entry_removes_the_key() {
        let (mut c, _) = Config::from_json(r#"{ "kubeconfigs": ["~/a", "~/b"] }"#);
        c.remove_kubeconfig("~/missing");
        assert_eq!(c.kubeconfigs().map(<[String]>::len), Some(2));
        c.remove_kubeconfig("~/a");
        assert_eq!(c.kubeconfigs(), Some(&["~/b".to_string()][..]));
        c.remove_kubeconfig("~/b");
        assert_eq!(c.kubeconfigs(), None);
        assert!(!c.raw.contains_key("kubeconfigs"));
    }

    #[test]
    fn zoom_is_clamped() {
        assert_eq!(one_warning(r#"{ "zoom": 10 }"#, "zoom:", "using 3.0").zoom(), Some(3.0));
        assert_eq!(one_warning(r#"{ "zoom": 0.1 }"#, "zoom:", "using 0.5").zoom(), Some(0.5));
        assert_eq!(one_warning(r#"{ "zoom": -2 }"#, "zoom:", "outside").zoom(), Some(0.5));
        assert_eq!(one_warning(r#"{ "zoom": "big" }"#, "zoom:", "number").zoom(), None);
        for (text, want) in [("0.5", 0.5), ("3", 3.0), ("3.0", 3.0), ("1.25", 1.25)] {
            let (c, w) = Config::from_json(&format!("{{ \"zoom\": {text} }}"));
            assert!(w.is_empty(), "{text}: {w:?}");
            assert_eq!(c.zoom(), Some(want));
        }

        let mut c = Config::default();
        c.set_zoom(99.0);
        assert_eq!(c.zoom(), Some(3.0));
        c.set_zoom(0.0);
        assert_eq!(c.zoom(), Some(0.5));
        c.set_zoom(f32::NAN);
        assert_eq!(c.zoom(), Some(0.5));
        c.set_zoom(1.1);
        assert_eq!(c.raw.get("zoom"), Some(&json!(1.1)));
    }

    #[test]
    fn clusters_mistakes() {
        one_warning(r#"{ "clusters": [] }"#, "clusters:", "a list");
        let c = one_warning(r#"{ "clusters": { "a": true, "b": { "read_only": true } } }"#, "clusters.a:", "object");
        assert!(c.cluster("b").read_only && !c.cluster("a").read_only);
        one_warning(r#"{ "clusters": { "": {} } }"#, "clusters:", "empty context name");
        let c = one_warning(r#"{ "clusters": { "a": { "read_only": "yes", "confirm": true } } }"#, "clusters.a.read_only:", "true or false");
        assert_eq!((c.cluster("a").read_only, c.cluster("a").confirm), (false, true));
        one_warning(r#"{ "clusters": { "a": { "confirm": 1 } } }"#, "clusters.a.confirm:", "true or false");
        // Context names may contain anything, dots and slashes included.
        let (c, w) = Config::from_json(r#"{ "clusters": { "arn:aws:eks:eu-1:1:cluster/x.y": { "confirm": true } } }"#);
        assert!(w.is_empty());
        assert!(c.cluster("arn:aws:eks:eu-1:1:cluster/x.y").confirm);
    }

    #[test]
    fn prometheus_mistakes_drop_the_override() {
        let prom = |body: &str| {
            let (c, w) = Config::from_json(&format!("{{ \"clusters\": {{ \"a\": {{ \"prometheus\": {body} }} }} }}"));
            (c.cluster("a").prometheus, w)
        };
        let (p, w) = prom("\"monitoring/prometheus\"");
        assert_eq!(p, None);
        assert!(w.len() == 1 && w[0].starts_with("clusters.a.prometheus:"), "{w:?}");

        // Each bad field is named, then one line says the override is off.
        for (body, key) in [
            (r#"{ "service": "p" }"#, "namespace"),
            (r#"{ "namespace": "m", "service": "" }"#, "service"),
            (r#"{ "namespace": 1, "service": "p" }"#, "namespace"),
            (r#"{ "namespace": "m", "service": "p", "port": 0 }"#, "port"),
            (r#"{ "namespace": "m", "service": "p", "port": 70000 }"#, "port"),
            (r#"{ "namespace": "m", "service": "p", "port": "9090" }"#, "port"),
            (r#"{ "namespace": "m", "service": "p", "port": 90.5 }"#, "port"),
            (r#"{ "namespace": "m", "service": "p", "scheme": "ftp" }"#, "scheme"),
            (r#"{ "namespace": "m", "service": "p", "scheme": 1 }"#, "scheme"),
            (r#"{ "namespace": "m", "service": "p", "path_prefix": 1 }"#, "path_prefix"),
        ] {
            let (p, w) = prom(body);
            assert_eq!(p, None, "{body}");
            assert_eq!(w.len(), 2, "{body}: {w:?}");
            assert!(w[0].starts_with(&format!("clusters.a.prometheus.{key}:")), "{body}: {w:?}");
            assert!(w[1].starts_with("clusters.a.prometheus:") && w[1].contains("automatically"), "{w:?}");
        }

        // Defaults for what is left out.
        let (p, w) = prom(r#"{ "namespace": "m", "service": "p" }"#);
        assert!(w.is_empty());
        assert_eq!(
            p,
            Some(PrometheusOverride {
                namespace: "m".into(),
                service: "p".into(),
                port: 9090,
                scheme: "http".into(),
                path_prefix: String::new()
            })
        );
        let (p, w) = prom(r#"{ "namespace": "m", "service": "p", "port": 8481, "scheme": "HTTPS", "path_prefix": "/select/0/prometheus" }"#);
        assert!(w.is_empty());
        let p = p.unwrap();
        assert_eq!((p.port, p.scheme.as_str(), p.path_prefix.as_str()), (8481, "https", "/select/0/prometheus"));
    }

    #[test]
    fn grafana_url_must_be_http() {
        for good in [
            "https://grafana.example.com",
            "http://grafana.example.com:3000",
            "HTTPS://Grafana.example.com/grafana",
            "http://10.0.0.5:3000/d/abc?orgId=1&var-x=y",
            "http://[::1]:3000",
            "http://localhost",
        ] {
            assert_eq!(http_url(good).as_deref(), Ok(good), "{good}");
        }
        assert_eq!(http_url(" https://g.example.com/ ").as_deref(), Ok("https://g.example.com"));
        assert_eq!(http_url("https://g.example.com/grafana//").as_deref(), Ok("https://g.example.com/grafana"));

        for bad in [
            "",
            "grafana.example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://grafana.example.com",
            "https://",
            "https:///path",
            "https://user:pw@grafana.example.com",
            "https://grafana.example.com/a b",
            "https://grafana.example.com/\"&calc",
            "https://grafana.example.com\\@evil.example",
            "https://grafana.example.com:port",
            "https://grafana.example.com:0",
            "https://-grafana.example.com",
            "https://graf$(id).example.com",
            "https://[::1",
            "https://[::1]x",
            "http://a\nb",
        ] {
            assert!(http_url(bad).is_err(), "{bad:?} was accepted");
        }

        let c = one_warning(r#"{ "clusters": { "a": { "grafana_url": "file:///x" } } }"#, "clusters.a.grafana_url:", "http://");
        assert_eq!(c.cluster("a").grafana_url, None);
        one_warning(r#"{ "clusters": { "a": { "grafana_url": 3000 } } }"#, "clusters.a.grafana_url:", "a number");
        one_warning(r#"{ "clusters": { "a": { "grafana_url": "https://u:p@g.example" } } }"#, "clusters.a.grafana_url:", "password");
    }

    // -- changing and saving ------------------------------------------

    const WITH_UNKNOWN: &str = r#"{
  "future_key": { "nested": [1, 2, { "x": null }] },
  "kubeconfigs": ["~/.kube/config"],
  "clusters": {
    "prod": { "read_only": false, "note": "ask ops first", "grafana_url": "not a url" },
    "dev": { "confirm": true }
  },
  "updates": { "channel": "stable", "mirror": "eu" },
  "zoom": "huge"
}"#;

    #[test]
    fn save_keeps_unknown_keys_and_other_content() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("config.json");
        std::fs::write(&path, WITH_UNKNOWN).unwrap();
        let (mut config, warnings) = Config::load_from(&path);
        assert_eq!(warnings.len(), 5, "{warnings:?}");

        config.set_read_only("prod", true);
        config.set_confirm("new/ctx", true);
        config.set_update_channel(Channel::Nightly);
        config.set_update_check(false);
        config.set_zoom(1.25);
        config.set_terminal(Some("kitty"));
        config.add_kubeconfig("~/work.yaml");
        assert!(config.is_dirty());
        assert_eq!(config.save(), Ok(true));
        assert!(!config.is_dirty());

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("}\n") && text.contains("\n  \""), "pretty-printed: {text}");
        let saved: Value = serde_json::from_str(&text).unwrap();
        let mut want: Value = serde_json::from_str(WITH_UNKNOWN).unwrap();
        want["clusters"]["prod"]["read_only"] = json!(true);
        want["clusters"]["new/ctx"] = json!({ "confirm": true });
        want["updates"]["channel"] = json!("nightly");
        want["updates"]["check"] = json!(false);
        want["zoom"] = json!(1.25);
        want["terminal"] = json!("kitty");
        want["kubeconfigs"] = json!(["~/.kube/config", "~/work.yaml"]);
        assert_eq!(saved, want);

        // What was loaded again matches what the app was shown.
        let (again, _) = Config::load_from(&path);
        assert_eq!(again.settings, config.settings);
        assert!(again.cluster("prod").read_only && again.cluster("new/ctx").confirm);
        assert_eq!(again.updates(), Updates { check: false, channel: Channel::Nightly });
        assert_eq!(again.kubeconfigs().map(<[String]>::len), Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_changed_writes_nothing() {
        let dir = temp_dir("nochange");
        let path = dir.join("sub").join("config.json");
        let (mut config, _) = Config::load_from(&path);
        assert_eq!(config.save(), Ok(false));
        assert!(!path.exists() && !path.parent().unwrap().exists());

        // Setting a value to what the file already says is not a change.
        let original = "{\"zoom\":1.5,\n\n \"updates\":{\"channel\":\"nightly\"}, \"terminal\":\"kitty\",\n\"kubeconfigs\":[\"a\"], \"clusters\":{\"p\":{\"read_only\":true}}}";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, original).unwrap();
        let (mut config, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty());
        config.set_zoom(1.5);
        config.set_update_channel(Channel::Nightly);
        config.set_terminal(Some("kitty"));
        config.add_kubeconfig("a");
        config.set_kubeconfigs(Some(&["a".to_string()]));
        config.set_read_only("p", true);
        config.set_terminal(Some("kitty"));
        assert!(!config.is_dirty());
        assert_eq!(config.save(), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original, "the file was rewritten");

        // A change and its reverse leave the file byte for byte as it was.
        config.set_read_only("p", false);
        config.set_read_only("p", true);
        assert_eq!(config.save(), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!config.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_the_directory_and_a_private_file() {
        let dir = temp_dir("create");
        let path = dir.join("a").join("b").join("config.json");
        let (mut config, _) = Config::load_from(&path);
        config.set_read_only("prod", true);
        assert_eq!(config.save(), Ok(true));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\n  \"clusters\": {\n    \"prod\": {\n      \"read_only\": true\n    }\n  }\n}\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // No temporary file is left next to it.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, [OsString::from("config.json")]);

        // A second save replaces the file (and tightens loose permissions).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        config.set_read_only("prod", false);
        assert_eq!(config.save(), Ok(true));
        assert!(!Config::load_from(&path).0.cluster("prod").read_only);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_keeps_edits_made_on_disk_meanwhile() {
        let dir = temp_dir("meanwhile");
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{ "zoom": 1.0 }"#).unwrap();
        let (mut config, _) = Config::load_from(&path);
        config.set_read_only("prod", true);
        // The user edits the file in an editor while the app runs.
        std::fs::write(&path, r#"{ "zoom": 2.0, "terminal": "kitty", "mine": 1 }"#).unwrap();
        assert_eq!(config.save(), Ok(true));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({ "zoom": 2.0, "terminal": "kitty", "mine": 1, "clusters": { "prod": { "read_only": true } } })
        );
        // And the config now reflects the file.
        assert_eq!(config.zoom(), Some(2.0));
        assert_eq!(config.terminal(), Some("kitty"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_to_overwrite_a_broken_file() {
        let dir = temp_dir("broken");
        let path = dir.join("config.json");
        let broken = "{ \"kubeconfigs\": [\"a\", \"b\"], oops";
        std::fs::write(&path, broken).unwrap();
        let (mut config, warnings) = Config::load_from(&path);
        assert_eq!(warnings.len(), 1);
        config.set_zoom(2.0);
        let err = config.save().unwrap_err();
        assert!(err.contains("not valid JSON") && err.contains("config.json"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        // Still pending: saving works once the file is fixed.
        assert!(config.is_dirty());
        std::fs::write(&path, "{ \"kubeconfigs\": [\"a\", \"b\"] }").unwrap();
        assert_eq!(config.save(), Ok(true));
        assert_eq!(config.kubeconfigs().map(<[String]>::len), Some(2));
        assert_eq!(config.zoom(), Some(2.0));

        // No path at all (no home directory): an error, not a panic.
        let mut nowhere = Config::default();
        nowhere.set_zoom(2.0);
        assert!(nowhere.save().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_a_symlink() {
        let dir = temp_dir("symlink");
        let real = dir.join("dotfiles").join("skiff.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "{}").unwrap();
        let link = dir.join("config.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (mut config, _) = Config::load_from(&link);
        config.set_zoom(2.0);
        assert_eq!(config.save(), Ok(true));
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(&real).unwrap().contains("\"zoom\": 2.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutators() {
        let mut c = Config::default();
        // The first entry starts the list; duplicates and blanks do nothing.
        c.add_kubeconfig(" ~/a.yaml ");
        c.add_kubeconfig("~/a.yaml");
        c.add_kubeconfig("  ");
        c.add_kubeconfig("~/b.yaml");
        assert_eq!(c.kubeconfigs(), Some(&["~/a.yaml".to_string(), "~/b.yaml".to_string()][..]));
        c.set_kubeconfigs(None);
        assert_eq!(c.kubeconfigs(), None);
        assert!(!c.raw.contains_key("kubeconfigs"));

        c.set_terminal(Some("Ghostty"));
        assert_eq!(c.terminal(), Some("Ghostty"));
        c.set_terminal(Some(" "));
        assert_eq!(c.terminal(), None);
        c.set_terminal(None);
        assert!(!c.raw.contains_key("terminal"));

        c.set_update_check(false);
        c.set_update_channel(Channel::Nightly);
        assert_eq!(c.updates(), Updates { check: false, channel: Channel::Nightly });

        // A mistake in the way of a change is replaced by the change.
        let (mut c, w) = Config::from_json(r#"{ "clusters": 5, "updates": "yes" }"#);
        assert_eq!(w.len(), 2);
        c.set_read_only("prod", true);
        c.set_update_channel(Channel::Nightly);
        assert!(c.cluster("prod").read_only);
        assert_eq!(c.updates().channel, Channel::Nightly);
        // Removing under something that is not there creates nothing.
        let mut raw = Map::new();
        apply(&mut raw, &Edit { path: vec!["a".into(), "b".into()], value: None });
        assert!(raw.is_empty());
    }
}
