//! Home (connecting, and the car at a glance) and the Health check.

use crate::app::{Page, ScanaApp, SourceChoice};
use egui::{RichText, Ui};
use scana_core::report::Tone;
use scana_core::session::{Command, LinkState};
use scana_proto::Bus;

impl ScanaApp {
    pub fn page_home(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        if self.model.connected() {
            self.home_connected(ui);
            return;
        }
        self.heading(ui, "Home", "Connect to a car, or try the demo car to learn the app at your desk.");
        match self.model.state.clone() {
            Some(LinkState::Connecting(s)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("{s}...")).strong());
                });
                ui.add_space(8.0);
            }
            Some(LinkState::Lost(s)) => {
                ui.label(RichText::new(s).color(pal.note).strong());
                ui.add_space(8.0);
            }
            Some(LinkState::Failed(s)) => {
                ui.label(RichText::new(s).color(pal.bad).strong());
                ui.add_space(8.0);
            }
            _ => {}
        }
        if self.settings.first_run {
            egui::Frame::new().fill(pal.accent.gamma_multiply(0.22)).corner_radius(8).inner_margin(12).show(ui, |ui| {
                ui.label(RichText::new("Welcome to Scana").size(19.0).strong());
                ui.label("Every page works on a simulated 2024 Hijet, so you can learn the app without the truck. Click anything to see what it means in the Learn panel on the right.");
                ui.add_space(4.0);
                if ui.button(RichText::new("Start the demo car").strong()).clicked() {
                    self.source_choice = SourceChoice::Demo;
                    self.connect_choice();
                }
            });
            ui.add_space(10.0);
        }
        egui::Frame::group(ui.style()).inner_margin(12).show(ui, |ui| {
            ui.label(RichText::new("Connect").size(18.0).strong());
            ui.radio_value(&mut self.source_choice, SourceChoice::Bridge, "Scana bridge: the Arduino Uno and CAN-BUS Shield on USB");
            if self.source_choice == SourceChoice::Bridge {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let label = self.port_choice.clone().unwrap_or_else(|| "Find it for me".into());
                    egui::ComboBox::from_id_salt("port").selected_text(label).width(320.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.port_choice, None, "Find it for me");
                        for p in self.ports.clone() {
                            let text = format!(
                                "{}  {}{}",
                                p.path,
                                p.product.clone().unwrap_or_default(),
                                if p.likely_bridge { "  (likely)" } else { "" }
                            );
                            ui.selectable_value(&mut self.port_choice, Some(p.path.clone()), text);
                        }
                    });
                    if ui.button("Look again").clicked() {
                        self.ports = scana_link::serial::list_ports();
                    }
                });
                if self.ports.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.label(RichText::new("No USB serial port found. Is the Uno plugged in?").color(pal.note));
                    });
                }
                // Which pair of OBD-II pins the cable or switch reaches.
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        ui.radio_value(&mut self.bus_choice, Bus::Main, "Pins 6 and 14 (main bus)");
                        ui.radio_value(
                            &mut self.bus_choice,
                            Bus::Second,
                            "Pins 3 and 11 (second bus: some Fords, Mazdas and Volvos)",
                        );
                        ui.label(
                            RichText::new("The second bus needs a cable or switch that reaches pins 3 and 11.")
                                .small()
                                .color(pal.dim),
                        );
                        if self.bus_choice == Bus::Second {
                            ui.horizontal(|ui| {
                                ui.label("Make (optional, for module names):");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.make_text).desired_width(120.0).hint_text("Ford"),
                                );
                            });
                        }
                    });
                });
            }
            ui.radio_value(&mut self.source_choice, SourceChoice::Demo, "Demo car: a simulated Hijet, engine running, with a dash display polling the same port");
            ui.radio_value(
                &mut self.source_choice,
                SourceChoice::DemoFaults,
                "Demo car with faults: key on, engine off, a stored code, a pending code and a failed test. Safe to try clearing codes",
            );
            ui.radio_value(
                &mut self.source_choice,
                SourceChoice::DemoCorolla,
                "Demo Corolla: a simulated 2006 Corolla with P0420 (a worn catalyst), engine warm. Try the diagnosis guide on it",
            );
            ui.radio_value(
                &mut self.source_choice,
                SourceChoice::DemoSecondBus,
                "Demo second bus: an invented simulated car on pins 3 and 11, with body modules. Scana only listens, then names modules",
            );
            ui.add_space(6.0);
            let busy = matches!(self.model.state, Some(LinkState::Connecting(_)));
            if ui.add_enabled(!busy, crate::theme::accent_button(&pal, "Connect")).clicked() {
                self.connect_choice();
            }
            let second = match self.source_choice {
                SourceChoice::Bridge => self.bus_choice == Bus::Second,
                SourceChoice::DemoSecondBus => true,
                _ => false,
            };
            let how = if second {
                "On pins 3 and 11 Scana only listens, at 125, then 500, then 250 kbit/s. It sends nothing until you say the car is parked and ask for modules."
            } else {
                "Scana listens first, then sends one standard request at a time to find the car's bus speed. It stops at the first sign of trouble."
            };
            ui.label(RichText::new(how).small().color(pal.dim));
        });
        ui.add_space(10.0);
        ui.label(RichText::new("Guided jobs").size(17.0).strong());
        for job in scana_db::learn::jobs() {
            ui.horizontal(|ui| {
                if ui.link(&job.title).clicked() {
                    self.open_job = Some(job.id.clone());
                    self.go(Page::Learn);
                }
                ui.label(RichText::new(&job.why).color(pal.dim));
            });
        }
    }

    fn home_connected(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        let Some(conn) = self.model.conn.clone() else { return };
        let profile = self.model.profile();
        self.heading(ui, &conn.vehicle, &conn.protocol());
        egui::Grid::new("car").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            if let Some(p) = profile {
                ui.label("Model");
                ui.label(&p.model);
                ui.end_row();
            }
            ui.label("Connected through");
            ui.label(&conn.link);
            ui.end_row();
            for e in &conn.ecus {
                let live = e.supported.iter().filter(|p| !p.is_multiple_of(0x20)).count();
                ui.label(format!("Computer {}", e.id));
                ui.label(format!("{}: {live} live values", e.name));
                ui.end_row();
            }
            if let Some(p) = profile
                && let Some(n) = &p.not_reported
            {
                ui.label("Not reported");
                ui.label(n);
                ui.end_row();
            }
        });
        if conn.other_tester {
            ui.label(
                RichText::new(
                    "Another tester is polling the car (a dash display?). Scana leaves wider gaps to share the bus.",
                )
                .color(pal.note),
            );
        }
        ui.add_space(10.0);
        if conn.bus == Bus::Second {
            ui.label(RichText::new("Scana is listening on the second bus (pins 3 and 11).").strong());
            ui.label(
                RichText::new("Health check, fault codes, live data and the other car pages need the main bus on pins 6 and 14: set the cable or switch back and connect again.")
                    .color(pal.note),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(crate::theme::accent_button(&pal, "Bus monitor")).clicked() {
                    self.go(Page::Monitor);
                }
                if ui.button("Find modules").clicked() {
                    self.go(Page::Modules);
                }
                let rec = self.model.recording.is_some();
                if ui.button(if rec { "Stop recording" } else { "Record this session" }).clicked() {
                    self.set_record(!rec);
                }
            });
            ui.add_space(10.0);
            egui::CollapsingHeader::new("How Scana found the bus").show(ui, |ui| {
                for l in &conn.log {
                    ui.label(RichText::new(l).color(pal.dim));
                }
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if ui.add(crate::theme::accent_button(&pal, "Health check")).clicked() {
                self.session.send(Command::HealthCheck);
                self.go(Page::Health);
            }
            if ui.button("Read fault codes").clicked() {
                self.session.send(Command::ReadCodes);
                self.go(Page::Codes);
            }
            if ui.button("Live data").clicked() {
                self.go(Page::Live);
            }
            if ui.button("Graphs").clicked() {
                self.go(Page::Graphs);
            }
            let rec = self.model.recording.is_some();
            if ui.button(if rec { "Stop recording" } else { "Record this session" }).clicked() {
                self.set_record(!rec);
            }
        });
        if let Some((report, _)) = &self.model.health {
            ui.add_space(10.0);
            ui.label(RichText::new(format!("Last health check, {}", report.when)).strong());
            for (tone, line) in report.summary().into_iter().take(6) {
                ui.label(RichText::new(format!("{} {line}", tone_mark(tone))).color(tone_color(self, tone)));
            }
        }
        ui.add_space(10.0);
        egui::CollapsingHeader::new("How Scana found the car").show(ui, |ui| {
            for l in &conn.log {
                ui.label(RichText::new(l).color(pal.dim));
            }
        });
    }

    pub fn page_health(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Health check",
            "The check engine light, every code, readiness, the freeze frame, test results and key values, in one go.",
        );
        if !self.need_obd(ui) {
            return;
        }
        ui.horizontal(|ui| {
            let busy = self.model.busy.is_some();
            if ui.add_enabled(!busy, crate::theme::accent_button(&pal, "Run the health check")).clicked() {
                self.session.send(Command::HealthCheck);
            }
            if let Some(b) = &self.model.busy {
                ui.spinner();
                ui.label(b);
            }
        });
        ui.add_space(8.0);
        let Some((report, path)) = self.model.health.clone() else {
            ui.label("No health check yet in this connection. It takes a few seconds, and saves a report you can keep or print.");
            return;
        };
        ui.label(RichText::new(format!("{} · {}", report.when, report.protocol)).color(pal.dim));
        ui.add_space(4.0);
        for (tone, line) in report.summary() {
            ui.label(RichText::new(format!("{} {line}", tone_mark(tone))).color(tone_color(self, tone)).size(16.0));
        }
        ui.add_space(8.0);
        if let Some(p) = path {
            ui.horizontal(|ui| {
                if ui.button("Open the report").clicked() {
                    self.open_path(&p);
                }
                if let Some(dir) = p.parent()
                    && ui.button("Open its folder").clicked()
                {
                    let d = dir.to_path_buf();
                    self.open_path(&d);
                }
                ui.label(RichText::new(p.display().to_string()).small().color(pal.dim));
            });
        }
        ui.add_space(8.0);
        for e in &report.ecus {
            if e.values.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(format!("Key values: {}", e.name)).default_open(true).show(ui, |ui| {
                egui::Grid::new(("hv", e.ecu.raw())).striped(true).num_columns(2).show(ui, |ui| {
                    for r in &e.values {
                        ui.label(r.name);
                        ui.label(r.display());
                        ui.end_row();
                    }
                });
            });
        }
    }
}

pub fn tone_mark(t: Tone) -> &'static str {
    match t {
        Tone::Good => "✔",
        Tone::Note => "⚠",
        Tone::Bad => "✖",
    }
}

pub fn tone_color(app: &ScanaApp, t: Tone) -> egui::Color32 {
    let p = app.pal();
    match t {
        Tone::Good => p.good,
        Tone::Note => p.note,
        Tone::Bad => p.bad,
    }
}
