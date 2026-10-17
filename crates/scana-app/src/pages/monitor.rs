//! The bus monitor: every frame Scana sees or sends, decoded where it can be. Read only.

use crate::app::ScanaApp;
use egui::{RichText, Ui};
use scana_core::client::Addressing;
use scana_core::describe::frame_meaning;

impl ScanaApp {
    pub fn page_monitor(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Bus monitor",
            "Every frame on the bus, newest at the bottom. Read only: nothing is sent from here.",
        );
        if !self.need_car(ui) {
            return;
        }
        if self.model.second_bus() {
            ui.label(
                RichText::new("Pins 3 and 11: Scana is only listening. Its own requests show as out, and only after you asked for modules.")
                    .color(pal.dim),
            );
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.model.frames_paused, "Pause");
            ui.checkbox(&mut self.monitor_obd_only, "OBD-II frames only");
            if ui.button("Clear").clicked() {
                self.model.frames.clear();
            }
            ui.label(RichText::new(format!("{} frames kept", self.model.frames.len())).color(pal.dim));
        });
        ui.add_space(4.0);
        let rows: Vec<_> = self
            .model
            .frames
            .iter()
            .filter(|r| {
                !self.monitor_obd_only
                    || Addressing::is_request_id(r.frame.id)
                    || Addressing::Standard.is_reply_id(r.frame.id)
                    || Addressing::Extended.is_reply_id(r.frame.id)
            })
            .cloned()
            .collect();
        let row_h = 20.0;
        ui.horizontal(|ui| {
            for (w, h) in [(80.0, "Time s"), (34.0, "Dir"), (86.0, "ID"), (210.0, "Data"), (200.0, "Meaning")] {
                ui.add_sized([w, row_h], egui::Label::new(RichText::new(h).strong()));
            }
        });
        let height = (ui.available_height() - 10.0).max(200.0);
        egui::ScrollArea::vertical()
            .id_salt("frames")
            .max_height(height)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                for r in &rows[range] {
                    ui.horizontal(|ui| {
                        let data: Vec<String> = r.frame.data().iter().map(|b| format!("{b:02X}")).collect();
                        let dir = if r.outgoing {
                            RichText::new("out").color(pal.accent)
                        } else {
                            RichText::new("in").color(pal.dim)
                        };
                        ui.add_sized(
                            [80.0, row_h],
                            egui::Label::new(RichText::new(format!("{:.3}", r.at_ms as f64 / 1000.0)).monospace()),
                        );
                        ui.add_sized([34.0, row_h], egui::Label::new(dir));
                        ui.add_sized(
                            [86.0, row_h],
                            egui::Label::new(RichText::new(r.frame.id.to_string()).monospace()),
                        );
                        ui.add_sized([210.0, row_h], egui::Label::new(RichText::new(data.join(" ")).monospace()));
                        ui.label(RichText::new(frame_meaning(&r.frame)).color(pal.dim));
                    });
                }
            });
    }
}
