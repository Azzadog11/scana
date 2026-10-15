//! Settings: look, size, polling, recording.

use crate::app::ScanaApp;
use crate::settings::{Settings, ThemeChoice, config_path};
use egui::{RichText, Ui};

impl ScanaApp {
    pub fn page_settings(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Settings", "Saved as you change them.");
        let before = self.settings.clone();

        ui.label(RichText::new("Look").size(17.0).strong());
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.settings.theme, ThemeChoice::Dark, "Dark");
            ui.radio_value(&mut self.settings.theme, ThemeChoice::HighContrast, "High contrast (daylight in the cab)");
            ui.radio_value(&mut self.settings.theme, ThemeChoice::Light, "Light");
        });
        ui.horizontal(|ui| {
            ui.label("Text and controls size");
            ui.add(egui::Slider::new(&mut self.settings.zoom, 0.7..=2.5).step_by(0.05));
            if ui.button("Normal").clicked() {
                self.settings.zoom = 1.0;
            }
        });
        ui.label(
            RichText::new("Ctrl + and Ctrl - change the size too. Pages rearrange themselves on small windows.")
                .small()
                .color(pal.dim),
        );
        ui.checkbox(&mut self.settings.show_learn, "Show the Learn panel (F1)");

        ui.add_space(10.0);
        ui.label(RichText::new("Polling").size(17.0).strong());
        ui.horizontal(|ui| {
            ui.label("Gap between requests, alone on the bus (ms)");
            ui.add(egui::Slider::new(&mut self.settings.gap_alone_ms, 20..=500));
        });
        ui.horizontal(|ui| {
            ui.label("Gap when another tester polls, such as a dash display (ms)");
            ui.add(egui::Slider::new(&mut self.settings.gap_shared_ms, 20..=1000));
        });
        ui.label(RichText::new("50 ms is 20 requests a second, plenty for OBD-II.").small().color(pal.dim));

        ui.add_space(10.0);
        ui.label(RichText::new("Recording").size(17.0).strong());
        ui.checkbox(&mut self.settings.auto_record, "Record every connection to a session folder");
        ui.label(RichText::new(format!("Sessions go to {}", self.sessions_dir.display())).small().color(pal.dim));

        ui.add_space(10.0);
        ui.label(RichText::new("Advanced").size(17.0).strong());
        ui.checkbox(&mut self.settings.advanced, "Advanced mode: show raw bytes on the live tiles");

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Back to the defaults").clicked() {
                self.settings = Settings { first_run: false, ..Settings::default() };
            }
            ui.label(RichText::new(format!("Settings file: {}", config_path().display())).small().color(pal.dim));
        });

        if self.settings != before {
            if self.settings.gap_alone_ms != before.gap_alone_ms || self.settings.gap_shared_ms != before.gap_shared_ms
            {
                self.push_gaps();
            }
            if self.settings.auto_record != before.auto_record
                && self.settings.auto_record
                && self.model.connected()
                && self.model.recording.is_none()
            {
                self.set_record(true);
            }
            self.save_settings();
        }
    }
}
