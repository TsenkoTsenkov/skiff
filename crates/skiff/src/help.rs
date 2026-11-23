//! The list of keyboard shortcuts (`?`).
//!
//! The keys that can be rebound are read from the live [`Keymap`], so a
//! custom `keys.json` shows up here; the fixed ones are listed next to
//! them.

use egui::{Align, Layout, RichText, Ui, Vec2};
use egui_material_icons::icons;

use crate::keymap::{Action, Group, Keymap};
use crate::theme::Palette;
use crate::widgets;

/// One line of the list: the keys (any one of them does it) and what
/// they do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub keys: Vec<String>,
    pub what: String,
}

fn fixed(keys: &[&str], what: &str) -> Shortcut {
    Shortcut { keys: keys.iter().map(|k| k.to_string()).collect(), what: what.to_string() }
}

fn bound(keymap: &Keymap, action: Action, mac: bool) -> Shortcut {
    Shortcut { keys: keymap.bindings(action).iter().map(|b| b.label(mac)).collect(), what: action.title().to_string() }
}

/// Every shortcut, grouped by where it applies. `mac` names the modifier
/// keys the way a Mac keyboard does.
pub fn shortcuts(keymap: &Keymap, mac: bool) -> Vec<(Group, Vec<Shortcut>)> {
    let cmd = if mac { "Cmd" } else { "Ctrl" };
    let with_cmd = |key: &str| format!("{cmd}+{key}");
    let from_keymap =
        |group: Group| Action::ALL.into_iter().filter(move |a| a.group() == group).map(|a| bound(keymap, a, mac));
    Group::ALL
        .into_iter()
        .map(|group| {
            let list: Vec<Shortcut> = match group {
                Group::Global => {
                    let mut list = vec![
                        fixed(&[&with_cmd("K")], "Search or jump to anything"),
                        fixed(&[&with_cmd(",")], "Settings"),
                    ];
                    list.extend(from_keymap(group));
                    list.push(fixed(&[&with_cmd("="), &with_cmd("-")], "Zoom in, zoom out"));
                    list.push(fixed(&[&with_cmd("0")], "Actual size"));
                    list.push(fixed(&["Esc"], "Close what is on top"));
                    list
                }
                Group::Navigation => from_keymap(group).collect(),
                Group::Tables => {
                    let mut list: Vec<Shortcut> = from_keymap(group).collect();
                    list.push(fixed(&["Esc"], "Clear the filter"));
                    list
                }
                Group::Details => {
                    let mut list: Vec<Shortcut> = from_keymap(group).collect();
                    list.push(fixed(&[&with_cmd("S")], "Apply the YAML you edited"));
                    list.push(fixed(&["Esc"], "Close the details"));
                    list
                }
                Group::Logs => vec![
                    fixed(&["Enter"], "Next match, in the find box"),
                    fixed(&["Shift+Enter"], "Previous match"),
                ],
                Group::Shell => {
                    let (copy, paste) = if mac { ("Cmd+C", "Cmd+V") } else { ("Ctrl+Shift+C", "Ctrl+Shift+V") };
                    let mut list = vec![
                        fixed(&[copy], "Copy the selection"),
                        fixed(&[paste], "Paste"),
                        fixed(&["Shift+PgUp", "Shift+PgDn"], "Scroll back and forth"),
                    ];
                    if !mac {
                        // Ctrl+K is the shell's own "delete to the end of the line".
                        list.push(fixed(&["Ctrl+Shift+K"], "Search or jump, from a shell"));
                    }
                    list
                }
            };
            (group, list)
        })
        .collect()
}

const COLUMN: f32 = 350.0;

fn group_ui(ui: &mut Ui, pal: &Palette, group: Group, list: &[Shortcut]) {
    widgets::caption(ui, group.title(), pal);
    ui.add_space(2.0);
    for shortcut in list {
        ui.allocate_ui_with_layout(Vec2::new(COLUMN, 26.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(COLUMN);
            ui.label(RichText::new(&shortcut.what).color(pal.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if shortcut.keys.is_empty() {
                    ui.label(RichText::new("not set").color(pal.faint).size(12.0));
                }
                for key in shortcut.keys.iter().rev() {
                    widgets::keycap(ui, key, pal);
                }
            });
        });
    }
    ui.add_space(14.0);
}

/// Draws the overlay. Returns true when it should close.
pub fn show(ctx: &egui::Context, pal: &Palette, keymap: &Keymap, mac: bool) -> bool {
    let groups = shortcuts(keymap, mac);
    let mut close = false;
    let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    let resp = egui::Modal::new(egui::Id::unique("keyboard-help")).frame(frame).show(ctx, |ui| {
        let screen = ui.ctx().content_rect();
        // Two columns where there is room for them.
        let two = screen.width() >= 2.0 * COLUMN + 120.0;
        let width = if two { 2.0 * COLUMN + 36.0 } else { COLUMN };
        ui.set_width(width.min(screen.width() - 48.0));
        ui.horizontal(|ui| {
            ui.label(widgets::icon(icons::ICON_KEYBOARD, 18.0).color(pal.accent));
            ui.label(widgets::semibold("Keyboard shortcuts", 15.0).color(pal.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(ui, icons::ICON_CLOSE, "Close").clicked() {
                    close = true;
                }
            });
        });
        ui.add(
            egui::Label::new(
                RichText::new("The keys for moving around can be changed in keys.json: Settings, Files.")
                    .color(pal.muted)
                    .size(12.0),
            )
            .wrap(),
        );
        ui.add_space(10.0);
        let column = |ui: &mut Ui, which: &[Group]| {
            ui.vertical(|ui| {
                ui.set_width(COLUMN);
                for (group, list) in groups.iter().filter(|(g, _)| which.contains(g)) {
                    group_ui(ui, pal, *group, list);
                }
            });
        };
        // (As tall as its content, up to what the window has room for.)
        let room = (screen.height() - 150.0).max(160.0);
        egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).show(ui, |ui| {
            if two {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 36.0;
                    column(ui, &[Group::Global, Group::Tables, Group::Details]);
                    column(ui, &[Group::Navigation, Group::Logs, Group::Shell]);
                });
            } else {
                column(ui, &Group::ALL);
            }
        });
    });
    close || resp.should_close()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(all: &'a [(Group, Vec<Shortcut>)], what: &str) -> &'a Shortcut {
        all.iter().flat_map(|(_, list)| list).find(|s| s.what == what).unwrap_or_else(|| panic!("no {what:?}"))
    }

    #[test]
    fn every_bindable_action_is_listed_once() {
        let all = shortcuts(&Keymap::default(), false);
        for action in Action::ALL {
            let n = all.iter().flat_map(|(_, list)| list).filter(|s| s.what == action.title()).count();
            assert_eq!(n, 1, "{}", action.name());
        }
        // Every group has something in it.
        assert!(all.iter().all(|(_, list)| !list.is_empty()));
        assert_eq!(all.len(), Group::ALL.len());
    }

    #[test]
    fn custom_bindings_show() {
        let (map, errors) = Keymap::from_json(r#"{ "down": ["n", "Ctrl+n"], "filter": [], "help": ["F1"] }"#);
        assert!(errors.is_empty(), "{errors:?}");
        let all = shortcuts(&map, false);
        assert_eq!(find(&all, Action::Down.title()).keys, ["n", "Ctrl+N"]);
        assert!(find(&all, Action::Filter.title()).keys.is_empty());
        assert_eq!(find(&all, Action::Help.title()).keys, ["F1"]);
        // Untouched ones keep the defaults.
        assert_eq!(find(&all, Action::Up.title()).keys, ["k", "↑"]);
    }

    #[test]
    fn modifier_names_follow_the_platform() {
        let mac = shortcuts(&Keymap::default(), true);
        let other = shortcuts(&Keymap::default(), false);
        assert_eq!(find(&mac, "Settings").keys, ["Cmd+,"]);
        assert_eq!(find(&other, "Settings").keys, ["Ctrl+,"]);
        assert_eq!(find(&mac, "Copy the selection").keys, ["Cmd+C"]);
        assert_eq!(find(&other, "Copy the selection").keys, ["Ctrl+Shift+C"]);
        assert!(mac.iter().flat_map(|(_, l)| l).all(|s| s.what != "Search or jump, from a shell"));
    }
}
