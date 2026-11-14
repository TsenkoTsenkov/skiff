//! `skiff-term`: an egui terminal widget driven by raw byte streams.
//!
//! It has no PTY and no networking. Bytes from a remote process (e.g. a `kubectl exec`
//! stdout stream) go into [`TerminalView::feed`]. [`TerminalView::ui`] paints the
//! grid and returns the bytes the user typed (plus terminal replies) in [`TermOutput`].
//!
//! Emulation is `alacritty_terminal` 0.26 (`Term` + vte `Processor`).

pub mod keys;

use std::sync::mpsc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color, CursorShape, NamedColor, Processor, Rgb, StdSyncHandler,
};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, CornerRadius, Event, EventFilter, FontId, Key, Pos2, Rect, Sense, Stroke, StrokeKind,
    Vec2, ViewportCommand,
};

pub use keys::{encode_key, encode_paste, encode_text, KeyMode};

/// Inner padding between the widget edge and the cell grid, in points.
const PAD: f32 = 4.0;
/// Lines scrolled per mouse-wheel "line" notch.
const WHEEL_LINES: f32 = 3.0;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Colours and font for a [`TerminalView`].
#[derive(Clone, Debug, PartialEq)]
pub struct TermStyle {
    /// Monospace font to draw with.
    pub font: FontId,
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection_bg: Color32,
    /// 0-7 normal, 8-15 bright.
    pub ansi: [Color32; 16],
    /// Allow font ligatures (egui 0.37 shapes text with harfrust, so fonts such as
    /// JetBrains Mono turn `--` / `->` into ligatures). Off by default: `ls -l`
    /// permission strings like `-rw-r--r--` should look literal.
    pub ligatures: bool,
}

impl Default for TermStyle {
    fn default() -> Self {
        // A muted dark palette (close to VS Code's default terminal colours).
        Self {
            font: FontId::monospace(13.0),
            fg: Color32::from_rgb(0xd4, 0xd4, 0xd4),
            bg: Color32::from_rgb(0x1a, 0x1b, 0x1f),
            cursor: Color32::from_rgb(0xd4, 0xd4, 0xd4),
            selection_bg: Color32::from_rgb(0x26, 0x4f, 0x78),
            ansi: [
                Color32::from_rgb(0x00, 0x00, 0x00),
                Color32::from_rgb(0xcd, 0x31, 0x31),
                Color32::from_rgb(0x0d, 0xbc, 0x79),
                Color32::from_rgb(0xe5, 0xe5, 0x10),
                Color32::from_rgb(0x24, 0x72, 0xc8),
                Color32::from_rgb(0xbc, 0x3f, 0xbc),
                Color32::from_rgb(0x11, 0xa8, 0xcd),
                Color32::from_rgb(0xe5, 0xe5, 0xe5),
                Color32::from_rgb(0x66, 0x66, 0x66),
                Color32::from_rgb(0xf1, 0x4c, 0x4c),
                Color32::from_rgb(0x23, 0xd1, 0x8b),
                Color32::from_rgb(0xf5, 0xf5, 0x43),
                Color32::from_rgb(0x3b, 0x8e, 0xea),
                Color32::from_rgb(0xd6, 0x70, 0xd6),
                Color32::from_rgb(0x29, 0xb8, 0xdb),
                Color32::from_rgb(0xff, 0xff, 0xff),
            ],
            ligatures: false,
        }
    }
}

/// What happened during one [`TerminalView::ui`] call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TermOutput {
    /// Bytes to write to the remote stdin this frame: keys, paste and terminal
    /// replies (`Event::PtyWrite`, e.g. cursor-position reports).
    pub input: Vec<u8>,
    /// `(cols, rows)` when the grid size changed (also on the first frame).
    pub resize: Option<(u16, u16)>,
    pub has_focus: bool,
    /// The remote rang the bell (BEL) since the last frame.
    pub bell: bool,
}

/// A terminal emulator widget. See the crate docs.
pub struct TerminalView {
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    events: mpsc::Receiver<TermEvent>,
    /// Replies generated while parsing, delivered with the next `TermOutput`.
    pending_input: Vec<u8>,
    bell: bool,
    title: Option<String>,
    cols: u16,
    rows: u16,
    reported: Option<(u16, u16)>,
    cell: Vec2,
    focus_requested: bool,
    scroll_accum: f32,
    /// Style seen by the last `ui` call (to answer OSC colour queries).
    style: TermStyle,
}

impl Default for TerminalView {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Listener(mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

#[derive(Clone, Copy)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// One horizontal run of cells sharing a style.
struct Run {
    row: usize,
    col: usize,
    cells: usize,
    text: String,
    fg: Color32,
    bg: Color32,
    flags: Flags,
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// xterm 256-colour palette entry (16..=255).
fn xterm_256(i: u8) -> Color32 {
    if i >= 232 {
        let v = 8 + 10 * (i - 232);
        Color32::from_rgb(v, v, v)
    } else {
        let i = i - 16;
        let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
    }
}

/// Colour for a palette index 0..=258 (256 = fg, 257 = bg, 258 = cursor), honouring
/// OSC 4/10/11 overrides stored in `colors`.
fn palette(index: usize, colors: &Colors, style: &TermStyle) -> Color32 {
    if let Some(c) = colors[index] {
        return rgb(c);
    }
    match index {
        0..=15 => style.ansi[index],
        16..=255 => xterm_256(index as u8),
        256 => style.fg,
        257 => style.bg,
        258 => style.cursor,
        _ => style.fg,
    }
}

fn resolve(color: Color, colors: &Colors, style: &TermStyle) -> Color32 {
    match color {
        Color::Spec(c) => rgb(c),
        Color::Indexed(i) => palette(usize::from(i), colors, style),
        Color::Named(n) => {
            let idx = n as usize;
            match n {
                NamedColor::Foreground | NamedColor::Background | NamedColor::Cursor => {
                    palette(idx, colors, style)
                }
                NamedColor::BrightForeground => colors[idx].map_or(style.fg, rgb),
                NamedColor::DimForeground => {
                    colors[idx].map_or_else(|| lerp(style.fg, style.bg, 0.33), rgb)
                }
                _ if idx < 16 => palette(idx, colors, style),
                // DimBlack..DimWhite
                _ => {
                    let base = idx - NamedColor::DimBlack as usize;
                    colors[idx]
                        .map_or_else(|| lerp(palette(base, colors, style), style.bg, 0.33), rgb)
                }
            }
        }
    }
}

impl TerminalView {
    /// A new 80x24 terminal (resized to the widget on the first `ui` call).
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let size = GridSize { cols: 80, rows: 24 };
        Self {
            term: Term::new(Config::default(), &size, Listener(tx)),
            parser: Processor::new(),
            events: rx,
            pending_input: Vec::new(),
            bell: false,
            title: None,
            cols: 80,
            rows: 24,
            reported: None,
            cell: Vec2::new(8.0, 16.0),
            focus_requested: false,
            scroll_accum: 0.0,
            style: TermStyle::default(),
        }
    }

    /// Feed a chunk of remote stdout/stderr. Call `ctx.request_repaint()` afterwards
    /// if you feed from outside the UI thread.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.drain_term_events();
    }

    /// Give the terminal keyboard focus on the next `ui` call.
    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    /// Window title set by the remote (OSC 0/2), if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The currently selected text, if any.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Current grid size `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Text of a visible row (0 = top of the viewport), trailing blanks trimmed.
    /// Mostly useful for tests and accessibility.
    pub fn row_text(&self, row: usize) -> String {
        let grid = self.term.grid();
        let line = Line(row as i32 - grid.display_offset() as i32);
        let mut s: String = grid[line]
            .into_iter()
            .filter(|c| {
                !c.flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            })
            .map(|c| c.c)
            .collect();
        s.truncate(s.trim_end().len());
        s
    }

    /// Lines scrolled back into history (0 = following the output).
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Scroll the viewport into history (`lines > 0`) or back down (`< 0`).
    pub fn scroll_lines(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Clear the selection.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Select everything on screen and in the scrollback (for "copy all").
    pub fn select_all(&mut self) {
        let grid = self.term.grid();
        let top = Point::new(grid.topmost_line(), Column(0));
        let bottom = Point::new(grid.bottommost_line(), grid.last_column());
        let mut sel = Selection::new(SelectionType::Simple, top, Side::Left);
        sel.update(bottom, Side::Right);
        self.term.selection = Some(sel);
    }

    fn drain_term_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                TermEvent::PtyWrite(s) => self.pending_input.extend_from_slice(s.as_bytes()),
                TermEvent::Title(t) => self.title = Some(t),
                TermEvent::ResetTitle => self.title = None,
                TermEvent::Bell => self.bell = true,
                // OSC 52 (the remote writing to, or reading, the local clipboard)
                // is deliberately not honoured: a process in a container must
                // not be able to plant text for the user to paste elsewhere.
                TermEvent::ColorRequest(index, fmt) => {
                    let c = palette(index, self.term.colors(), &self.style);
                    let reply = fmt(Rgb {
                        r: c.r(),
                        g: c.g(),
                        b: c.b(),
                    });
                    self.pending_input.extend_from_slice(reply.as_bytes());
                }
                TermEvent::TextAreaSizeRequest(fmt) => {
                    let reply = fmt(WindowSize {
                        num_lines: self.rows,
                        num_cols: self.cols,
                        cell_width: self.cell.x.round() as u16,
                        cell_height: self.cell.y.round() as u16,
                    });
                    self.pending_input.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            }
        }
    }

    fn key_mode(&self) -> KeyMode {
        KeyMode {
            app_cursor: self.term.mode().contains(TermMode::APP_CURSOR),
        }
    }

    fn copy_selection(&self, ctx: &egui::Context) {
        if let Some(text) = self.selection_text() {
            ctx.copy_text(text);
        }
    }

    /// Viewport position -> grid point + side of the cell.
    fn point_at(&self, origin: Pos2, pos: Pos2) -> (Point, Side) {
        let x = ((pos.x - origin.x) / self.cell.x).max(0.0);
        let y = ((pos.y - origin.y) / self.cell.y).max(0.0);
        let col = (x as usize).min(usize::from(self.cols).saturating_sub(1));
        let row = (y as usize).min(usize::from(self.rows).saturating_sub(1));
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let line = Line(row as i32 - self.term.grid().display_offset() as i32);
        (Point::new(line, Column(col)), side)
    }

    /// Draw the terminal into all of `ui.available_size()` and handle input.
    pub fn ui(&mut self, ui: &mut egui::Ui, style: &TermStyle) -> TermOutput {
        if *style != self.style {
            self.style = style.clone();
        }
        let ctx = ui.ctx().clone();
        let is_mac = ctx.os() == egui::os::OperatingSystem::Mac;
        let mut out = TermOutput::default();

        // ---- layout & resize ------------------------------------------------
        let (cell_w, cell_h) =
            ctx.fonts_mut(|f| (f.glyph_width(&style.font, 'M'), f.row_height(&style.font)));
        self.cell = Vec2::new(cell_w.max(1.0), cell_h.max(1.0));
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Terminal, true, "Terminal"));
        let grid_origin = rect.min + Vec2::splat(PAD);
        let cols =
            (((rect.width() - 2.0 * PAD) / self.cell.x).floor() as i64).clamp(2, 1000) as u16;
        let rows =
            (((rect.height() - 2.0 * PAD) / self.cell.y).floor() as i64).clamp(1, 1000) as u16;
        if (cols, rows) != (self.cols, self.rows) {
            self.term.resize(GridSize {
                cols: usize::from(cols),
                rows: usize::from(rows),
            });
            self.cols = cols;
            self.rows = rows;
        }
        if self.reported != Some((cols, rows)) {
            self.reported = Some((cols, rows));
            out.resize = Some((cols, rows));
        }

        // ---- focus ------------------------------------------------------------
        if self.focus_requested || resp.clicked() || resp.drag_started() {
            resp.request_focus();
            self.focus_requested = false;
        }
        let has_focus = resp.has_focus();
        if has_focus {
            // Keep Tab / arrows / Esc for the terminal instead of egui focus navigation.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        }
        out.has_focus = has_focus;

        // ---- mouse: selection --------------------------------------------------
        if resp.double_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let (p, side) = self.point_at(grid_origin, pos);
                self.term.selection = Some(Selection::new(SelectionType::Semantic, p, side));
            }
        } else if resp.drag_started_by(egui::PointerButton::Primary) {
            let start = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos());
            if let Some(pos) = start {
                let (p, side) = self.point_at(grid_origin, pos);
                self.term.selection = Some(Selection::new(SelectionType::Simple, p, side));
            }
        } else if resp.clicked() {
            self.term.selection = None;
        }
        if let (true, Some(pos)) = (
            resp.dragged_by(egui::PointerButton::Primary),
            resp.interact_pointer_pos(),
        ) {
            let (p, side) = self.point_at(grid_origin, pos);
            if let Some(sel) = &mut self.term.selection {
                sel.update(p, side);
            }
        }

        // ---- mouse: wheel -------------------------------------------------------
        let mut typed = Vec::new();
        if resp.hovered() {
            let rows_f = f32::from(self.rows);
            let lines: f32 = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::MouseWheel { unit, delta, .. } => Some(match unit {
                            egui::MouseWheelUnit::Point => delta.y / self.cell.y,
                            egui::MouseWheelUnit::Line => delta.y * WHEEL_LINES,
                            egui::MouseWheelUnit::Page => delta.y * rows_f,
                        }),
                        _ => None,
                    })
                    .sum()
            });
            self.scroll_accum += lines;
            let whole = self.scroll_accum.trunc() as i32;
            if whole != 0 {
                self.scroll_accum -= whole as f32;
                let mode = *self.term.mode();
                if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL)
                {
                    // Full-screen apps (less, vim): wheel becomes arrow keys.
                    let key = if whole > 0 {
                        Key::ArrowUp
                    } else {
                        Key::ArrowDown
                    };
                    let seq =
                        encode_key(key, egui::Modifiers::NONE, self.key_mode()).unwrap_or_default();
                    for _ in 0..whole.unsigned_abs() {
                        typed.extend_from_slice(&seq);
                    }
                } else {
                    self.term.scroll_display(Scroll::Delta(whole));
                }
            }
        }

        // ---- keyboard ------------------------------------------------------------
        if has_focus {
            let (events, mods_now) = ui.input(|i| (i.events.clone(), i.modifiers));
            let mode = self.key_mode();
            let bracketed = self.term.mode().contains(TermMode::BRACKETED_PASTE);
            let mut keyboard = Vec::new();
            for ev in events {
                match ev {
                    Event::Text(t) => {
                        // Ctrl combos are handled from the Key event (AltGr = Ctrl+Alt still types).
                        if !(mods_now.ctrl && !mods_now.alt) && !mods_now.mac_cmd {
                            keyboard.extend(encode_text(&t, mods_now.alt && !is_mac));
                        }
                    }
                    Event::Key {
                        key,
                        pressed: true,
                        modifiers: m,
                        ..
                    } => {
                        let copy = (m.ctrl && m.shift && key == Key::C)
                            || (is_mac && m.mac_cmd && key == Key::C);
                        let paste = (m.ctrl && m.shift && key == Key::V)
                            || (is_mac && m.mac_cmd && key == Key::V);
                        if copy {
                            self.copy_selection(&ctx);
                        } else if paste {
                            // eframe answers with Event::Paste on the next frame.
                            ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
                        } else if is_mac && m.mac_cmd {
                            // Cmd+… belongs to the app, not the remote.
                        } else if let Some(bytes) = encode_key(key, m, mode) {
                            keyboard.extend(bytes);
                        }
                    }
                    // eframe turns Ctrl+C / Ctrl+X into Copy / Cut on Linux & Windows
                    // (Cmd+C / Cmd+X on macOS) before we see a Key event.
                    Event::Copy => {
                        if is_mac {
                            self.copy_selection(&ctx);
                        } else {
                            keyboard.push(0x03);
                        }
                    }
                    Event::Cut => {
                        if !is_mac {
                            keyboard.push(0x18);
                        }
                    }
                    Event::Paste(text) => keyboard.extend(encode_paste(&text, bracketed)),
                    _ => {}
                }
            }
            if !keyboard.is_empty() {
                self.term.scroll_display(Scroll::Bottom);
                typed.extend(keyboard);
            }
        }

        // ---- terminal events ------------------------------------------------------
        self.drain_term_events();
        out.input = std::mem::take(&mut self.pending_input);
        out.input.extend(typed);
        out.bell = std::mem::take(&mut self.bell);

        // ---- paint -------------------------------------------------------------------
        if ui.is_rect_visible(rect) {
            self.paint(ui, rect, grid_origin, style, has_focus);
        }
        out
    }

    fn paint(&self, ui: &egui::Ui, rect: Rect, origin: Pos2, style: &TermStyle, focused: bool) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, style.bg);

        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let cursor_vis = content.cursor.shape != CursorShape::Hidden;
        let cursor_row = content.cursor.point.line.0 + offset;
        let cursor_col = content.cursor.point.column.0;
        let block_cursor = cursor_vis && focused && content.cursor.shape == CursorShape::Block;
        let selection = content.selection;

        let mut runs: Vec<Run> = Vec::with_capacity(usize::from(self.rows) * 2);
        for cell in content.display_iter {
            let flags = cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let row = (cell.point.line.0 + offset) as usize;
            let col = cell.point.column.0;

            // Bold text with one of the 8 base colours is drawn bright (xterm behaviour).
            let fg_color = match cell.fg {
                Color::Named(n) if flags.contains(Flags::BOLD) && (n as usize) < 8 => {
                    Color::Named(n.to_bright())
                }
                c => c,
            };
            let mut fg = resolve(fg_color, colors, style);
            let mut bg = resolve(cell.bg, colors, style);
            if flags.contains(Flags::DIM) {
                fg = lerp(fg, bg, 0.4);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if selection.is_some_and(|s| s.contains(cell.point)) {
                bg = style.selection_bg;
            }
            let at_cursor = block_cursor && row as i32 == cursor_row && col == cursor_col;
            if at_cursor {
                fg = style.bg;
                bg = style.cursor;
            }
            let c = if flags.contains(Flags::HIDDEN) {
                ' '
            } else {
                cell.c
            };
            let wide = flags.contains(Flags::WIDE_CHAR);
            let style_flags =
                flags & (Flags::BOLD | Flags::ITALIC | Flags::ALL_UNDERLINES | Flags::STRIKEOUT);

            // Extend the previous run if possible: same row/style, contiguous,
            // ASCII only (so egui's glyph advance == our cell width), not the cursor.
            let ascii = c.is_ascii() && !wide && !at_cursor;
            if let Some(last) = runs.last_mut() {
                if ascii
                    && last.row == row
                    && last.col + last.cells == col
                    && last.fg == fg
                    && last.bg == bg
                    && last.flags == style_flags
                    && last.text.is_ascii()
                    && !last.cursor_like()
                {
                    last.text.push(c);
                    last.cells += 1;
                    continue;
                }
            }
            let mut text = String::new();
            text.push(c);
            runs.push(Run {
                row,
                col,
                cells: if wide { 2 } else { 1 },
                text,
                fg,
                bg,
                flags: style_flags,
            });
            if at_cursor {
                // Mark the run so nothing is appended to it.
                runs.last_mut().unwrap().flags.insert(Flags::HIDDEN);
            }
        }

        let cw = self.cell.x;
        let ch = self.cell.y;
        let cell_rect = |row: usize, col: usize, cells: usize| {
            Rect::from_min_size(
                origin + Vec2::new(col as f32 * cw, row as f32 * ch),
                Vec2::new(cells as f32 * cw, ch),
            )
        };

        // Backgrounds first, then text, so wide glyphs are not overdrawn.
        for run in &runs {
            if run.bg != style.bg {
                painter.rect_filled(
                    cell_rect(run.row, run.col, run.cells),
                    CornerRadius::ZERO,
                    run.bg,
                );
            }
        }
        for run in &runs {
            if run.text.trim().is_empty()
                && !run
                    .flags
                    .intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
            {
                continue;
            }
            let r = cell_rect(run.row, run.col, run.cells);
            let mut fmt = TextFormat {
                font_id: style.font.clone(),
                color: run.fg,
                italics: run.flags.contains(Flags::ITALIC),
                ..Default::default()
            };
            if run.flags.intersects(Flags::ALL_UNDERLINES) {
                fmt.underline = Stroke::new(1.0, run.fg);
            }
            if run.flags.contains(Flags::STRIKEOUT) {
                fmt.strikethrough = Stroke::new(1.0, run.fg);
            }
            let job = if style.ligatures {
                LayoutJob::single_section(run.text.clone(), fmt)
            } else {
                no_ligature_job(&run.text, fmt)
            };
            let galley = painter.layout_job(job);
            // Centre glyphs whose advance differs from the cell width (wide / fallback fonts).
            let dx = if run.text.is_ascii() {
                0.0
            } else {
                ((r.width() - galley.size().x) / 2.0).max(0.0)
            };
            let pos = r.min + Vec2::new(dx, 0.0);
            if run.flags.contains(Flags::BOLD) {
                // No bold face is loaded: fake it with a second pass shifted by ~1 px.
                painter.galley(pos + Vec2::new(0.6, 0.0), galley.clone(), run.fg);
            }
            painter.galley(pos, galley, run.fg);
        }

        // Non-block cursors (unfocused = hollow block, or beam/underline shapes).
        if cursor_vis
            && !block_cursor
            && cursor_row >= 0
            && (cursor_row as usize) < usize::from(self.rows)
        {
            let r = cell_rect(cursor_row as usize, cursor_col, 1);
            match (focused, content.cursor.shape) {
                (true, CursorShape::Beam) => {
                    painter.rect_filled(
                        Rect::from_min_size(r.min, Vec2::new(2.0, ch)),
                        CornerRadius::ZERO,
                        style.cursor,
                    );
                }
                (true, CursorShape::Underline) => {
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(r.min.x, r.max.y - 2.0), r.max),
                        CornerRadius::ZERO,
                        style.cursor,
                    );
                }
                _ => {
                    painter.rect_stroke(
                        r.shrink(0.5),
                        CornerRadius::ZERO,
                        Stroke::new(1.0, style.cursor),
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }
}

/// One shaping section per ASCII punctuation char (sections are shaped independently,
/// so this prevents ligatures), letters/digits/spaces stay grouped for speed.
fn no_ligature_job(text: &str, fmt: TextFormat) -> LayoutJob {
    let mut job = LayoutJob {
        text: text.to_owned(),
        ..Default::default()
    };
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_punctuation() {
            if start < i {
                job.sections.push(section(start..i, &fmt));
            }
            job.sections.push(section(i..i + 1, &fmt));
            start = i + 1;
        }
    }
    if start < text.len() {
        job.sections.push(section(start..text.len(), &fmt));
    }
    job
}

fn section(byte_range: std::ops::Range<usize>, fmt: &TextFormat) -> egui::text::LayoutSection {
    use egui::epaint::text::ByteIndex;
    egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: ByteIndex(byte_range.start)..ByteIndex(byte_range.end),
        format: fmt.clone(),
    }
}

impl Run {
    /// Runs holding the block cursor are flagged HIDDEN (never extended).
    fn cursor_like(&self) -> bool {
        self.flags.contains(Flags::HIDDEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_writes_grid() {
        let mut t = TerminalView::new();
        t.feed(b"hello\r\n\x1b[31mred\x1b[0m world\r\n");
        assert_eq!(t.row_text(0), "hello");
        assert_eq!(t.row_text(1), "red world");
        assert_eq!(t.row_text(2), "");
    }

    #[test]
    fn wraps_long_lines_and_utf8() {
        let mut t = TerminalView::new(); // 80 cols
        let long: String = (0..100)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        t.feed(long.as_bytes());
        t.feed("\r\nüñí 中文\r\n".as_bytes());
        assert_eq!(t.row_text(0).len(), 80);
        assert_eq!(t.row_text(1), &long[80..]);
        assert_eq!(t.row_text(2), "üñí 中文");
    }

    #[test]
    fn replies_title_bell_and_selection() {
        let mut t = TerminalView::new();
        t.feed(b"\x1b]0;my-pod: /app\x07abc\x07\x1b[6n");
        assert_eq!(t.title(), Some("my-pod: /app"));
        assert!(t.bell);
        assert_eq!(t.pending_input, b"\x1b[1;4R");
        t.select_all();
        assert_eq!(t.selection_text().as_deref().map(str::trim), Some("abc"));
    }

    #[test]
    fn scrollback() {
        let mut t = TerminalView::new(); // 24 rows
        for i in 0..100 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(t.row_text(0), "line 77");
        t.scroll_lines(10);
        assert_eq!(t.display_offset(), 10);
        assert_eq!(t.row_text(0), "line 67");
        t.scroll_lines(-1000);
        assert_eq!(t.display_offset(), 0);
    }

    #[test]
    fn colour_resolution() {
        let s = TermStyle::default();
        let colors = Colors::default();
        assert_eq!(
            resolve(Color::Named(NamedColor::Red), &colors, &s),
            s.ansi[1]
        );
        assert_eq!(resolve(Color::Indexed(9), &colors, &s), s.ansi[9]);
        assert_eq!(
            resolve(Color::Indexed(196), &colors, &s),
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            resolve(Color::Indexed(232), &colors, &s),
            Color32::from_rgb(8, 8, 8)
        );
        assert_eq!(
            resolve(Color::Spec(Rgb { r: 1, g: 2, b: 3 }), &colors, &s),
            Color32::from_rgb(1, 2, 3)
        );
        assert_eq!(
            resolve(Color::Named(NamedColor::Background), &colors, &s),
            s.bg
        );
    }
}
