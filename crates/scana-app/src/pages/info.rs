//! Vehicle info, and the Modules placeholder (stage 3).

use crate::app::ScanaApp;
use egui::{RichText, Ui};
use scana_core::session::Command;

impl ScanaApp {
    pub fn page_info(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Vehicle info", "What the car says about itself, and how Scana is talking to it.");
        if !self.need_obd(ui) {
            return;
        }
        let Some(conn) = self.model.conn.clone() else { return };
        if ui.add(crate::theme::accent_button(&pal, "Read vehicle information")).clicked() {
            self.session.send(Command::ReadInfo);
        }
        ui.add_space(8.0);
        if let Some(list) = self.model.info.clone() {
            for (ecu, i) in list {
                ui.label(RichText::new(self.model.ecu_name(ecu)).size(17.0).strong());
                egui::Grid::new(("vi", ecu.raw())).num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    let nr = || "not reported".to_string();
                    ui.label("VIN");
                    ui.label(RichText::new(i.vin.clone().unwrap_or_else(nr)).monospace());
                    ui.end_row();
                    ui.label("Calibration ID");
                    ui.label(RichText::new(i.calibration_id.clone().unwrap_or_else(nr)).monospace());
                    ui.end_row();
                    ui.label("Computer name");
                    ui.label(i.ecu_name.clone().unwrap_or_else(nr));
                    ui.end_row();
                });
                ui.add_space(6.0);
            }
            ui.label(RichText::new("Some Japanese-market engine computers don't report a VIN.").small().color(pal.dim));
        }
        ui.add_space(10.0);
        ui.label(RichText::new("Connection").size(17.0).strong());
        egui::Grid::new("conn").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
            ui.label("Protocol");
            ui.label(conn.protocol());
            ui.end_row();
            ui.label("Link");
            ui.label(&conn.link);
            ui.end_row();
            for e in &conn.ecus {
                ui.label(format!("{} ({})", e.name, e.id));
                ui.label(format!("{} PIDs supported", e.supported.iter().filter(|p| !p.is_multiple_of(0x20)).count()));
                ui.end_row();
            }
            ui.label("Profile");
            ui.label(
                self.model
                    .profile()
                    .map_or("none: Scana uses generic OBD-II".to_string(), |p| format!("{} ({})", p.name, p.model)),
            );
            ui.end_row();
        });
        ui.add_space(6.0);
        egui::CollapsingHeader::new("How Scana found the car").show(ui, |ui| {
            for l in &conn.log {
                ui.label(RichText::new(l).color(pal.dim));
            }
        });
    }
}
