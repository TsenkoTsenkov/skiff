//! Colours, fonts and egui style for Skiff.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{Color32, CornerRadius, FontData, FontFamily, FontId, Margin, Stroke, TextStyle};
use skiff_core::Tone;

static INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.otf");
static INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.otf");
static JETBRAINS_MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

pub const ROW_HEIGHT: f32 = 26.0;
pub const TOP_BAR_HEIGHT: f32 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Main content background.
    pub bg: Color32,
    /// Sidebar, bars and docks.
    pub panel: Color32,
    /// Cards, inputs, popups.
    pub raised: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub border: Color32,
    pub text: Color32,
    /// Secondary text.
    pub muted: Color32,
    /// The quietest text (hints, units, counts) and small icons. Like
    /// `text` and `muted` it keeps a contrast of 4.5:1 on the surfaces
    /// text sits on (see the tests); for hairlines use [`Palette::outline`].
    pub faint: Color32,
    pub accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub info: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub const fn dark() -> Self {
        Palette {
            dark: true,
            bg: rgb(0x0E1014),
            panel: rgb(0x13161B),
            raised: rgb(0x1A1E25),
            hover: rgb(0x1F242D),
            selected: rgb(0x1B2B47),
            border: rgb(0x242933),
            text: rgb(0xD5DAE3),
            muted: rgb(0x9BA3B2),
            faint: rgb(0x848D9D),
            accent: rgb(0x5B9DFF),
            ok: rgb(0x3FB97A),
            warn: rgb(0xE3A545),
            err: rgb(0xF0616D),
            info: rgb(0x58B4E8),
        }
    }

    pub const fn light() -> Self {
        Palette {
            dark: false,
            bg: rgb(0xFFFFFF),
            panel: rgb(0xF5F6F8),
            raised: rgb(0xFFFFFF),
            hover: rgb(0xECEEF2),
            selected: rgb(0xDCE8FF),
            border: rgb(0xDFE2E8),
            text: rgb(0x1C2330),
            muted: rgb(0x4F5968),
            faint: rgb(0x626C7D),
            accent: rgb(0x2367EA),
            ok: rgb(0x187F48),
            warn: rgb(0x98651A),
            err: rgb(0xD13041),
            info: rgb(0x1A75AC),
        }
    }

    pub fn tone(&self, tone: Tone) -> Color32 {
        match tone {
            Tone::Normal => self.text,
            Tone::Muted => self.muted,
            Tone::Ok => self.ok,
            Tone::Info => self.info,
            Tone::Warn => self.warn,
            Tone::Err => self.err,
        }
    }

    /// A line that only decorates (the edge of a hovered control): quieter
    /// than any text colour, which a hairline may be.
    pub fn outline(&self) -> Color32 {
        rgb(if self.dark { 0x5C6473 } else { 0x9AA3B2 })
    }

    /// A translucent tint of `c`, for badges and subtle fills.
    pub fn tint(&self, c: Color32, alpha: f32) -> Color32 {
        let a = alpha.clamp(0.0, 1.0);
        let mix = |fg: u8, bg: u8| (fg as f32 * a + bg as f32 * (1.0 - a)).round() as u8;
        Color32::from_rgb(mix(c.r(), self.bg.r()), mix(c.g(), self.bg.g()), mix(c.b(), self.bg.b()))
    }

    /// Stable colour for a log source / label index.
    pub fn series(&self, i: usize) -> Color32 {
        const DARK: [u32; 8] = [0x5B9DFF, 0x3FB97A, 0xE3A545, 0xC792EA, 0x58B4E8, 0xF78C6C, 0x89DDFF, 0xF07178];
        const LIGHT: [u32; 8] = [0x2367EA, 0x187F48, 0x98651A, 0x8250DF, 0x1A75AC, 0xC2410C, 0x0E7490, 0xBE123C];
        rgb(if self.dark { DARK[i % 8] } else { LIGHT[i % 8] })
    }
}

/// Width of the outline around the control that has the keyboard.
pub const FOCUS_WIDTH: f32 = 2.0;

/// The outline around the control that has the keyboard focus.
pub fn focus_stroke(p: &Palette) -> Stroke {
    Stroke::new(FOCUS_WIDTH, p.accent)
}

/// Relative luminance of a colour (WCAG 2.x), 0 for black to 1 for white.
pub fn luminance(c: Color32) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG contrast ratio of two opaque colours, from 1 (the same) to 21
/// (black on white). Body text needs 4.5.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn install_fonts(ctx: &egui::Context) {
    ctx.add_font(FontInsert::new(
        "Inter",
        FontData::from_static(INTER),
        vec![InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Highest }],
    ));
    ctx.add_font(FontInsert::new(
        "Inter-SemiBold",
        FontData::from_static(INTER_SEMIBOLD),
        vec![InsertFontFamily { family: semibold(), priority: FontPriority::Highest }],
    ));
    // Regular Inter as fallback so the semibold family is never empty of glyphs.
    ctx.add_font(FontInsert::new(
        "Inter-Fallback",
        FontData::from_static(INTER),
        vec![InsertFontFamily { family: semibold(), priority: FontPriority::Lowest }],
    ));
    ctx.add_font(FontInsert::new(
        "JetBrainsMono",
        FontData::from_static(JETBRAINS_MONO),
        vec![InsertFontFamily { family: FontFamily::Monospace, priority: FontPriority::Highest }],
    ));
    // Icons: also register as a fallback of the semibold family so labels
    // that mix text and icons render in every weight.
    let mut icons = egui_material_icons::font_insert();
    icons.families.push(InsertFontFamily { family: semibold(), priority: FontPriority::Lowest });
    icons.families.push(InsertFontFamily { family: FontFamily::Monospace, priority: FontPriority::Lowest });
    ctx.add_font(icons);
}

/// Applies the palette to the egui style.
pub fn apply(ctx: &egui::Context, p: &Palette) {
    ctx.set_theme(if p.dark { egui::Theme::Dark } else { egui::Theme::Light });
    // Style both of egui's theme slots: which one is active can be decided
    // elsewhere (system preference, windowing backend) after we run, and a
    // half-applied palette is unreadable.
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(17.0, semibold())),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 6.0);
        s.button_padding = egui::vec2(10.0, 5.0);
        s.interact_size = egui::vec2(30.0, 26.0);
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(12);
        s.indent = 14.0;
        s.scroll.bar_width = 8.0;
        s.scroll.floating = true;

        style.interaction.tooltip_delay = 0.35;
        style.interaction.selectable_labels = false;

        let v = &mut style.visuals;
        *v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = p.panel;
        v.window_fill = p.raised;
        v.window_stroke = Stroke::new(1.0, p.border);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.extreme_bg_color = p.bg;
        v.faint_bg_color = p.tint(p.muted, 0.045);
        v.code_bg_color = p.raised;
        v.text_edit_bg_color = Some(p.raised);
        v.hyperlink_color = p.accent;
        v.warn_fg_color = p.warn;
        v.error_fg_color = p.err;
        v.selection.bg_fill = p.selected;
        // Also the frame of the text field that has the keyboard.
        v.selection.stroke = focus_stroke(p);
        v.override_text_color = None;
        v.weak_text_color = Some(p.muted);
        v.indent_has_left_vline = false;
        v.button_frame = true;
        v.striped = false;

        let radius = CornerRadius::same(6);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = p.panel;
        w.noninteractive.weak_bg_fill = p.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        w.noninteractive.corner_radius = radius;

        w.inactive.bg_fill = p.raised;
        w.inactive.weak_bg_fill = p.raised;
        w.inactive.bg_stroke = Stroke::new(1.0, p.border);
        w.inactive.fg_stroke = Stroke::new(1.0, p.text);
        w.inactive.corner_radius = radius;
        w.inactive.expansion = 0.0;

        w.hovered.bg_fill = p.hover;
        w.hovered.weak_bg_fill = p.hover;
        w.hovered.bg_stroke = Stroke::new(1.0, p.outline());
        w.hovered.fg_stroke = Stroke::new(1.0, p.text);
        w.hovered.corner_radius = radius;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = p.selected;
        w.active.weak_bg_fill = p.selected;
        // "Active" is what egui draws a control with while it is pressed
        // and while it has the keyboard focus: the focus outline.
        w.active.bg_stroke = focus_stroke(p);
        w.active.fg_stroke = Stroke::new(1.0, p.text);
        w.active.corner_radius = radius;
        w.active.expansion = 0.0;

        w.open.bg_fill = p.hover;
        w.open.weak_bg_fill = p.hover;
        w.open.bg_stroke = Stroke::new(1.0, p.outline());
        w.open.fg_stroke = Stroke::new(1.0, p.text);
        w.open.corner_radius = radius;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_is_the_wcag_ratio() {
        let (black, white) = (Color32::BLACK, Color32::WHITE);
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 0.001);
        // A pair with a published ratio: #767676 on white is 4.54.
        assert!((contrast(rgb(0x767676), white) - 4.54).abs() < 0.01);
        assert_eq!(contrast(black, white), contrast(white, black));
    }

    /// Text must be readable: WCAG AA asks for 4.5:1.
    #[test]
    fn text_colours_have_enough_contrast_in_both_themes() {
        const AA: f32 = 4.5;
        for p in [Palette::dark(), Palette::light()] {
            let name = if p.dark { "dark" } else { "light" };
            let check = |what: &str, fg: Color32, on: &str, bg: Color32| {
                let ratio = contrast(fg, bg);
                assert!(ratio >= AA, "{name}: {what} on {on} is {ratio:.2}:1, below {AA}:1");
            };
            // The three greys of text, on every surface text is written
            // on, a hovered row included.
            for (what, fg) in [("text", p.text), ("muted", p.muted), ("faint", p.faint)] {
                for (on, bg) in [("bg", p.bg), ("panel", p.panel), ("raised", p.raised), ("hover", p.hover)] {
                    check(what, fg, on, bg);
                }
            }
            // A selected row carries names and secondary text.
            check("text", p.text, "selected", p.selected);
            check("muted", p.muted, "selected", p.selected);
            // Status words and links, on the plain surfaces.
            for (what, fg) in [("accent", p.accent), ("ok", p.ok), ("warn", p.warn), ("err", p.err), ("info", p.info)] {
                for (on, bg) in [("bg", p.bg), ("panel", p.panel), ("raised", p.raised)] {
                    check(what, fg, on, bg);
                }
            }
            // Log source names.
            for i in 0..8 {
                check(&format!("series {i}"), p.series(i), "bg", p.bg);
            }
            // The focus outline has to stand out from what is around it
            // (3:1 for a graphical object).
            for (on, bg) in [("bg", p.bg), ("panel", p.panel), ("raised", p.raised)] {
                let ratio = contrast(p.accent, bg);
                assert!(ratio >= 3.0, "{name}: focus outline on {on} is {ratio:.2}:1");
            }
        }
        const { assert!(FOCUS_WIDTH >= 2.0) };
    }

    #[test]
    fn tint_blends_toward_background() {
        let p = Palette::dark();
        assert_eq!(p.tint(p.accent, 0.0), p.bg);
        assert_eq!(p.tint(p.accent, 1.0), p.accent);
    }
}
