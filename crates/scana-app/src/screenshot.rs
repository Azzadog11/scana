//! `scana-app --screenshots DIR`: drives the demo cars through every page and saves a PNG of
//! each, at 1366 x 768 (a common laptop screen) and once narrower, to check the layout reflows.
//! Uses default settings and writes sessions inside DIR, so it never touches the user's files.

use crate::app::{Page, ScanaApp, Topic};
use crate::settings::ThemeChoice;
use egui::{ColorImage, ViewportCommand};
use scana_core::session::{Command, Source};
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
            Topic(Some(crate::app::Topic::Term("Freeze frame".into()))),
            Wait(1.0),
            Shot("08-learn"),
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
            Read("tests"),
            Go(Page::Tests),
            Topic(None),
            Wait(3.0),
            Shot("12-tests"),
            OpenClear,
            Wait(0.8),
            Shot("13-clear-dialog"),
            CloseClear,
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
