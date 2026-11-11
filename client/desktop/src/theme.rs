//! Farben und Stil aus dem DMXCraft-Designsystem (Theme „Dunkelblau“).

use egui::{Color32, CornerRadius, Stroke};

pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0e, 0x12);
pub const SURFACE: Color32 = Color32::from_rgb(0x12, 0x16, 0x1c);
pub const RAISED: Color32 = Color32::from_rgb(0x1a, 0x20, 0x28);
pub const RAISED_HOVER: Color32 = Color32::from_rgb(0x23, 0x2b, 0x35);
pub const BORDER: Color32 = Color32::from_rgb(0x28, 0x30, 0x3a);
pub const ACCENT: Color32 = Color32::from_rgb(0x19, 0xe3, 0xf0);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x0c, 0x5c, 0x64);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xec, 0xf2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x95, 0xa2, 0xb0);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x55, 0x60, 0x6c);
pub const DANGER: Color32 = Color32::from_rgb(0xef, 0x44, 0x55);
pub const OK: Color32 = Color32::from_rgb(0x34, 0xd3, 0x99);
pub const WARN: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    let r = CornerRadius::same(4);
    v.panel_fill = SURFACE;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(8);
    v.extreme_bg_color = BG;
    v.faint_bg_color = RAISED;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = r;
    for (ws, fill) in [(&mut w.inactive, RAISED), (&mut w.hovered, RAISED_HOVER), (&mut w.active, ACCENT_DIM), (&mut w.open, RAISED_HOVER)] {
        ws.bg_fill = fill;
        ws.weak_bg_fill = fill;
        ws.bg_stroke = Stroke::new(1.0, BORDER);
        ws.fg_stroke = Stroke::new(1.0, TEXT);
        ws.corner_radius = r;
        ws.expansion = 0.0;
    }
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 28.0;
    });
}

/// Überschrift-Stil: Großbuchstaben, kräftig, klein.
pub fn label(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase()).size(12.0).strong().color(TEXT_DIM)
}

/// Text für Footer-Buttons.
pub fn button_text(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase()).size(12.0).strong()
}
