//! Scana: the desktop app.
//!
//! ```text
//! scana-app                    start normally
//! scana-app --demo             connect to the demo car at once
//! scana-app --demo-faults      connect to the demo car with faults
//! scana-app --screenshots DIR  drive every page and save PNGs (for checking the layout)
//! ```

mod app;
mod chart;
mod names;
mod pages;
mod screenshot;
mod settings;
mod state;
mod theme;

use app::ScanaApp;
use scana_core::recorder;
use scana_core::session::{SessionHandle, Source, WorkerSettings};
use screenshot::ShotPlan;
use settings::Settings;
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "Scana: a laptop scan tool for cars with OBD-II on CAN

Usage: scana-app [--demo | --demo-faults] [--screenshots DIR]

  --demo             connect to the demo car (a simulated Hijet) at once
  --demo-faults      connect to the demo car with faults (key on, engine off)
  --screenshots DIR  drive every page with the demo cars and save PNGs in DIR
  --version          print the version
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut shots: Option<PathBuf> = None;
    let mut bridge_shots = false;
    let mut demo: Option<Source> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--demo" => demo = Some(Source::Demo),
            "--demo-faults" => demo = Some(Source::DemoFaults),
            "--screenshots" => match args.next() {
                Some(d) => shots = Some(PathBuf::from(d)),
                None => {
                    eprintln!("--screenshots needs a folder");
                    return ExitCode::FAILURE;
                }
            },
            "--shot-bridge" => match args.next() {
                Some(d) => {
                    shots = Some(PathBuf::from(d));
                    bridge_shots = true;
                }
                None => {
                    eprintln!("--shot-bridge needs a folder");
                    return ExitCode::FAILURE;
                }
            },
            "--version" => {
                println!("scana-app {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown option {other:?}\n\n{HELP}");
                return ExitCode::FAILURE;
            }
        }
    }

    let screenshot_mode = shots.is_some();
    let (settings, settings_msg) = if screenshot_mode { (Settings::default(), None) } else { Settings::load() };
    let sessions_dir = shots.as_ref().map_or_else(recorder::sessions_dir, |d| d.join("sessions"));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Scana")
            .with_app_id("scana")
            .with_inner_size([1366.0, 768.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Scana",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let worker = WorkerSettings {
                sessions_dir: sessions_dir.clone(),
                auto_record: settings.auto_record && !screenshot_mode,
            };
            let handle = SessionHandle::spawn(worker, move || ctx.request_repaint())?;
            let plan = shots.map(|d| if bridge_shots { ShotPlan::bridge(d) } else { ShotPlan::new(d) });
            let mut app = ScanaApp::new(handle, settings, !screenshot_mode, sessions_dir, plan);
            if let Some(m) = settings_msg {
                app.model.banner(m, true);
            }
            if let Some(src) = demo {
                app.connect(src);
            }
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Scana couldn't start its window: {e}");
            ExitCode::FAILURE
        }
    }
}
