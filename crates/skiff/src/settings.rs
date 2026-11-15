//! The settings page: which kubeconfig files are read, the clusters they
//! define, and general preferences.

use std::path::PathBuf;

use egui::{Align, Frame, Layout, Margin, RichText, ScrollArea, Stroke, Ui};
use egui_material_icons::icons;
use skiff_core::kubeconfig::{self, FileReport};
use skiff_core::{ConnStatus, Tone};

use super::SkiffApp;
use crate::theme::Palette;
use crate::widgets;

/// Settings page state that is not a setting itself.
#[derive(Default)]
pub struct SettingsUi {
    /// Text of the "add a kubeconfig file" field.
    new_path: String,
    /// Per-file reports, read when the page opens or the files change
    /// (reading files every frame would be wasteful).
    reports: Option<Vec<FileReport>>,
    suggestions: Vec<PathBuf>,
    show_hidden: bool,
}

impl SettingsUi {
    /// Forget what was read from disk; it is read again when shown.
    pub fn invalidate(&mut self) {
        self.reports = None;
    }
}

/// Log history choices: (label, lines).
pub const LOG_TAILS: [(&str, Option<i64>); 4] =
    [("last 100", Some(100)), ("last 1,000", Some(1000)), ("last 10,000", Some(10_000)), ("all", None)];

fn card(ui: &mut Ui, pal: &Palette, title: &str, subtitle: &str, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(pal.panel)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(8)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(widgets::semibold(title, 14.0).color(pal.text));
            if !subtitle.is_empty() {
                ui.add(egui::Label::new(RichText::new(subtitle).color(pal.muted).size(12.0)).wrap());
            }
            ui.add_space(8.0);
            add(ui);
        });
    ui.add_space(12.0);
}

impl SkiffApp {
    pub(super) fn settings_view(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        if self.settings.reports.is_none() {
            self.settings.reports = Some(kubeconfig::inspect(&kubeconfig::kubeconfig_paths()));
            self.settings.suggestions = kubeconfig::suggestions();
        }
        ui.horizontal(|ui| {
            ui.label(widgets::semibold("Settings", 17.0).color(pal.text));
        });
        ui.add_space(10.0);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(980.0);
            self.kubeconfig_card(ui, &pal);
            self.clusters_card(ui, &pal);
            self.general_card(ui, &pal);
            self.about_card(ui, &pal);
        });
    }

    fn kubeconfig_card(&mut self, ui: &mut Ui, pal: &Palette) {
        let subtitle = "Clusters come from these files, merged like kubectl merges $KUBECONFIG (the first file that \
                        defines a name wins). Files are only read; Skiff never changes them.";
        let mut changed: Option<Option<Vec<PathBuf>>> = None;
        let mut reload = false;
        card(ui, pal, "Kubeconfig files", subtitle, |ui| {
            let custom = kubeconfig::kubeconfig_override();
            let mut use_custom = custom.is_some();
            ui.horizontal(|ui| {
                if ui.radio(!use_custom, "Same as kubectl").on_hover_text("$KUBECONFIG, else ~/.kube/config").clicked() {
                    use_custom = false;
                    if custom.is_some() {
                        changed = Some(None);
                    }
                }
                if ui.radio(use_custom, "Choose files").clicked() && custom.is_none() {
                    // Start from what is in effect now.
                    changed = Some(Some(kubeconfig::default_kubeconfig_paths()));
                }
            });
            ui.add_space(6.0);
            let reports = self.settings.reports.clone().unwrap_or_default();
            let mut remove: Option<usize> = None;
            let mut raise: Option<usize> = None;
            for (i, r) in reports.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (dot, detail) = match &r.contexts {
                        Ok(names) => (pal.ok, widgets::plural(names.len(), "context")),
                        Err(e) => (pal.err, e.clone()),
                    };
                    widgets::dot(ui, dot);
                    ui.add(egui::Label::new(RichText::new(r.path.display().to_string()).font(crate::theme::mono(12.5))).truncate())
                        .on_hover_text(match &r.contexts {
                            Ok(names) => names.join("\n"),
                            Err(e) => e.clone(),
                        });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if custom.is_some() {
                            if widgets::icon_button(ui, icons::ICON_CLOSE, "Stop using this file").clicked() {
                                remove = Some(i);
                            }
                            if i > 0 && widgets::icon_button(ui, icons::ICON_ARROW_UPWARD, "Move up (earlier files win)").clicked() {
                                raise = Some(i);
                            }
                        }
                        let color = if r.contexts.is_ok() { pal.muted } else { pal.err };
                        ui.add(egui::Label::new(RichText::new(detail).color(color).size(12.0)).truncate());
                    });
                });
            }
            if reports.is_empty() {
                ui.label(RichText::new("No files.").color(pal.muted));
            }
            if let Some(mut list) = custom.clone() {
                if let Some(i) = remove {
                    list.remove(i);
                    changed = Some(Some(list.clone()));
                }
                if let Some(i) = raise {
                    list.swap(i, i - 1);
                    changed = Some(Some(list.clone()));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let id = egui::Id::unique("settings-new-kubeconfig");
                    let edit = widgets::search_box(ui, &mut self.settings.new_path, "Path to a kubeconfig file", 420.0, id);
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let path = self.settings.new_path.trim().to_string();
                    if (ui.add_enabled(!path.is_empty(), egui::Button::new("Add")).clicked() || enter) && !path.is_empty() {
                        let p = kubeconfig::expand_home(&PathBuf::from(&path));
                        if !list.contains(&p) {
                            list.push(p);
                            changed = Some(Some(list.clone()));
                        }
                        self.settings.new_path.clear();
                    }
                });
                let suggestions: Vec<PathBuf> =
                    self.settings.suggestions.iter().filter(|p| !list.contains(p)).cloned().collect();
                if !suggestions.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Found in ~/.kube:").color(pal.muted).size(12.0));
                        for p in suggestions {
                            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let btn = egui::Button::new((widgets::icon(icons::ICON_ADD, 14.0), RichText::new(name).size(12.0)));
                            if ui.add(btn).on_hover_text(p.display().to_string()).clicked() {
                                list.push(p);
                                changed = Some(Some(list.clone()));
                            }
                        }
                    });
                }
            }
            ui.add_space(8.0);
            if widgets::action_button(ui, icons::ICON_REFRESH, "Reload kubeconfig")
                .on_hover_text("Read the files again (new or removed contexts, changed credentials)")
                .clicked()
            {
                reload = true;
            }
        });
        if let Some(paths) = changed {
            kubeconfig::set_kubeconfig_paths(paths.clone());
            self.kubeconfigs = paths.map(|v| v.iter().map(|p| p.display().to_string()).collect());
            reload = true;
        }
        if reload {
            self.reload_contexts(true);
        }
    }

    fn clusters_card(&mut self, ui: &mut Ui, pal: &Palette) {
        let subtitle = "Every context in the files above. Connections stay open while you switch between clusters; \
                        hidden clusters are left out of the cluster menu and the command palette.";
        let mut switch_to = None;
        let mut disconnect = None;
        let mut reconnect = None;
        let mut toggle_hidden: Option<String> = None;
        card(ui, pal, "Clusters", subtitle, |ui| {
            if let Some(err) = &self.contexts_error {
                ui.add(egui::Label::new(RichText::new(err).color(pal.err)).wrap());
            }
            if let Some(w) = &self.all_contexts.warning {
                ui.add(egui::Label::new(RichText::new(w).color(pal.warn).size(12.0)).wrap());
            }
            let hidden_count = self.all_contexts.items.iter().filter(|c| self.hidden.contains(&c.name)).count();
            if hidden_count > 0 {
                ui.checkbox(&mut self.settings.show_hidden, format!("Show hidden clusters ({hidden_count})"));
            }
            // Fixed-width cells: truncating labels in an auto-sized grid
            // shrink to nothing.
            let full = ui.available_width();
            let actions_w = 110.0;
            let name_w = (full * 0.26).clamp(140.0, 300.0);
            let state_w = (full * 0.2).clamp(110.0, 240.0);
            let server_w = (full - name_w - state_w - actions_w - 3.0 * 12.0).max(80.0);
            let cell = |ui: &mut Ui, w: f32, add: &mut dyn FnMut(&mut Ui)| {
                ui.allocate_ui_with_layout(egui::vec2(w, 34.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_width(w);
                    add(ui);
                });
            };
            for c in &self.all_contexts.items {
                let hidden = self.hidden.contains(&c.name);
                if hidden && !self.settings.show_hidden {
                    continue;
                }
                let status = self.sessions.get(&c.name).map(|s| s.status());
                let (dot, state) = match status.as_deref() {
                    Some(ConnStatus::Connected { version }) => (pal.ok, format!("connected · {version}")),
                    Some(ConnStatus::Connecting) => (pal.warn, "connecting".to_string()),
                    Some(ConnStatus::Failed(e)) => (pal.err, format!("failed: {e}")),
                    None => (pal.faint, "not connected".to_string()),
                };
                let active = self.active.as_deref() == Some(c.name.as_str());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    cell(ui, name_w, &mut |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        widgets::dot(ui, dot);
                        let name = if active { widgets::semibold(&c.name, 13.0).color(pal.accent) } else { RichText::new(&c.name) };
                        let name = if hidden { name.color(pal.faint) } else { name };
                        ui.add(egui::Label::new(name).truncate()).on_hover_text(&c.name);
                    });
                    cell(ui, server_w, &mut |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(egui::Label::new(RichText::new(&c.server).color(pal.muted).size(12.0)).truncate());
                            let mut who = format!("user {}", c.user);
                            if let Some(ns) = &c.namespace {
                                who.push_str(&format!(" · namespace {ns}"));
                            }
                            ui.add(egui::Label::new(RichText::new(who).color(pal.faint).size(11.5)).truncate());
                        });
                    });
                    cell(ui, state_w, &mut |ui| {
                        ui.add(egui::Label::new(RichText::new(&state).color(pal.muted).size(12.0)).truncate())
                            .on_hover_text(&state);
                    });
                    cell(ui, actions_w, &mut |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if !active && widgets::icon_button(ui, icons::ICON_LOGIN, "Open this cluster").clicked() {
                            switch_to = Some(c.name.clone());
                        }
                        if status.is_some() {
                            if widgets::icon_button(ui, icons::ICON_SYNC, "Reconnect and reload everything (picks up new credentials)").clicked() {
                                reconnect = Some(c.name.clone());
                            }
                            if widgets::icon_button(ui, icons::ICON_POWER_SETTINGS_NEW, "Disconnect").clicked() {
                                disconnect = Some(c.name.clone());
                            }
                        }
                        let (ic, tip) = if hidden {
                            (icons::ICON_VISIBILITY, "Show in the cluster menu again")
                        } else {
                            (icons::ICON_VISIBILITY_OFF, "Hide from the cluster menu (the kubeconfig is not changed)")
                        };
                        if widgets::icon_button(ui, ic, tip).clicked() {
                            toggle_hidden = Some(c.name.clone());
                        }
                    });
                });
            }
            if self.all_contexts.items.is_empty() && self.contexts_error.is_none() {
                ui.label(RichText::new("The files above define no contexts.").color(pal.muted));
            }
        });
        if let Some(name) = reconnect {
            if let Some(s) = self.sessions.get(&name) {
                s.reconnect();
                self.toast(format!("Reconnecting to {name}"), Tone::Normal);
            }
        }
        if let Some(name) = disconnect {
            self.disconnect(&name);
        }
        if let Some(name) = switch_to {
            self.switch_context(&name);
        }
        if let Some(name) = toggle_hidden {
            if !self.hidden.remove(&name) {
                self.hidden.insert(name);
            }
            self.refresh_visible_contexts();
        }
    }

    fn general_card(&mut self, ui: &mut Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        card(ui, pal, "General", "", |ui| {
            egui::Grid::new("settings-general").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label("Theme");
                ui.horizontal(|ui| {
                    if ui.radio(self.pal.dark, "Dark").clicked() && !self.pal.dark {
                        self.set_theme(&ctx, true);
                    }
                    if ui.radio(!self.pal.dark, "Light").clicked() && self.pal.dark {
                        self.set_theme(&ctx, false);
                    }
                });
                ui.end_row();

                ui.label("Log history").on_hover_text("How many earlier lines a new log tab loads");
                ui.horizontal(|ui| {
                    for (label, n) in LOG_TAILS {
                        if ui.radio(self.log_tail == n, label).clicked() {
                            self.log_tail = n;
                            self.dock.default_tail = n;
                        }
                    }
                });
                ui.end_row();

                ui.label("Sidebar");
                ui.checkbox(&mut self.sidebar_open, "Show the resource sidebar");
                ui.end_row();

                ui.label("Table columns");
                ui.horizontal(|ui| {
                    let customised = self.tables.values().filter(|t| !t.col_visible.is_empty() || !t.widths.is_empty()).count();
                    let text = if customised == 0 {
                        "All tables use the default columns.".to_string()
                    } else {
                        format!("{} with chosen columns or widths.", widgets::plural(customised, "table"))
                    };
                    ui.label(RichText::new(text).color(pal.muted).size(12.0));
                    if customised > 0 && ui.button("Reset all").clicked() {
                        for t in self.tables.values_mut() {
                            t.reset_columns();
                        }
                    }
                });
                ui.end_row();

                if cfg!(target_os = "macos") {
                    ui.label("Updates");
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut self.auto_update_check, "Check for new versions automatically").changed() {
                            if self.auto_update_check {
                                self.updater.enable();
                            } else {
                                self.updater.disable();
                            }
                        }
                        if ui.button("Check now").clicked() {
                            self.run_command(&ctx, crate::palette::Command::CheckForUpdates);
                        }
                    });
                    ui.end_row();
                }
            });
        });
    }

    fn about_card(&mut self, ui: &mut Ui, pal: &Palette) {
        card(ui, pal, "About", "", |ui| {
            egui::Grid::new("settings-about").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                ui.label(RichText::new("Version").color(pal.muted));
                ui.label(format!("{} (build {})", env!("CARGO_PKG_VERSION"), crate::update::build_number()));
                ui.end_row();
                ui.label(RichText::new("Last frame").color(pal.muted));
                ui.label(format!("{:.1} ms of CPU", self.last_ui.as_secs_f32() * 1000.0));
                ui.end_row();
                if let Some(rss) = crate::app::resident_memory() {
                    ui.label(RichText::new("Memory").color(pal.muted));
                    ui.label(skiff_core::fmt_bytes(rss as f64));
                    ui.end_row();
                }
                let watches: usize = self.sessions.values().map(|s| s.active_watches()).sum();
                ui.label(RichText::new("Connections").color(pal.muted));
                ui.label(format!(
                    "{} · {}",
                    widgets::plural(self.sessions.len(), "cluster"),
                    widgets::plural(watches, "live watch")
                ));
                ui.end_row();
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_tail_choices_include_the_default() {
        let default = skiff_core::LogOptions::default().tail;
        assert!(LOG_TAILS.iter().any(|(_, n)| *n == default));
    }
}
