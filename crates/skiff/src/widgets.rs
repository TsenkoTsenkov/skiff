//! Small reusable UI pieces.

use egui::{Align, Color32, CornerRadius, FontId, Layout, Response, RichText, Sense, Stroke, Ui, Vec2};
use egui_material_icons::MaterialIcon;

use crate::theme::{self, Palette};

pub fn icon(ic: MaterialIcon, size: f32) -> RichText {
    ic.rich_text().size(size)
}

/// Borderless icon button that lights up on hover.
pub fn icon_button(ui: &mut Ui, ic: MaterialIcon, tooltip: &str) -> Response {
    let resp = ui.add(
        egui::Button::new(icon(ic, 16.0))
            .frame_when_inactive(false)
            .min_size(Vec2::new(26.0, 24.0)),
    );
    named(resp, tooltip)
}

/// Gives an icon-only control an accessible name and a matching tooltip.
pub fn named(resp: Response, name: &str) -> Response {
    let label = name.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    resp.on_hover_text(name)
}

pub fn icon_toggle(ui: &mut Ui, on: bool, ic: MaterialIcon, tooltip: &str, pal: &Palette) -> Response {
    let text = icon(ic, 16.0).color(if on { pal.accent } else { pal.muted });
    let resp = ui.add(
        egui::Button::new(text)
            .frame_when_inactive(on)
            .fill(if on { pal.selected } else { Color32::TRANSPARENT })
            .min_size(Vec2::new(26.0, 24.0)),
    );
    named(resp, tooltip)
}

/// Button with a leading icon.
pub fn action_button(ui: &mut Ui, ic: MaterialIcon, label: &str) -> Response {
    ui.add(egui::Button::new((icon(ic, 15.0), label)))
}

pub fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, theme::semibold()))
}

/// Rounded badge.
pub fn pill(ui: &mut Ui, text: &str, color: Color32, pal: &Palette) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(11.5), color);
    let size = galley.size() + Vec2::new(14.0, 5.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(9), pal.tint(color, 0.16));
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    let label = text.to_string();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::Role::Label, true, &label));
    resp
}

/// A `key=value` chip, used for labels.
pub fn chip(ui: &mut Ui, text: &str, pal: &Palette) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::mono(11.5), pal.muted);
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if resp.hovered() { pal.hover } else { pal.raised };
        ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
        ui.painter().rect_stroke(rect, CornerRadius::same(5), Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, pal.muted);
    }
    resp
}

pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// Uppercase section caption.
pub fn caption(ui: &mut Ui, text: &str, pal: &Palette) {
    ui.label(
        RichText::new(text.to_uppercase())
            .font(FontId::new(10.5, theme::semibold()))
            .color(pal.faint)
            .extra_letter_spacing(0.6),
    );
}

/// Section heading inside the detail panel.
pub fn section(ui: &mut Ui, title: &str, pal: &Palette) {
    ui.add_space(10.0);
    caption(ui, title, pal);
    ui.add_space(2.0);
}

/// One "Key  value" line with a fixed-width key column.
pub fn kv(ui: &mut Ui, key: &str, pal: &Palette, add_value: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(118.0, 18.0), Layout::left_to_right(Align::Min), |ui| {
            ui.set_min_width(118.0);
            ui.label(RichText::new(key).color(pal.muted));
        });
        ui.vertical(|ui| add_value(ui));
    });
}

pub fn kv_text(ui: &mut Ui, key: &str, value: &str, pal: &Palette) {
    if value.is_empty() {
        return;
    }
    kv(ui, key, pal, |ui| {
        ui.add(egui::Label::new(value).wrap().selectable(true));
    });
}

/// Search field with a leading magnifier and a clear button.
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32, id: egui::Id) -> Response {
    let resp = ui.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .hint_text(format!("{}  {}", egui_material_icons::icons::ICON_SEARCH.codepoint, hint))
            .desired_width(width)
            .margin(egui::Margin::symmetric(8, 5)),
    );
    resp
}

/// Centred message for empty or failed views.
pub fn empty_state(ui: &mut Ui, ic: MaterialIcon, title: &str, detail: &str, pal: &Palette) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(24.0));
        ui.label(icon(ic, 36.0).color(pal.faint));
        ui.add_space(6.0);
        ui.label(semibold(title, 15.0).color(pal.text));
        if !detail.is_empty() {
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(detail).color(pal.muted)).wrap());
        }
    });
}

/// Thin horizontal usage bar (0..=1).
pub fn usage_bar(ui: &mut Ui, fraction: f32, width: f32, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 6.0), Sense::hover());
    let f = fraction.clamp(0.0, 1.0);
    let color = if f > 0.9 {
        pal.err
    } else if f > 0.75 {
        pal.warn
    } else {
        pal.accent
    };
    ui.painter().rect_filled(rect, CornerRadius::same(3), pal.tint(pal.muted, 0.18));
    let mut fill = rect;
    fill.set_width(rect.width() * f);
    ui.painter().rect_filled(fill, CornerRadius::same(3), color);
}

/// "1 mount", "3 mounts".
pub fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{} {noun}s", thousands(n))
    }
}

/// Decoded size of a base64 string without decoding it.
pub fn base64_len(encoded: &str) -> usize {
    let t = encoded.trim_end();
    let padding = t.bytes().rev().take_while(|b| *b == b'=').count();
    (t.len() / 4 * 3).saturating_sub(padding)
}

/// Formats 12345 as "12,345".
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64_len, plural, thousands};

    #[test]
    fn plurals_and_sizes() {
        assert_eq!(plural(1, "key"), "1 key");
        assert_eq!(plural(0, "key"), "0 keys");
        assert_eq!(plural(2500, "line"), "2,500 lines");
        assert_eq!(base64_len("aGVsbG8="), 5);
        assert_eq!(base64_len("aGVsbG8h"), 6);
        assert_eq!(base64_len("YQ=="), 1);
        assert_eq!(base64_len(""), 0);
    }

    #[test]
    fn groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
