//! Looks: dark (default), high contrast (for a cab in daylight: many laptop screens are only 250 nits)
//! and light. Text sizes are a little larger than egui's defaults, for reading at arm's length.

use crate::settings::ThemeChoice;
use egui::{Color32, FontFamily, FontId, Stroke, TextStyle, Visuals};

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
    pub series: [Color32; 6],
}

pub fn palette(t: ThemeChoice) -> Palette {
    match t {
        ThemeChoice::Dark => Palette {
            tile: Color32::from_rgb(0x20, 0x27, 0x2F),
            tile_border: Color32::from_rgb(0x2C, 0x34, 0x3D),
            tile_text: Color32::from_rgb(0xF2, 0xF2, 0xF2),
            dim: Color32::from_rgb(0x9A, 0xA4, 0xAE),
            good: Color32::from_rgb(0x4C, 0xC3, 0x7A),
            note: Color32::from_rgb(0xF5, 0xB0, 0x41),
            bad: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            accent: Color32::from_rgb(0x2F, 0x6F, 0xB0),
            series: [
                Color32::from_rgb(0x5D, 0xAD, 0xE2),
                Color32::from_rgb(0xF5, 0xB0, 0x41),
                Color32::from_rgb(0x58, 0xD6, 0x8D),
                Color32::from_rgb(0xE5, 0x98, 0xD8),
                Color32::from_rgb(0xF0, 0x7B, 0x6B),
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
            accent: Color32::from_rgb(0x00, 0xA2, 0xFF),
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
            tile: Color32::from_rgb(0xEE, 0xF1, 0xF4),
            tile_border: Color32::from_rgb(0xC8, 0xCF, 0xD6),
            tile_text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
            dim: Color32::from_rgb(0x55, 0x5F, 0x69),
            good: Color32::from_rgb(0x1B, 0x7E, 0x3C),
            note: Color32::from_rgb(0x9A, 0x62, 0x00),
            bad: Color32::from_rgb(0xC0, 0x1C, 0x28),
            accent: Color32::from_rgb(0x1F, 0x5F, 0xA8),
            series: [
                Color32::from_rgb(0x1F, 0x77, 0xB4),
                Color32::from_rgb(0xD6, 0x7A, 0x00),
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
    let visuals = match t {
        ThemeChoice::Dark => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(0x1A, 0x1F, 0x26);
            v.window_fill = Color32::from_rgb(0x1D, 0x23, 0x2B);
            v.extreme_bg_color = Color32::from_rgb(0x12, 0x16, 0x1B);
            v
        }
        ThemeChoice::HighContrast => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::BLACK;
            v.window_fill = Color32::BLACK;
            v.extreme_bg_color = Color32::BLACK;
            v.override_text_color = Some(Color32::WHITE);
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.5, Color32::WHITE);
            v.widgets.inactive.bg_stroke = Stroke::new(1.5, Color32::WHITE);
            v.widgets.hovered.bg_stroke = Stroke::new(2.0, Color32::from_rgb(0xFF, 0xD6, 0x00));
            v.selection.bg_fill = Color32::from_rgb(0x00, 0x5F, 0xCC);
            v
        }
        ThemeChoice::Light => Visuals::light(),
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
