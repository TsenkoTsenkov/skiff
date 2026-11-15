//! The bottom dock: log streams, shells and port forwards.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Align, FontId, Frame, Layout, Margin, Panel, RichText, TextFormat, Ui};
use egui_material_icons::icons;
use skiff_core::logs::strip_ansi;
use skiff_core::{ContainerInfo, Job, LogOptions, LogStream, LogTarget, Session, SourceState, Tone};

use crate::log_text;
use crate::shell::ShellTab;
use crate::theme::{self, Palette};
use crate::widgets;

const LOG_ROW: f32 = 18.0;

pub struct LogsTab {
    session: Arc<Session>,
    targets_job: Option<Job<Vec<LogTarget>>>,
    targets: Vec<LogTarget>,
    stream: Option<Arc<LogStream>>,
    error: Option<String>,
    options: LogOptions,
    filter: String,
    wrap: bool,
    timestamps: bool,
    jump_to_end: bool,
    // Incremental filter state: absolute sequence numbers of matching lines.
    matched: VecDeque<u64>,
    scanned: u64,
    matched_filter: String,
    matched_stream: usize,
    /// Show only the lines that match (otherwise all, with matches marked).
    only_matching: bool,
    /// The match that next / previous moved to (absolute sequence number).
    current: Option<u64>,
    /// Bring the current match into view on the next frame.
    reveal_current: bool,
    focus_filter: bool,
}

impl LogsTab {
    fn new(session: &Arc<Session>, job: Job<Vec<LogTarget>>, tail: Option<i64>) -> Self {
        LogsTab {
            session: session.clone(),
            targets_job: Some(job),
            targets: Vec::new(),
            stream: None,
            error: None,
            options: LogOptions { tail, ..LogOptions::default() },
            filter: String::new(),
            wrap: false,
            timestamps: false,
            jump_to_end: true,
            matched: VecDeque::new(),
            scanned: 0,
            matched_filter: String::new(),
            matched_stream: 0,
            only_matching: true,
            current: None,
            reveal_current: false,
            focus_filter: false,
        }
    }

    fn restart(&mut self) {
        if let Some(s) = self.stream.take() {
            s.stop();
        }
        if !self.targets.is_empty() {
            self.stream = Some(self.session.logs(self.targets.clone(), self.options.clone()));
        }
        self.matched.clear();
        self.scanned = 0;
        self.jump_to_end = true;
    }

    fn poll(&mut self) {
        if let Some(job) = &self.targets_job {
            if let Some(res) = job.take() {
                self.targets_job = None;
                match res {
                    Ok(targets) => {
                        self.targets = targets;
                        self.restart();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    /// Keeps `matched` in sync with the buffer, scanning only new lines.
    fn update_filter(&mut self, stream: &Arc<LogStream>) {
        let needle = self.filter.to_lowercase();
        let stream_id = Arc::as_ptr(stream) as usize;
        if needle != self.matched_filter || stream_id != self.matched_stream {
            self.matched.clear();
            self.scanned = 0;
            self.matched_filter = needle.clone();
            self.matched_stream = stream_id;
            self.current = None;
        }
        if needle.is_empty() {
            return;
        }
        stream.with(|buf| {
            let base = buf.dropped;
            while self.matched.front().is_some_and(|seq| *seq < base) {
                self.matched.pop_front();
            }
            let start = self.scanned.max(base);
            let end = base + buf.lines.len() as u64;
            for seq in start..end {
                let line = &buf.lines[(seq - base) as usize];
                if contains_ci(&line.text, &needle) {
                    self.matched.push_back(seq);
                }
            }
            self.scanned = end;
        });
    }

    /// Index of the current match in `matched`, if it is still there.
    fn current_index(&self) -> Option<usize> {
        let seq = self.current?;
        self.matched.binary_search(&seq).ok()
    }

    /// Moves to the next (`forward`) or previous match, wrapping around.
    fn step_match(&mut self, forward: bool) {
        let n = self.matched.len();
        if n == 0 {
            return;
        }
        let next = match self.current_index() {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            // Starting out: the newest match going up, the oldest going down.
            None if forward => 0,
            None => n - 1,
        };
        self.current = self.matched.get(next).copied();
        self.reveal_current = true;
    }

    /// The command that follows these logs with `kubectl`.
    fn kubectl_command(&self, stream: &LogStream) -> String {
        let targets: Vec<(String, String, String)> =
            stream.targets.iter().map(|t| (t.namespace.clone(), t.pod.clone(), t.container.clone())).collect();
        crate::external::logs_command(
            &self.session.context,
            &targets,
            self.options.tail,
            self.options.previous,
            self.timestamps,
        )
    }

    fn ui(&mut self, ui: &mut Ui, pal: &Palette, toasts: &mut Vec<(String, Tone)>) {
        if let Some(err) = &self.error {
            widgets::empty_state(ui, icons::ICON_ERROR, "Couldn't open logs", err, pal);
            return;
        }
        let Some(stream) = self.stream.clone() else {
            widgets::empty_state(ui, icons::ICON_SYNC, "Finding containers…", "", pal);
            return;
        };
        self.update_filter(&stream);
        let filtering = !self.matched_filter.is_empty();
        let (total, dropped) = stream.with(|b| (b.lines.len(), b.dropped));
        let only = filtering && self.only_matching;
        let shown = if only { self.matched.len() } else { total };

        let mut restart = false;
        ui.horizontal(|ui| {
            let id = egui::Id::unique("log-filter").with(Arc::as_ptr(&stream) as usize);
            let edit = widgets::search_box(ui, &mut self.filter, "Find in logs", 260.0, id);
            if std::mem::take(&mut self.focus_filter) {
                edit.request_focus();
            }
            // Enter / Shift+Enter step through the matches and keep the
            // keyboard in the box.
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let back = ui.input(|i| i.modifiers.shift);
                // Taken here: the table drawn after the dock opens the
                // selected row on Enter.
                ui.input_mut(|i| {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                    i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter);
                });
                self.step_match(!back);
                self.focus_filter = true;
            }
            if filtering {
                let pos = self.current_index().map(|i| i + 1);
                let count = match (pos, self.matched.len()) {
                    (_, 0) => "no matches".to_string(),
                    (Some(p), n) => format!("{} / {}", widgets::thousands(p), widgets::thousands(n)),
                    (None, n) => widgets::plural(n, "match"),
                };
                let color = if self.matched.is_empty() { pal.err } else { pal.muted };
                ui.label(RichText::new(count).color(color).size(12.0));
                if widgets::icon_button(ui, icons::ICON_KEYBOARD_ARROW_UP, "Previous match (Shift+Enter)").clicked() {
                    self.step_match(false);
                }
                if widgets::icon_button(ui, icons::ICON_KEYBOARD_ARROW_DOWN, "Next match (Enter)").clicked() {
                    self.step_match(true);
                }
                let tip = if self.only_matching {
                    "Showing only matching lines — click to show all lines with matches highlighted"
                } else {
                    "Showing all lines — click to show only matching lines"
                };
                if widgets::icon_toggle(ui, self.only_matching, icons::ICON_FILTER_LIST, tip, pal).clicked() {
                    self.only_matching = !self.only_matching;
                    self.reveal_current = self.current.is_some();
                }
                ui.add_space(6.0);
            }
            if widgets::icon_toggle(ui, self.wrap, icons::ICON_WRAP_TEXT, "Wrap long lines", pal).clicked() {
                self.wrap = !self.wrap;
            }
            if widgets::icon_toggle(ui, self.timestamps, icons::ICON_SCHEDULE, "Show timestamps", pal).clicked() {
                self.timestamps = !self.timestamps;
            }
            if widgets::icon_toggle(ui, self.options.previous, icons::ICON_HISTORY, "Previous container instance", pal)
                .clicked()
            {
                self.options.previous = !self.options.previous;
                restart = true;
            }
            let tail_label = match self.options.tail {
                Some(n) => format!("last {n}"),
                None => "all".to_string(),
            };
            let tail = ui.add(egui::Button::new(RichText::new(tail_label).size(12.0))).on_hover_text("History to load");
            egui::Popup::menu(&tail).show(|ui| {
                for (label, n) in [("last 100", Some(100)), ("last 1000", Some(1000)), ("last 10000", Some(10_000)), ("all", None)] {
                    if ui.add(egui::Button::selectable(self.options.tail == n, label)).clicked() {
                        self.options.tail = n;
                        restart = true;
                        ui.close();
                    }
                }
            });
            if widgets::icon_button(ui, icons::ICON_VERTICAL_ALIGN_BOTTOM, "Jump to latest").clicked() {
                self.jump_to_end = true;
            }
            if widgets::icon_button(ui, icons::ICON_CONTENT_COPY, "Copy shown lines").clicked() {
                let text = self.collect_text(&stream);
                let lines = text.lines().count();
                ui.ctx().copy_text(text);
                toasts.push((format!("Copied {lines} lines"), Tone::Normal));
            }
            if widgets::icon_button(ui, icons::ICON_DELETE, "Clear").clicked() {
                stream.clear();
                self.matched.clear();
                self.scanned = 0;
            }
            if widgets::icon_button(ui, icons::ICON_OPEN_IN_NEW, "Follow these logs in your terminal app (kubectl)").clicked() {
                match crate::external::open(&self.kubectl_command(&stream)) {
                    Ok(app) => toasts.push((format!("Opened in {app}"), Tone::Normal)),
                    Err(e) => toasts.push((e, Tone::Err)),
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let states = stream.states();
                let live = states.iter().filter(|s| matches!(s, SourceState::Streaming)).count();
                let failed: Vec<&str> = states
                    .iter()
                    .filter_map(|s| match s {
                        SourceState::Failed(e) => Some(e.as_str()),
                        _ => None,
                    })
                    .collect();
                let mut summary = if filtering {
                    format!("{} of {}", widgets::thousands(shown), widgets::plural(total, "line"))
                } else {
                    widgets::plural(total, "line")
                };
                if dropped > 0 {
                    summary.push_str(&format!(" (+{} older dropped)", widgets::thousands(dropped as usize)));
                }
                ui.label(RichText::new(summary).color(pal.muted).size(12.0));
                if !failed.is_empty() {
                    widgets::pill(ui, &format!("{} failed", failed.len()), pal.err, pal).on_hover_text(failed.join("\n"));
                }
                let retrying = states.iter().filter(|s| matches!(s, SourceState::Reconnecting)).count();
                if retrying > 0 {
                    widgets::pill(ui, &format!("reconnecting {retrying}"), pal.warn, pal);
                }
                if live > 0 {
                    widgets::pill(ui, &format!("streaming {live}/{}", states.len()), pal.ok, pal);
                } else if retrying > 0 {
                    // Covered by the pill above.
                } else if states.iter().any(|s| matches!(s, SourceState::Connecting)) {
                    ui.spinner();
                } else {
                    widgets::pill(ui, "ended", pal.muted, pal);
                }
            });
        });
        if restart {
            self.restart();
            return;
        }

        let multi = stream.targets.len() > 1;
        let multi_pod = stream.targets.iter().any(|t| t.pod != stream.targets[0].pod);
        let prefixes: Vec<String> = stream
            .targets
            .iter()
            .map(|t| if multi_pod { format!("{}/{}", t.pod, t.container) } else { t.container.clone() })
            .collect();
        let font = theme::mono(12.0);
        let timestamps = self.timestamps;
        let wrap = self.wrap;
        let ansi = log_text::ansi16(pal.dark);
        let needle = self.matched_filter.clone();
        let current_seq = self.current;
        let look = log_text::Look {
            font: font.clone(),
            default: pal.text,
            match_fg: egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
            match_bg: egui::Color32::from_rgb(0xF5, 0xD5, 0x4A),
            current_bg: egui::Color32::from_rgb(0xFF, 0x96, 0x32),
        };
        // The row to bring into view: where the current match is drawn.
        let reveal_row: Option<usize> = if std::mem::take(&mut self.reveal_current) {
            match (self.current_index(), self.current) {
                (Some(i), _) if only => Some(i),
                (Some(_), Some(seq)) => seq.checked_sub(dropped).map(|r| r as usize),
                _ => None,
            }
        } else {
            None
        };

        Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.style_mut().interaction.selectable_labels = true;
            ui.style_mut().interaction.multi_widget_text_select = true;
            ui.spacing_mut().item_spacing.y = 0.0;
            if shown == 0 {
                let msg = if only { "No lines match the filter" } else { "No log output yet" };
                widgets::empty_state(ui, icons::ICON_SUBJECT, msg, "", pal);
                return;
            }
            let mut area = if wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() }
                .auto_shrink([false, false])
                .stick_to_bottom(true);
            let jump = std::mem::take(&mut self.jump_to_end) && reveal_row.is_none();
            if jump && !wrap {
                // Any offset past the end is clamped to the last row. (An
                // "infinite" offset overflows the row arithmetic.)
                area = area.vertical_scroll_offset(shown as f32 * LOG_ROW);
            }
            if let (Some(row), false) = (reveal_row, wrap) {
                let half = (ui.available_height() / 2.0 - LOG_ROW).max(0.0);
                area = area.vertical_scroll_offset((row as f32 * LOG_ROW - half).max(0.0));
            }
            let line_job = |ui: &Ui, buf: &skiff_core::logs::LogBuffer, row: usize| -> Option<LayoutJob> {
                // The stream may have evicted lines since the filter was
                // brought up to date; such rows simply are not drawn.
                let idx = if only { self.matched.get(row)?.checked_sub(buf.dropped)? as usize } else { row };
                let line = buf.lines.get(idx)?;
                let seq = buf.dropped + idx as u64;
                let mut job = LayoutJob::default();
                if wrap {
                    job.wrap.max_width = ui.available_width();
                } else {
                    job.wrap.max_width = f32::INFINITY;
                }
                let fmt = |color| TextFormat { font_id: font.clone(), color, ..Default::default() };
                if timestamps && line.time_ms > 0 {
                    job.append(&format_time(line.time_ms), 0.0, fmt(pal.faint));
                    job.append("  ", 0.0, fmt(pal.faint));
                }
                if multi {
                    let src = line.source as usize;
                    job.append(prefixes.get(src).map(String::as_str).unwrap_or("?"), 0.0, fmt(pal.series(src)));
                    job.append("  ", 0.0, fmt(pal.faint));
                }
                let parsed = log_text::parse(&line.text, &ansi);
                let matches = log_text::find_all(&parsed.text, &needle);
                let look = log_text::Look { default: level_color(&parsed.text, pal), ..look.clone() };
                log_text::append(&mut job, &parsed, &matches, current_seq == Some(seq), &look);
                Some(job)
            };
            if wrap {
                // Wrapped lines have varying heights, so only the tail is laid
                // out. The jobs are built under the buffer lock and laid out
                // after releasing it, so the stream is never blocked on text
                // layout.
                const WRAP_WINDOW: usize = 1000;
                let first = shown.saturating_sub(WRAP_WINDOW);
                area.show(ui, |ui| {
                    let jobs: Vec<(usize, LayoutJob)> = stream.with(|buf| {
                        (first..shown).filter_map(|row| line_job(ui, buf, row).map(|j| (row, j))).collect()
                    });
                    if first > 0 {
                        ui.label(
                            RichText::new(format!("{} earlier lines hidden while wrapping", widgets::thousands(first)))
                                .color(pal.faint)
                                .size(11.5),
                        );
                    }
                    for (row, job) in jobs {
                        let resp = ui.add(egui::Label::new(job).selectable(true));
                        if reveal_row == Some(row) {
                            resp.scroll_to_me(Some(Align::Center));
                        }
                    }
                    if jump {
                        ui.scroll_to_cursor(Some(Align::BOTTOM));
                    }
                });
            } else {
                area.show_rows(ui, LOG_ROW, shown, |ui, range| {
                    stream.with(|buf| {
                        for row in range {
                            if let Some(job) = line_job(ui, buf, row) {
                                // Fixed-height, left-aligned rows keep the
                                // virtualized scroll position exact.
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width().max(1.0), LOG_ROW),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.add(egui::Label::new(job).selectable(true).extend());
                                    },
                                );
                            }
                        }
                    });
                });
            }
        });
    }

    fn collect_text(&self, stream: &Arc<LogStream>) -> String {
        let filtering = !self.matched_filter.is_empty() && self.only_matching;
        stream.with(|buf| {
            let mut out = String::new();
            let mut push = |line: &skiff_core::LogLine| {
                out.push_str(&strip_ansi(&line.text));
                out.push('\n');
            };
            if filtering {
                for seq in &self.matched {
                    let line = seq.checked_sub(buf.dropped).and_then(|i| buf.lines.get(i as usize));
                    if let Some(line) = line {
                        push(line);
                    }
                }
            } else {
                buf.lines.iter().for_each(&mut push);
            }
            out
        })
    }
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let (h, n) = (hay.as_bytes(), needle_lower.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|i| h[i..i + n.len()].iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

fn level_color(text: &str, pal: &Palette) -> egui::Color32 {
    // Only look near the start: that's where the level lives, and it keeps
    // this cheap for very long lines.
    let head = &text.as_bytes()[..text.len().min(96)];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
    if has(b"error") || has(b"fatal") || has(b"panic") || has(b" err ") || has(b"[err]") {
        pal.err
    } else if has(b"warn") {
        pal.warn
    } else if has(b"debug") || has(b"trace") {
        pal.muted
    } else {
        pal.text
    }
}

/// `HH:MM:SS.mmm` in UTC.
fn format_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}.{:03}", day / 3600, (day % 3600) / 60, day % 60, ms.rem_euclid(1000))
}

enum TabKind {
    Logs(Box<LogsTab>),
    Shell(Box<ShellTab>),
    Forwards,
}

struct Tab {
    id: u64,
    title: String,
    context: String,
    kind: TabKind,
}

pub struct Dock {
    pub open: bool,
    /// Earlier lines a new log tab loads (Settings).
    pub default_tail: Option<i64>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    /// The dock was drawn in the previous frame.
    was_shown: bool,
    /// The user's last click was inside the dock (or it was just opened):
    /// find shortcuts are then for the dock's tab, not for the table.
    attention: bool,
}

impl Default for Dock {
    fn default() -> Self {
        Dock {
            open: false,
            default_tail: LogOptions::default().tail,
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            was_shown: false,
            attention: false,
        }
    }
}

#[derive(Default)]
pub struct DockOutput {
    pub terminal_focused: bool,
    pub toasts: Vec<(String, Tone)>,
}

impl Dock {
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    fn push(&mut self, title: String, context: &str, kind: TabKind) {
        self.next_id += 1;
        self.tabs.push(Tab { id: self.next_id, title, context: context.to_string(), kind });
        self.active = self.tabs.len() - 1;
        self.open = true;
        self.attention = true;
    }

    /// Whether a find shortcut pressed now is meant for the dock.
    pub fn has_attention(&self) -> bool {
        self.open && !self.tabs.is_empty() && self.attention
    }

    pub fn open_logs(&mut self, session: &Arc<Session>, title: String, job: Job<Vec<LogTarget>>) {
        self.push(title, &session.context, TabKind::Logs(Box::new(LogsTab::new(session, job, self.default_tail))));
    }

    /// Opens a shell in the pod's first container.
    pub fn open_shell(&mut self, session: &Arc<Session>, ns: &str, pod: &str, job: Job<Vec<ContainerInfo>>) {
        let tab = ShellTab::resolving(session, ns, pod, job);
        self.push(pod.to_string(), &session.context, TabKind::Shell(Box::new(tab)));
    }

    pub fn open_shell_in(&mut self, session: &Arc<Session>, ns: &str, pod: &str, container: &str) {
        let tab = ShellTab::start(session, ns, pod, container);
        self.push(format!("{pod} · {container}"), &session.context, TabKind::Shell(Box::new(tab)));
    }

    pub fn show_forwards(&mut self) {
        match self.tabs.iter().position(|t| matches!(t.kind, TabKind::Forwards)) {
            Some(i) => self.active = i,
            None => self.push("Port forwards".to_string(), "", TabKind::Forwards),
        }
        self.open = true;
    }

    /// Gives the keyboard to the shell in the front tab, if the dock is open
    /// and that tab is a shell.
    pub fn focus_shell(&mut self) {
        if !self.open {
            return;
        }
        if let Some(Tab { kind: TabKind::Shell(s), .. }) = self.tabs.get_mut(self.active) {
            s.focus();
        }
    }

    /// What the first shell tab's terminal currently shows, whether or not
    /// that tab is in front (for tests and diagnostics).
    #[cfg(feature = "terminal")]
    pub fn shell_screen(&self) -> Option<String> {
        self.tabs.iter().find_map(|t| match &t.kind {
            TabKind::Shell(s) => Some(s.screen_text()),
            _ => None,
        })
    }

    pub fn close_for_context(&mut self, context: &str) {
        self.tabs.retain(|t| t.context != context);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    pub fn ui(&mut self, ui: &mut Ui, pal: &Palette, sessions: &HashMap<String, Arc<Session>>) -> DockOutput {
        let mut out = DockOutput::default();
        let shown = if self.open { Some(self.active.min(self.tabs.len().saturating_sub(1))) } else { None };
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            match &mut tab.kind {
                TabKind::Logs(l) => {
                    l.poll();
                    // Streams that are not on screen keep collecting lines
                    // but need not wake the UI for each one.
                    if let Some(stream) = &l.stream {
                        stream.set_visible(shown == Some(i));
                    }
                }
                TabKind::Shell(s) => s.poll(ui.ctx()),
                TabKind::Forwards => {}
            }
        }
        if !self.open || self.tabs.is_empty() {
            self.was_shown = false;
            return out;
        }
        let mut close: Option<usize> = None;
        let mut hide = false;
        // Choosing a shell's tab, or bringing the dock back with a shell in
        // front, is asking for that shell: it gets the keyboard.
        let mut focus_shell = !self.was_shown;
        self.was_shown = true;
        // Find: Cmd/Ctrl+F in logs; in a shell the terminal's own shortcut
        // (Ctrl+F is a key the shell uses). Taken before the tab draws: a
        // focused terminal passes every other key to the remote program.
        let mut find = false;
        if self.attention {
            let active = self.active.min(self.tabs.len() - 1);
            let shell_shortcut = crate::shell::find_shortcut(ui.ctx());
            let cmd_f = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F);
            find = ui.input_mut(|i| match self.tabs[active].kind {
                TabKind::Shell(_) => i.consume_shortcut(&shell_shortcut),
                TabKind::Logs(_) => i.consume_shortcut(&cmd_f) || i.consume_shortcut(&shell_shortcut),
                TabKind::Forwards => false,
            });
        }
        let panel = Panel::bottom("dock")
            .resizable(true)
            .default_size(300.0)
            .size_range(120.0..=1200.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin { left: 10, right: 8, top: 6, bottom: 4 }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    egui::ScrollArea::horizontal().max_width(ui.available_width() - 40.0).show(ui, |ui| {
                        for (i, tab) in self.tabs.iter().enumerate() {
                            let ic = match tab.kind {
                                TabKind::Logs(_) => icons::ICON_SUBJECT,
                                TabKind::Shell(_) => icons::ICON_TERMINAL,
                                TabKind::Forwards => icons::ICON_SWAP_HORIZ,
                            };
                            let label = (widgets::icon(ic, 14.0), tab.title.as_str());
                            if ui.add(egui::Button::selectable(i == self.active, label)).clicked() {
                                self.active = i;
                                focus_shell = true;
                            }
                            let x = egui::Button::new(widgets::icon(icons::ICON_CLOSE, 13.0).color(pal.muted))
                                .frame_when_inactive(false)
                                .min_size(egui::vec2(18.0, 20.0));
                            if ui.add(x).on_hover_text("Close tab").clicked() {
                                close = Some(i);
                            }
                            ui.add_space(6.0);
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, icons::ICON_KEYBOARD_ARROW_DOWN, "Hide panel").clicked() {
                            hide = true;
                        }
                    });
                });
                ui.add_space(2.0);
                let active = self.active.min(self.tabs.len() - 1);
                let tab = &mut self.tabs[active];
                if find {
                    match &mut tab.kind {
                        TabKind::Logs(l) => l.focus_filter = true,
                        TabKind::Shell(s) => s.open_find(),
                        TabKind::Forwards => {}
                    }
                }
                if focus_shell {
                    if let TabKind::Shell(s) = &mut tab.kind {
                        s.focus();
                    }
                }
                ui.push_id(tab.id, |ui| match &mut tab.kind {
                    TabKind::Logs(l) => l.ui(ui, pal, &mut out.toasts),
                    TabKind::Shell(s) => out.terminal_focused = s.ui(ui, pal, &mut out.toasts),
                    TabKind::Forwards => forwards_ui(ui, pal, sessions, &mut out.toasts),
                });
            });
        let dock_rect = panel.response.rect;
        if let Some(pos) = ui.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None }) {
            self.attention = dock_rect.contains(pos);
        }
        if out.terminal_focused {
            self.attention = true;
        }
        if let Some(i) = close {
            self.tabs.remove(i);
            if self.active >= i && self.active > 0 {
                self.active -= 1;
            }
            if self.tabs.is_empty() {
                self.open = false;
            }
        }
        if hide {
            self.open = false;
        }
        out
    }
}

fn forwards_ui(ui: &mut Ui, pal: &Palette, sessions: &HashMap<String, Arc<Session>>, toasts: &mut Vec<(String, Tone)>) {
    let mut any = false;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut names: Vec<&String> = sessions.keys().collect();
        names.sort();
        for ctx_name in names {
            let session = &sessions[ctx_name];
            for f in session.forwards() {
                any = true;
                ui.horizontal(|ui| {
                    widgets::dot(ui, if f.is_active() { pal.ok } else { pal.muted });
                    ui.label(RichText::new(format!("localhost:{}", f.local_port)).font(FontId::monospace(12.5)).color(pal.text));
                    ui.label(RichText::new("→").color(pal.faint));
                    ui.label(format!("{}:{}", f.label, f.remote_port));
                    ui.label(RichText::new(format!("{} / {}", f.namespace, ctx_name)).color(pal.muted).size(12.0));
                    let n = f.connections.load(Ordering::Relaxed);
                    ui.label(RichText::new(widgets::plural(n as usize, "connection")).color(pal.faint).size(12.0));
                    if let Some(err) = f.last_error.lock().clone() {
                        widgets::pill(ui, "error", pal.err, pal).on_hover_text(err);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, icons::ICON_STOP, "Stop forwarding").clicked() {
                            session.stop_forward(f.id);
                        }
                        if widgets::icon_button(ui, icons::ICON_CONTENT_COPY, "Copy URL").clicked() {
                            ui.ctx().copy_text(f.url());
                            toasts.push((format!("Copied {}", f.url()), Tone::Normal));
                        }
                        if widgets::icon_button(ui, icons::ICON_OPEN_IN_NEW, "Open in browser").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(f.url()));
                        }
                    });
                });
            }
        }
        if !any {
            widgets::empty_state(
                ui,
                icons::ICON_SWAP_HORIZ,
                "No active port forwards",
                "Start one from a pod or service (right-click → Port forward).",
                pal,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_contains() {
        assert!(contains_ci("Hello ERROR world", "error"));
        assert!(!contains_ci("Hello", "error"));
        assert!(contains_ci("x", ""));
    }

    #[test]
    fn times() {
        assert_eq!(format_time(1704067201500), "00:00:01.500");
        assert_eq!(format_time(1704110400000), "12:00:00.000");
    }

    #[test]
    fn levels() {
        let p = Palette::dark();
        assert_eq!(level_color("2024 ERROR boom", &p), p.err);
        assert_eq!(level_color("level=warn msg", &p), p.warn);
        assert_eq!(level_color("all good", &p), p.text);
    }
}
