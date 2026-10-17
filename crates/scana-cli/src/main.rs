//! `scana`: the command line. Checks the bridge, watches the bus, finds the car, and reads
//! live values, fault codes and vehicle information; finds and reads every module (stage 3).
//! `--sim` uses a simulated Hijet.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use scana_core::describe::frame_meaning;
use scana_core::modules::{self, DataKind, DiscoverOptions, ModuleInfo};
use scana_core::readers::{self, PidReading};
use scana_core::recorder::{self, Recorder};
use scana_core::report;
use scana_core::safety::{self, CLEAR_WARNING, ClearConsent};
use scana_core::session::{Command, LinkState, SessionHandle, Source, Update, WorkerSettings};
use scana_core::{Channel, Client, DetectOptions, Detected, FrameSink, detect};
use scana_db::dtc as dtcdb;
use scana_link::serial::{SlcanLink, list_ports};
use scana_link::sim::SimLink;
use scana_link::{Link, LinkEvent, OpenMode};
use scana_proto::obd::Reply;
use scana_proto::{Bitrate, CanId, Frame, candump, mode06, obd, pids, uds};
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
    /// Use a simulated 2006 Corolla with P0420 (a worn catalyst) instead of the bridge.
    #[arg(long, global = true)]
    sim_corolla: bool,
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
    /// Find every module in the car (ABS, airbag, body...) with harmless read requests, then
    /// read each one's name, part number and fault codes. Only while the car is still.
    Modules {
        /// Also try this gateway ID with every address byte (hex, for example 750). The
        /// vehicle profile's maker family adds its own.
        #[arg(long)]
        gateway: Vec<String>,
        /// Try both 11-bit and 29-bit request IDs, whatever the car's OBD-II uses.
        #[arg(long)]
        both: bool,
        /// How long to wait for each module's answer, in ms.
        #[arg(long, default_value_t = 60)]
        window_ms: u64,
    },
    /// Read one module: identity and fault codes. ADDRESS is its request ID (7B0), with an
    /// address byte after a slash for modules behind a gateway (750/40), or 29-bit (18DA28F1).
    Module {
        address: String,
        /// Scan a range of data IDs, for example F180-F19F (UDS) or 00-FF (KWP local IDs).
        #[arg(long)]
        scan: Option<String>,
        /// "did" (UDS 22) or "lid" (KWP 21). Default: whatever the module speaks.
        #[arg(long)]
        kind: Option<String>,
        /// Watch these data IDs (comma-separated hex) and print each change.
        #[arg(long)]
        watch: Option<String>,
        /// How long to watch, in seconds.
        #[arg(long, default_value_t = 20)]
        seconds: u64,
    },
    /// Send one read request to a module and show the answer, for example `send 7B0 21 01`.
    /// Anything that changes a module is refused.
    Send {
        address: String,
        /// The request in hex: 22 F1 90, or 22F190.
        #[arg(required = true, num_args = 1..)]
        request: Vec<String>,
    },
    /// Work through a fault code step by step: what it means, the likely causes, and tests that
    /// tell them apart. Works without a car (you answer from what you see, or another scan
    /// tool's readings); with --car, Scana measures what it can.
    Diagnose {
        /// The fault code, for example P0420.
        code: String,
        /// Connect to the car (the bridge, or --sim) and let Scana measure tests itself.
        #[arg(long)]
        car: bool,
        /// Answers given in advance: test=result,test=result (for scripts).
        #[arg(long)]
        answers: Option<String>,
        /// Only list the guide's tests and the current ranking; ask nothing.
        #[arg(long)]
        list: bool,
        /// The car's make, for maker-specific code numbers (for example Toyota).
        #[arg(long)]
        make: Option<String>,
    },
    /// Explain a fault code, a word, or a tool: Scana's knowledge base.
    Explain {
        /// A code (P0420), or words (fuel trim, oxygen sensor).
        #[arg(required = true, num_args = 1..)]
        what: Vec<String>,
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
    if let Cmd::Explain { what } = &cli.cmd {
        return explain(&what.join(" "));
    }
    if let Cmd::Diagnose { code, car, answers, list, make } = &cli.cmd {
        let client = if *car {
            let link = open_link(&cli)?;
            Some(Client::new(link))
        } else {
            None
        };
        return diagnose(code, client, answers.as_deref(), *list, make.as_deref());
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
        Cmd::Modules { gateway, both, window_ms } => find_modules(&mut client, &gateway, both, window_ms),
        Cmd::Module { address, scan, kind, watch, seconds } => {
            one_module(&mut client, &address, scan.as_deref(), kind.as_deref(), watch.as_deref(), seconds)
        }
        Cmd::Send { address, request } => send(&mut client, &address, &request.join(" ")),
        Cmd::Diagnose { .. } | Cmd::Explain { .. } => Ok(()),
    }
}

/// Word-wraps text at about 90 columns with an indent, for the terminal.
fn wrap(text: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut line = String::from(indent);
    for word in text.split_whitespace() {
        if line.len() + word.len() > 92 && !line.trim().is_empty() {
            out.push_str(line.trim_end());
            out.push('\n');
            line = String::from(indent);
        }
        line.push_str(word);
        line.push(' ');
    }
    out.push_str(line.trim_end());
    out
}

fn explain(what: &str) -> Result<()> {
    use scana_db::{diag, learn};
    let mut said = false;
    if let Some(code) = scana_proto::Dtc::parse(what.trim()) {
        let info = dtcdb::lookup(code);
        println!("{code}: {}", info.summary());
        println!("{}", wrap(&format!("Group: {}. System: {}.", info.group, info.system.name()), "  "));
        if info.manufacturer_specific {
            println!(
                "{}",
                wrap(
                    "Maker-specific: its exact meaning depends on the make. The maker's workshop manual has it.",
                    "  "
                )
            );
        }
        if let Some(a) = info.advice {
            println!("\nWhat it usually means:\n{}", wrap(&a.means, "  "));
            println!("What to check first:");
            for c in &a.check {
                println!("{}", wrap(&format!("- {c}"), "  "));
            }
        }
        if let Some(g) = diag::guide_for(code) {
            println!("\nScana has a step-by-step guide: {}.", g.title);
            println!("  Run: scana diagnose {code}");
        }
        for (m, g) in diag::maker_hints(code) {
            println!("\nOn {m}, {code} means: {}.", g.title);
            println!("  Run: scana diagnose {code} --make \"{m}\"");
        }
        for m in dtcdb::maker_meanings(code) {
            println!("\nOn {} cars, {code} means: {}.", m.makes.join(" and "), m.title);
            println!("{}", wrap(&m.advice.means, "  "));
            println!("What to check first:");
            for c in &m.advice.check {
                println!("{}", wrap(&format!("- {c}"), "  "));
            }
            println!("Sources:");
            for src in &m.sources {
                println!("{}", wrap(&format!("- {src}"), "  "));
            }
        }
        return Ok(());
    }
    let q = what.to_lowercase();
    if let Some(t) = learn::term(what) {
        println!("{}\n{}", t.name, wrap(&t.text, "  "));
        said = true;
    }
    let near: Vec<_> = learn::glossary()
        .iter()
        .filter(|t| {
            !t.name.eq_ignore_ascii_case(what)
                && (t.name.to_lowercase().contains(&q) || t.text.to_lowercase().contains(&q))
        })
        .collect();
    if !near.is_empty() {
        println!("{}Glossary words that mention \"{what}\":", if said { "\n" } else { "" });
        for t in near.iter().take(8) {
            println!("{}", wrap(&format!("{}: {}", t.name, t.text), "  "));
        }
        said = true;
    }
    for t in diag::tools().iter().filter(|t| t.name.to_lowercase().contains(&q) || t.id.contains(&q)) {
        println!(
            "\nTool: {} (cost: {})\n{}\n{}",
            t.name,
            t.cost,
            wrap(&t.what, "  "),
            wrap(&format!("How: {}", t.how), "  ")
        );
        if let Some(s) = &t.safety {
            println!("{}", wrap(&format!("Safety: {s}"), "  "));
        }
        said = true;
    }
    let guides = diag::search(what);
    if !guides.is_empty() {
        println!("\nDiagnosis guides:");
        for g in guides {
            println!("  {}: {} (codes {})", g.id, g.title, g.codes.join(", "));
        }
        said = true;
    }
    if !said {
        println!("Scana has nothing on \"{what}\" yet. Try a code (P0420) or a shorter word.");
    }
    Ok(())
}

/// Collects live samples for a test: waits for its precondition, then samples while its hold
/// condition is met, until enough seconds are collected (or 3 minutes pass).
fn measure_live(
    client: &mut Client,
    ecu: CanId,
    auto: &scana_db::diag::Auto,
) -> Result<scana_core::diagnose::Measured> {
    use scana_core::diagnose::{self, Sample};
    let pids = diagnose::live_pids(auto);
    let want_s = f64::from(auto.seconds.unwrap_or(20));
    let start = Instant::now();
    let mut samples: Vec<Sample> = Vec::new();
    let mut good_s = 0.0;
    let mut last_good: Option<Instant> = None;
    let mut last_msg = String::new();
    while good_s < want_s {
        if start.elapsed() > Duration::from_secs(180) {
            bail!("the test's conditions weren't met within 3 minutes");
        }
        let mut now_vals: std::collections::HashMap<u8, f64> = std::collections::HashMap::new();
        for chunk in pids.chunks(obd::MAX_PIDS_PER_REQUEST) {
            for r in readers::read_pids(client, ecu, chunk)? {
                if let Some(v) = r.decoded.as_ref().and_then(|d| d.main()).map(|q| q.value) {
                    now_vals.insert(r.pid, v);
                }
            }
        }
        let t_ms = client.now_ms();
        let need_ok =
            auto.need.as_ref().is_none_or(|n| n.pid().and_then(|p| now_vals.get(&p)).is_some_and(|v| n.holds(*v)));
        let hold_ok =
            auto.hold.as_ref().is_none_or(|h| h.pid().and_then(|p| now_vals.get(&p)).is_some_and(|v| h.holds(*v)));
        let msg = if !need_ok {
            auto.need.as_ref().map(|n| format!("Waiting: {}", n.text)).unwrap_or_default()
        } else if !hold_ok {
            auto.hold.as_ref().map(|h| format!("{} (now outside the range)", h.text)).unwrap_or_default()
        } else {
            String::new()
        };
        if !msg.is_empty() && msg != last_msg {
            println!("    {msg}");
        }
        last_msg = msg;
        if need_ok && hold_ok {
            let now = Instant::now();
            if let Some(prev) = last_good {
                good_s += now.duration_since(prev).as_secs_f64().min(1.0);
            }
            last_good = Some(now);
            for (&pid, &value) in &now_vals {
                samples.push(Sample { t_ms, pid, value });
            }
        } else {
            last_good = None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(diagnose::evaluate_live(auto, &samples))
}

fn diagnose(
    code_text: &str,
    client: Option<Client>,
    answers: Option<&str>,
    list: bool,
    make: Option<&str>,
) -> Result<()> {
    use scana_core::diagnose::Diagnosis;
    use scana_db::diag;
    let Some(code) = scana_proto::Dtc::parse(code_text) else {
        // Not a code: a guide's name, such as no-start, or words from its title.
        let g = diag::guide(code_text).or_else(|| {
            let found = diag::search(code_text);
            if found.len() == 1 { found.first().copied() } else { None }
        });
        let Some(g) = g else {
            println!("{code_text} isn't a code or a guide name. Symptom guides (no code needed):");
            for g in diag::symptoms() {
                println!("  scana diagnose {}   ({})", g.id, g.title);
            }
            return Ok(());
        };
        return run_guide(Diagnosis::new(g, None), client, answers, list, make);
    };
    let hints = diag::maker_hints(code);
    let found = Diagnosis::for_code_make(code, make).or_else(|| {
        let (m, g) = hints.first()?;
        println!(
            "{code} is a maker-specific number. On {m} it means: {}. Check it means the same on this car (--make tells Scana the make).\n",
            g.title
        );
        Some(Diagnosis::new(g, Some(code)))
    });
    let Some(d) = found else {
        println!("{code}: {}", dtcdb::lookup(code).summary());
        println!("Scana has no step-by-step guide for {code} yet. `scana explain {code}` shows what it knows.");
        return Ok(());
    };
    run_guide(d, client, answers, list, make)
}

/// Works through a guide in the terminal: prints it, measures what it can on a connected
/// car, asks the rest, and saves a report.
fn run_guide(
    mut d: scana_core::diagnose::Diagnosis,
    client: Option<Client>,
    answers: Option<&str>,
    list: bool,
    make: Option<&str>,
) -> Result<()> {
    use scana_core::diagnose::{self, Diagnosis};
    use scana_db::diag;
    let g = d.guide;
    let code_label = d.code.map_or_else(|| g.id.clone(), |c| c.to_string());
    println!("{code_label}: {}\n", g.title);
    println!("{}\n", wrap(&g.summary, ""));
    println!("How it works:");
    for p in &g.how_it_works {
        println!("{}", wrap(p, "  "));
    }
    if !g.safety.is_empty() {
        println!("\nSafety:");
        for s in &g.safety {
            println!("{}", wrap(&format!("! {s}"), "  "));
        }
    }
    for n in g.notes_for(make) {
        println!("\n[{}] {}:", n.kind_label(), n.heading());
        println!("{}", wrap(&n.text, "  "));
        println!("{}", wrap(&format!("Source: {}", n.source), "  "));
    }
    println!("\n{}", wrap(diag::RECALL_CHECK, ""));
    if let Some(list) = answers {
        for pair in list.split(',').filter(|p| !p.trim().is_empty()) {
            let (t, r) = pair.split_once('=').with_context(|| format!("answers look like test=result, not {pair}"))?;
            if d.test(t.trim()).and_then(|x| x.result(r.trim())).is_none() {
                bail!("the guide has no test {t} with a result {r}");
            }
            d.answer(t.trim(), r.trim());
        }
    }

    // With a car: read what Scana can, and measure the quick tests.
    let mut car_codes: Vec<scana_proto::Dtc> = Vec::new();
    let mut live: Option<(Client, CanId, Vec<u8>)> = None;
    if let Some(mut client) = client {
        println!("\nConnecting to the car to measure what Scana can...");
        let det = find_car(&mut client, true)?;
        let ecus = ecus_with_pids(&mut client, &det)?;
        for service in [obd::STORED_CODES, obd::PENDING_CODES] {
            for (_, list) in readers::read_codes(&mut client, service)? {
                car_codes.extend(list);
            }
        }
        car_codes.sort();
        car_codes.dedup();
        let mut tests = Vec::new();
        let mut statuses = Vec::new();
        let mut frames = Vec::new();
        for (ecu, supported) in &ecus {
            tests.push((*ecu, readers::test_results(&mut client, *ecu).unwrap_or_default()));
            if supported.contains(&0x01) {
                statuses.push(readers::readiness(&mut client, *ecu)?);
            }
            if let Some(f) = readers::freeze_frame(&mut client, *ecu)? {
                frames.push(f);
            }
        }
        let first = d.fix_first_present(&car_codes);
        if !first.is_empty() {
            println!(
                "\nFix these codes first, they often cause {code_label}: {}",
                first.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")
            );
        }
        for t in &g.test {
            let Some(a) = &t.auto else { continue };
            let m = match a.kind.as_str() {
                "codes" => diagnose::evaluate_codes(a, &car_codes),
                "mode06" => diagnose::evaluate_mode06(a, &tests),
                "readiness" => diagnose::evaluate_readiness(a, &statuses),
                "freeze" => diagnose::evaluate_freeze(a, &frames),
                _ => continue,
            };
            if let Some(r) = m.result.as_deref().filter(|r| t.result(r).is_some())
                && !d.answers.contains_key(&t.id)
            {
                d.answer(&t.id, r);
            }
            d.measured.insert(t.id.clone(), m);
        }
        // Say what was read: these tests are answered now and won't be asked.
        if !d.measured.is_empty() {
            println!("\nScana read these from the car:");
            for t in g.test.iter().filter(|t| d.measured.contains_key(&t.id)) {
                let m = &d.measured[&t.id];
                let answer = d.chosen(&t.id).map(|r| format!(" Answer: {}.", r.text)).unwrap_or_default();
                println!("{}", wrap(&format!("- {}: {}{answer}", t.title, m.text), "  "));
            }
        }
        if let Some((ecu, supported)) = ecus.first() {
            live = Some((client, *ecu, supported.clone()));
        }
    }

    let print_ranking = |d: &Diagnosis| {
        println!("\nLikely causes now:");
        for r in d.ranking().iter().take(4) {
            let bar = "#".repeat((r.share * 30.0).round() as usize);
            println!("  {:>3.0} % {bar:<30} {}", r.share * 100.0, r.cause.name);
        }
    };
    if list {
        println!("\nTests in this guide:");
        for t in &g.test {
            let needs: Vec<String> = t
                .needs
                .iter()
                .filter(|n| *n != "scan-tool")
                .map(|n| diag::tool(n).map_or(n.clone(), |x| x.name.clone()))
                .collect();
            let auto = if t.auto.is_some() { " [Scana can measure]" } else { "" };
            let tools = if needs.is_empty() { String::new() } else { format!(" (needs: {})", needs.join(", ")) };
            let answer = d.chosen(&t.id).map(|r| format!(" -> {}", r.text)).unwrap_or_default();
            println!("  {}: {}{tools}{auto}{answer}", t.id, t.title);
        }
        print_ranking(&d);
        return Ok(());
    }

    println!("\nAnswer each test with its number. s skips a test, q finishes.");
    let stdin = std::io::stdin();
    let mut skipped: Vec<String> = Vec::new();
    loop {
        let next = g.test.iter().filter(|t| !d.answers.contains_key(&t.id) && !skipped.contains(&t.id));
        let next: Vec<&scana_db::diag::Test> = next.collect();
        let Some(t) = next.iter().find(|t| t.no_tools()).or_else(|| next.first()).copied() else { break };
        print_ranking(&d);
        println!("\n== {} ==", t.title);
        println!("{}", wrap(&t.why, "  "));
        let tools: Vec<&str> = t
            .needs
            .iter()
            .filter(|n| *n != "scan-tool")
            .filter_map(|n| diag::tool(n).map(|x| x.name.as_str()))
            .collect();
        if !tools.is_empty() {
            println!(
                "{}",
                wrap(
                    &format!(
                        "Needs: {} (skip with s if you don't have one; `scana explain` describes it)",
                        tools.join(", ")
                    ),
                    "  "
                )
            );
        }
        if t.needs.iter().any(|n| n == "scan-tool") && live.is_none() {
            println!("  Needs live data: read it from your scan tool and choose the answer that matches.");
        }
        for (i, step) in t.steps.iter().enumerate() {
            println!("{}", wrap(&format!("{}. {step}", i + 1), "  "));
        }
        for sft in &t.safety {
            println!("{}", wrap(&format!("! {sft}"), "  "));
        }
        // Scana can measure a live test on the connected car.
        if let (Some(a), Some((client, ecu, supported))) = (&t.auto, live.as_mut())
            && diagnose::live_supported(a, supported)
            && !d.measured.contains_key(&t.id)
        {
            print!("  Scana can measure this. Press Enter to start (or type s to answer yourself): ");
            std::io::stdout().flush().ok();
            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? > 0 && line.trim().is_empty() {
                match measure_live(client, *ecu, a) {
                    Ok(m) => {
                        d.measured.insert(t.id.clone(), m);
                    }
                    Err(e) => println!("  Couldn't measure: {e:#}"),
                }
            }
        }
        if let Some(m) = d.measured.get(&t.id) {
            println!("{}", wrap(&format!("Scana measured: {}", m.text), "  "));
        }
        let suggested = d.measured.get(&t.id).and_then(|m| m.result.clone()).filter(|r| t.result(r).is_some());
        for (i, r) in t.result.iter().enumerate() {
            let mark = if suggested.as_deref() == Some(r.id.as_str()) { "  <- Scana's reading" } else { "" };
            println!("    {}) {}{mark}", i + 1, r.text);
        }
        print!("  Your answer: ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // input closed
        }
        let input = line.trim().to_lowercase();
        if input == "q" {
            break;
        }
        if input == "s" || (input.is_empty() && suggested.is_none()) {
            skipped.push(t.id.clone());
            continue;
        }
        let chosen = if input.is_empty() {
            suggested.clone()
        } else {
            input.parse::<usize>().ok().and_then(|n| t.result.get(n.wrapping_sub(1))).map(|r| r.id.clone())
        };
        let Some(rid) = chosen else {
            println!("  Type a number from the list, s or q.");
            continue;
        };
        d.answer(&t.id, &rid);
        if let Some(r) = t.result(&rid) {
            if let Some(n) = &r.note {
                println!("{}", wrap(&format!("Note: {n}"), "  "));
            }
            if let Some(to) = &r.goto
                && let Some(other) = diag::guide(to)
            {
                println!(
                    "  See also: scana diagnose {} ({})",
                    other.codes.first().cloned().unwrap_or_default(),
                    other.title
                );
            }
            if r.stop {
                println!("  Fix that first. You can carry on with the other tests, or q to finish.");
            }
        }
    }
    println!("\n== Result ==");
    for l in d.summary_lines() {
        println!("{}", wrap(&l, "  "));
    }
    let dir = recorder::sessions_dir().join("diagnoses");
    std::fs::create_dir_all(&dir).with_context(|| format!("can't make {}", dir.display()))?;
    let when = chrono_now();
    let path = dir.join(format!("{}_{code_label}.html", when.replace([' ', ':'], "-")));
    std::fs::write(&path, d.to_html("", &when)).with_context(|| format!("can't write {}", path.display()))?;
    println!("\nReport saved: {}", path.display());
    Ok(())
}

/// The local time as "2026-10-16 10:30".
fn chrono_now() -> String {
    report::now_text()
}

fn open_link(cli: &Cli) -> Result<Box<dyn Link>> {
    if cli.sim_corolla {
        let link = SimLink::new(VirtualCar::corolla_2006());
        eprintln!("Using a {}: a 2006 Corolla with P0420", link.describe());
        return Ok(Box::new(link));
    }
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
    if cli.sim_corolla {
        return Source::DemoCorolla;
    }
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
    if let Some(r) = link.start_report() {
        println!("Started:   {} ({r})", scana_proto::slcan::describe_reset(&r));
    }
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
            Some(LinkEvent::BridgeReady { version, reset, .. }) => {
                bail!("the bridge restarted, scana-bridge {version}: {}", scana_proto::slcan::describe_reset(&reset))
            }
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
        if per_ecu.is_empty() {
            let why = if service == obd::PERMANENT_CODES {
                "no ECU answered (normal for cars from before about 2010, which don't keep permanent codes)"
            } else {
                "no ECU answered"
            };
            println!("{label:<10} {why}");
        }
        for (ecu, codes) in per_ecu {
            if codes.is_empty() {
                println!("{label:<10} {ecu}: none");
            } else {
                for c in codes {
                    println!("{label:<10} {ecu}: {c}  {}", dtcdb::lookup(c).summary());
                }
            }
        }
    }
    Ok(())
}

/// The car's maker family: from the matching vehicle profile, else the make in its VIN.
fn family_of(client: &mut Client, d: &Detected) -> Result<Option<&'static str>> {
    let ecus = ecus_with_pids(client, d)?;
    Ok(readers::identify(client, &ecus)?.family())
}

fn print_module(m: &ModuleInfo) {
    println!("\n{}  [{}, {}]", m.display_name(), m.channel.label(), m.dialect.name());
    println!("  replies from {}", m.channel.rx);
    for (k, v) in &m.identity {
        println!("  {k}: {v}");
    }
    match &m.codes {
        Ok(c) if c.is_empty() => println!("  Fault codes: none"),
        Ok(c) => {
            for d in c {
                println!("  Code {}", report::module_code_line(d));
            }
        }
        Err(e) => println!("  Fault codes not read: {e}"),
    }
}

fn find_modules(client: &mut Client, gateways: &[String], both: bool, window_ms: u64) -> Result<()> {
    let d = find_car(client, true)?;
    let family = family_of(client, &d)?;
    let ecus = ecus_with_pids(client, &d)?;
    if let Some(note) = modules::check_still(client, &ecus)? {
        println!("Note: {note}");
    }
    let mut opts = DiscoverOptions::for_car(d.addressing, family);
    opts.window_ms = window_ms;
    if both {
        opts.standard = true;
        opts.extended = true;
    }
    for g in gateways {
        let id = u16::from_str_radix(g.trim_start_matches("0x"), 16)
            .ok()
            .filter(|x| *x <= 0x7FF)
            .with_context(|| format!("{g} is not an 11-bit gateway ID"))?;
        if !opts.gateways.contains(&id) {
            opts.gateways.push(id);
        }
    }
    if let Some(f) = family {
        println!("Maker family: {f} (module names below are this family's usual ones until a module gives its own)");
    }
    println!("Looking for modules (one harmless request per address; Ctrl-C stops)...");
    let start = Instant::now();
    let discovery = modules::discover(client, &opts, |p| {
        match p {
            modules::Progress::Listening => eprintln!("  listening to the bus for 1 s first"),
            modules::Progress::Probing { done, total, at } => {
                if done % 32 == 0 {
                    eprintln!("  {done} of {total} addresses (now {at})");
                }
            }
            modules::Progress::Found(f) => {
                eprintln!("  found a module at {} (replies from {})", f.channel.label(), f.channel.rx)
            }
        }
        true
    })?;
    println!(
        "Tried {} addresses in {:.0} s: {} module(s) answered.",
        discovery.tried,
        start.elapsed().as_secs_f64(),
        discovery.found.len()
    );
    if !discovery.skipped.is_empty() {
        println!(
            "Left alone (already in use on the bus): {}",
            discovery.skipped.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")
        );
    }
    for f in &discovery.found {
        let m = modules::read_module(client, f.channel, Some(f), family)?;
        print_module(&m);
    }
    Ok(())
}

fn parse_channel(address: &str) -> Result<Channel> {
    Channel::parse(address).with_context(|| {
        format!(
            "{address} is not a module address. Use a request ID like 7B0, 750/40 (with an address byte) or 18DA28F1"
        )
    })
}

fn parse_range(s: &str) -> Result<(u16, u16)> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let p = |t: &str| u16::from_str_radix(t.trim().trim_start_matches("0x"), 16);
    match (p(a), p(b)) {
        (Ok(a), Ok(b)) => Ok((a.min(b), a.max(b))),
        _ => bail!("{s} is not a range of hex IDs like F180-F19F"),
    }
}

fn one_module(
    client: &mut Client,
    address: &str,
    scan: Option<&str>,
    kind: Option<&str>,
    watch: Option<&str>,
    seconds: u64,
) -> Result<()> {
    let ch = parse_channel(address)?;
    let d = find_car(client, true)?;
    let family = family_of(client, &d)?;
    let m = modules::read_module(client, ch, None, family)?;
    print_module(&m);
    let kind = match kind.map(str::to_ascii_lowercase).as_deref() {
        Some("did") => DataKind::Did,
        Some("lid") => DataKind::Lid,
        Some(other) => bail!("--kind is did or lid, not {other}"),
        None => DataKind::for_dialect(m.dialect),
    };
    if let Some(r) = scan {
        let (from, to) = parse_range(r)?;
        println!(
            "\nScanning {} {from:X}-{to:X} on {}...",
            if kind == DataKind::Did { "DIDs" } else { "LIDs" },
            ch.label()
        );
        let items = modules::scan_data(client, ch, kind, from..=to, |done, total| {
            if done % 64 == 0 && done > 0 {
                eprintln!("  {done} of {total}");
            }
            true
        })?;
        println!("{} answered:", items.len());
        for i in &items {
            let name = if kind == DataKind::Did { uds::did_name(i.id) } else { None };
            let text = uds::text_of(&i.bytes).map(|t| format!("  \"{t}\"")).unwrap_or_default();
            println!(
                "  {:<9} {}{text}{}",
                kind.label(i.id),
                uds::hex(&i.bytes),
                name.map(|n| format!("  ({n})")).unwrap_or_default()
            );
        }
    }
    if let Some(list) = watch {
        let ids: Vec<u16> = list
            .split(',')
            .map(|t| u16::from_str_radix(t.trim().trim_start_matches("0x"), 16))
            .collect::<std::result::Result<_, _>>()
            .with_context(|| format!("--watch wants hex IDs separated by commas, not {list}"))?;
        println!("\nWatching for {seconds} s. Do something (lights, doors, pedals) and see which bytes change.");
        let end = Instant::now() + Duration::from_secs(seconds);
        let mut last: std::collections::HashMap<u16, Vec<u8>> = std::collections::HashMap::new();
        let start = Instant::now();
        while Instant::now() < end {
            for &id in &ids {
                match modules::read_data(client, ch, kind, id) {
                    Ok(bytes) => {
                        if last.get(&id) != Some(&bytes) {
                            let changed: Vec<usize> = match last.get(&id) {
                                Some(old) => (0..bytes.len()).filter(|&i| old.get(i) != bytes.get(i)).collect(),
                                None => vec![],
                            };
                            let marks = if changed.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "  changed byte{} {}",
                                    if changed.len() == 1 { "" } else { "s" },
                                    changed.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
                                )
                            };
                            println!(
                                "  {:>6.1} s  {:<9} {}{marks}",
                                start.elapsed().as_secs_f64(),
                                kind.label(id),
                                uds::hex(&bytes)
                            );
                            last.insert(id, bytes);
                        }
                    }
                    Err(e) if modules::is_fatal(&e) => return Err(e.into()),
                    Err(e) => println!("  {}: {e}", kind.label(id)),
                }
            }
        }
    }
    Ok(())
}

fn send(client: &mut Client, address: &str, hex: &str) -> Result<()> {
    let ch = parse_channel(address)?;
    let request = uds::parse_hex(hex).with_context(|| format!("{hex} is not hex bytes"))?;
    find_car(client, true)?;
    println!("Sending {} to {} ({})", uds::hex(&request), ch.label(), uds::service_name(request[0]));
    match modules::console_request(client, ch, &request)? {
        None => println!("No answer within {} ms.", client.options.physical_timeout_ms),
        Some(Reply::Positive { service, data }) => {
            println!("Answer: {:02X} {}", service + 0x40, uds::hex(&data));
            if let Some(t) = uds::text_of(&data) {
                println!("As text: {t}");
            }
        }
        Some(Reply::Negative { service, code }) => {
            println!("Refused (7F {service:02X} {code:02X}): {}", obd::negative_code_text(code));
        }
    }
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
                "  {:<42} {:<40} {:>14}  limits {:<12} {}",
                mode06::monitor_name(t.mid),
                mode06::test_name(t.mid, t.tid),
                t.value_text(),
                t.limits_text(),
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
    let id = readers::identify(client, &ecus)?;
    let vehicle = id.name.clone();
    let protocol = format!("ISO 15765-4 CAN, {}, {}", d.addressing.name(), d.bitrate);
    let report = report::run_health_check(client, &ecus, id.profile, &vehicle, &protocol)?;
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
