//! Past sessions: open their reports and folders, and load their values into the graphs.

use crate::app::{Page, ScanaApp};
use egui::{RichText, Ui};
use scana_core::store;

impl ScanaApp {
    pub fn page_sessions(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Sessions and reports",
            "Each recorded connection and each health check has a folder: frames, values and the report.",
        );
        let dir = self.sessions_dir.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new(dir.display().to_string()).monospace().color(pal.dim));
            if ui.button("Open the folder").clicked() {
                let _ = std::fs::create_dir_all(&dir);
                self.open_path(&dir);
            }
            if ui.button("Look again").clicked() {
                self.sessions = None;
            }
        });
        ui.add_space(6.0);
        if self.sessions.is_none() {
            self.sessions = Some(store::list_sessions(&dir));
        }
        let list = self.sessions.clone().unwrap_or_default();
        if list.is_empty() {
            ui.label("No sessions yet. Turn on Record in the top bar, or run a health check.");
            return;
        }
        for s in list {
            egui::Frame::group(ui.style()).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let car = s.summary.as_ref().map_or("?".to_string(), |x| x.vehicle.clone());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&s.name).monospace().strong());
                    ui.label(RichText::new(car).color(pal.dim));
                });
                let what = s.summary.as_ref().map_or("No summary: still running, or cut short.".to_string(), |x| {
                    let mut t = format!("{} frames, {} values", x.frames, x.values);
                    if x.markers > 0 {
                        t.push_str(&format!(", {} marks", x.markers));
                    }
                    if !x.codes.is_empty() {
                        t.push_str(&format!(". Codes: {}", x.codes.join(", ")));
                    }
                    t
                });
                ui.label(what);
                ui.horizontal(|ui| {
                    if s.has_report && ui.button("Open the report").clicked() {
                        self.open_path(&s.dir.join("report.html"));
                    }
                    if ui.button("Open the folder").clicked() {
                        self.open_path(&s.dir);
                    }
                    let has_values = s.summary.as_ref().is_none_or(|x| x.values > 0);
                    if has_values && ui.button("Show in Graphs").clicked() {
                        match store::load_values(&s.dir) {
                            Ok(l) => {
                                self.loaded_selected = l.series.keys().take(4).cloned().collect();
                                self.loaded = Some((s.name.clone(), l));
                                self.graph_offset_s = 0.0;
                                self.graph_window_s = f64::INFINITY;
                                self.go(Page::Graphs);
                            }
                            Err(e) => self.model.banner(format!("Couldn't load {}: {e}", s.name), true),
                        }
                    }
                });
            });
            ui.add_space(4.0);
        }
    }
}
