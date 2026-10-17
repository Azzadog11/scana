//! `scana-app --screenshots DIR`: drives the demo cars through every page and saves a PNG of
//! each, at 1366 x 768 (a common laptop screen) and once narrower, to check the layout reflows.
//! Uses default settings and writes sessions inside DIR, so it never touches the user's files.

use crate::app::{Page, ScanaApp, Topic};
use crate::settings::ThemeChoice;
use egui::{ColorImage, ViewportCommand};
use scana_core::Channel;
use scana_core::modules::DataKind;
use scana_core::session::{Command, Source, WatchSpec};
use scana_proto::{CanId, Dtc};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

type Slot = Arc<Mutex<Option<Arc<ColorImage>>>>;

#[derive(Clone)]
enum Act {
    Size(f32, f32),
    Connect(Source),
    Go(Page),
    Topic(Option<Topic>),
    Read(&'static str),
    OpenClear,
    CloseClear,
    Theme(ThemeChoice),
    Record(bool),
    LoadNewestSession,
    WaitConnected,
    WaitHealth,
    WaitCodes,
    Wait(f64),
    Shot(&'static str),
    FindModules,
    WaitDiscovery,
    SelectModule(&'static str),
    /// Scan a module's data IDs in the explorer: address, kind, from, to.
    Explore(&'static str, DataKind, u16, u16),
    WaitScan,
    Watch,
    SelectByte(u16, usize),
    Advanced(bool),
    Console(&'static str),
    Diagnose(&'static str),
    Answer(&'static str, &'static str),
    MeasureTest(&'static str),
    /// Wait until the diagnosis's live measurement (if any) has finished.
    WaitMeasured,
    OpenTest(&'static str),
    CloseDiag,
    Lookup(&'static str),
    /// Type in the sidebar's search box.
    Search(&'static str),
}

pub struct ShotPlan {
    dir: PathBuf,
    steps: Vec<Act>,
    idx: usize,
    started: Option<Instant>,
    pending: Option<Slot>,
}

impl ShotPlan {
    pub fn new(dir: PathBuf) -> Self {
        use Act::*;
        let ecm = CanId::Standard(0x7E8);
        let steps = vec![
            Size(1366.0, 768.0),
            Wait(1.5),
            Shot("01-home-welcome"),
            Diagnose("P0420"),
            Answer("leak-check", "none"),
            Answer("age", "high"),
            OpenTest("rear-compare"),
            Wait(1.0),
            Shot("21-diagnose-no-car"),
            CloseDiag,
            Connect(Source::Demo),
            WaitConnected,
            Record(true),
            Wait(9.0),
            Go(Page::Home),
            Wait(0.8),
            Shot("02-home-connected"),
            Go(Page::Live),
            Topic(Some(crate::app::Topic::Pid(ecm, 0x05))),
            Wait(1.0),
            Shot("03-live"),
            Go(Page::Graphs),
            Topic(None),
            Wait(1.0),
            Shot("04-graphs"),
            Read("readiness"),
            Go(Page::Readiness),
            Wait(2.0),
            Shot("05-readiness"),
            Read("info"),
            Go(Page::Info),
            Wait(2.5),
            Shot("06-info"),
            Go(Page::Monitor),
            Wait(1.0),
            Shot("07-monitor"),
            Go(Page::Learn),
            Topic(Some(crate::app::Topic::Library("fuel-trim".into()))),
            Wait(1.0),
            Shot("08-library"),
            Go(Page::Glossary),
            Topic(Some(crate::app::Topic::Term("Freeze frame".into()))),
            Wait(1.0),
            Shot("34-glossary"),
            Go(Page::Live),
            Search("vacuum"),
            Topic(Some(crate::app::Topic::Library("pattern-vacuum-leak".into()))),
            Wait(1.0),
            Shot("35-search"),
            Search(""),
            Go(Page::Monitor),
            Topic(Some(crate::app::Topic::Library("reading-bus-monitor".into()))),
            Wait(1.0),
            Shot("36-monitor-topic"),
            // A maker-specific code on the Hijet (a Daihatsu): the make's meaning shows.
            Topic(Some(crate::app::Topic::Dtc(Dtc(0x1603)))),
            Wait(1.0),
            Shot("37-maker-code"),
            Lookup("P0420"),
            Topic(None),
            Wait(1.0),
            Shot("23-learn-lookup"),
            Lookup(""),
            Go(Page::Diagnose),
            Wait(1.0),
            Shot("25-diagnose-list"),
            Diagnose("C0200"),
            Wait(1.0),
            Shot("24-diagnose-maker-code"),
            CloseDiag,
            Go(Page::Settings),
            Topic(None),
            Wait(0.8),
            Shot("09-settings"),
            Record(false),
            Wait(1.0),
            Connect(Source::DemoFaults),
            WaitConnected,
            Read("health"),
            Go(Page::Health),
            WaitHealth,
            Wait(1.0),
            Shot("10-health"),
            Read("codes"),
            Read("freeze"),
            Go(Page::Codes),
            WaitCodes,
            Topic(Some(crate::app::Topic::Dtc(Dtc(0x0128)))),
            Wait(1.0),
            Shot("11-codes"),
            Topic(None),
            Diagnose("P0171"),
            MeasureTest("other-codes"),
            Wait(2.5),
            Answer("listen", "none"),
            Answer("both-banks", "inline"),
            OpenTest("trim-idle"),
            Wait(1.0),
            Shot("22-diagnose-lean"),
            CloseDiag,
            Read("tests"),
            Go(Page::Tests),
            Topic(None),
            Wait(3.0),
            Shot("12-tests"),
            OpenClear,
            Wait(0.8),
            Shot("13-clear-dialog"),
            CloseClear,
            FindModules,
            Go(Page::Modules),
            Wait(5.0),
            Shot("18-modules-searching"),
            WaitDiscovery,
            SelectModule("7C0"),
            Topic(Some(crate::app::Topic::Dtc(Dtc(0xC100)))),
            Wait(1.0),
            Shot("19-modules"),
            Topic(None),
            Explore("750/40", DataKind::Lid, 0x00, 0x10),
            WaitScan,
            Watch,
            Wait(14.0),
            SelectByte(1, 0),
            Advanced(true),
            Console("21 02"),
            Wait(1.5),
            Shot("20-explorer"),
            Advanced(false),
            Go(Page::Home),
            Go(Page::Sessions),
            Wait(1.0),
            Shot("14-sessions"),
            LoadNewestSession,
            Wait(1.0),
            Shot("17-session-graphs"),
            Theme(ThemeChoice::HighContrast),
            Go(Page::Live),
            Wait(1.5),
            Shot("15-live-high-contrast"),
            Theme(ThemeChoice::Dark),
            Size(1024.0, 700.0),
            Wait(1.5),
            Shot("16-live-narrow"),
            // The simulated 2006 Corolla with P0420: an older car with no profile.
            Size(1366.0, 768.0),
            Connect(Source::DemoCorolla),
            WaitConnected,
            Go(Page::Home),
            Wait(1.0),
            Shot("26-corolla-home"),
            Read("health"),
            Go(Page::Health),
            WaitHealth,
            Wait(1.0),
            Shot("27-corolla-health"),
            Read("codes"),
            Read("freeze"),
            Go(Page::Codes),
            WaitCodes,
            Topic(Some(crate::app::Topic::Dtc(Dtc(0x0420)))),
            Wait(1.0),
            Shot("28-corolla-codes"),
            Topic(None),
            Read("tests"),
            Go(Page::Tests),
            Wait(3.0),
            Shot("29-corolla-tests"),
            Go(Page::Live),
            Topic(Some(crate::app::Topic::Pid(ecm, 0x3C))),
            Wait(2.0),
            Shot("30-corolla-live"),
            Topic(None),
            // The quick tests (other codes, the catalyst test result) are read when the guide opens.
            Diagnose("P0420"),
            Wait(3.0),
            Answer("age", "high"),
            MeasureTest("rear-compare"),
            OpenTest("rear-compare"),
            Wait(3.0),
            Shot("31-corolla-diagnose-measuring"),
            WaitMeasured,
            MeasureTest("rear-level"),
            WaitMeasured,
            OpenTest("rear-level"),
            Wait(1.5),
            Shot("32-corolla-diagnose-result"),
            CloseDiag,
        ];
        ShotPlan { dir, steps, idx: 0, started: None, pending: None }
    }

    /// The real bridge with no car: the app should say it is looking, and keep trying.
    pub fn bridge(dir: PathBuf) -> Self {
        use Act::*;
        let steps = vec![
            Size(1366.0, 768.0),
            Connect(Source::Bridge { port: None }),
            Wait(9.0),
            Shot("bridge-1-home"),
            Go(Page::Monitor),
            Wait(1.0),
            Shot("bridge-2-monitor"),
        ];
        ShotPlan { dir, steps, idx: 0, started: None, pending: None }
    }
}

/// Runs one step a frame. Returns `None` when finished (the window is closed).
pub fn step(app: &mut ScanaApp, mut plan: ShotPlan, ctx: &egui::Context) -> Option<ShotPlan> {
    ctx.request_repaint();
    let Some(act) = plan.steps.get(plan.idx).cloned() else {
        ctx.send_viewport_cmd(ViewportCommand::Close);
        return None;
    };
    let elapsed = plan.started.get_or_insert_with(Instant::now).elapsed().as_secs_f64();
    let done = match act {
        Act::Size(w, h) => {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(w, h)));
            true
        }
        Act::Connect(s) => {
            app.connect(s);
            true
        }
        Act::Go(p) => {
            app.go(p);
            true
        }
        Act::Topic(t) => {
            app.topic = t;
            true
        }
        Act::Read(what) => {
            app.session.send(match what {
                "codes" => Command::ReadCodes,
                "freeze" => Command::ReadFreeze,
                "readiness" => Command::ReadReadiness,
                "tests" => Command::ReadTests,
                "info" => Command::ReadInfo,
                _ => Command::HealthCheck,
            });
            true
        }
        Act::OpenClear => {
            app.clear_dialog = Some(false);
            true
        }
        Act::CloseClear => {
            app.clear_dialog = None;
            true
        }
        Act::Theme(t) => {
            app.settings.theme = t;
            true
        }
        Act::Record(on) => {
            app.set_record(on);
            true
        }
        Act::LoadNewestSession => {
            let list = scana_core::store::list_sessions(&app.sessions_dir);
            if let Some(s) = list.iter().find(|s| s.summary.as_ref().is_some_and(|x| x.values > 0))
                && let Ok(l) = scana_core::store::load_values(&s.dir)
            {
                app.loaded_selected = l.series.keys().take(3).cloned().collect();
                app.loaded = Some((s.name.clone(), l));
                app.graph_window_s = f64::INFINITY;
                app.go(Page::Graphs);
            }
            true
        }
        Act::FindModules => {
            app.session.send(Command::FindModules);
            true
        }
        Act::WaitDiscovery => (app.model.discovery.is_some() && app.model.busy.is_none()) || elapsed > 120.0,
        Act::SelectModule(label) => {
            app.module_selected = Channel::parse(label);
            true
        }
        Act::Explore(label, kind, from, to) => {
            if let Some(ch) = Channel::parse(label) {
                app.open_explorer(ch, kind);
                app.explore_from = format!("{from:02X}");
                app.explore_to = format!("{to:02X}");
                app.session.send(Command::ScanData { channel: ch, kind, from, to });
            }
            true
        }
        Act::WaitScan => (!app.model.explore.items.is_empty() && app.model.busy.is_none()) || elapsed > 30.0,
        Act::Watch => {
            if let Some(ch) = app.explore_channel {
                let ids = app.model.explore.items.keys().copied().collect();
                app.session.send(Command::Watch(Some(WatchSpec { channel: ch, kind: app.explore_kind, ids })));
                app.explore_watch = true;
            }
            true
        }
        Act::SelectByte(id, byte) => {
            app.explore_byte = Some((id, byte));
            true
        }
        Act::Advanced(on) => {
            app.settings.advanced = on;
            true
        }
        Act::Console(hex) => {
            if let (Some(ch), Some(req)) = (app.explore_channel, scana_proto::uds::parse_hex(hex)) {
                app.console_text = hex.into();
                app.session.send(Command::Console { channel: ch, request: req });
            }
            true
        }
        Act::Diagnose(code) => {
            if let Some(c) = Dtc::parse(code) {
                app.start_diagnosis(c);
            }
            true
        }
        Act::Answer(test, result) => {
            if let Some(d) = app.diag.as_mut() {
                d.answer(test, result);
            }
            true
        }
        Act::MeasureTest(test) => {
            if let Some(t) = app.diag.as_ref().and_then(|d| d.test(test)) {
                app.shot_measure(t);
            }
            true
        }
        Act::WaitMeasured => app.diag_measure.is_none() || elapsed > 200.0,
        Act::OpenTest(test) => {
            app.diag_open = Some(test.to_string());
            true
        }
        Act::Search(text) => {
            app.search_text = text.to_string();
            true
        }
        Act::Lookup(text) => {
            app.glossary_filter = text.to_string();
            app.go(Page::Learn);
            true
        }
        Act::CloseDiag => {
            app.cancel_measure();
            app.diag = None;
            true
        }
        Act::WaitConnected => (app.model.connected() && elapsed > 0.5) || elapsed > 20.0,
        Act::WaitHealth => app.model.health.is_some() || elapsed > 40.0,
        Act::WaitCodes => (app.model.codes.is_some() && app.model.freeze.is_some()) || elapsed > 20.0,
        Act::Wait(s) => elapsed >= s,
        Act::Shot(name) => match plan.pending.take() {
            None => {
                let slot: Slot = Arc::new(Mutex::new(None));
                let s2 = slot.clone();
                let c2 = ctx.clone();
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    if let Ok(mut g) = s2.lock() {
                        *g = Some(img);
                    }
                    c2.request_repaint();
                })));
                plan.pending = Some(slot);
                false
            }
            Some(slot) => {
                let img = slot.lock().ok().and_then(|mut g| g.take());
                match img {
                    Some(img) => {
                        let path = plan.dir.join(format!("{name}.png"));
                        match save_png(&path, &img) {
                            Ok(()) => eprintln!("saved {}", path.display()),
                            Err(e) => eprintln!("couldn't save {}: {e}", path.display()),
                        }
                        true
                    }
                    None if elapsed > 10.0 => {
                        eprintln!("no screenshot came back for {name}");
                        true
                    }
                    None => {
                        plan.pending = Some(slot);
                        false
                    }
                }
            }
        },
    };
    if done {
        plan.idx += 1;
        plan.started = None;
    }
    Some(plan)
}

fn save_png(path: &Path, img: &ColorImage) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    let data: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    writer.write_image_data(&data).map_err(|e| e.to_string())
}
