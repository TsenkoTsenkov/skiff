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
}

impl Action {
    pub const ALL: [Action; 12] = [
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

    fn pressed_in(&self, i: &InputState) -> bool {
        i.events.iter().any(|e| {
            matches!(e, Event::Key { key, pressed: true, modifiers, .. }
                if *key == self.key && modifiers.matches_exact(self.mods))
        })
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

    /// `$SKIFF_KEYMAP`, else `skiff/keys.json` in `$XDG_CONFIG_HOME` or
    /// `~/.config` (on every platform).
    pub fn path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("SKIFF_KEYMAP").filter(|p| !p.is_empty()) {
            return Some(PathBuf::from(p));
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(base.join("skiff").join("keys.json"))
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

    pub fn bindings(&self, action: Action) -> &[Binding] {
        self.bindings.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether a key bound to `action` was pressed this frame.
    pub fn pressed(&self, i: &InputState, action: Action) -> bool {
        self.bindings(action).iter().any(|b| b.pressed_in(i))
    }

    /// Whether `ev` is a press of a key bound to `action`.
    pub fn is_event(&self, ev: &Event, action: Action) -> bool {
        matches!(ev, Event::Key { key, pressed: true, modifiers, .. }
            if self.bindings(action).iter().any(|b| b.key == *key && modifiers.matches_exact(b.mods)))
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
