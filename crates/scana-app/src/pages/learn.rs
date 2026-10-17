//! The Library (topics on reading data, the car's systems, and CAN and diagnostics; guided jobs;
//! how OBD-II works), the Glossary, and the Learn panel beside every page.

use crate::app::{Page, ScanaApp, Topic};
use crate::widgets::Look;
use egui::{Align, Layout, RichText, Ui, Widget};
use scana_db::library::{self, Link};
use scana_db::{dtc as dtcdb, learn};
use scana_proto::pids;

/// Topics to start with on each page, shown in the Learn panel under the page's help.
fn start_here(page: Page) -> &'static [&'static str] {
    match page {
        Page::Home => &["reading-live-data", "obd2-overview", "obd-connector"],
        Page::Health => &["readiness-monitors", "freeze-frame-reading", "mode06-reading", "dtc-format"],
        Page::Codes => &["dtc-format", "freeze-frame-reading", "permanent-codes"],
        Page::Diagnose => &["test-conditions", "reading-live-data", "using-graphs"],
        Page::Live => &["reading-live-data", "fuel-trim", "update-rates"],
        Page::Graphs => &["using-graphs", "test-conditions"],
        Page::Readiness => &["readiness-monitors"],
        Page::Tests => &["mode06-reading"],
        Page::Info => &["vin", "obd-services"],
        Page::Modules => &["car-networks", "uds", "gateways-addressing"],
        Page::Explorer => &["uds-reading", "kwp2000", "decoding-signals"],
        Page::Monitor => &["reading-bus-monitor", "can-frame", "can-id"],
        Page::Sessions => &["using-graphs"],
        Page::Learn => &["reading-live-data", "engine-management", "can-bus"],
        Page::Glossary | Page::Settings => &[],
    }
}

/// Library topics that link to `entry` (such as "pid:06" or "code:P0420") in their related list.
fn topics_linking(entry: &str) -> Vec<Link> {
    library::topics()
        .iter()
        .filter(|t| t.related.iter().any(|r| r.eq_ignore_ascii_case(entry)))
        .map(|t| Link::Topic(t.id.clone()))
        .take(6)
        .collect()
}

impl ScanaApp {
    pub fn page_learn(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Library",
            "How to read the car's data, how its systems work, and how scan tools talk to it. Click a topic, or any underlined word anywhere in Scana, to read it in the Learn panel.",
        );
        ui.horizontal(|ui| {
            ui.label(RichText::new("Find:").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.glossary_filter)
                    .hint_text("a topic, a code (P0420) or a tool")
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
        }
        if !filter.is_empty() {
            for t in scana_db::diag::tools()
                .iter()
                .filter(|t| t.name.to_lowercase().contains(&filter) || t.what.to_lowercase().contains(&filter))
            {
                ui.label(RichText::new(format!("Tool: {} (cost: {})", t.name, t.cost)).strong());
                self.linked(ui, &t.what);
                ui.label(RichText::new(&t.how).color(pal.dim));
                if let Some(sf) = &t.safety {
                    ui.label(RichText::new(sf).color(pal.note));
                }
                ui.add_space(4.0);
            }
        }
        let matches = |t: &library::Topic| {
            filter.is_empty()
                || t.title.to_lowercase().contains(&filter)
                || t.aliases.iter().any(|a| a.to_lowercase().contains(&filter))
                || t.summary.to_lowercase().contains(&filter)
        };
        for (group, title) in library::GROUPS {
            let topics: Vec<&library::Topic> =
                library::topics().iter().filter(|t| t.group == group && matches(t)).collect();
            if topics.is_empty() {
                continue;
            }
            ui.add_space(10.0);
            ui.label(RichText::new(format!("{title} ({})", topics.len())).size(18.0).strong());
            ui.add_space(4.0);
            self.topic_cards(ui, &topics);
        }
        ui.add_space(14.0);
        egui::CollapsingHeader::new(RichText::new("How OBD-II works: a short lesson").size(17.0).strong())
            .id_salt("lesson")
            .show(ui, |ui| {
                for s in learn::lesson() {
                    ui.label(RichText::new(&s.title).strong());
                    self.linked(ui, &s.text);
                    ui.add_space(4.0);
                }
            });
        ui.add_space(6.0);
        ui.label(RichText::new("Guided jobs").size(17.0).strong());
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
    }

    /// Topics as cards, two to a row: title (click to read) and summary.
    fn topic_cards(&mut self, ui: &mut Ui, topics: &[&library::Topic]) {
        let pal = self.pal();
        let gap = 8.0;
        let width = ((ui.available_width() - gap) / 2.0).max(220.0);
        let mut open = None;
        for row in topics.chunks(2) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for t in row {
                    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                        let selected = self.topic == Some(Topic::Library(t.id.clone()));
                        egui::Frame::new()
                            .fill(if selected { pal.sel_bg } else { pal.tile })
                            .stroke(egui::Stroke::new(1.0, if selected { pal.accent } else { pal.tile_border }))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(width - 22.0);
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(&t.title).strong().color(pal.link))
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() {
                                    open = Some(t.id.clone());
                                }
                                ui.label(RichText::new(&t.summary).small().color(pal.dim));
                            });
                    });
                }
            });
            ui.add_space(gap);
        }
        if let Some(id) = open {
            self.open_topic(Topic::Library(id));
        }
    }

    pub fn page_glossary(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Glossary", "Every word Scana explains, A to Z. Click a word to read more about it.");
        ui.horizontal(|ui| {
            ui.label(RichText::new("Find:").strong());
            ui.add(egui::TextEdit::singleline(&mut self.glossary_filter).hint_text("a word").desired_width(320.0));
            if !self.glossary_filter.is_empty() && ui.small_button("Clear").clicked() {
                self.glossary_filter.clear();
            }
        });
        let filter = self.glossary_filter.trim().to_lowercase();
        let shown: Vec<&'static learn::Term> = learn::glossary()
            .iter()
            .filter(|t| {
                filter.is_empty() || t.name.to_lowercase().contains(&filter) || t.text.to_lowercase().contains(&filter)
            })
            .collect();
        ui.label(RichText::new(format!("{} of {} words", shown.len(), learn::glossary().len())).color(pal.dim));
        let mut letter = None;
        for t in shown {
            let first = t.name.chars().next().map(|c| c.to_ascii_uppercase());
            if first != letter {
                letter = first;
                ui.add_space(6.0);
                ui.label(
                    RichText::new(first.map(String::from).unwrap_or_default()).size(18.0).strong().color(pal.accent),
                );
            }
            let resp = ui
                .add(egui::Label::new(RichText::new(&t.name).strong().color(pal.link)).sense(egui::Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                self.open_topic(Topic::Term(t.name.clone()));
            }
            self.linked_look(ui, &t.text, Look::default(), &[Link::Term(t.name.clone())]);
            ui.add_space(2.0);
        }
    }

    /// What Scana knows about one code, on the Library page.
    fn code_card(&mut self, ui: &mut Ui, code: scana_proto::Dtc) {
        let pal = self.pal();
        let info = dtcdb::lookup_for(code, self.model.make().as_deref());
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{code}  {}", info.summary())).size(17.0).strong());
            ui.label(RichText::new(format!("Group: {}. System: {}.", info.group, info.system.name())).color(pal.dim));
            if let Some(m) = info.maker {
                ui.label(RichText::new(maker_line(m)).color(pal.note));
                for src in &m.sources {
                    ui.label(RichText::new(format!("Source: {src}")).small().color(pal.dim));
                }
            } else if info.manufacturer_specific {
                ui.label(
                    RichText::new(
                        "Maker-specific: its exact meaning depends on the make. The maker's workshop manual has it.",
                    )
                    .color(pal.note),
                );
            }
            if let Some(a) = info.advice {
                ui.label(RichText::new("What it usually means").strong());
                self.linked(ui, &a.means);
                ui.label(RichText::new("What to check first").strong());
                self.linked_bullets(ui, &a.check, &[]);
            }
            let make = self.model.make();
            if let Some(g) = scana_db::diag::guide_for_make(code, make.as_deref()) {
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

    /// The panel on the right: what the chosen thing means, or help for the page, with Back and
    /// Forward through what was read.
    pub fn learn_panel(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        ui.add_space(6.0);
        let kind = match &self.topic {
            Some(Topic::Library(_)) => "LEARN · LIBRARY",
            Some(Topic::Term(_)) => "LEARN · WORD",
            Some(Topic::Pid(..)) => "LEARN · LIVE VALUE",
            Some(Topic::Dtc(_)) => "LEARN · CODE",
            None => "LEARN · THIS PAGE",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(kind).small().strong().color(pal.accent));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!self.learn_forward.is_empty(), egui::Button::new("▸").small())
                    .on_hover_text("Forward")
                    .clicked()
                {
                    self.learn_go_forward();
                }
                if ui
                    .add_enabled(!self.learn_back.is_empty(), egui::Button::new("◂").small())
                    .on_hover_text("Back")
                    .clicked()
                {
                    self.learn_go_back();
                }
            });
        });
        egui::ScrollArea::vertical().id_salt("learn-panel").auto_shrink([false, false]).show(ui, |ui| {
            match self.topic.clone() {
                Some(Topic::Library(id)) => self.learn_topic(ui, &id),
                Some(Topic::Pid(ecu, pid)) => self.learn_pid(ui, ecu, pid),
                Some(Topic::Dtc(code)) => self.learn_dtc(ui, code),
                Some(Topic::Term(name)) => self.learn_term(ui, &name),
                None => {
                    if let Some(h) = learn::page(self.page.id()) {
                        ui.label(RichText::new(&h.title).size(20.0).strong());
                        self.linked(ui, &h.text);
                    }
                    let start: Vec<Link> = start_here(self.page).iter().map(|id| Link::Topic(id.to_string())).collect();
                    if !start.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Start here").strong().color(pal.accent));
                        self.link_chips(ui, &start);
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Underlined words anywhere in Scana: rest the pointer on one to see what it means, click it to read more here.",
                        )
                        .small()
                        .color(pal.dim),
                    );
                }
            }
        });
    }

    /// A Library topic in full.
    fn learn_topic(&mut self, ui: &mut Ui, id: &str) {
        let pal = self.pal();
        let Some(t) = library::topic(id) else {
            ui.label("This topic isn't in the Library.");
            return;
        };
        let mut seen = vec![Link::Topic(t.id.clone())];
        ui.label(RichText::new(&t.title).size(20.0).strong());
        ui.label(RichText::new(library::group_title(&t.group)).small().color(pal.dim));
        ui.add_space(2.0);
        let found = self.linked_look(ui, &t.summary, Look::strong(), &seen);
        seen.extend(found);
        ui.add_space(4.0);
        for p in &t.body {
            let found = self.linked_look(ui, p, Look::default(), &seen);
            seen.extend(found);
            ui.add_space(4.0);
        }
        let section = |ui: &mut Ui, title: &str| {
            ui.add_space(4.0);
            ui.label(RichText::new(title).strong().color(pal.accent));
        };
        if !t.how_to_read.is_empty() {
            section(ui, "How to read it");
            seen = self.linked_bullets(ui, &t.how_to_read, &seen);
        }
        if !t.parts.is_empty() {
            section(ui, "The parts");
            seen = self.linked_bullets(ui, &t.parts, &seen);
        }
        if !t.faults.is_empty() {
            section(ui, "Common faults");
            seen = self.linked_bullets(ui, &t.faults, &seen);
        }
        if !t.more_detail.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(RichText::new("More detail").strong()).id_salt(("more", &t.id)).show(
                ui,
                |ui| {
                    for p in &t.more_detail {
                        let found = self.linked_look(ui, p, Look::default(), &seen);
                        seen.extend(found);
                        ui.add_space(4.0);
                    }
                },
            );
        }
        let related: Vec<Link> = t.related.iter().filter_map(|r| Link::parse(r)).collect();
        if !related.is_empty() {
            section(ui, "Related");
            self.link_chips(ui, &related);
        }
        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new(format!("Sources ({})", t.sources.len())).small().color(pal.dim))
            .id_salt(("sources", &t.id))
            .show(ui, |ui| {
                for s in &t.sources {
                    ui.label(RichText::new(format!("• {s}")).small().color(pal.dim));
                }
            });
    }

    fn learn_term(&mut self, ui: &mut Ui, name: &str) {
        let pal = self.pal();
        let Some(t) = learn::term(name) else { return };
        ui.label(RichText::new(&t.name).size(20.0).strong());
        self.linked_look(ui, &t.text, Look::default(), &[Link::Term(t.name.clone())]);
        let more: Vec<Link> = library::search(&t.name, 12)
            .into_iter()
            .filter(|h| matches!(h.link, Link::Topic(_)))
            .map(|h| h.link)
            .take(4)
            .collect();
        if !more.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Read more").strong().color(pal.accent));
            self.link_chips(ui, &more);
        }
    }

    fn learn_dtc(&mut self, ui: &mut Ui, code: scana_proto::Dtc) {
        let pal = self.pal();
        let info = dtcdb::lookup_for(code, self.model.make().as_deref());
        ui.label(RichText::new(code.to_string()).size(22.0).strong());
        self.linked_look(ui, &info.summary(), Look { size: Some(16.0), strong: true, ..Look::default() }, &[]);
        if let Some(m) = info.maker {
            ui.label(RichText::new(maker_line(m)).small().color(pal.note));
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Group").strong().color(pal.dim));
        self.linked(ui, &format!("This is {}.", info.group));
        ui.label(RichText::new("System").strong().color(pal.dim));
        ui.label(info.system.name());
        if let Some(a) = info.advice {
            ui.add_space(4.0);
            ui.label(RichText::new("What it usually means").strong().color(pal.dim));
            self.linked(ui, &a.means);
            ui.label(RichText::new("What to check first").strong().color(pal.dim));
            self.linked_bullets(ui, &a.check, &[]);
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("General guidance: check the maker's workshop manual before replacing parts.")
                .small()
                .color(pal.dim),
        );
        let make = self.model.make();
        if let Some(g) = scana_db::diag::guide_for_make(code, make.as_deref()) {
            ui.add_space(4.0);
            if crate::theme::accent_button(&pal, &format!("Diagnose {code} step by step"))
                .ui(ui)
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
        let mut more = topics_linking(&format!("code:{code}"));
        for extra in ["dtc-format", "freeze-frame-reading"] {
            if more.len() < 4 {
                more.push(Link::Topic(extra.into()));
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Read more").strong().color(pal.accent));
        self.link_chips(ui, &more);
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
        let section = |ui: &mut Ui, title: &str| {
            ui.add_space(2.0);
            ui.label(RichText::new(title).strong().color(pal.dim));
        };
        if let Some(e) = learn::pid(pid) {
            section(ui, "What it is");
            self.linked(ui, &e.what);
            if let Some(n) = &e.normal {
                section(ui, "Normal");
                self.linked(ui, n);
            }
            if let Some(r) = &e.read_with {
                section(ui, "Read it with");
                self.linked(ui, r);
            }
        } else {
            ui.label("Scana has no explanation for this value yet.");
        }
        if let Some(p) = self.model.profile() {
            if let Some(n) = p.note(pid) {
                section(ui, "This truck");
                self.linked(ui, n);
            }
            if let Some(b) = p.baseline(pid) {
                section(ui, "At the 2026-10-10 scan");
                ui.label(format!("{b} (cold engine, 52 s after starting)."));
            }
        }
        let more = topics_linking(&format!("pid:{pid:02X}"));
        if !more.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Read more").strong().color(pal.accent));
            self.link_chips(ui, &more);
        }
    }
}

/// Where a make-specific meaning comes from, in one line.
fn maker_line(m: &dtcdb::MakerCode) -> String {
    format!(
        "The meaning on {} cars, from the sources in the Library. Other makes use this code differently.",
        m.makes.join(" and ")
    )
}
