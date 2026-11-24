//! Full-page states of the main area: the first-run welcome page and the
//! "can't connect" page.

use std::path::{Path, PathBuf};

use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, Stroke, Ui};
use egui_material_icons::{icons, MaterialIcon};
use skiff_core::{ErrorInfo, ErrorKind, KubeconfigProblem};

use crate::theme::{self, Palette};
use crate::widgets;

/// The widest a page's text column gets.
const COLUMN: f32 = 560.0;

/// Lays `add` out in a column of at most [`COLUMN`] points, centred in the
/// space that is left, with everything in it centred.
fn column(ui: &mut Ui, top: f32, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let width = (ui.available_width() - 32.0).clamp(240.0, COLUMN);
        let pad = ((ui.available_width() - width) / 2.0).max(0.0);
        ui.add_space(top);
        ui.horizontal_top(|ui| {
            ui.add_space(pad);
            ui.allocate_ui_with_layout(egui::vec2(width, 0.0), Layout::top_down(Align::Center), |ui| {
                ui.set_width(width);
                add(ui);
            });
        });
        ui.add_space(24.0);
    });
}

/// A row of buttons, centred. `widths` are the buttons' minimum widths, in
/// the order `add` draws them.
fn button_row(ui: &mut Ui, widths: &[f32], add: impl FnOnce(&mut Ui, &[f32])) {
    let gap = 8.0;
    let total: f32 = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    let pad = ((ui.available_width() - total) / 2.0).max(0.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 32.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(pad - gap);
            add(ui, widths);
        },
    );
}

/// `path` with the home directory written as `~`.
pub fn short_path(path: &Path) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    match home.as_deref().filter(|h| !h.as_os_str().is_empty()).and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

/// `text` in the monospace font, shortened from the front (`…/kube/config`)
/// until it is at most `width` wide.
fn elide_front(ui: &Ui, text: &str, width: f32) -> String {
    let wide = |t: &str| ui.painter().layout_no_wrap(t.to_owned(), theme::mono(12.5), egui::Color32::WHITE).size().x;
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

/// What the last kubeconfig load looked at.
#[derive(Clone, Debug, Default)]
pub struct KubeconfigReport {
    /// Every file that was looked for, including missing ones.
    pub searched: Vec<PathBuf>,
    /// Files that were read and merged.
    pub files: Vec<PathBuf>,
    /// Files that exist but could not be used.
    pub problems: Vec<KubeconfigProblem>,
}

pub enum WelcomeAction {
    AddFile,
    Reload,
}

/// The page shown while no kubeconfig context is known at all: where Skiff
/// looked, what was wrong with what it found, and how to go on from here.
/// `picking`: the "open file" dialog is up.
pub fn welcome(ui: &mut Ui, pal: &Palette, report: &KubeconfigReport, picking: bool) -> Option<WelcomeAction> {
    let mut action = None;
    let top = (ui.available_height() * 0.14).max(20.0);
    column(ui, top, |ui| {
        ui.label(widgets::icon(icons::ICON_SAILING, 44.0).color(pal.accent));
        ui.add_space(8.0);
        ui.label(widgets::semibold("Welcome to Skiff", 20.0).color(pal.text));
        ui.add_space(4.0);
        let lead = if !report.problems.is_empty() {
            "None of your kubeconfig files has a cluster that Skiff can show."
        } else if !report.files.is_empty() {
            "Your kubeconfig has no contexts yet, so there is no cluster to show."
        } else {
            "No kubeconfig found yet. Skiff shows the clusters kubectl knows about."
        };
        ui.add(egui::Label::new(RichText::new(lead).color(pal.muted).size(13.5)).wrap());
        ui.add_space(16.0);

        button_row(ui, &[184.0, 96.0], |ui, widths| {
            let add = widgets::primary_button(ui, pal, icons::ICON_FILE_OPEN, "Add kubeconfig file…", widths[0], !picking)
                .on_hover_text("Choose a kubeconfig file. You can also drop one onto this window.");
            if add.clicked() {
                action = Some(WelcomeAction::AddFile);
            }
            let reload = ui
                .add(
                    egui::Button::new((widgets::icon(icons::ICON_REFRESH, 16.0), "Reload"))
                        .min_size(egui::vec2(widths[1], 32.0)),
                )
                .on_hover_text("Read the kubeconfig files again");
            if reload.clicked() {
                action = Some(WelcomeAction::Reload);
            }
        });
        ui.add_space(20.0);

        if !report.searched.is_empty() {
            searched_card(ui, pal, report);
            ui.add_space(14.0);
        }
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Clusters get there with your cloud's command line: aws eks update-kubeconfig, \
                     gcloud container clusters get-credentials or az aks get-credentials. \
                     Skiff notices new ones when you come back to this window.",
                )
                .color(pal.muted)
                .size(12.5),
            )
            .wrap(),
        );
    });
    action
}

/// The files that were looked for, each with what became of it.
fn searched_card(ui: &mut Ui, pal: &Palette, report: &KubeconfigReport) {
    Frame::new()
        .fill(pal.raised)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                widgets::caption(ui, "Looked in", pal);
                ui.add_space(4.0);
                for path in &report.searched {
                    let problem = report.problems.iter().find(|p| &p.path == path);
                    let (status, color) = if problem.is_some() {
                        ("can't be used", pal.err)
                    } else if report.files.contains(path) {
                        ("no contexts", pal.warn)
                    } else {
                        ("not found", pal.muted)
                    };
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            widgets::pill(ui, status, color, pal);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                // Cut at the front when it does not fit:
                                // the file name is the telling part.
                                let full = short_path(path);
                                let shown = elide_front(ui, &full, ui.available_width() - 8.0);
                                let label = ui.add(
                                    egui::Label::new(RichText::new(&shown).font(theme::mono(12.5)).color(pal.text))
                                        .selectable(true),
                                );
                                if shown != full {
                                    label.on_hover_text(full);
                                }
                            });
                        });
                    });
                    if let Some(problem) = problem {
                        ui.add(
                            egui::Label::new(RichText::new(&problem.message).color(pal.err).size(12.0))
                                .wrap()
                                .selectable(true),
                        );
                        ui.add_space(2.0);
                    }
                }
            });
        });
}

pub enum FailureAction {
    Retry,
    CopyDetails,
}

fn failure_icon(kind: ErrorKind) -> MaterialIcon {
    match kind {
        ErrorKind::Unauthorized | ErrorKind::Forbidden => icons::ICON_KEY_OFF,
        ErrorKind::ExecMissing | ErrorKind::ExecFailed => icons::ICON_EXTENSION_OFF,
        ErrorKind::Tls => icons::ICON_GPP_BAD,
        ErrorKind::Timeout => icons::ICON_TIMER_OFF,
        ErrorKind::Kubeconfig => icons::ICON_DESCRIPTION,
        _ => icons::ICON_CLOUD_OFF,
    }
}

/// The page shown instead of a view while the cluster cannot be reached:
/// what happened, what to do about it, and the address that was tried.
pub fn connection_failed(
    ui: &mut Ui,
    pal: &Palette,
    context: &str,
    server: &str,
    info: &ErrorInfo,
) -> Option<FailureAction> {
    let mut action = None;
    let top = (ui.available_height() * 0.22).max(20.0);
    column(ui, top, |ui| {
        ui.label(widgets::icon(failure_icon(info.kind), 40.0).color(pal.err));
        ui.add_space(8.0);
        ui.add(egui::Label::new(widgets::semibold(format!("Can't connect to {context}"), 17.0).color(pal.text)).wrap());
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(&info.summary).color(pal.text)).wrap().selectable(true));
        if let Some(hint) = &info.hint {
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(hint).color(pal.muted)).wrap());
        }
        if !server.is_empty() {
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(RichText::new(server).font(theme::mono(12.0)).color(pal.muted))
                    .wrap()
                    .selectable(true),
            )
            .on_hover_text("The API server address in your kubeconfig");
        }
        ui.add_space(16.0);
        button_row(ui, &[96.0, 128.0], |ui, widths| {
            if widgets::primary_button(ui, pal, icons::ICON_REFRESH, "Retry", widths[0], true).clicked() {
                action = Some(FailureAction::Retry);
            }
            let copy = ui
                .add(
                    egui::Button::new((widgets::icon(icons::ICON_CONTENT_COPY, 16.0), "Copy details"))
                        .min_size(egui::vec2(widths[1], 32.0)),
                )
                .on_hover_text("Copy the cluster, its address and the error, to share or search for");
            if copy.clicked() {
                action = Some(FailureAction::CopyDetails);
            }
        });
    });
    action
}

/// The text "Copy details" puts on the clipboard for a failed connection.
pub fn failure_details(context: &str, server: &str, file: Option<&Path>, plugin: Option<&str>, info: &ErrorInfo) -> String {
    let mut out = format!("Skiff could not connect to {context}\n");
    if !server.is_empty() {
        out.push_str(&format!("Server: {server}\n"));
    }
    if let Some(file) = file {
        out.push_str(&format!("Kubeconfig: {}\n", file.display()));
    }
    if let Some(plugin) = plugin {
        out.push_str(&format!("Credential plugin: {plugin}\n"));
    }
    out.push_str(&format!("Error: {}\n", info.summary));
    if let Some(hint) = &info.hint {
        out.push_str(&format!("Hint: {hint}\n"));
    }
    out
}

/// The page shown while the first connection to a cluster is being made.
/// `detail`: what it is waiting for, when that is known.
pub fn connecting(ui: &mut Ui, pal: &Palette, context: &str, detail: Option<&str>) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(24.0));
        ui.add(egui::Spinner::new().size(28.0));
        ui.add_space(10.0);
        ui.label(widgets::semibold(format!("Connecting to {context}…"), 15.0).color(pal.text));
        if let Some(detail) = detail {
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(detail).color(pal.muted)).wrap());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_name_everything_needed_to_ask_for_help() {
        let info = ErrorInfo::new(ErrorKind::Unreachable, "Cannot reach https://10.0.0.1:6443: Connection refused", Some("Check your VPN."));
        let text = failure_details("prod", "https://10.0.0.1:6443", Some(Path::new("/home/me/.kube/config")), Some("aws"), &info);
        for needle in ["prod", "Server: https://10.0.0.1:6443", "Kubeconfig: /home/me/.kube/config", "Credential plugin: aws", "Connection refused", "Hint: Check your VPN."] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
        let bare = failure_details("prod", "", None, None, &ErrorInfo::other("boom"));
        assert_eq!(bare, "Skiff could not connect to prod\nError: boom\n");
    }

    #[test]
    fn paths_outside_home_are_shown_in_full() {
        assert_eq!(short_path(Path::new("/definitely/not/home/config")), "/definitely/not/home/config");
    }
}
