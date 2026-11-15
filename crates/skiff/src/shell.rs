//! A dock tab hosting an interactive shell inside a container.

use std::sync::Arc;

use egui::{RichText, Ui};
use egui_material_icons::icons;
use skiff_core::exec::default_shell;
use skiff_core::{ContainerInfo, ExecSession, ExecState, Job, Session, Tone};

use crate::theme::Palette;
use crate::widgets;

pub struct ShellTab {
    session: Arc<Session>,
    ns: String,
    pod: String,
    container: Option<String>,
    containers: Vec<String>,
    containers_job: Option<Job<Vec<ContainerInfo>>>,
    exec: Option<Arc<ExecSession>>,
    error: Option<String>,
    #[cfg(feature = "terminal")]
    term: skiff_term::TerminalView,
    /// The terminal had to be started afresh because the emulator failed on
    /// something the container printed.
    display_reset: bool,
    /// Find in the scrollback: shown, its text, where the last find landed,
    /// and whether the box should take the keyboard.
    find_open: bool,
    find_text: String,
    find_searched: String,
    #[cfg(feature = "terminal")]
    find_result: Option<skiff_term::FindResult>,
    focus_find: bool,
}

/// The shortcut that opens find in a shell: Cmd+F on macOS. Elsewhere Ctrl+F
/// belongs to the shell (cursor forward), so it is Ctrl+Shift+F, as in most
/// terminal apps.
pub fn find_shortcut(ctx: &egui::Context) -> egui::KeyboardShortcut {
    use egui::{Key, KeyboardShortcut, Modifiers};
    if ctx.os() == egui::os::OperatingSystem::Mac {
        KeyboardShortcut::new(Modifiers::MAC_CMD, Key::F)
    } else {
        KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::F)
    }
}

/// Output is fed to the terminal a slice at a time and for at most this long
/// per frame, so that a process which prints without end cannot stall the
/// rest of the app. What is left stays queued in the exec session (whose
/// cap then holds the remote back) until the next frame.
#[cfg(feature = "terminal")]
const FEED_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);
#[cfg(feature = "terminal")]
const FEED_SLICE: usize = 32 * 1024;

impl ShellTab {
    fn blank(session: &Arc<Session>, ns: &str, pod: &str) -> Self {
        ShellTab {
            session: session.clone(),
            ns: ns.to_string(),
            pod: pod.to_string(),
            container: None,
            containers: Vec::new(),
            containers_job: None,
            exec: None,
            error: None,
            #[cfg(feature = "terminal")]
            term: skiff_term::TerminalView::new(),
            display_reset: false,
            find_open: false,
            find_text: String::new(),
            find_searched: String::new(),
            #[cfg(feature = "terminal")]
            find_result: None,
            focus_find: false,
        }
    }

    /// Opens the find bar (or gives it the keyboard again).
    pub fn open_find(&mut self) {
        self.find_open = true;
        self.focus_find = true;
    }

    #[cfg(feature = "terminal")]
    fn find_bar(&mut self, ui: &mut Ui, pal: &Palette) {
        let mut close = false;
        let mut step: Option<bool> = None;
        ui.horizontal(|ui| {
            let id = ui.scope_id().with("shell-find");
            let edit = widgets::search_box(ui, &mut self.find_text, "Find in scrollback", 240.0, id);
            if std::mem::take(&mut self.focus_find) {
                edit.request_focus();
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                // Enter goes back in time (to older output), Shift+Enter
                // forward; the box keeps the keyboard.
                step = Some(!ui.input(|i| i.modifiers.shift));
                ui.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                    i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter);
                });
                self.focus_find = true;
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            if !self.find_text.is_empty() {
                let (text, color) = match self.find_result {
                    None => ("no matches".to_string(), pal.err),
                    Some(r) => {
                        let total = if r.total >= skiff_term::MAX_COUNTED_MATCHES {
                            format!("{}+", r.total)
                        } else {
                            r.total.to_string()
                        };
                        (format!("{} / {total}", r.index), pal.muted)
                    }
                };
                ui.label(RichText::new(text).color(color).size(12.0));
            }
            if widgets::icon_button(ui, icons::ICON_KEYBOARD_ARROW_UP, "Older match (Enter)").clicked() {
                step = Some(true);
            }
            if widgets::icon_button(ui, icons::ICON_KEYBOARD_ARROW_DOWN, "Newer match (Shift+Enter)").clicked() {
                step = Some(false);
            }
            if widgets::icon_button(ui, icons::ICON_CLOSE, "Close find (Esc)").clicked() {
                close = true;
            }
        });
        // Typing searches as you go, from the newest output upwards.
        let changed = self.find_text != self.find_searched;
        if changed || step.is_some() {
            let needle = self.find_text.clone();
            if changed {
                self.with_term(|t| t.end_find());
            }
            self.find_searched = needle.clone();
            self.find_result = self.with_term(|t| t.find(&needle, step.unwrap_or(true))).flatten();
        }
        if close {
            self.find_open = false;
            self.find_text.clear();
            self.find_searched.clear();
            self.find_result = None;
            self.with_term(|t| t.end_find());
            self.term.request_focus();
        }
    }

    /// Runs something on the terminal emulator. The emulator digests
    /// whatever a container prints; should that ever make it panic, the
    /// damage stays here: the screen starts afresh and everything else in
    /// the app (other shells, edits, port forwards) carries on.
    #[cfg(feature = "terminal")]
    fn with_term<R>(&mut self, f: impl FnOnce(&mut skiff_term::TerminalView) -> R) -> Option<R> {
        let result = contained(&mut self.term, f);
        self.display_reset |= result.is_none();
        result
    }

    /// Looks up the pod's containers first, then opens the first one.
    pub fn resolving(session: &Arc<Session>, ns: &str, pod: &str, job: Job<Vec<ContainerInfo>>) -> Self {
        let mut tab = Self::blank(session, ns, pod);
        tab.containers_job = Some(job);
        tab
    }

    pub fn start(session: &Arc<Session>, ns: &str, pod: &str, container: &str) -> Self {
        let mut tab = Self::blank(session, ns, pod);
        tab.containers = vec![container.to_string()];
        tab.connect(container);
        tab
    }

    fn connect(&mut self, container: &str) {
        if let Some(old) = self.exec.take() {
            old.close();
        }
        self.container = Some(container.to_string());
        self.error = None;
        self.display_reset = false;
        #[cfg(feature = "terminal")]
        {
            self.term = skiff_term::TerminalView::new();
            self.term.request_focus();
        }
        self.exec = Some(self.session.exec(&self.ns, &self.pod, container, default_shell()));
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.containers_job {
            if let Some(res) = job.take() {
                self.containers_job = None;
                match res {
                    Ok(list) => {
                        self.containers = list.iter().filter(|c| !c.init).map(|c| c.name.clone()).collect();
                        match self.containers.first().cloned() {
                            Some(first) => self.connect(&first),
                            None => self.error = Some("pod has no containers".into()),
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        // Output is consumed on every frame, shown or not, and what the
        // terminal answers (cursor position, colours, ...) goes back at
        // once: an answer delivered later would be typed into whatever is
        // running by then.
        #[cfg(feature = "terminal")]
        if let Some(exec) = self.exec.clone() {
            let started = std::time::Instant::now();
            loop {
                let bytes = exec.take_output_up_to(FEED_SLICE);
                if bytes.is_empty() {
                    break;
                }
                self.with_term(|t| t.feed(&bytes));
                if started.elapsed() >= FEED_BUDGET {
                    break;
                }
            }
            // (Also when nothing new arrived: output held back by the
            // program is released after a moment.)
            if let Some(Some(wait)) = self.with_term(|t| t.tick()) {
                ctx.request_repaint_after(wait);
            }
            let replies = self.term.take_replies();
            if !replies.is_empty() {
                exec.write(&replies);
            }
        }
        #[cfg(not(feature = "terminal"))]
        let _ = ctx;
    }

    /// The text currently on the terminal's screen, one line per row (for
    /// tests and diagnostics).
    #[cfg(feature = "terminal")]
    pub fn screen_text(&self) -> String {
        let (_, rows) = self.term.size();
        (0..usize::from(rows)).map(|r| self.term.row_text(r)).collect::<Vec<_>>().join("\n")
    }

    /// Gives the terminal the keyboard on the next frame.
    pub fn focus(&mut self) {
        #[cfg(feature = "terminal")]
        self.term.request_focus();
    }

    /// Draws the tab; returns whether the terminal has keyboard focus.
    pub fn ui(&mut self, ui: &mut Ui, pal: &Palette, toasts: &mut Vec<(String, Tone)>) -> bool {
        if let Some(err) = &self.error {
            widgets::empty_state(ui, icons::ICON_ERROR, "Couldn't open a shell", err, pal);
            return false;
        }
        let Some(exec) = self.exec.clone() else {
            widgets::empty_state(ui, icons::ICON_SYNC, "Finding containers…", "", pal);
            return false;
        };
        let state = exec.state();
        let mut switch_to: Option<String> = None;
        let mut reconnect = false;
        // (Wrapping: with several containers or a narrow window, the state
        // and the Reconnect button must not end up outside the window.)
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{}/{}", self.ns, self.pod)).color(pal.muted).size(12.0));
            if self.containers.len() > 1 {
                for c in &self.containers {
                    let selected = self.container.as_deref() == Some(c.as_str());
                    if ui.add(egui::Button::selectable(selected, RichText::new(c).size(12.0))).clicked() && !selected {
                        switch_to = Some(c.clone());
                    }
                }
            } else if let Some(c) = &self.container {
                ui.label(RichText::new(c).color(pal.text).size(12.0));
            }
            match &state {
                ExecState::Connecting => {
                    ui.spinner();
                }
                ExecState::Running => {
                    widgets::pill(ui, "connected", pal.ok, pal);
                }
                ExecState::Exited(reason) => {
                    widgets::pill(ui, "ended", pal.muted, pal).on_hover_text(reason);
                    reconnect = ui.small_button("Reconnect").clicked();
                }
                ExecState::Failed(err) => {
                    widgets::pill(ui, "failed", pal.err, pal).on_hover_text(err);
                    reconnect = ui.small_button("Retry").clicked();
                }
            }
            if widgets::action_button(ui, icons::ICON_OPEN_IN_NEW, "Open in terminal app")
                .on_hover_text("Open this shell in your own terminal (iTerm, Terminal, ...) with kubectl exec")
                .clicked()
            {
                let cmd =
                    crate::external::exec_command(&self.session.context, &self.ns, &self.pod, self.container.as_deref());
                match crate::external::open(&cmd) {
                    Ok(app) => toasts.push((format!("Opened in {app}"), Tone::Normal)),
                    Err(e) => toasts.push((e, Tone::Err)),
                }
            }
        });
        if let ExecState::Failed(err) = &state {
            ui.add(egui::Label::new(RichText::new(err).color(pal.err)).wrap().selectable(true));
        }
        if self.display_reset {
            ui.label(
                RichText::new("The display was reset: the program printed something the terminal could not show. The session itself is unaffected.")
                    .color(pal.warn)
                    .size(12.0),
            );
        }
        #[cfg(feature = "terminal")]
        if self.find_open {
            self.find_bar(ui, pal);
        }
        let focused = self.terminal(ui, pal, &exec);
        if let Some(c) = switch_to {
            self.connect(&c);
        } else if reconnect {
            if let Some(c) = self.container.clone() {
                self.connect(&c);
            }
        }
        focused
    }

    #[cfg(feature = "terminal")]
    fn terminal(&mut self, ui: &mut Ui, pal: &Palette, exec: &Arc<ExecSession>) -> bool {
        let style = term_style(pal);
        let Some(out) = self.with_term(|t| t.ui(ui, &style)) else { return false };
        if !out.input.is_empty() {
            exec.write(&out.input);
        }
        if let Some((cols, rows)) = out.resize {
            exec.resize(cols, rows);
        }
        out.has_focus
    }

    #[cfg(not(feature = "terminal"))]
    fn terminal(&mut self, ui: &mut Ui, pal: &Palette, _exec: &Arc<ExecSession>) -> bool {
        widgets::empty_state(
            ui,
            icons::ICON_TERMINAL,
            "Terminal support not built",
            "Rebuild with the `terminal` feature enabled.",
            pal,
        );
        false
    }
}

/// Runs `f` on the terminal; if it panics, the terminal is replaced by a
/// fresh one and `None` is returned.
#[cfg(feature = "terminal")]
fn contained<R>(term: &mut skiff_term::TerminalView, f: impl FnOnce(&mut skiff_term::TerminalView) -> R) -> Option<R> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *term)));
    if result.is_err() {
        *term = skiff_term::TerminalView::new();
    }
    result.ok()
}

#[cfg(all(test, feature = "terminal"))]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_emulator_is_replaced_not_fatal() {
        let mut term = skiff_term::TerminalView::new();
        term.feed(b"before");
        assert_eq!(contained(&mut term, |t| t.row_text(0)), Some("before".to_string()));
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result: Option<()> = contained(&mut term, |_| panic!("the emulator fell over"));
        std::panic::set_hook(quiet);
        assert!(result.is_none());
        assert_eq!(term.row_text(0), "", "a fresh screen");
        term.feed(b"after");
        assert_eq!(term.row_text(0), "after", "and it keeps working");
    }
}

#[cfg(feature = "terminal")]
fn term_style(pal: &Palette) -> skiff_term::TermStyle {
    use egui::Color32;
    let c = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
    let ansi = if pal.dark {
        [
            0x1A1E25, 0xF0616D, 0x3FB97A, 0xE3A545, 0x5B9DFF, 0xC792EA, 0x58B4E8, 0xC8CED8, 0x5C6473, 0xFF7B86,
            0x5FD498, 0xF2BD68, 0x82B4FF, 0xD9B0F5, 0x7CCBF2, 0xF2F4F8,
        ]
    } else {
        // Dark enough to read on white; 7 and 15 ("white") are greys, so
        // that white text is visible and still works as a background.
        [
            0x24292F, 0xCF222E, 0x116329, 0x7D4E00, 0x0969DA, 0x8250DF, 0x1B7C83, 0x6E7781, 0x57606A, 0xA40E26,
            0x1A7F37, 0x633C01, 0x1F6FD0, 0x8B5CF0, 0x1A7F8E, 0x8C959F,
        ]
    };
    skiff_term::TermStyle {
        font: crate::theme::mono(12.5),
        fg: pal.text,
        bg: pal.bg,
        cursor: pal.accent,
        selection_bg: pal.tint(pal.accent, if pal.dark { 0.38 } else { 0.28 }),
        ansi: ansi.map(c),
        ligatures: false,
        // Programs choose colour pairs for a classic dark terminal; whatever
        // they choose stays readable in both themes.
        min_contrast: 4.5,
    }
}
