//! Diagnose: works through a guide for a fault code. Likely causes with their shares, and
//! tests that tell them apart. Every test can be answered by hand (from what you see, or
//! another scan tool's readings); on a connected CAN car Scana measures some itself.

use crate::app::{Page, ScanaApp, Topic};
use egui::{RichText, Ui};
use scana_core::diagnose::{self, Diagnosis, Measured, Sample};
use scana_core::session::{Command, Update};
use scana_db::diag::{self, Test};
use scana_proto::{CanId, Dtc};
use std::time::Instant;

/// A live measurement in progress.
pub struct DiagMeasure {
    pub test: String,
    pub ecu: CanId,
    pub pids: Vec<u8>,
    pub samples: Vec<Sample>,
    /// Seconds of samples taken while the conditions held.
    pub good_s: f64,
    pub last_good_ms: Option<u64>,
    /// Latest value of each PID.
    pub latest: std::collections::HashMap<u8, f64>,
    /// The live values chosen before, put back afterwards.
    pub saved_live: Vec<(CanId, u8)>,
    pub started: Instant,
}

/// What a one-off read is waiting for, to answer a test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagWait {
    Codes(String),
    Tests(String),
    Readiness(String),
    Freeze(String),
}

impl ScanaApp {
    /// Starts a diagnosis for a code (from the Codes page, the Learn panel, or typed).
    pub fn start_diagnosis(&mut self, code: Dtc) {
        let make = self.model.make();
        let found = match Diagnosis::for_code_make(code, make.as_deref()) {
            Some(d) => Some(d),
            None => {
                // A maker-specific number: offer the guide it means on the makes that use it.
                let hints = diag::maker_hints(code);
                if let Some((_, g)) = hints.first() {
                    let makes: Vec<&str> = hints.iter().map(|(m, _)| *m).collect();
                    self.model.banner(
                        format!(
                            "{code} is a maker-specific number. On {} it means: {}. Check it means the same on this car.",
                            makes.join(", "),
                            g.title
                        ),
                        false,
                    );
                    Some(Diagnosis::new(g, Some(code)))
                } else {
                    None
                }
            }
        };
        match found {
            Some(d) => {
                self.cancel_measure();
                self.diag = Some(d);
                self.diag_open = None;
                self.diag_code_text = code.to_string();
                self.go(Page::Diagnose);
                self.measure_quick_tests();
            }
            None => self.model.banner(
                format!("Scana has no step-by-step guide for {code} yet. The Learn panel shows what it knows."),
                false,
            ),
        }
    }

    /// Opens a guide that starts from a symptom (no code).
    pub fn start_guide(&mut self, g: &'static diag::Guide) {
        self.cancel_measure();
        self.diag = Some(Diagnosis::new(g, None));
        self.diag_open = None;
        self.go(Page::Diagnose);
        self.measure_quick_tests();
    }

    /// On a connected car, reads what the quick tests need (codes, test results, readiness,
    /// the freeze frame) as soon as a guide opens, like the command line does. Live tests
    /// wait for the user: they need the engine held at a speed.
    fn measure_quick_tests(&mut self) {
        self.diag_wait.clear();
        if !self.model.connected() {
            return;
        }
        let Some(d) = &self.diag else { return };
        let quick: Vec<&'static Test> = d
            .guide
            .test
            .iter()
            .filter(|t| t.auto.as_ref().is_some_and(|a| a.kind != "live") && !d.answers.contains_key(&t.id))
            .collect();
        for t in quick {
            self.measure(t);
        }
    }

    /// Sees every update first: collects live samples for a measurement, and answers tests
    /// whose reads have arrived.
    pub fn diag_hook(&mut self, u: &Update) {
        if let (Update::Values(values), Some(m)) = (u, self.diag_measure.as_mut()) {
            for v in values.iter().filter(|v| v.ecu == m.ecu && m.pids.contains(&v.reading.pid)) {
                if let Some(x) = v.reading.decoded.as_ref().and_then(|d| d.main()).map(|q| q.value) {
                    m.latest.insert(v.reading.pid, x);
                }
            }
            let Some(d) = &self.diag else { return };
            let Some(auto) = d.test(&m.test).and_then(|t| t.auto.as_ref()) else { return };
            let need_ok =
                auto.need.as_ref().is_none_or(|n| n.pid().and_then(|p| m.latest.get(&p)).is_some_and(|v| n.holds(*v)));
            let hold_ok =
                auto.hold.as_ref().is_none_or(|h| h.pid().and_then(|p| m.latest.get(&p)).is_some_and(|v| h.holds(*v)));
            let t_ms = values.iter().map(|v| v.t_ms).max().unwrap_or(0);
            if need_ok && hold_ok {
                if let Some(prev) = m.last_good_ms {
                    m.good_s += (t_ms.saturating_sub(prev) as f64 / 1000.0).min(1.0);
                }
                m.last_good_ms = Some(t_ms);
                for v in values.iter().filter(|v| v.ecu == m.ecu && m.pids.contains(&v.reading.pid)) {
                    if let Some(x) = v.reading.decoded.as_ref().and_then(|d| d.main()).map(|q| q.value) {
                        m.samples.push(Sample { t_ms: v.t_ms, pid: v.reading.pid, value: x });
                    }
                }
            } else {
                m.last_good_ms = None;
            }
            if m.good_s >= f64::from(auto.seconds.unwrap_or(20)) {
                let result = diagnose::evaluate_live(auto, &m.samples);
                let test = m.test.clone();
                self.finish_measure(&test, result);
            }
        }
    }

    /// Answers a waiting test once its read arrived (called after the model is updated).
    pub fn diag_after_updates(&mut self) {
        if self.diag.is_none() {
            self.diag_wait.clear();
            return;
        }
        let mut done = Vec::new();
        for w in self.diag_wait.clone() {
            if let Some((test, measured)) = self.evaluate_wait(&w) {
                done.push(w);
                self.apply_measured(&test, measured);
            }
        }
        self.diag_wait.retain(|w| !done.contains(w));
    }

    /// A waiting test's measurement, once the read it waits for has arrived.
    fn evaluate_wait(&self, w: &DiagWait) -> Option<(String, Measured)> {
        let d = self.diag.as_ref()?;
        Some(match w {
            DiagWait::Codes(t) => {
                let sets = self.model.codes.as_ref()?;
                let codes: Vec<Dtc> =
                    sets.iter().flat_map(|s| s.stored.iter().chain(s.pending.iter()).copied()).collect();
                let a = d.test(t)?.auto.as_ref()?;
                (t.clone(), diagnose::evaluate_codes(a, &codes))
            }
            DiagWait::Tests(t) => {
                let r = self.model.tests.as_ref()?;
                let a = d.test(t)?.auto.as_ref()?;
                (t.clone(), diagnose::evaluate_mode06(a, r))
            }
            DiagWait::Readiness(t) => {
                let r = self.model.readiness.as_ref()?;
                let a = d.test(t)?.auto.as_ref()?;
                let statuses: Vec<_> = r.iter().map(|(_, s)| s.clone()).collect();
                (t.clone(), diagnose::evaluate_readiness(a, &statuses))
            }
            DiagWait::Freeze(t) => {
                let f = self.model.freeze.as_ref()?;
                let a = d.test(t)?.auto.as_ref()?;
                (t.clone(), diagnose::evaluate_freeze(a, f))
            }
        })
    }

    fn apply_measured(&mut self, test: &str, m: Measured) {
        let Some(d) = self.diag.as_mut() else { return };
        if let Some(r) = m.result.clone()
            && d.test(test).is_some_and(|t| t.result(&r).is_some())
        {
            d.answer(test, &r);
        }
        d.measured.insert(test.to_string(), m);
    }

    fn finish_measure(&mut self, test: &str, m: Measured) {
        if let Some(meas) = self.diag_measure.take() {
            self.model.selected = meas.saved_live.clone();
            self.session.send(Command::SetLive(meas.saved_live));
        }
        self.apply_measured(test, m);
    }

    pub fn cancel_measure(&mut self) {
        if let Some(meas) = self.diag_measure.take() {
            self.model.selected = meas.saved_live.clone();
            self.session.send(Command::SetLive(meas.saved_live));
        }
    }

    /// Asks Scana to measure a test on the connected car.
    fn measure(&mut self, t: &Test) {
        let Some(auto) = &t.auto else { return };
        match auto.kind.as_str() {
            "codes" => {
                self.diag_wait.push(DiagWait::Codes(t.id.clone()));
                self.model.codes = None;
                self.session.send(Command::ReadCodes);
            }
            "mode06" => {
                self.diag_wait.push(DiagWait::Tests(t.id.clone()));
                self.model.tests = None;
                self.session.send(Command::ReadTests);
            }
            "readiness" => {
                self.diag_wait.push(DiagWait::Readiness(t.id.clone()));
                self.model.readiness = None;
                self.session.send(Command::ReadReadiness);
            }
            "freeze" => {
                self.diag_wait.push(DiagWait::Freeze(t.id.clone()));
                self.model.freeze = None;
                self.session.send(Command::ReadFreeze);
            }
            "live" => {
                let Some(ecu) = self.model.conn.as_ref().and_then(|c| c.ecus.first()).map(|e| e.id) else { return };
                self.cancel_measure();
                let pids = diagnose::live_pids(auto);
                let saved = self.model.selected.clone();
                let live: Vec<(CanId, u8)> = pids.iter().map(|p| (ecu, *p)).collect();
                self.model.selected = live.clone();
                self.session.send(Command::SetLive(live));
                self.diag_measure = Some(DiagMeasure {
                    test: t.id.clone(),
                    ecu,
                    pids,
                    samples: vec![],
                    good_s: 0.0,
                    last_good_ms: None,
                    latest: Default::default(),
                    saved_live: saved,
                    started: Instant::now(),
                });
            }
            _ => {}
        }
    }

    /// For screenshot mode: measure a test as if its button were pressed.
    pub fn shot_measure(&mut self, t: &Test) {
        self.measure(t);
    }

    /// True if Scana can measure this test on the car connected now.
    fn can_measure(&self, t: &Test) -> bool {
        let Some(a) = &t.auto else { return false };
        if !self.model.connected() {
            return false;
        }
        match a.kind.as_str() {
            "live" => self
                .model
                .conn
                .as_ref()
                .and_then(|c| c.ecus.first())
                .is_some_and(|e| diagnose::live_supported(a, &e.supported)),
            _ => true,
        }
    }

    pub fn page_diagnose(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Diagnose",
            "Work through a fault code: the likely causes, and tests that tell them apart. Works without a car too: answer from what you see or another scan tool.",
        );
        let mut start: Option<Dtc> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("Code:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.diag_code_text).desired_width(80.0).hint_text("P0420"));
            let go = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add(crate::theme::accent_button(&pal, "Start")).clicked() || go {
                match Dtc::parse(&self.diag_code_text) {
                    Some(c) => start = Some(c),
                    None => self.model.banner("Type a fault code like P0420.", true),
                }
            }
            if let Some(sets) = &self.model.codes {
                let codes: Vec<Dtc> =
                    sets.iter().flat_map(|s| s.stored.iter().chain(s.pending.iter()).copied()).collect();
                if !codes.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new("This car:").color(pal.dim));
                    let make = self.model.make();
                    for c in codes {
                        let has =
                            diag::guide_for_make(c, make.as_deref()).is_some() || !diag::maker_hints(c).is_empty();
                        let b = egui::Button::new(RichText::new(c.to_string()).monospace());
                        if ui.add_enabled(has, b).on_disabled_hover_text("No guide for this code yet").clicked() {
                            start = Some(c);
                        }
                    }
                }
            }
        });
        if let Some(c) = start {
            self.start_diagnosis(c);
        }
        if self.diag.is_none() {
            ui.add_space(6.0);
            ui.label(RichText::new("No code? Start from what you notice:").strong());
            ui.horizontal_wrapped(|ui| {
                for g in diag::symptoms() {
                    if ui.button(&g.title).clicked() {
                        self.start_guide(g);
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Guides by code ({}):", diag::guides().len() - diag::symptoms().len()))
                        .strong(),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.diag_filter)
                        .hint_text("filter: misfire, P04, brake...")
                        .desired_width(220.0),
                );
            });
            let f = self.diag_filter.to_lowercase();
            let mut list: Vec<&'static diag::Guide> = diag::guides()
                .iter()
                .filter(|g| !(g.codes.is_empty() && g.maker_codes.is_empty()))
                .filter(|g| {
                    f.is_empty()
                        || g.title.to_lowercase().contains(&f)
                        || g.codes.iter().any(|c| c.to_lowercase().contains(&f))
                        || g.maker_codes.iter().any(|m| m.codes.iter().any(|c| c.to_lowercase().contains(&f)))
                })
                .collect();
            list.sort_by(|a, b| a.title.cmp(&b.title));
            for g in list {
                ui.horizontal_wrapped(|ui| {
                    if ui.link(&g.title).clicked() {
                        self.start_guide(g);
                    }
                    let mut codes = g.codes.join(", ");
                    for m in &g.maker_codes {
                        codes.push_str(&format!(
                            "{}{}: {}",
                            if codes.is_empty() { "" } else { "; " },
                            m.make,
                            m.codes.join(", ")
                        ));
                    }
                    ui.label(RichText::new(codes).small().color(pal.dim));
                });
            }
            return;
        }
        let Some(d) = self.diag.clone() else { return };
        let g = d.guide;
        ui.add_space(6.0);
        let code = d.code.map(|c| c.to_string()).unwrap_or_default();
        ui.label(RichText::new(format!("{code}  {}", g.title)).size(18.0).strong());
        self.linked(ui, &g.summary);
        // Codes on the car that should be fixed first.
        if let Some(sets) = &self.model.codes {
            let codes: Vec<Dtc> = sets.iter().flat_map(|s| s.stored.iter().chain(s.pending.iter()).copied()).collect();
            let first = d.fix_first_present(&codes);
            if !first.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "Fix first: {} on this car often cause {code}.",
                        first.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")
                    ))
                    .color(pal.note)
                    .strong(),
                );
            }
        }
        egui::CollapsingHeader::new("How it works").id_salt(("how", &g.id)).show(ui, |ui| {
            for p in &g.how_it_works {
                self.linked(ui, p);
            }
            if !g.terms.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Words:").color(pal.dim));
                    for t in &g.terms {
                        if ui.small_button(t).clicked() {
                            self.open_topic(Topic::Term(t.clone()));
                        }
                    }
                });
            }
        });
        if !g.safety.is_empty() {
            egui::CollapsingHeader::new(RichText::new("Safety").color(pal.note)).id_salt(("safety", &g.id)).show(
                ui,
                |ui| {
                    self.linked_bullets(ui, &g.safety, &[]);
                },
            );
        }
        if !g.note.is_empty() {
            egui::CollapsingHeader::new("Notes for particular makes").id_salt(("notes", &g.id)).show(ui, |ui| {
                for n in &g.note {
                    ui.label(
                        RichText::new(format!(
                            "{} {}{}",
                            n.make,
                            n.models.as_deref().unwrap_or(""),
                            n.years.as_deref().map(|y| format!(" ({y})")).unwrap_or_default()
                        ))
                        .strong(),
                    );
                    self.linked(ui, &n.text);
                    ui.label(RichText::new(format!("Source: {}", n.source)).small().color(pal.dim));
                }
            });
        }
        for (t, r) in d.stops() {
            ui.label(
                RichText::new(format!("Fix first ({}): {}", t.title, r.note.as_deref().unwrap_or(&r.text)))
                    .color(pal.bad),
            );
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Likely causes").size(16.0).strong());
        if d.answered() == 0 {
            ui.label(
                RichText::new(
                    "Before any tests: roughly how often each one is the cause. Each answer below moves them.",
                )
                .small()
                .color(pal.dim),
            );
        }
        let width = ui.available_width().min(760.0);
        for r in d.ranking() {
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(r.share as f32).desired_width(120.0).fill(pal.accent));
                ui.label(RichText::new(format!("{:>3.0} %", r.share * 100.0)).monospace());
                let label = ui.label(RichText::new(&r.cause.name).size(15.0));
                let mut tip = format!("{}\n\nFix: {}", r.cause.detail, r.cause.fix);
                if let Some(c) = &r.cause.cost {
                    tip.push_str(&format!("\nParts cost: {c}"));
                }
                if !r.why.is_empty() {
                    tip.push_str(&format!("\n\nMoved by:\n{}", r.why.join("\n")));
                }
                label.on_hover_text(tip);
            });
        }
        let _ = width;

        ui.add_space(8.0);
        ui.label(RichText::new("Tests").size(16.0).strong());
        ui.label(
            RichText::new("Do them in any order. Tests needing no tools come first; skip any you can't do.")
                .small()
                .color(pal.dim),
        );
        let next = d.next_test().map(|t| t.id.clone());
        let mut order: Vec<&Test> = g.test.iter().filter(|t| t.no_tools()).collect();
        order.extend(g.test.iter().filter(|t| !t.no_tools()));
        for t in order {
            self.test_card(ui, &d, t, next.as_deref() == Some(t.id.as_str()));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Save the diagnosis report").clicked() {
                self.save_diagnosis();
            }
            if ui.button("Start again").clicked() {
                self.cancel_measure();
                if let Some(d) = self.diag.as_mut() {
                    d.answers.clear();
                    d.measured.clear();
                }
            }
            if ui.button("Close").clicked() {
                self.cancel_measure();
                self.diag = None;
            }
        });
        ui.label(RichText::new("The shares are a guide to what to check first, not proof. Check the workshop manual before replacing parts.").small().color(pal.dim));
        ui.collapsing("Sources", |ui| {
            for s in &g.sources {
                ui.label(RichText::new(s).small());
            }
        });
    }

    fn test_card(&mut self, ui: &mut Ui, d: &Diagnosis, t: &Test, is_next: bool) {
        let pal = self.pal();
        let chosen = d.chosen(&t.id);
        let mark = if chosen.is_some() { "✔" } else { "○" };
        let mut title = RichText::new(format!("{mark}  {}", t.title));
        if chosen.is_some() {
            title = title.color(pal.good);
        } else if is_next {
            title = title.strong();
        }
        let tools: Vec<String> = t
            .needs
            .iter()
            .filter(|n| *n != "scan-tool")
            .map(|n| diag::tool(n).map_or(n.clone(), |x| x.name.clone()))
            .collect();
        let open = self.diag_open.as_deref() == Some(t.id.as_str()) || (self.diag_open.is_none() && is_next);
        let header =
            egui::CollapsingHeader::new(title).id_salt(("test", &d.guide.id, &t.id)).open(Some(open)).show(ui, |ui| {
                self.linked_look(ui, &t.why, crate::widgets::Look::dim(&pal), &[]);
                if !tools.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("Needs: {}.", tools.join(", "))).color(pal.note));
                        for n in t.needs.iter().filter(|n| *n != "scan-tool") {
                            if let Some(tool) = diag::tool(n) {
                                ui.label(RichText::new("What's that?").small().underline())
                                    .on_hover_text(format!("{}\n\n{}\n\nCost: {}", tool.what, tool.how, tool.cost));
                            }
                        }
                    });
                }
                for (i, s) in t.steps.iter().enumerate() {
                    self.linked(ui, &format!("{}. {s}", i + 1));
                }
                for s in &t.safety {
                    ui.label(RichText::new(format!("! {s}")).color(pal.note));
                }
                if self.can_measure(t) {
                    let measuring = self.diag_measure.as_ref().is_some_and(|m| m.test == t.id);
                    if measuring {
                        self.measure_status(ui, t);
                    } else if ui.button(RichText::new("Let Scana measure it").strong()).clicked() {
                        self.measure(t);
                    }
                } else if t.needs.iter().any(|n| n == "scan-tool") && !self.model.connected() {
                    ui.label(
                        RichText::new(
                            "Not connected: read this from another scan tool and choose the answer that matches.",
                        )
                        .small()
                        .color(pal.dim),
                    );
                }
                if let Some(m) = d.measured.get(&t.id) {
                    ui.label(RichText::new(format!("Scana measured: {}", m.text)).color(pal.accent));
                }
                ui.add_space(2.0);
                for r in &t.result {
                    let selected = chosen.is_some_and(|c| c.id == r.id);
                    if ui.radio(selected, &r.text).clicked() {
                        if let Some(dd) = self.diag.as_mut() {
                            if selected {
                                dd.clear(&t.id);
                            } else {
                                dd.answer(&t.id, &r.id);
                            }
                        }
                        self.diag_open = None;
                    }
                }
                if let Some(c) = chosen {
                    if let Some(n) = &c.note {
                        self.linked_look(
                            ui,
                            n,
                            crate::widgets::Look { color: Some(pal.note), ..Default::default() },
                            &[],
                        );
                    }
                    if let Some(to) = &c.goto
                        && let Some(other) = diag::guide(to)
                        && ui.link(format!("Open the guide: {}", other.title)).clicked()
                    {
                        self.start_guide(other);
                    }
                }
            });
        if header.header_response.clicked() {
            self.diag_open = if open { Some(String::new()) } else { Some(t.id.clone()) };
        }
    }

    fn measure_status(&mut self, ui: &mut Ui, t: &Test) {
        let pal = self.pal();
        let Some(auto) = &t.auto else { return };
        let Some(m) = &self.diag_measure else { return };
        let want = f64::from(auto.seconds.unwrap_or(20));
        let mut lines = Vec::new();
        if let Some(n) = &auto.need {
            match n.pid().and_then(|p| m.latest.get(&p)) {
                Some(v) if n.holds(*v) => {}
                Some(v) => lines.push(format!("{} (now {v:.0})", n.text)),
                None => lines.push(format!("{} (waiting for a reading)", n.text)),
            }
        }
        if let Some(h) = &auto.hold {
            match h.pid().and_then(|p| m.latest.get(&p)) {
                Some(v) if h.holds(*v) => {}
                Some(v) => lines.push(format!("{} (now {v:.0})", h.text)),
                None => lines.push(format!("{} (waiting for a reading)", h.text)),
            }
        }
        if lines.is_empty() {
            ui.label(RichText::new("Measuring: keep it steady.").color(pal.good));
        }
        for l in lines {
            ui.label(RichText::new(l).color(pal.note).strong());
        }
        let (good_s, slow) = (m.good_s, m.started.elapsed().as_secs() > 240);
        let mut stop = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::ProgressBar::new((good_s / want) as f32)
                    .desired_width(240.0)
                    .fill(pal.accent)
                    .text(format!("{good_s:.0} of {want:.0} s")),
            );
            stop = ui.button("Stop measuring").clicked();
        });
        if stop {
            self.cancel_measure();
        }
        if slow {
            ui.label(
                RichText::new("This is taking a while: check the conditions above, or answer by hand.")
                    .small()
                    .color(pal.dim),
            );
        }
    }

    fn save_diagnosis(&mut self) {
        let Some(d) = &self.diag else { return };
        let vehicle = self.model.conn.as_ref().map_or_else(String::new, |c| c.vehicle.clone());
        let when = scana_core::report::now_text();
        let dir = self.sessions_dir.join("diagnoses");
        let code = d.code.map(|c| c.to_string()).unwrap_or_else(|| d.guide.id.clone());
        let path = dir.join(format!("{}_{code}.html", when.replace([' ', ':'], "-")));
        let html = d.to_html(&vehicle, &when);
        let result = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, html));
        match result {
            Ok(()) => self.model.banner(format!("Diagnosis saved: {}", path.display()), false),
            Err(e) => self.model.banner(format!("Couldn't save the diagnosis in {}: {e}", dir.display()), true),
        }
    }
}
