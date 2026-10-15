//! The Learn page (guided jobs, glossary, lesson) and the Learn panel beside every page.

use crate::app::{Page, ScanaApp, Topic};
use egui::{RichText, Ui};
use scana_db::{dtc as dtcdb, learn};
use scana_proto::pids;

impl ScanaApp {
    pub fn page_learn(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Learn", "Guided jobs, the glossary, and how OBD-II works.");
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
        ui.label(RichText::new("Glossary").size(18.0).strong());
        ui.horizontal(|ui| {
            ui.label("Find:");
            ui.add(
                egui::TextEdit::singleline(&mut self.glossary_filter)
                    .hint_text("for example: trim")
                    .desired_width(220.0),
            );
        });
        let filter = self.glossary_filter.to_lowercase();
        for t in learn::glossary() {
            if !filter.is_empty()
                && !t.name.to_lowercase().contains(&filter)
                && !t.text.to_lowercase().contains(&filter)
            {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                if ui.link(RichText::new(&t.name).strong()).clicked() {
                    self.topic = Some(Topic::Term(t.name.clone()));
                }
                ui.label(&t.text);
            });
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
