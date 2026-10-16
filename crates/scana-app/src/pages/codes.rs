//! Fault codes and freeze frames, and the clear-codes button (safety level 1).

use crate::app::{ScanaApp, Topic};
use egui::{RichText, Ui};
use scana_core::session::Command;
use scana_db::dtc as dtcdb;
use scana_proto::Dtc;

impl ScanaApp {
    pub fn page_codes(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Fault codes",
            "Stored codes are confirmed faults; pending codes were seen once; permanent codes can't be cleared by a tool. Click a code to learn about it.",
        );
        if !self.need_car(ui) {
            return;
        }
        ui.horizontal(|ui| {
            if ui.add(crate::theme::accent_button(&pal, "Read codes")).clicked() {
                self.session.send(Command::ReadCodes);
            }
            if ui.button("Read freeze frames").clicked() {
                self.session.send(Command::ReadFreeze);
            }
            ui.add_space(20.0);
            if ui
                .button(RichText::new("Clear codes...").color(pal.bad))
                .on_hover_text("Safety level 1: asks first, checks the engine is off, saves a report first")
                .clicked()
            {
                self.clear_dialog = Some(false);
            }
            if let Some(b) = &self.model.busy {
                ui.spinner();
                ui.label(b);
            }
        });
        ui.add_space(8.0);
        let Some(sets) = self.model.codes.clone() else {
            ui.label("Press Read codes to ask every engine-related computer for its codes.");
            return;
        };
        for set in &sets {
            ui.label(RichText::new(self.model.ecu_name(set.ecu)).size(17.0).strong());
            if set.stored.is_empty() && set.pending.is_empty() && set.permanent.is_empty() {
                ui.label(RichText::new("✔ No stored, pending or permanent codes.").color(pal.good));
            }
            for (label, list, color) in [
                ("Stored", &set.stored, pal.bad),
                ("Pending", &set.pending, pal.note),
                ("Permanent", &set.permanent, pal.bad),
            ] {
                for code in list {
                    self.code_row(ui, label, *code, color);
                }
            }
            if !set.permanent_read {
                ui.label(
                    RichText::new(
                        "This computer doesn't report permanent codes: normal for cars from before about 2010.",
                    )
                    .small()
                    .color(pal.dim),
                );
            }
            ui.add_space(8.0);
        }
        if let Some(frames) = self.model.freeze.clone() {
            if frames.is_empty() {
                ui.label(RichText::new("No freeze frame stored.").color(pal.dim));
            }
            for ff in frames {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Freeze frame: stored by {} ({})", ff.dtc, self.model.ecu_name(ff.ecu)))
                        .size(17.0)
                        .strong(),
                );
                ui.label(RichText::new("The values at the moment the code was stored.").color(pal.dim));
                egui::Grid::new(("ff", ff.ecu.raw())).striped(true).num_columns(2).spacing([24.0, 4.0]).show(
                    ui,
                    |ui| {
                        for r in &ff.readings {
                            if ui.link(r.name).clicked() {
                                self.open_topic(Topic::Pid(ff.ecu, r.pid));
                            }
                            ui.label(r.display());
                            ui.end_row();
                        }
                    },
                );
            }
        }
    }

    fn code_row(&mut self, ui: &mut Ui, label: &str, code: Dtc, color: egui::Color32) {
        let info = dtcdb::lookup(code);
        let selected = self.topic == Some(Topic::Dtc(code));
        let mut diagnose = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{label:<9}")).color(color).monospace());
            let text = RichText::new(format!("{code}  {}", info.summary())).size(16.0);
            if ui.selectable_label(selected, text).clicked() {
                self.open_topic(Topic::Dtc(code));
            }
            let kind = if info.manufacturer_specific { "maker-specific" } else { "generic" };
            ui.label(RichText::new(kind).small().color(self.pal().dim));
            if scana_db::diag::guide_for_make(code, self.model.profile().and_then(|p| p.make.as_deref())).is_some()
                && ui.small_button("Diagnose").on_hover_text("Work through the likely causes step by step").clicked()
            {
                diagnose = true;
            }
        });
        if diagnose {
            self.start_diagnosis(code);
        }
    }
}
