//! A dock tab hosting an interactive shell inside a container.

use std::sync::Arc;

use egui::{RichText, Ui};
use egui_material_icons::icons;
use skiff_core::exec::default_shell;
use skiff_core::{ContainerInfo, ExecSession, ExecState, Job, Session};

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
}

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
        }
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
        #[cfg(feature = "terminal")]
        {
            self.term = skiff_term::TerminalView::new();
            self.term.request_focus();
        }
        self.exec = Some(self.session.exec(&self.ns, &self.pod, container, default_shell()));
    }

    pub fn poll(&mut self) {
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
        // Output is consumed on every frame, shown or not: the terminal
        // keeps a bounded scrollback, the raw byte queue does not.
        #[cfg(feature = "terminal")]
        if let Some(exec) = &self.exec {
            let bytes = exec.take_output();
            if !bytes.is_empty() {
                self.term.feed(&bytes);
            }
        }
    }

    /// The text currently on the terminal's screen, one line per row (for
    /// tests and diagnostics).
    #[cfg(feature = "terminal")]
    pub fn screen_text(&self) -> String {
        let (_, rows) = self.term.size();
        (0..usize::from(rows)).map(|r| self.term.row_text(r)).collect::<Vec<_>>().join("\n")
    }

    /// Draws the tab; returns whether the terminal has keyboard focus.
    pub fn ui(&mut self, ui: &mut Ui, pal: &Palette) -> bool {
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
        ui.horizontal(|ui| {
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
        });
        if let ExecState::Failed(err) = &state {
            ui.add(egui::Label::new(RichText::new(err).color(pal.err)).wrap().selectable(true));
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
        let out = self.term.ui(ui, &style);
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
        [
            0x1C2330, 0xD43F4E, 0x1E9E5A, 0xB7791F, 0x2F6FEB, 0x8250DF, 0x1E88C8, 0x5E6878, 0x9AA3B2, 0xE0525F,
            0x26B56A, 0xC98A28, 0x4C86F5, 0x9567EA, 0x2A9BDB, 0x1C2330,
        ]
    };
    skiff_term::TermStyle {
        font: crate::theme::mono(12.5),
        fg: pal.text,
        bg: pal.bg,
        cursor: pal.accent,
        selection_bg: pal.selected,
        ansi: ansi.map(c),
        ligatures: false,
    }
}
