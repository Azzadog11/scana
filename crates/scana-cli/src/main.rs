//! `scana`: the command line. Checks the bridge, watches the bus, finds the car, and reads
//! live values, fault codes and vehicle information. `--sim` uses a simulated Hijet.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use scana_core::describe::frame_meaning;
use scana_core::readers::{self, PidReading};
use scana_core::recorder::{self, Recorder};
use scana_core::report;
use scana_core::safety::{self, CLEAR_WARNING, ClearConsent};
use scana_core::session::{Command, LinkState, SessionHandle, Source, Update, WorkerSettings};
use scana_core::{Client, DetectOptions, Detected, FrameSink, detect};
use scana_link::serial::{SlcanLink, list_ports};
use scana_link::sim::SimLink;
use scana_link::{Link, LinkEvent, OpenMode};
use scana_proto::{Bitrate, CanId, Frame, candump, mode06, obd, pids};
use scana_sim::VirtualCar;
use std::fs::File;
use std::io::{BufRead, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "scana", version, about = "Scana: a laptop scan tool for cars with OBD-II on CAN")]
struct Cli {
    /// Use a simulated 2024 Hijet instead of the bridge.
    #[arg(long, global = true)]
    sim: bool,
    /// With --sim: add a dash display polling on the same bus.
    #[arg(long, global = true)]
    sim_dash: bool,
    /// With --sim: the demo car with faults (key on, engine off).
    #[arg(long, global = true)]
    sim_faults: bool,
    /// The bridge's serial port (default: find it).
    #[arg(long, global = true)]
    port: Option<String>,
    /// Write every frame to this file, in candump format.
    #[arg(long, global = true)]
    log: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List USB serial ports that might be a bridge.
    Ports,
    /// Check the bridge: version, counters and self-test. Doesn't touch the bus.
    Probe,
    /// Send frames through the CAN chip's internal loopback and check they come back.
    /// Nothing reaches the bus.
    Loopback {
        #[arg(long, default_value_t = 200)]
        count: u32,
    },
    /// Show frames on the bus in listen-only mode. Never sends anything.
    Monitor {
        #[arg(long, default_value = "500k")]
        bitrate: String,
        /// Stop after this many seconds (default: run until Ctrl-C).
        #[arg(long)]
        seconds: Option<u64>,
    },
    /// Find the car: bit rate, addressing and OBD ECUs.
    Detect {
        /// Only listen; never send a request.
        #[arg(long)]
        listen_only: bool,
    },
    /// List the live values the car supports, and read them.
    Pids {
        /// Read them this many times, once a second.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
    },
    /// Read stored, pending and permanent fault codes from every ECU.
    Codes,
    /// Read vehicle information: VIN, calibration ID, ECU name.
    Info,
    /// The freeze frame: the values when a code was stored.
    Freeze,
    /// On-board test results (mode 06), with their limits.
    Tests,
    /// The health check: light, codes, readiness, freeze frame, tests and key values. Saves a report.
    Health,
    /// Record a drive: polls every live value the car supports and saves a session folder
    /// (frames.log, values.csv, session.toml). Press Enter to stop. Reconnects by itself.
    Record {
        /// Stop after this many minutes (default: when you press Enter).
        #[arg(long)]
        minutes: Option<f64>,
        /// Which live values: "all" (default), or PIDs such as 0C,0D,05.
        #[arg(long, default_value = "all")]
        pids: String,
        /// Where to save the session (default: ~/.local/share/scana/sessions).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Clear fault codes (safety level 1): asks first, checks the engine is off, saves a report first.
    Clear {
        /// Skip the typed confirmation (you still get the checks).
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\nError: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Cmd::Ports = cli.cmd {
        return ports();
    }
    if let Cmd::Record { minutes, pids, dir } = &cli.cmd {
        // The session worker opens its own link.
        return record(source_of(&cli), *minutes, pids, dir.clone());
    }
    let link = open_link(&cli)?;
    let mut client = Client::new(link);
    if let Some(path) = &cli.log {
        let sink = CandumpSink::create(path)?;
        eprintln!("Logging frames to {}", path.display());
        client.set_sink(Box::new(sink));
    }
    match cli.cmd {
        Cmd::Ports => Ok(()),
        Cmd::Probe => probe(&mut client),
        Cmd::Loopback { count } => loopback(&mut client, count),
        Cmd::Monitor { bitrate, seconds } => monitor(&mut client, &bitrate, seconds),
        Cmd::Detect { listen_only } => {
            find_car(&mut client, !listen_only)?;
            Ok(())
        }
        Cmd::Pids { repeat } => read_all_pids(&mut client, repeat),
        Cmd::Codes => codes(&mut client),
        Cmd::Info => info(&mut client),
        Cmd::Freeze => freeze(&mut client),
        Cmd::Tests => tests(&mut client),
        Cmd::Health => health(&mut client).map(|_| ()),
        Cmd::Clear { yes } => clear(&mut client, yes),
        Cmd::Record { .. } => Ok(()),
    }
}

fn open_link(cli: &Cli) -> Result<Box<dyn Link>> {
    if cli.sim {
        let car = if cli.sim_faults {
            let mut c = VirtualCar::hijet_with_faults();
            c.set_engine_running(false);
            c
        } else if cli.sim_dash {
            VirtualCar::hijet().with_dash()
        } else {
            VirtualCar::hijet()
        };
        let link = SimLink::new(car);
        eprintln!("Using a {}", link.describe());
        return Ok(Box::new(link));
    }
    eprintln!("Connecting to the bridge (the Uno resets: about 2 s)...");
    let link = SlcanLink::connect(cli.port.as_deref())?;
    eprintln!("Connected: {}", link.describe());
    if !link.info().chip_ok {
        eprintln!("Warning: the CAN chip on the shield isn't answering. Is the shield pressed fully onto the Uno?");
    }
    Ok(Box::new(link))
}

fn source_of(cli: &Cli) -> Source {
    match (cli.sim, cli.sim_faults) {
        (true, true) => Source::DemoFaults,
        (true, false) => Source::Demo,
        _ => Source::Bridge { port: cli.port.clone() },
    }
}

/// Records a drive through the session worker, like the app's Record button with every value chosen.
fn record(source: Source, minutes: Option<f64>, pids_arg: &str, dir: Option<PathBuf>) -> Result<()> {
    let wanted: Option<Vec<u8>> = if pids_arg.eq_ignore_ascii_case("all") {
        None
    } else {
        let list: Result<Vec<u8>, _> = pids_arg.split(',').map(|p| u8::from_str_radix(p.trim(), 16)).collect();
        Some(list.with_context(|| format!("--pids: {pids_arg:?} isn't a list like 0C,0D,05"))?)
    };
    let sessions_dir = dir.unwrap_or_else(recorder::sessions_dir);
    let worker = WorkerSettings { sessions_dir: sessions_dir.clone(), auto_record: false };
    let h = SessionHandle::spawn(worker, || {})?;

    // Enter stops the recording; reading stdin happens on its own thread.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            // Only a real line stops it: with no terminal (input closed) it runs until --minutes or Ctrl-C.
            let mut line = String::new();
            if let Ok(n) = std::io::stdin().lock().read_line(&mut line)
                && n > 0
            {
                stop.store(true, Ordering::Relaxed);
            }
        });
    }
    let start = Instant::now();
    let deadline = minutes.map(|m| start + Duration::from_secs_f64(m.max(0.0) * 60.0));
    let finished = || stop.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d);

    println!(
        "Scana record: {}. Press Enter to stop{}.",
        source.label(),
        match minutes {
            Some(m) => format!(" (or it stops by itself after {m} min)"),
            None => String::new(),
        }
    );
    h.send(Command::Connect(source));
    let mut folder: Option<PathBuf> = None;
    let mut values: u64 = 0;
    let mut pids_seen = std::collections::BTreeSet::new();
    let mut last_status = Instant::now();
    let mut state = String::from("connecting");
    let mut stats: Option<scana_core::session::Stats> = None;
    while !finished() {
        let Some(u) = h.wait(Duration::from_millis(250)) else {
            if last_status.elapsed() >= Duration::from_secs(10) {
                last_status = Instant::now();
                status_line(start, &state, values, pids_seen.len(), stats.as_ref());
            }
            continue;
        };
        match u {
            Update::Connected(info) => {
                println!("Found: {}, {} computer(s): {}", info.protocol(), info.ecus.len(), info.vehicle);
                if info.other_tester {
                    println!("Another tester is polling (a dash display?): Scana leaves wider gaps.");
                }
                let live: Vec<(CanId, u8)> = info
                    .ecus
                    .iter()
                    .flat_map(|e| e.supported.iter().map(move |p| (e.id, *p)))
                    .filter(|(_, p)| !pids::is_support_pid(*p))
                    .filter(|(_, p)| wanted.as_ref().is_none_or(|w| w.contains(p)))
                    .collect();
                println!("Polling {} live values.", live.len());
                h.send(Command::SetLive(live));
                if folder.is_none() {
                    h.send(Command::Record(true));
                }
            }
            Update::Recording(Some(p)) => {
                println!("Recording to {}", p.display());
                println!("You can drive now. Leave the laptop alone until you are parked again.");
                folder = Some(p);
            }
            Update::Values(v) => {
                values += v.len() as u64;
                pids_seen.extend(v.iter().map(|x| (x.ecu, x.reading.pid)));
            }
            Update::State(s) => {
                state = match &s {
                    LinkState::Connected => "connected".into(),
                    LinkState::Connecting(t) | LinkState::Lost(t) | LinkState::Failed(t) => t.clone(),
                    LinkState::Disconnected => "disconnected".into(),
                };
                if !matches!(s, LinkState::Connected) {
                    println!("[{}] {state}", clock(start));
                }
            }
            Update::Stats(s) => stats = Some(s),
            Update::Message(m) => println!("[{}] {m}", clock(start)),
            Update::Error(e) => println!("[{}] Problem: {e}", clock(start)),
            _ => {}
        }
        if last_status.elapsed() >= Duration::from_secs(10) {
            last_status = Instant::now();
            status_line(start, &state, values, pids_seen.len(), stats.as_ref());
        }
    }
    println!("Stopping...");
    h.send(Command::Record(false));
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(Update::Recording(None)) = h.wait(Duration::from_millis(200)) {
            break;
        }
    }
    h.send(Command::Disconnect);
    drop(h);
    match folder {
        Some(f) => println!("Saved {values} values from {} PIDs in {}", pids_seen.len(), f.display()),
        None => println!("Nothing was recorded: the car never answered."),
    }
    Ok(())
}

fn clock(start: Instant) -> String {
    let s = start.elapsed().as_secs();
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn status_line(start: Instant, state: &str, values: u64, pids: usize, stats: Option<&scana_core::session::Stats>) {
    let rate = stats.map_or(String::new(), |s| format!(", {:.0} requests/s", s.requests_per_s));
    println!("[{}] {state}: {values} values from {pids} PIDs{rate}", clock(start));
}

fn ports() -> Result<()> {
    let ports = list_ports();
    if ports.is_empty() {
        println!("No USB serial ports found. Is the Arduino plugged in?");
        return Ok(());
    }
    println!("{:<16} {:<10} {:<8} Product", "Port", "USB ID", "Bridge?");
    for p in ports {
        println!(
            "{:<16} {:04x}:{:04x}  {:<8} {}",
            p.path,
            p.vid,
            p.pid,
            if p.likely_bridge { "likely" } else { "" },
            p.product.unwrap_or_default()
        );
    }
    Ok(())
}

fn probe(client: &mut Client) -> Result<()> {
    let link = client.link();
    println!("Link:      {}", link.describe());
    match link.self_test()? {
        Some(Ok(())) => println!("Self-test: PASS (the CAN chip answers, registers work, a loopback frame came back)"),
        Some(Err((step, detail))) => {
            println!("Self-test: FAIL at step {step} (read {detail:02X})");
            println!("           {}", self_test_hint(step, detail));
        }
        None => println!("Self-test: not available on this link"),
    }
    if let Some(h) = link.health()? {
        println!(
            "Counters:  up {} s, frames in {}, out {}, send errors {}, chip overruns {}, bad commands {}",
            h.uptime_s, h.rx, h.tx, h.tx_errors, h.hw_overruns, h.bad_commands
        );
        println!("Bus:       mode {}, TEC {}, REC {}, EFLG {:02X}", mode_name(h.mode), h.tec, h.rec, h.eflg);
    }
    Ok(())
}

fn self_test_hint(step: u8, detail: u8) -> &'static str {
    match (step, detail) {
        (1, 0x00 | 0xFF) => "Nothing answers on SPI: check the shield is pressed fully onto the Uno.",
        (1, _) => "The chip answered but isn't in configuration mode after a reset.",
        (2, _) => "Register write and read-back failed: an SPI wiring or chip problem.",
        (3, _) => "The loopback frame wasn't sent: the chip didn't enter loopback mode or didn't transmit.",
        (4, _) => "The loopback frame never came back.",
        _ => "The loopback frame came back changed.",
    }
}

fn mode_name(c: char) -> &'static str {
    match c {
        'C' => "closed",
        'N' => "normal",
        'L' => "listen-only",
        'K' => "loopback",
        _ => "unknown",
    }
}

fn loopback(client: &mut Client, count: u32) -> Result<()> {
    let link = client.link();
    link.set_bitrate(Bitrate::K500)?;
    link.open(OpenMode::Loopback)?;
    println!("Loopback: {count} frames through the CAN chip (nothing reaches the bus)...");
    let mut lat = Vec::with_capacity(count as usize);
    let (mut lost, mut wrong) = (0u32, 0u32);
    let start = Instant::now();
    for i in 0..count {
        let frame = test_frame(i)?;
        let t0 = Instant::now();
        link.send(&frame)?;
        let deadline = t0 + Duration::from_millis(200);
        let mut got = None;
        while got.is_none() && Instant::now() < deadline {
            if let Some(LinkEvent::Frame(tf)) = link.recv(deadline.saturating_duration_since(Instant::now()))? {
                got = Some(tf.frame);
            }
        }
        match got {
            Some(f) if f == frame => lat.push(t0.elapsed()),
            Some(f) => {
                wrong += 1;
                println!("  frame {i}: sent {frame}, got back {f}");
            }
            None => {
                lost += 1;
                println!("  frame {i}: sent {frame}, nothing came back");
            }
        }
    }
    let total = start.elapsed();
    link.close()?;
    let ok = lat.len();
    lat.sort();
    let avg = lat.iter().sum::<Duration>().checked_div(ok.max(1) as u32).unwrap_or_default();
    println!("Back OK:   {ok} of {count}   lost {lost}   changed {wrong}");
    if let (Some(min), Some(max)) = (lat.first(), lat.last()) {
        let p99 = lat[(ok * 99 / 100).min(ok - 1)];
        println!(
            "Round trip: min {:.1} ms, average {:.1} ms, 99% under {:.1} ms, max {:.1} ms",
            ms(*min),
            ms(avg),
            ms(p99),
            ms(*max)
        );
    }
    println!("Rate:      {:.0} frames a second, one at a time", f64::from(count) / total.as_secs_f64());
    if let Some(h) = link.health()? {
        println!(
            "Bridge:    frames in {}, out {}, send errors {}, bad commands {}",
            h.rx, h.tx, h.tx_errors, h.bad_commands
        );
    }
    if lost + wrong > 0 {
        bail!("{} frames failed the loopback test", lost + wrong);
    }
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Frames that exercise both ID sizes, every length, remote requests and varied data.
fn test_frame(i: u32) -> Result<Frame> {
    let len = (i % 9) as u8;
    let id = if i % 3 == 2 {
        CanId::Extended(0x18DA_00F1 ^ (i.wrapping_mul(977) & 0x1FFF_FFFF))
    } else {
        CanId::Standard((0x100 + i.wrapping_mul(13)) as u16 & 0x7FF)
    };
    if i % 17 == 16 {
        return Ok(Frame::remote(id, len)?);
    }
    let data: Vec<u8> = (0..len).map(|k| (i.wrapping_mul(31) + u32::from(k) * 7) as u8).collect();
    Ok(Frame::new(id, &data)?)
}

fn monitor(client: &mut Client, bitrate: &str, seconds: Option<u64>) -> Result<()> {
    let b = Bitrate::parse(bitrate).with_context(|| format!("unknown bit rate {bitrate:?}; try 500k or 250k"))?;
    client.link().set_bitrate(b)?;
    client.link().open(OpenMode::ListenOnly)?;
    println!("Listening at {b} (listen-only: Scana sends nothing). Ctrl-C to stop.");
    println!("{:>9}  {:<8} {:<3} {:<24} Meaning", "Time s", "ID", "Len", "Data");
    let start = client.now_ms();
    let end = seconds.map(|s| start + s * 1000);
    let mut frames = 0u64;
    loop {
        let now = client.now_ms();
        if end.is_some_and(|e| now >= e) {
            break;
        }
        match client.link().recv(Duration::from_millis(250))? {
            Some(LinkEvent::Frame(tf)) => {
                frames += 1;
                let data: Vec<String> = tf.frame.data().iter().map(|b| format!("{b:02X}")).collect();
                println!(
                    "{:>9.3}  {:<8} {:<3} {:<24} {}",
                    (tf.at_ms - start) as f64 / 1000.0,
                    tf.frame.id.to_string(),
                    tf.frame.len(),
                    data.join(" "),
                    frame_meaning(&tf.frame)
                );
            }
            Some(LinkEvent::Bus(e)) => println!("           bus event: {e:?}"),
            Some(LinkEvent::BridgeReady { .. }) => bail!("the bridge restarted"),
            None => {}
        }
    }
    client.link().close()?;
    println!("{frames} frames in {:.1} s", (client.now_ms() - start) as f64 / 1000.0);
    Ok(())
}

fn find_car(client: &mut Client, transmit: bool) -> Result<Detected> {
    println!("Looking for the car{}...", if transmit { "" } else { " (listen only)" });
    let opts = DetectOptions { transmit, ..DetectOptions::default() };
    let d = detect(client, &opts)?;
    for line in &d.log {
        println!("  {line}");
    }
    println!(
        "Found: {}, {}, ECU{} {}",
        d.bitrate,
        d.addressing.name(),
        if d.ecus.len() == 1 { "" } else { "s" },
        d.ecus.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", ")
    );
    if d.other_tester {
        println!("Note: another tester is polling the car (a dash display?). Scana slows down to share the bus.");
    }
    Ok(d)
}

fn read_all_pids(client: &mut Client, repeat: u32) -> Result<()> {
    let d = find_car(client, true)?;
    for &ecu in &d.ecus {
        let supported = readers::supported_pids(client, ecu)?;
        let live: Vec<u8> = supported.iter().copied().filter(|p| !pids::is_support_pid(*p)).collect();
        println!("\nECU {ecu}: {} live values supported", live.len());
        for round in 0..repeat.max(1) {
            let t0 = client.now_ms();
            let readings = readers::read_pids(client, ecu, &live)?;
            if round > 0 {
                println!();
            }
            print_readings(&readings);
            println!("(read in {} ms)", client.now_ms() - t0);
            if round + 1 < repeat {
                client.idle_until(t0 + 1000)?;
            }
        }
    }
    Ok(())
}

fn print_readings(readings: &[PidReading]) {
    println!("{:<4} {:<44} Value", "PID", "Name");
    for r in readings {
        println!("{:02X}   {:<44} {}", r.pid, r.name, r.display());
    }
}

fn codes(client: &mut Client) -> Result<()> {
    let d = find_car(client, true)?;
    for &ecu in &d.ecus {
        if let Ok(r) = readers::read_pids(client, ecu, &[0x01])
            && let Some(dec) = r.first().and_then(|x| x.decoded.as_ref())
        {
            println!("ECU {ecu}: {}", dec.text.clone().unwrap_or_default());
        }
    }
    for (service, label) in
        [(obd::STORED_CODES, "Stored"), (obd::PENDING_CODES, "Pending"), (obd::PERMANENT_CODES, "Permanent")]
    {
        let per_ecu = readers::read_codes(client, service)?;
        for (ecu, codes) in per_ecu {
            if codes.is_empty() {
                println!("{label:<10} {ecu}: none");
            } else {
                for c in codes {
                    let kind = if c.is_manufacturer_specific() { "maker-specific" } else { "generic" };
                    println!("{label:<10} {ecu}: {c}  ({}, {kind})", c.system().name());
                }
            }
        }
    }
    println!("(Descriptions of each code come in stage 2.)");
    Ok(())
}

fn info(client: &mut Client) -> Result<()> {
    let d = find_car(client, true)?;
    for &ecu in &d.ecus {
        let i = readers::vehicle_info(client, ecu)?;
        println!("\nECU {ecu}");
        println!("  VIN:            {}", i.vin.as_deref().unwrap_or("not reported"));
        println!("  Calibration ID: {}", i.calibration_id.as_deref().unwrap_or("not reported"));
        println!("  ECU name:       {}", i.ecu_name.as_deref().unwrap_or("not reported"));
    }
    Ok(())
}

fn ecus_with_pids(client: &mut Client, d: &Detected) -> Result<Vec<(CanId, Vec<u8>)>> {
    d.ecus.iter().map(|e| Ok((*e, readers::supported_pids(client, *e)?))).collect()
}

fn freeze(client: &mut Client) -> Result<()> {
    let d = find_car(client, true)?;
    for &ecu in &d.ecus {
        match readers::freeze_frame(client, ecu)? {
            None => println!("ECU {ecu}: no freeze frame stored."),
            Some(ff) => {
                println!("\nECU {ecu}: freeze frame stored by {}", ff.dtc);
                print_readings(&ff.readings);
            }
        }
    }
    Ok(())
}

fn tests(client: &mut Client) -> Result<()> {
    let d = find_car(client, true)?;
    for &ecu in &d.ecus {
        let results = readers::test_results(client, ecu)?;
        println!("\nECU {ecu}: {} test results", results.len());
        for t in results {
            println!(
                "  {:<42} {:<40} {:>10.3} {:<6} limits {:.3} to {:.3}  {}",
                mode06::monitor_name(t.mid),
                mode06::test_name(t.mid, t.tid),
                t.value,
                t.unit,
                t.min,
                t.max,
                if t.passed() { "pass" } else { "FAIL" }
            );
        }
    }
    Ok(())
}

/// Runs the health check, prints the summary and saves the report in a session folder.
fn health(client: &mut Client) -> Result<PathBuf> {
    let d = find_car(client, true)?;
    let ecus = ecus_with_pids(client, &d)?;
    let profile = ecus.first().and_then(|e| scana_db::profile::find_match(&e.1));
    let vehicle = profile.map_or("Unknown car".to_string(), |p| p.name.clone());
    let protocol = format!("ISO 15765-4 CAN, {}, {}", d.addressing.name(), d.bitrate);
    let report = report::run_health_check(client, &ecus, profile, &vehicle, &protocol)?;
    println!("\nHealth check: {vehicle}");
    for (tone, line) in report.summary() {
        let mark = match tone {
            report::Tone::Good => "ok  ",
            report::Tone::Note => "note",
            report::Tone::Bad => "BAD ",
        };
        println!("  {mark} {line}");
    }
    let now = client.now_ms();
    let mut rec = Recorder::start(&recorder::sessions_dir(), &vehicle, &client.link().describe(), now)?;
    rec.summary.codes = report.all_codes();
    let path = rec.write_file("report.html", &report.to_html())?;
    rec.finish()?;
    println!("Report saved: {}", path.display());
    Ok(path)
}

fn clear(client: &mut Client, yes: bool) -> Result<()> {
    println!("Clearing codes also erases:");
    for line in CLEAR_WARNING {
        println!("  - {line}");
    }
    if !yes {
        print!("Type CLEAR to continue, anything else to stop: ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if answer.trim() != "CLEAR" {
            println!("Stopped: nothing was sent.");
            return Ok(());
        }
    }
    println!("Saving a health report first...");
    health(client)?;
    let d = find_car(client, true)?;
    let ecus = ecus_with_pids(client, &d)?;
    let out = safety::clear_codes(client, &ecus, ClearConsent::confirmed_by_user())?;
    for e in &out.cleared {
        println!("ECU {e}: codes cleared.");
    }
    for (e, why) in &out.refused {
        println!("ECU {e}: refused: {why}.");
    }
    for (e, codes) in &out.codes_after {
        println!("ECU {e}: {} stored code(s) now.", codes.len());
    }
    Ok(())
}

/// Writes every frame to a candump log, flushed at least once a second.
struct CandumpSink {
    out: BufWriter<File>,
    epoch_us: u64,
    last_flush: Instant,
}

impl CandumpSink {
    fn create(path: &PathBuf) -> Result<Self> {
        let f = File::create(path).with_context(|| format!("can't create {}", path.display()))?;
        let epoch_us = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0);
        Ok(CandumpSink { out: BufWriter::new(f), epoch_us, last_flush: Instant::now() })
    }
}

impl FrameSink for CandumpSink {
    fn frame(&mut self, frame: &Frame, at_ms: u64, _outgoing: bool) {
        let _ = writeln!(self.out, "{}", candump::format_line(frame, self.epoch_us + at_ms * 1000, "can0"));
        if self.last_flush.elapsed() >= Duration::from_secs(1) {
            let _ = self.out.flush();
            self.last_flush = Instant::now();
        }
    }
}

impl Drop for CandumpSink {
    fn drop(&mut self) {
        let _ = self.out.flush();
    }
}
