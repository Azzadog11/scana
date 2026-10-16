//! Looks: workshop dark (default: dark grey with an orange accent), high contrast (for a cab in
//! daylight: many laptop screens are only 250 nits) and light. Text sizes are a little larger
//! than egui's defaults, for reading at arm's length.

use crate::settings::ThemeChoice;
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

/// Colours the pages use, per theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub tile: Color32,
    pub tile_border: Color32,
    pub tile_text: Color32,
    pub dim: Color32,
    pub good: Color32,
    pub note: Color32,
    pub bad: Color32,
    pub accent: Color32,
    /// Text on an accent-coloured button.
    pub on_accent: Color32,
    /// Linked words: hover for the meaning, click to read more.
    pub link: Color32,
    /// The page behind the content, and the side panels.
    pub page_bg: Color32,
    pub panel_bg: Color32,
    /// The selected menu item's background.
    pub sel_bg: Color32,
    pub series: [Color32; 6],
}

pub fn palette(t: ThemeChoice) -> Palette {
    match t {
        ThemeChoice::Dark => Palette {
            tile: Color32::from_rgb(0x26, 0x28, 0x2D),
            tile_border: Color32::from_rgb(0x34, 0x37, 0x3D),
            tile_text: Color32::from_rgb(0xEC, 0xEC, 0xEC),
            dim: Color32::from_rgb(0x9D, 0xA1, 0xA8),
            good: Color32::from_rgb(0x4C, 0xC3, 0x7A),
            note: Color32::from_rgb(0xF2, 0xC9, 0x4C),
            bad: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            accent: Color32::from_rgb(0xF2, 0x8C, 0x28),
            on_accent: Color32::from_rgb(0x17, 0x18, 0x1B),
            link: Color32::from_rgb(0xFF, 0xA6, 0x5A),
            page_bg: Color32::from_rgb(0x18, 0x19, 0x1C),
            panel_bg: Color32::from_rgb(0x1F, 0x20, 0x24),
            sel_bg: Color32::from_rgb(0x2D, 0x2A, 0x26),
            series: [
                Color32::from_rgb(0xF2, 0x8C, 0x28),
                Color32::from_rgb(0x5D, 0xAD, 0xE2),
                Color32::from_rgb(0x58, 0xD6, 0x8D),
                Color32::from_rgb(0xE5, 0x98, 0xD8),
                Color32::from_rgb(0xF2, 0xC9, 0x4C),
                Color32::from_rgb(0xC8, 0xD0, 0x5A),
            ],
        },
        ThemeChoice::HighContrast => Palette {
            tile: Color32::BLACK,
            tile_border: Color32::WHITE,
            tile_text: Color32::WHITE,
            dim: Color32::from_rgb(0xDD, 0xDD, 0xDD),
            good: Color32::from_rgb(0x00, 0xFF, 0x66),
            note: Color32::from_rgb(0xFF, 0xD6, 0x00),
            bad: Color32::from_rgb(0xFF, 0x40, 0x40),
            accent: Color32::from_rgb(0xFF, 0x9A, 0x1F),
            on_accent: Color32::BLACK,
            link: Color32::from_rgb(0xFF, 0xD6, 0x00),
            page_bg: Color32::BLACK,
            panel_bg: Color32::BLACK,
            sel_bg: Color32::from_rgb(0x33, 0x33, 0x33),
            series: [
                Color32::from_rgb(0x00, 0xD0, 0xFF),
                Color32::from_rgb(0xFF, 0xD6, 0x00),
                Color32::from_rgb(0x00, 0xFF, 0x66),
                Color32::from_rgb(0xFF, 0x66, 0xFF),
                Color32::from_rgb(0xFF, 0x80, 0x40),
                Color32::WHITE,
            ],
        },
        ThemeChoice::Light => Palette {
            tile: Color32::from_rgb(0xF3, 0xF1, 0xEE),
            tile_border: Color32::from_rgb(0xD3, 0xCE, 0xC7),
            tile_text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
            dim: Color32::from_rgb(0x5C, 0x5A, 0x57),
            good: Color32::from_rgb(0x1B, 0x7E, 0x3C),
            note: Color32::from_rgb(0x8A, 0x62, 0x00),
            bad: Color32::from_rgb(0xC0, 0x1C, 0x28),
            accent: Color32::from_rgb(0xD9, 0x6C, 0x0A),
            on_accent: Color32::WHITE,
            link: Color32::from_rgb(0xB0, 0x52, 0x00),
            page_bg: Color32::from_rgb(0xFB, 0xFA, 0xF8),
            panel_bg: Color32::from_rgb(0xF1, 0xEF, 0xEB),
            sel_bg: Color32::from_rgb(0xF7, 0xE3, 0xCF),
            series: [
                Color32::from_rgb(0xD9, 0x6C, 0x0A),
                Color32::from_rgb(0x1F, 0x77, 0xB4),
                Color32::from_rgb(0x2C, 0xA0, 0x2C),
                Color32::from_rgb(0x94, 0x67, 0xBD),
                Color32::from_rgb(0xD6, 0x27, 0x28),
                Color32::from_rgb(0x55, 0x55, 0x55),
            ],
        },
    }
}

/// Applies the theme and text sizes to every style.
pub fn apply(ctx: &egui::Context, t: ThemeChoice) {
    let pal = palette(t);
    let visuals = match t {
        ThemeChoice::Dark => {
            let mut v = Visuals::dark();
            v.panel_fill = pal.panel_bg;
            v.window_fill = Color32::from_rgb(0x23, 0x24, 0x28);
            v.extreme_bg_color = Color32::from_rgb(0x15, 0x16, 0x19);
            v.faint_bg_color = pal.tile;
            v.hyperlink_color = pal.link;
            v.selection.bg_fill = Color32::from_rgb(0x5A, 0x3A, 0x16);
            v.selection.stroke = Stroke::new(1.0, pal.accent);
            v.window_stroke = Stroke::new(1.0, pal.tile_border);
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.tile_border);
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xDF, 0xDF, 0xDF));
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xE6, 0xE6, 0xE6));
            v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x2A, 0x2C, 0x31);
            v.widgets.inactive.bg_fill = Color32::from_rgb(0x2A, 0x2C, 0x31);
            v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x36, 0x38, 0x3E);
            v.widgets.hovered.bg_fill = Color32::from_rgb(0x36, 0x38, 0x3E);
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.accent);
            v.widgets.active.weak_bg_fill = Color32::from_rgb(0x42, 0x44, 0x4A);
            v.widgets.active.bg_fill = Color32::from_rgb(0x42, 0x44, 0x4A);
            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                w.corner_radius = CornerRadius::same(6);
            }
            v
        }
        ThemeChoice::HighContrast => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::BLACK;
            v.window_fill = Color32::BLACK;
            v.extreme_bg_color = Color32::BLACK;
            v.override_text_color = Some(Color32::WHITE);
            v.hyperlink_color = pal.link;
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.5, Color32::WHITE);
            v.widgets.inactive.bg_stroke = Stroke::new(1.5, Color32::WHITE);
            v.widgets.hovered.bg_stroke = Stroke::new(2.0, Color32::from_rgb(0xFF, 0xD6, 0x00));
            v.selection.bg_fill = Color32::from_rgb(0x8A, 0x4A, 0x00);
            v
        }
        ThemeChoice::Light => {
            let mut v = Visuals::light();
            v.panel_fill = pal.panel_bg;
            v.hyperlink_color = pal.link;
            v.selection.bg_fill = pal.sel_bg;
            v.selection.stroke = Stroke::new(1.0, pal.accent);
            v
        }
    };
    ctx.set_theme(if t == ThemeChoice::Light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}

/// A filled accent button: the page's main action.
pub fn accent_button(pal: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).strong().color(pal.on_accent)).fill(pal.accent)
}
