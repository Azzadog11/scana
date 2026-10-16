//! The Learn page (guided jobs, glossary, lesson) and the Learn panel beside every page.

use crate::app::{Page, ScanaApp, Topic};
use egui::{RichText, Ui};
use scana_db::{dtc as dtcdb, learn};
use scana_proto::pids;

impl ScanaApp {
    pub fn page_learn(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Learn", "Look up any code, word or tool; guided jobs; and how OBD-II works.");
        ui.horizontal(|ui| {
            ui.label(RichText::new("Look up:").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.glossary_filter)
                    .hint_text("a code (P0420), a word (fuel trim) or a tool")
                    .desired_width(320.0),
            );
            if !self.glossary_filter.is_empty() && ui.small_button("Clear").clicked() {
                self.glossary_filter.clear();
            }
        });
        let query = self.glossary_filter.trim().to_string();
        let filter = query.to_lowercase();
        if let Some(code) = scana_proto::Dtc::parse(&query) {
            self.code_card(ui, code);
        } else if !filter.is_empty() {
            let guides = scana_db::diag::search(&query);
            if !guides.is_empty() {
                ui.label(RichText::new("Diagnosis guides").strong());
                for g in guides {
                    ui.horizontal_wrapped(|ui| {
                        if ui.link(&g.title).clicked()
                            && let Some(c) =
                                g.codes.first().and_then(|c| scana_proto::Dtc::parse(c.split('-').next().unwrap_or("")))
                        {
                            self.start_diagnosis(c);
                        }
                        ui.label(RichText::new(g.codes.join(", ")).small().color(pal.dim));
                    });
                }
                ui.add_space(4.0);
            }
            for t in scana_db::diag::tools()
                .iter()
                .filter(|t| t.name.to_lowercase().contains(&filter) || t.what.to_lowercase().contains(&filter))
            {
                ui.label(RichText::new(format!("Tool: {} (cost: {})", t.name, t.cost)).strong());
                ui.label(&t.what);
                ui.label(RichText::new(&t.how).color(pal.dim));
                if let Some(sf) = &t.safety {
                    ui.label(RichText::new(sf).color(pal.note));
                }
                ui.add_space(4.0);
            }
        }
        ui.add_space(8.0);
        let shown: Vec<&scana_db::learn::Term> = learn::glossary()
            .iter()
            .filter(|t| {
                filter.is_empty() || t.name.to_lowercase().contains(&filter) || t.text.to_lowercase().contains(&filter)
            })
            .collect();
        ui.label(
            RichText::new(format!("Glossary ({} of {} words)", shown.len(), learn::glossary().len()))
                .size(18.0)
                .strong(),
        );
        for t in shown {
            ui.horizontal_wrapped(|ui| {
                if ui.link(RichText::new(&t.name).strong()).clicked() {
                    self.topic = Some(Topic::Term(t.name.clone()));
                }
                ui.label(&t.text);
            });
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Guided jobs").size(18.0).strong());
        for job in learn::jobs() {
            let open = self.open_job.as_deref() == Some(job.id.as_str());
            let resp = egui::CollapsingHeader::new(RichText::new(&job.title).strong())
                .id_salt(&job.id)
                .open(if open { Some(true) } else { None })
                .show(ui, |ui| {
                    ui.label(RichText::new(&job.why).color(pal.dim));
                    for (i, step) in job.steps.iter().enumerate() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("{}.", i + 1));
                            ui.label(&step.text);
                            if let Some(p) = step.page.as_deref().and_then(Page::from_id)
                                && p != Page::Learn
                                && ui.small_button(format!("Go to {}", p.title())).clicked()
                            {
                                self.go(p);
                            }
                        });
                    }
                });
            if open && resp.header_response.clicked() {
                self.open_job = None;
            }
        }
        ui.add_space(12.0);
        ui.label(RichText::new("How OBD-II works").size(18.0).strong());
        for s in learn::lesson() {
            ui.label(RichText::new(&s.title).strong());
            ui.label(&s.text);
            ui.add_space(4.0);
        }
    }

    /// The panel on the right: what the selected thing means, or help for the page.
    /// What Scana knows about one code, on the Learn page.
    fn code_card(&mut self, ui: &mut Ui, code: scana_proto::Dtc) {
        let pal = self.pal();
        let info = dtcdb::lookup(code);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{code}  {}", info.summary())).size(17.0).strong());
            ui.label(RichText::new(format!("Group: {}. System: {}.", info.group, info.system.name())).color(pal.dim));
            if info.manufacturer_specific {
                ui.label(
                    RichText::new(
                        "Maker-specific: its exact meaning depends on the make. The maker's workshop manual has it.",
                    )
                    .color(pal.note),
                );
            }
            if let Some(a) = info.advice {
                ui.label(RichText::new("What it usually means").strong());
                ui.label(&a.means);
                ui.label(RichText::new("What to check first").strong());
                for c in &a.check {
                    ui.label(format!("• {c}"));
                }
            }
            let make = self.model.profile().and_then(|p| p.make.as_deref());
            if let Some(g) = scana_db::diag::guide_for_make(code, make) {
                if ui.button(RichText::new(format!("Diagnose {code} step by step: {}", g.title)).strong()).clicked() {
                    self.start_diagnosis(code);
                }
            } else {
                for (m, g) in scana_db::diag::maker_hints(code) {
                    ui.label(RichText::new(format!("On {m}, {code} means: {}.", g.title)).color(pal.note));
                    if ui.button("Open that guide").clicked() {
                        self.start_diagnosis(code);
                    }
                }
            }
            ui.label(
                RichText::new("General guidance: check the maker's workshop manual before replacing parts.")
                    .small()
                    .color(pal.dim),
            );
        });
    }

    pub fn learn_panel(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        ui.add_space(6.0);
        ui.label(RichText::new("LEARN").small().strong().color(pal.accent));
        egui::ScrollArea::vertical().id_salt("learn-panel").auto_shrink([false, false]).show(ui, |ui| {
            match self.topic.clone() {
                Some(Topic::Pid(ecu, pid)) => self.learn_pid(ui, ecu, pid),
                Some(Topic::Dtc(code)) => {
                    let info = dtcdb::lookup(code);
                    ui.label(RichText::new(code.to_string()).size(22.0).strong());
                    ui.label(RichText::new(info.summary()).size(16.0).strong());
                    ui.add_space(4.0);
                    section(ui, &pal, "Group", &format!("This is {}.", info.group));
                    section(ui, &pal, "System", info.system.name());
                    if let Some(a) = info.advice {
                        section(ui, &pal, "What it usually means", &a.means);
                        ui.label(RichText::new("What to check first").strong().color(pal.dim));
                        for c in &a.check {
                            ui.label(format!("• {c}"));
                        }
                        ui.add_space(4.0);
                    }
                    ui.label(
                        RichText::new("General guidance: check the maker's workshop manual before replacing parts.")
                            .small()
                            .color(pal.dim),
                    );
                    if ui.link("What's a freeze frame?").clicked() {
                        self.topic = Some(Topic::Term("Freeze frame".into()));
                    }
                    let make = self.model.profile().and_then(|p| p.make.as_deref());
                    if let Some(g) = scana_db::diag::guide_for_make(code, make) {
                        ui.add_space(4.0);
                        if ui
                            .button(RichText::new(format!("Diagnose {code} step by step")).strong())
                            .on_hover_text(&g.title)
                            .clicked()
                        {
                            self.start_diagnosis(code);
                        }
                    } else {
                        for (m, g) in scana_db::diag::maker_hints(code) {
                            ui.add_space(4.0);
                            ui.label(RichText::new(format!("On {m}, {code} means: {}.", g.title)).color(pal.note));
                            if ui.button("Open that guide").clicked() {
                                self.start_diagnosis(code);
                            }
                        }
                    }
                }
                Some(Topic::Term(name)) => {
                    if let Some(t) = learn::term(&name) {
                        ui.label(RichText::new(&t.name).size(20.0).strong());
                        ui.label(&t.text);
                    }
                    self.glossary_links(ui);
                }
                None => {
                    if let Some(h) = learn::page(self.page.id()) {
                        ui.label(RichText::new(&h.title).size(20.0).strong());
                        ui.label(&h.text);
                    }
                    self.glossary_links(ui);
                }
            }
        });
    }

    fn learn_pid(&mut self, ui: &mut Ui, ecu: scana_proto::CanId, pid: u8) {
        let pal = self.pal();
        let name = pids::def(pid).map_or("Unknown PID", |d| d.name);
        ui.label(RichText::new(name).size(20.0).strong());
        ui.label(RichText::new(format!("PID {pid:02X} from {}", self.model.ecu_name(ecu))).small().color(pal.dim));
        if let Some(l) = self.model.live.get(&(ecu, pid)) {
            ui.label(RichText::new(format!("Now: {}", l.reading.display())).size(16.0));
        }
        ui.add_space(4.0);
        if let Some(e) = learn::pid(pid) {
            section(ui, &pal, "What it is", &e.what);
            if let Some(n) = &e.normal {
                section(ui, &pal, "Normal", n);
            }
            if let Some(r) = &e.read_with {
                section(ui, &pal, "Read it with", r);
            }
        } else {
            ui.label("Scana has no explanation for this value yet.");
        }
        if let Some(p) = self.model.profile() {
            if let Some(n) = p.note(pid) {
                section(ui, &pal, "This truck", n);
            }
            if let Some(b) = p.baseline(pid) {
                section(ui, &pal, "At the 2026-10-10 scan", &format!("{b} (cold engine, 52 s after starting)."));
            }
        }
        self.glossary_links(ui);
    }

    fn glossary_links(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.label(RichText::new("Glossary").small().strong().color(self.pal().dim));
        ui.horizontal_wrapped(|ui| {
            for name in
                ["MIL", "Freeze frame", "Readiness monitor", "Fuel trim", "Closed loop", "PID", "DTC", "Pending code"]
            {
                if ui.small_button(name).clicked() {
                    self.topic = Some(Topic::Term(name.into()));
                }
            }
        });
    }
}

fn section(ui: &mut Ui, pal: &crate::theme::Palette, title: &str, text: &str) {
    ui.label(RichText::new(title).strong().color(pal.dim));
    ui.label(text);
    ui.add_space(4.0);
}
