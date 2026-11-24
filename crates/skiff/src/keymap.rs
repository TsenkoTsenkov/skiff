//! Keyboard bindings for moving around the app (vim-style by default),
//! customisable from a JSON file.
//!
//! The file maps action names to lists of keys; actions it leaves out keep
//! their defaults, and an empty list unbinds an action:
//!
//! ```json
//! {
//!   "down": ["j", "ArrowDown"],
//!   "up": ["k", "ArrowUp"],
//!   "focus_left": ["h"],
//!   "focus_right": ["l"],
//!   "bottom": ["Shift+G", "End"]
//! }
//! ```
//!
//! A key is an egui key name (`j`, `ArrowDown`, `PageUp`, `Enter`, `/`, `[`,
//! `F5`, …), optionally after modifiers joined with `+`: `Ctrl`, `Shift`,
//! `Alt` (or `Option`), `Cmd` (`Ctrl` on Linux and Windows, `⌘` on macOS).
//! A single upper-case letter means that letter with Shift: `G` is `Shift+G`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{Event, InputState, Key, Modifiers};

/// Something a key can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// Move the selection (table rows, sidebar entries) down one.
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    /// Open what is selected: a row's details, or a sidebar entry.
    Open,
    /// Move the keyboard to the pane on the left (table → sidebar).
    FocusLeft,
    /// Move the keyboard to the pane on the right (sidebar → table), or
    /// open the selected row's details from the table.
    FocusRight,
    /// Next / previous tab of the detail panel.
    NextTab,
    PrevTab,
    /// Focus the table filter.
    Filter,
    /// Show the list of keyboard shortcuts.
    Help,
}

/// Where a shortcut applies, for the list of shortcuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Global,
    Navigation,
    Tables,
    Details,
    Logs,
    Shell,
}

impl Group {
    pub const ALL: [Group; 6] =
        [Group::Global, Group::Navigation, Group::Tables, Group::Details, Group::Logs, Group::Shell];

    pub fn title(self) -> &'static str {
        match self {
            Group::Global => "Everywhere",
            Group::Navigation => "Moving around",
            Group::Tables => "Tables",
            Group::Details => "Details",
            Group::Logs => "Logs",
            Group::Shell => "Shell",
        }
    }
}

impl Action {
    pub const ALL: [Action; 13] = [
        Action::Down,
        Action::Up,
        Action::PageDown,
        Action::PageUp,
        Action::Top,
        Action::Bottom,
        Action::Open,
        Action::FocusLeft,
        Action::FocusRight,
        Action::NextTab,
        Action::PrevTab,
        Action::Filter,
        Action::Help,
    ];

    /// The name used in the keymap file.
    pub fn name(self) -> &'static str {
        match self {
            Action::Down => "down",
            Action::Up => "up",
            Action::PageDown => "page_down",
            Action::PageUp => "page_up",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::Open => "open",
            Action::FocusLeft => "focus_left",
            Action::FocusRight => "focus_right",
            Action::NextTab => "next_tab",
            Action::PrevTab => "prev_tab",
            Action::Filter => "filter",
            Action::Help => "help",
        }
    }

    /// What the action does, for the list of shortcuts.
    pub fn title(self) -> &'static str {
        match self {
            Action::Down => "Next row or sidebar entry",
            Action::Up => "Previous row or sidebar entry",
            Action::PageDown => "Down a page",
            Action::PageUp => "Up a page",
            Action::Top => "First row",
            Action::Bottom => "Last row",
            Action::Open => "Open the selected row or sidebar entry",
            Action::FocusLeft => "Move to the sidebar",
            Action::FocusRight => "Back to the table, or open the selected row",
            Action::NextTab => "Next tab",
            Action::PrevTab => "Previous tab",
            Action::Filter => "Filter the table",
            Action::Help => "Show keyboard shortcuts",
        }
    }

    /// Where the action applies.
    pub fn group(self) -> Group {
        match self {
            Action::Down | Action::Up | Action::PageDown | Action::PageUp | Action::Top | Action::Bottom => {
                Group::Navigation
            }
            Action::Open | Action::FocusLeft | Action::FocusRight => Group::Navigation,
            Action::NextTab | Action::PrevTab => Group::Details,
            Action::Filter => Group::Tables,
            Action::Help => Group::Global,
        }
    }

    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == name)
    }

    fn defaults(self) -> &'static [&'static str] {
        match self {
            Action::Down => &["j", "ArrowDown"],
            Action::Up => &["k", "ArrowUp"],
            Action::PageDown => &["Ctrl+d", "PageDown"],
            Action::PageUp => &["Ctrl+u", "PageUp"],
            Action::Top => &["g", "Home"],
            Action::Bottom => &["G", "End"],
            Action::Open => &["Enter"],
            Action::FocusLeft => &["h", "ArrowLeft"],
            Action::FocusRight => &["l", "ArrowRight"],
            Action::NextTab => &["]"],
            Action::PrevTab => &["["],
            Action::Filter => &["/"],
            Action::Help => &["?"],
        }
    }
}

/// A key with the modifiers that must be held with it (exactly those).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: Key,
    pub mods: Modifiers,
}

impl Binding {
    pub fn parse(text: &str) -> Result<Binding, String> {
        let text = text.trim();
        let bad = || format!("unknown key \"{text}\"");
        // Split off modifiers; the key itself may be "+".
        let (mod_part, key_part) = match text.rfind('+') {
            Some(i) if i + 1 < text.len() => (&text[..i], &text[i + 1..]),
            Some(i) if i > 0 => (&text[..i - 1], "+"),
            _ => ("", text),
        };
        let mut mods = Modifiers::NONE;
        for m in mod_part.split('+').filter(|m| !m.is_empty()) {
            mods |= match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => Modifiers::CTRL,
                "shift" => Modifiers::SHIFT,
                "alt" | "option" | "opt" => Modifiers::ALT,
                "cmd" | "command" | "super" | "meta" => Modifiers::COMMAND,
                _ => return Err(format!("unknown modifier \"{m}\" in \"{text}\"")),
            };
        }
        let key = Key::from_name(key_part).ok_or_else(bad)?;
        let mut chars = key_part.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_uppercase() {
                mods |= Modifiers::SHIFT;
            }
        }
        Ok(Binding { key, mods })
    }

    /// Whether a press of `key` with `modifiers` held is this binding.
    ///
    /// A symbol that is typed with Shift on most keyboards (`?`, `+`, `:`)
    /// matches with or without Shift held: where it sits differs between
    /// layouts, and whoever binds `?` means the character.
    pub fn matches(&self, key: Key, modifiers: Modifiers) -> bool {
        if key != self.key {
            return false;
        }
        let mut held = modifiers;
        if shifted_symbol(key) && !self.mods.shift {
            held.shift = false;
        }
        held.matches_exact(self.mods)
    }

    fn pressed_in(&self, i: &InputState) -> bool {
        i.events
            .iter()
            .any(|e| matches!(e, Event::Key { key, pressed: true, modifiers, .. } if self.matches(*key, *modifiers)))
    }

    /// The binding as it is shown to the user: `j`, `Shift+G`, `Ctrl+D`,
    /// `↓`. `mac` names the modifiers the way a Mac keyboard does.
    pub fn label(&self, mac: bool) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let m = self.mods;
        // (`command` is Ctrl everywhere but on a Mac.)
        if m.ctrl || (m.command && !mac) {
            parts.push("Ctrl");
        }
        if m.mac_cmd || (m.command && mac) {
            parts.push("Cmd");
        }
        if m.alt {
            parts.push(if mac { "Option" } else { "Alt" });
        }
        if m.shift {
            parts.push("Shift");
        }
        let name = match self.key {
            Key::ArrowDown => "↓".to_string(),
            Key::ArrowUp => "↑".to_string(),
            Key::ArrowLeft => "←".to_string(),
            Key::ArrowRight => "→".to_string(),
            Key::PageDown => "Page Down".to_string(),
            Key::PageUp => "Page Up".to_string(),
            Key::Escape => "Esc".to_string(),
            key => {
                let name = key.symbol_or_name();
                // A letter on its own is shown as it is typed.
                let letter = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic());
                if letter && parts.is_empty() {
                    name.to_ascii_lowercase()
                } else {
                    name.to_string()
                }
            }
        };
        parts.push(&name);
        parts.join("+")
    }
}

/// Keys that stand for a character typed with Shift on most layouts.
fn shifted_symbol(key: Key) -> bool {
    matches!(
        key,
        Key::Questionmark | Key::Plus | Key::Colon | Key::Exclamationmark | Key::Pipe | Key::OpenCurlyBracket | Key::CloseCurlyBracket
    )
}

impl Binding {
    /// The binding in the form the keymap file takes (what
    /// [`Binding::parse`] reads back).
    pub fn to_config(&self) -> String {
        let mut out = String::new();
        let m = self.mods;
        if m.command {
            out.push_str("Cmd+");
        } else if m.ctrl {
            out.push_str("Ctrl+");
        }
        if m.alt {
            out.push_str("Alt+");
        }
        if m.shift {
            out.push_str("Shift+");
        }
        // (Arrows by name: their symbols are not what the file takes.)
        let arrow = matches!(self.key, Key::ArrowDown | Key::ArrowUp | Key::ArrowLeft | Key::ArrowRight);
        let name = if arrow { self.key.name() } else { self.key.symbol_or_name() };
        let letter = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic());
        if letter {
            out.push_str(&name.to_ascii_lowercase());
        } else {
            out.push_str(name);
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: BTreeMap<Action, Vec<Binding>>,
}

impl Default for Keymap {
    fn default() -> Self {
        let bindings = Action::ALL
            .into_iter()
            .map(|a| (a, a.defaults().iter().map(|k| Binding::parse(k).expect("default binding")).collect()))
            .collect();
        Keymap { bindings }
    }
}

impl Keymap {
    /// The keymap from the user's file, if there is one. Problems in the file
    /// are returned alongside a keymap built from what could be understood.
    pub fn load() -> (Keymap, Option<String>) {
        let Some(path) = Self::path() else { return (Keymap::default(), None) };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (map, errors) = Keymap::from_json(&text);
                let err = (!errors.is_empty()).then(|| format!("{}: {}", path.display(), errors.join("; ")));
                (map, err)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Keymap::default(), None),
            Err(e) => (Keymap::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    /// `$SKIFF_KEYMAP`, else `keys.json` in the config directory shared with
    /// `config.json`: `$XDG_CONFIG_HOME/skiff` or `~/.config/skiff` on Linux
    /// and macOS, `%APPDATA%\skiff` on Windows (where a file that is only at
    /// the old `%USERPROFILE%\.config\skiff` location is still used).
    pub fn path() -> Option<PathBuf> {
        crate::config::keymap_path()
    }

    /// Defaults overridden by the file's entries, and what was wrong with it.
    pub fn from_json(text: &str) -> (Keymap, Vec<String>) {
        let mut map = Keymap::default();
        let mut errors = Vec::new();
        let value: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return (map, vec![format!("not valid JSON ({e})")]),
        };
        let Some(obj) = value.as_object() else {
            return (map, vec!["expected an object of action names to key lists".to_string()]);
        };
        for (name, keys) in obj {
            let Some(action) = Action::from_name(name) else {
                let known: Vec<_> = Action::ALL.iter().map(|a| a.name()).collect();
                errors.push(format!("unknown action \"{name}\" (known: {})", known.join(", ")));
                continue;
            };
            let list: Vec<&str> = match keys {
                serde_json::Value::String(s) => vec![s.as_str()],
                serde_json::Value::Array(a) => a.iter().filter_map(|k| k.as_str()).collect(),
                _ => {
                    errors.push(format!("\"{name}\" should be a list of keys"));
                    continue;
                }
            };
            let mut bindings = Vec::new();
            for k in list {
                match Binding::parse(k) {
                    Ok(b) => bindings.push(b),
                    Err(e) => errors.push(e),
                }
            }
            map.bindings.insert(action, bindings);
        }
        (map, errors)
    }

    /// A keymap file that spells out the default bindings: a starting
    /// point to edit (every line of it may be deleted again).
    pub fn starter_json() -> String {
        let defaults = Keymap::default();
        let mut out = String::from("{\n");
        for (i, action) in Action::ALL.into_iter().enumerate() {
            let keys: Vec<String> =
                defaults.bindings(action).iter().map(|b| format!("{:?}", b.to_config())).collect();
            let comma = if i + 1 < Action::ALL.len() { "," } else { "" };
            out.push_str(&format!("  {:?}: [{}]{comma}\n", action.name(), keys.join(", ")));
        }
        out.push_str("}\n");
        out
    }

    pub fn bindings(&self, action: Action) -> &[Binding] {
        self.bindings.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether a key bound to `action` was pressed this frame.
    pub fn pressed(&self, i: &InputState, action: Action) -> bool {
        self.bindings(action).iter().any(|b| b.pressed_in(i))
    }

    /// Whether the user's keymap binds `key` with these modifiers held to
    /// any action: a built-in shortcut on the same keys then stands back.
    pub fn binds(&self, key: Key, modifiers: Modifiers) -> bool {
        self.bindings.values().flatten().any(|b| b.matches(key, modifiers))
    }

    /// Whether a press of `key` this frame belongs to the keymap.
    pub fn claims(&self, i: &InputState, key: Key) -> bool {
        i.events.iter().any(
            |e| matches!(e, Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && self.binds(key, *modifiers)),
        )
    }

    /// Whether `ev` is a press of a key bound to `action`.
    pub fn is_event(&self, ev: &Event, action: Action) -> bool {
        matches!(ev, Event::Key { key, pressed: true, modifiers, .. }
            if self.bindings(action).iter().any(|b| b.matches(*key, *modifiers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys() {
        assert_eq!(Binding::parse("j").unwrap(), Binding { key: Key::J, mods: Modifiers::NONE });
        assert_eq!(Binding::parse("G").unwrap(), Binding { key: Key::G, mods: Modifiers::SHIFT });
        assert_eq!(Binding::parse("Shift+g").unwrap(), Binding { key: Key::G, mods: Modifiers::SHIFT });
        assert_eq!(Binding::parse("Ctrl+d").unwrap(), Binding { key: Key::D, mods: Modifiers::CTRL });
        assert_eq!(Binding::parse("Cmd+Shift+K").unwrap().mods, Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!(Binding::parse("ArrowDown").unwrap().key, Key::ArrowDown);
        assert_eq!(Binding::parse("/").unwrap().key, Key::Slash);
        assert_eq!(Binding::parse("+").unwrap().key, Key::Plus);
        assert_eq!(Binding::parse("Ctrl++").unwrap(), Binding { key: Key::Plus, mods: Modifiers::CTRL });
        assert!(Binding::parse("Hyper+j").is_err());
        assert!(Binding::parse("nope").is_err());
    }

    #[test]
    fn file_overrides_defaults() {
        let (map, errors) = Keymap::from_json(r#"{"down": ["n"], "up": "e", "filter": [], "bogus": ["x"], "top": ["zz"]}"#);
        assert_eq!(map.bindings(Action::Down), &[Binding::parse("n").unwrap()]);
        assert_eq!(map.bindings(Action::Up), &[Binding::parse("e").unwrap()]);
        assert!(map.bindings(Action::Filter).is_empty());
        // Untouched actions keep their defaults.
        assert_eq!(map.bindings(Action::FocusLeft), Keymap::default().bindings(Action::FocusLeft));
        assert_eq!(errors.len(), 2, "{errors:?}");
    }

    #[test]
    fn labels_read_like_keycaps() {
        let label = |text: &str, mac: bool| Binding::parse(text).unwrap().label(mac);
        assert_eq!(label("j", false), "j");
        assert_eq!(label("G", false), "Shift+G");
        assert_eq!(label("Ctrl+d", false), "Ctrl+D");
        assert_eq!(label("Cmd+k", false), "Ctrl+K");
        assert_eq!(label("Cmd+k", true), "Cmd+K");
        assert_eq!(label("Alt+Enter", true), "Option+Enter");
        assert_eq!(label("ArrowDown", false), "↓");
        assert_eq!(label("PageUp", false), "Page Up");
        assert_eq!(label("/", false), "/");
        assert_eq!(label("?", false), "?");
    }

    #[test]
    fn the_starter_file_is_the_default_keymap() {
        let text = Keymap::starter_json();
        let (map, errors) = Keymap::from_json(&text);
        assert!(errors.is_empty(), "{errors:?}");
        for action in Action::ALL {
            assert_eq!(map.bindings(action), Keymap::default().bindings(action), "{}", action.name());
            assert!(text.contains(&format!("\"{}\"", action.name())));
        }
    }

    #[test]
    fn a_shifted_symbol_matches_with_or_without_shift() {
        let map = Keymap::default();
        let ev = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        assert!(map.is_event(&ev(Key::Questionmark, Modifiers::SHIFT), Action::Help));
        assert!(map.is_event(&ev(Key::Questionmark, Modifiers::NONE), Action::Help));
        assert!(!map.is_event(&ev(Key::Questionmark, Modifiers::CTRL), Action::Help));
        // Not for ordinary keys: Shift+/ is not the filter key.
        assert!(!map.is_event(&ev(Key::Slash, Modifiers::SHIFT), Action::Filter));
    }

    #[test]
    fn a_binding_on_the_find_keys_is_the_users() {
        // What Ctrl+F arrives as on Linux and Windows.
        let ctrl = Modifiers::CTRL | Modifiers::COMMAND;
        assert!(!Keymap::default().binds(Key::F, ctrl), "the defaults leave Ctrl+F to find");
        let (map, errors) = Keymap::from_json(r#"{"page_down": ["Ctrl+f"]}"#);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(map.binds(Key::F, ctrl));
        assert!(!map.binds(Key::F, Modifiers::NONE));
        assert!(!map.binds(Key::D, ctrl), "replaced, not added to");
    }

    #[test]
    fn every_action_is_described() {
        for action in Action::ALL {
            assert!(!action.title().is_empty());
            assert!(Group::ALL.contains(&action.group()));
        }
    }

    #[test]
    fn matches_exact_modifiers() {
        let map = Keymap::default();
        let ev = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        assert!(map.is_event(&ev(Key::G, Modifiers::SHIFT), Action::Bottom));
        assert!(!map.is_event(&ev(Key::G, Modifiers::SHIFT), Action::Top));
        assert!(map.is_event(&ev(Key::G, Modifiers::NONE), Action::Top));
        assert!(!map.is_event(&ev(Key::J, Modifiers::CTRL), Action::Down));
        assert!(map.is_event(&ev(Key::D, Modifiers::CTRL), Action::PageDown));
    }
}
