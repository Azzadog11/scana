//! Modules (stage 3): finds every computer in the car that answers, and shows each one's
//! name, part numbers and fault codes. Read only.

use crate::app::{Page, ScanaApp, Topic};
use egui::{RichText, Ui};
use scana_core::Channel;
use scana_core::modules::{DataKind, ModuleInfo, NameFrom};
use scana_core::session::Command;
use scana_db::dtc as dtcdb;
use scana_proto::uds;

impl ScanaApp {
    pub fn page_modules(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Modules",
            "Every computer in the car that answers a scan tool: brakes, airbags, body, the instrument cluster. Scana only reads.",
        );
        if !self.need_car(ui) {
            return;
        }
        let running = self.model.discovering;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!running && self.model.busy.is_none(), crate::theme::accent_button(&pal, "Find modules"))
                .on_hover_text("One harmless 'are you there?' request to each likely address, about 500 in all")
                .clicked()
            {
                self.module_selected = None;
                self.session.send(Command::FindModules);
            }
            if running && ui.button("Stop").clicked() {
                self.session.send(Command::Stop);
            }
            if let Some(b) = &self.model.busy {
                ui.spinner();
                ui.label(RichText::new(b).color(pal.note));
            }
        });
        if let Some(p) = self.model.progress {
            ui.add(egui::ProgressBar::new(p).desired_width(ui.available_width().min(520.0)).show_percentage());
        }
        if self.model.modules.is_empty() && !running {
            ui.add_space(4.0);
            ui.label("Press Find modules. Scana listens to the bus first and leaves alone any address already in use, then asks each likely address once. It takes under a minute, and only runs while the car is still.");
            ui.label(RichText::new("Key on is enough; the engine can run or not.").color(pal.dim));
            return;
        }
        if let Some(d) = &self.model.discovery {
            let mut line = format!("Found {} module(s). Tried {} addresses.", d.found, d.tried);
            if !d.skipped.is_empty() {
                line.push_str(&format!(
                    " Left alone because they were in use: {}.",
                    d.skipped.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")
                ));
            }
            if d.stopped_early {
                line.push_str(" Stopped before the end.");
            }
            ui.label(RichText::new(line).color(pal.dim));
            if let Some(n) = &d.note {
                ui.label(RichText::new(n).color(pal.note));
            }
        }
        ui.add_space(6.0);
        let modules = self.model.modules.clone();
        egui::Grid::new("modules-table").striped(true).num_columns(4).spacing([18.0, 6.0]).show(ui, |ui| {
            ui.label(RichText::new("Module").strong());
            ui.label(RichText::new("Address").strong());
            ui.label(RichText::new("Speaks").strong());
            ui.label(RichText::new("Fault codes").strong());
            ui.end_row();
            for m in &modules {
                let selected = self.module_selected == Some(m.channel);
                if ui.selectable_label(selected, RichText::new(m.display_name()).size(15.0)).clicked() {
                    self.module_selected = if selected { None } else { Some(m.channel) };
                }
                ui.label(RichText::new(m.channel.label()).monospace());
                ui.label(if m.identity.is_empty() && m.name_from != NameFrom::Module && m.codes.is_err() {
                    RichText::new("reading...").color(pal.dim)
                } else {
                    RichText::new(m.dialect.name())
                });
                match &m.codes {
                    Ok(c) if c.is_empty() => ui.label(RichText::new("✔ none").color(pal.good)),
                    Ok(c) => {
                        let active = c.iter().any(|d| d.state().active);
                        let color = if active { pal.bad } else { pal.note };
                        ui.label(RichText::new(format!("{} code(s)", c.len())).color(color).strong())
                    }
                    Err(e) if e == "not read yet" => ui.label(RichText::new("...").color(pal.dim)),
                    Err(_) => ui.label(RichText::new("not read").color(pal.dim)),
                };
                ui.end_row();
            }
        });
        if modules.len() > 1 && self.module_selected.is_none() {
            ui.add_space(4.0);
            ui.label(RichText::new("Click a module to see its details and codes.").color(pal.dim));
        }
        let Some(sel) = self.module_selected else { return };
        let Some(m) = modules.iter().find(|m| m.channel == sel).cloned() else { return };
        ui.add_space(10.0);
        ui.separator();
        self.module_details(ui, &m);
    }

    fn module_details(&mut self, ui: &mut Ui, m: &ModuleInfo) {
        let pal = self.pal();
        ui.label(RichText::new(m.display_name()).size(18.0).strong());
        let how = match m.name_from {
            NameFrom::Module => "The module gave this name itself.".to_string(),
            NameFrom::Address => {
                "Name guessed from its address: the usual module there for this make. The workshop manual will confirm it."
                    .to_string()
            }
            NameFrom::Unknown => "Scana doesn't know what usually sits at this address.".to_string(),
        };
        ui.label(RichText::new(how).color(pal.dim));
        ui.label(
            RichText::new(format!(
                "Asked on {}, answers from {}{}. Speaks {}.",
                m.channel.tx,
                m.channel.rx,
                m.channel.ext.map(|a| format!(" with address byte {a:02X}")).unwrap_or_default(),
                m.dialect.name()
            ))
            .color(pal.dim),
        );
        ui.add_space(4.0);
        if !m.identity.is_empty() {
            egui::Grid::new(("identity", m.channel.label())).num_columns(2).spacing([24.0, 3.0]).show(ui, |ui| {
                for (k, v) in &m.identity {
                    ui.label(RichText::new(k).color(pal.dim));
                    ui.label(v);
                    ui.end_row();
                }
            });
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Fault codes").size(16.0).strong());
        match &m.codes {
            Ok(c) if c.is_empty() => {
                ui.label(RichText::new("✔ No fault codes stored in this module.").color(pal.good));
            }
            Ok(c) => {
                let make = self.model.make();
                for d in c {
                    let info = dtcdb::lookup_for(d.code, make.as_deref());
                    let state = d.state();
                    let color = if state.active { pal.bad } else { pal.note };
                    ui.horizontal_wrapped(|ui| {
                        let selected = self.topic == Some(Topic::Dtc(d.code));
                        if ui.selectable_label(selected, RichText::new(d.to_string()).monospace().strong()).clicked() {
                            self.open_topic(Topic::Dtc(d.code));
                        }
                        ui.label(RichText::new(state.words()).color(color));
                    });
                    ui.label(format!("    {}", info.summary()));
                    if let Some(ft) = d.failure_type
                        && ft != 0
                    {
                        ui.label(RichText::new(format!("    Detail: {}.", uds::failure_type_text(ft))).color(pal.dim));
                    }
                }
                ui.label(
                    RichText::new("Clearing module codes comes later, with its own checks. Fix the cause first.")
                        .small()
                        .color(pal.dim),
                );
            }
            Err(e) => {
                ui.label(RichText::new(format!("Couldn't read codes: {e}")).color(pal.note));
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(self.model.busy.is_none(), egui::Button::new("Read again")).clicked() {
                self.session.send(Command::ReadModule(m.channel));
            }
            if ui.button("Open in the data explorer").clicked() {
                self.open_explorer(m.channel, DataKind::for_dialect(m.dialect));
            }
        });
    }

    /// Opens the data explorer on a module, with a sensible first range for its dialect.
    pub fn open_explorer(&mut self, channel: Channel, kind: DataKind) {
        if self.explore_channel != Some(channel) {
            self.explore_channel = Some(channel);
            self.explore_kind = kind;
            let (from, to) = if kind == DataKind::Lid { ("00", "FF") } else { ("F180", "F19F") };
            self.explore_from = from.into();
            self.explore_to = to.into();
        }
        self.go(Page::Explorer);
    }
}
