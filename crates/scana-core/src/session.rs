//! The session worker: owns the link and the client on its own thread, so the screen never
//! waits on the car. The app sends [`Command`]s and receives [`Update`]s.
//!
//! It polls live values, runs one-off reads (codes, freeze frames, readiness, tests, vehicle
//! info), the health check and the clear-codes gate, records sessions, and recovers by itself:
//! a pulled USB cable or a bridge reset reconnects every 2 s; a silent car (key off) is looked
//! for again every 5 s.
//!
//! Stage 3 adds module discovery, reading each module, the data explorer (scan and watch) and
//! the read-only console. Long jobs check for [`Command::Stop`] between requests.

use crate::client::{Addressing, Channel, Client, CoreError, FrameSink};
use crate::detect::{DetectOptions, SECOND_BUS_LISTEN_MS, detect, detect_second_bus};
use crate::modules::{self, DataItem, DataKind, DiscoverOptions, ModuleInfo, Progress};
use crate::poller::Poller;
use crate::readers::{self, FreezeFrame, PidReading, VehicleInfo};
use crate::recorder::{Recorder, SessionSummary};
use crate::report::{self, HealthReport};
use crate::safety::{self, ClearConsent, ClearOutcome};
use scana_link::serial::SlcanLink;
use scana_link::sim::SimLink;
use scana_link::{Link, LinkError, OpenMode};
use scana_proto::mode06::TestResult;
use scana_proto::obd::Reply;
use scana_proto::pids::MonitorStatus;
use scana_proto::{Bitrate, Bus, CanId, Dtc, Frame, obd};
use scana_sim::VirtualCar;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Where the car is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The scana-bridge on USB; `None` finds the port. `bus`: which OBD pins the cable or
    /// switch reaches (the main bus is pins 6 and 14, the second pins 3 and 11).
    Bridge { port: Option<String>, bus: Bus },
    /// The simulated Hijet, engine running, with a dash display polling the same port.
    Demo,
    /// The simulated Hijet with faults, key on and engine off, so clearing codes can be tried.
    DemoFaults,
    /// A simulated 2006 Corolla with P0420 and a worn catalyst, engine running and warm.
    DemoCorolla,
    /// One of the invented test cars (`scana_sim::VirtualCar::by_name`). `engine_off`: key on,
    /// engine off, so clearing codes can be tried. `bus`: the pins the simulated link is
    /// wired to (the `ford-ms-can` car is on the second bus).
    SimCar { name: String, engine_off: bool, bus: Bus },
}

/// The words for a command that needs the main bus, shown when it is asked of the second.
pub const NEEDS_MAIN_BUS: &str =
    "That needs the main bus on pins 6 and 14: set the cable or switch back and connect again.";

/// The vehicle label of a connection to the second bus.
pub const SECOND_BUS_LABEL: &str = "Second CAN bus (pins 3 and 11)";

/// The app's words for sending on the second bus without the parked tick.
pub const NEEDS_PARKED_APP: &str =
    "Scana can't read the car's speed on pins 3 and 11. Park the car, tick the box to say so, then try again.";

impl Source {
    /// Which pins this source is wired to.
    pub fn bus(&self) -> Bus {
        match self {
            Source::Bridge { bus, .. } | Source::SimCar { bus, .. } => *bus,
            _ => Bus::Main,
        }
    }

    pub fn label(&self) -> String {
        let pins = if self.bus() == Bus::Second { ", pins 3 and 11" } else { "" };
        match self {
            Source::Bridge { port: Some(p), .. } => format!("Scana bridge on {p}{pins}"),
            Source::Bridge { port: None, .. } => format!("Scana bridge (USB){pins}"),
            Source::Demo => "Demo car".into(),
            Source::DemoFaults => "Demo car with faults".into(),
            Source::DemoCorolla => "Demo Corolla with P0420".into(),
            Source::SimCar { name, engine_off: false, .. } => format!("Simulated car: {name}{pins}"),
            Source::SimCar { name, engine_off: true, .. } => format!("Simulated car: {name} (engine off){pins}"),
        }
    }
}

pub enum Command {
    Connect(Source),
    Disconnect,
    /// The live values to poll: (ECU, PID).
    SetLive(Vec<(CanId, u8)>),
    ReadCodes,
    ReadFreeze,
    ReadReadiness,
    ReadTests,
    ReadInfo,
    HealthCheck,
    ClearCodes(ClearConsent),
    Record(bool),
    Mark(String),
    SetGaps {
        alone_ms: u64,
        shared_ms: u64,
    },
    /// Stage 3: find every module, then read each one's identity and codes. On the second
    /// bus this sends requests, and Scana can't read speed there: `parked` is the user saying
    /// the car is parked, and without it the search is refused. Ignored on the main bus.
    FindModules {
        parked: bool,
    },
    /// The car's make, if the user gives it, for the second bus where no VIN can be read.
    /// Send it before `Connect`. It picks the module names and gateways.
    SetMake(Option<String>),
    /// Read one module again: identity and codes.
    ReadModule(Channel),
    /// Read every data ID from `from` to `to` on one module (the data explorer).
    ScanData {
        channel: Channel,
        kind: DataKind,
        from: u16,
        to: u16,
    },
    /// Keep reading these data IDs from one module, or stop watching (`None`).
    Watch(Option<WatchSpec>),
    /// One read request typed by the user. Refused unless it only reads.
    Console {
        channel: Channel,
        request: Vec<u8>,
    },
    /// Stop a long job: module discovery or a data scan.
    Stop,
    Shutdown,
}

/// Data IDs to keep reading from one module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchSpec {
    pub channel: Channel,
    pub kind: DataKind,
    pub ids: Vec<u16>,
}

/// How a module discovery ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoverySummary {
    pub found: usize,
    pub tried: usize,
    /// IDs heard on the bus and so left alone.
    pub skipped: Vec<CanId>,
    pub stopped_early: bool,
    /// For example: no computer reports speed, so the car couldn't be checked as still.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    Disconnected,
    Connecting(String),
    Connected,
    /// Lost; trying again by itself. The text says what happened.
    Lost(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcuView {
    pub id: CanId,
    pub name: String,
    /// Supported live PIDs, including the support masks.
    pub supported: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub source: Source,
    /// Which pins this connection is on.
    pub bus: Bus,
    pub link: String,
    pub bitrate: Bitrate,
    pub addressing: Addressing,
    pub ecus: Vec<EcuView>,
    pub profile_id: Option<String>,
    pub vehicle: String,
    /// The make: from the profile, else from the VIN.
    pub make: Option<String>,
    pub other_tester: bool,
    pub log: Vec<String>,
    /// The live values being polled right after connecting: (ECU, PID).
    pub live: Vec<(CanId, u8)>,
}

impl ConnectionInfo {
    pub fn protocol(&self) -> String {
        match self.bus {
            Bus::Main => format!("ISO 15765-4 CAN, {}, {}", self.addressing.name(), self.bitrate),
            Bus::Second => format!("CAN on pins 3 and 11, {}, {}", self.addressing.name(), self.bitrate),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveValue {
    pub ecu: CanId,
    pub t_ms: u64,
    pub reading: PidReading,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeSet {
    pub ecu: CanId,
    pub stored: Vec<Dtc>,
    pub pending: Vec<Dtc>,
    pub permanent: Vec<Dtc>,
    /// False if the ECU didn't answer for permanent codes (cars before about 2010 don't keep them).
    pub permanent_read: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub requests_per_s: f64,
    pub frames_per_s: f64,
    pub other_tester: bool,
    pub bus_errors: u32,
}

#[derive(Clone, Debug)]
pub enum Update {
    State(LinkState),
    Connected(ConnectionInfo),
    Values(Vec<LiveValue>),
    Codes(Vec<CodeSet>),
    Freeze(Vec<FreezeFrame>),
    Readiness(Vec<(CanId, MonitorStatus)>),
    Tests(Vec<(CanId, Vec<TestResult>)>),
    Info(Vec<(CanId, VehicleInfo)>),
    Health {
        report: Box<HealthReport>,
        path: Option<PathBuf>,
    },
    Cleared {
        outcome: ClearOutcome,
        report_path: Option<PathBuf>,
    },
    /// Recording on (with its folder) or off.
    Recording(Option<PathBuf>),
    Frame {
        frame: Frame,
        at_ms: u64,
        outgoing: bool,
    },
    Stats(Stats),
    /// Something slow is running ("Reading codes"), or finished (`None`).
    Busy(Option<String>),
    /// How far a long job has got, 0 to 1, or finished (`None`).
    Progress(Option<f32>),
    /// Module discovery started (the list is cleared) ...
    DiscoveryStarted,
    /// ... and ended.
    Discovery(DiscoverySummary),
    /// What Scana read from one module (or just its address and likely name, while
    /// discovery is still running).
    Module(Box<ModuleInfo>),
    /// A data scan's results; `complete` is false if it was stopped.
    DataScan {
        channel: Channel,
        kind: DataKind,
        items: Vec<DataItem>,
        complete: bool,
    },
    /// One watched data value.
    DataValue {
        channel: Channel,
        kind: DataKind,
        id: u16,
        bytes: Vec<u8>,
        t_ms: u64,
    },
    /// The console's answer: a reply, silence (`Ok(None)`), or why it wasn't sent.
    ConsoleReply {
        channel: Channel,
        request: Vec<u8>,
        reply: Result<Option<Reply>, String>,
    },
    Message(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct WorkerSettings {
    pub sessions_dir: PathBuf,
    /// Record every connection.
    pub auto_record: bool,
}

impl Default for WorkerSettings {
    fn default() -> Self {
        WorkerSettings { sessions_dir: crate::recorder::sessions_dir(), auto_record: false }
    }
}

type Wake = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    ui: Sender<Update>,
    wake: Wake,
    rec: Mutex<Option<Recorder>>,
    frames: AtomicU64,
    /// Added to the link's clock for times the app sees. Each link's clock starts at 0, so
    /// without this the graphs went back to the start after every reconnect.
    offset_ms: AtomicU64,
}

impl Shared {
    /// The app's time for a link time: it keeps counting through reconnects.
    fn app_ms(&self, link_ms: u64) -> u64 {
        link_ms + self.offset_ms.load(Ordering::Relaxed)
    }

    fn send(&self, u: Update) {
        let _ = self.ui.send(u);
        (self.wake)();
    }

    /// One line in the session's `events.log`, if recording.
    fn note(&self, text: &str) {
        if let Ok(mut r) = self.rec.lock()
            && let Some(r) = r.as_mut()
        {
            r.event(text);
        }
    }
}

struct WorkerSink(Arc<Shared>);

impl FrameSink for WorkerSink {
    fn frame(&mut self, frame: &Frame, at_ms: u64, outgoing: bool) {
        if let Ok(mut r) = self.0.rec.lock()
            && let Some(r) = r.as_mut()
        {
            r.frame(frame, at_ms, outgoing);
        }
        self.0.frames.fetch_add(1, Ordering::Relaxed);
        self.0.send(Update::Frame { frame: *frame, at_ms: self.0.app_ms(at_ms), outgoing });
    }

    fn event(&mut self, text: &str, _at_ms: u64) {
        self.0.note(text);
    }
}

/// The app's end of the worker.
pub struct SessionHandle {
    tx: Sender<Command>,
    updates: Receiver<Update>,
    join: Option<JoinHandle<()>>,
}

impl SessionHandle {
    /// Starts the worker. `wake` is called after every update (the app asks for a repaint).
    pub fn spawn(settings: WorkerSettings, wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (ui, updates) = mpsc::channel();
        let shared = Arc::new(Shared {
            ui,
            wake: Arc::new(wake),
            rec: Mutex::new(None),
            frames: AtomicU64::new(0),
            offset_ms: AtomicU64::new(0),
        });
        let join = thread::Builder::new()
            .name("scana-session".into())
            .spawn(move || Worker::new(shared, rx, settings).run())?;
        Ok(SessionHandle { tx, updates, join: Some(join) })
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    /// Every update waiting, without blocking.
    pub fn poll(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }

    /// Waits up to `timeout` for the next update (for tests and the command line).
    pub fn wait(&self, timeout: Duration) -> Option<Update> {
        self.updates.recv_timeout(timeout).ok()
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lost {
    /// The bridge went away: reconnect the link.
    Link,
    /// The link is fine but the car stopped answering: look for it again.
    Car,
}

struct Worker {
    shared: Arc<Shared>,
    rx: Receiver<Command>,
    settings: WorkerSettings,
    client: Option<Client>,
    conn: Option<ConnectionInfo>,
    source: Option<Source>,
    poller: Poller,
    live: Vec<(CanId, u8)>,
    lost: Option<(Lost, Instant)>,
    gaps: (u64, u64),
    misses: u32,
    bus_errors: u32,
    stats_at: Instant,
    stats_requests: u64,
    stats_frames: u64,
    record_wanted: bool,
    /// Modules found this connection (for the health check and the session).
    modules: Vec<ModuleInfo>,
    watch: Option<(WatchSpec, usize)>,
    /// The watch's turn next (it alternates with the live values).
    watch_turn: bool,
    /// Commands that arrived during a long job, handled after it.
    pending: VecDeque<Command>,
    /// When this connection began: the app's clock, kept through reconnects.
    started: Instant,
    /// The make the user gave for the second bus.
    make_hint: Option<String>,
    /// Second bus: the user said the car is parked (this connection), so Scana may send
    /// module requests there. Cleared when the link is lost and found again.
    parked: bool,
}

/// Collects commands that arrive during a long job. True if the job should stop: the user
/// pressed Stop, or asked to disconnect, connect elsewhere or quit (kept for afterwards).
fn stop_requested(rx: &Receiver<Command>, pending: &mut VecDeque<Command>) -> bool {
    let mut stop = false;
    while let Ok(c) = rx.try_recv() {
        match c {
            Command::Stop => stop = true,
            c @ (Command::Disconnect | Command::Shutdown | Command::Connect(_)) => {
                stop = true;
                pending.push_back(c);
            }
            other => pending.push_back(other),
        }
    }
    stop
}

impl Worker {
    fn new(shared: Arc<Shared>, rx: Receiver<Command>, settings: WorkerSettings) -> Self {
        Worker {
            record_wanted: settings.auto_record,
            shared,
            rx,
            settings,
            client: None,
            conn: None,
            source: None,
            poller: Poller::new(),
            live: vec![],
            lost: None,
            gaps: (50, 100),
            misses: 0,
            bus_errors: 0,
            stats_at: Instant::now(),
            stats_requests: 0,
            stats_frames: 0,
            modules: vec![],
            watch: None,
            watch_turn: false,
            pending: VecDeque::new(),
            started: Instant::now(),
            make_hint: None,
            parked: false,
        }
    }

    /// Which pins the connection is on.
    fn bus(&self) -> Bus {
        self.source.as_ref().map_or(Bus::Main, Source::bus)
    }

    /// Refuses (with the app's words) a job that needs the main bus, when on the second.
    /// True means it was refused.
    fn refuse_on_second_bus(&self) -> bool {
        if self.bus() == Bus::Second {
            self.send(Update::Error(NEEDS_MAIN_BUS.into()));
            return true;
        }
        false
    }

    /// Second bus: a job that sends needs the parked tick first.
    fn parked_or_refuse(&self) -> Result<(), CoreError> {
        if self.bus() == Bus::Second && !self.parked { Err(CoreError::NeedsParked) } else { Ok(()) }
    }

    /// Second bus: opens the bus in normal mode (it can transmit) for a job that sends, after
    /// the parked check. Listening alone keeps it listen-only.
    fn open_for_sending(&mut self) -> Result<(), CoreError> {
        if self.bus() != Bus::Second {
            return Ok(());
        }
        self.parked_or_refuse()?;
        let rate =
            self.conn.as_ref().map(|c| c.bitrate).ok_or_else(|| CoreError::BadRequest("not connected".into()))?;
        let client = self.client()?;
        if client.link().mode() != Some(OpenMode::Normal) {
            if client.link().mode().is_some() {
                client.link().close()?;
            }
            client.link().set_bitrate(rate)?;
            client.link().open(OpenMode::Normal)?;
        }
        Ok(())
    }

    /// Second bus: back to listen-only once nothing needs to send.
    fn back_to_listening(&mut self) {
        if self.bus() != Bus::Second || self.watch.is_some() {
            return;
        }
        let rate = self.conn.as_ref().map(|c| c.bitrate);
        if let (Some(rate), Some(client)) = (rate, self.client.as_mut())
            && client.link().mode() == Some(OpenMode::Normal)
        {
            let _ = client.link().close();
            let _ = client.link().set_bitrate(rate);
            let _ = client.link().open(OpenMode::ListenOnly);
        }
    }

    /// Sets up a new link's client: the frame sink, the gaps, and the app clock offset.
    fn attach(&self, client: &mut Client) {
        client.set_sink(Box::new(WorkerSink(self.shared.clone())));
        client.set_bus(self.bus());
        client.options.min_gap_ms = self.gaps.0;
        client.options.shared_gap_ms = self.gaps.1;
        let since = self.started.elapsed().as_millis() as u64;
        self.shared.offset_ms.store(since.saturating_sub(client.now_ms()), Ordering::Relaxed);
    }

    fn send(&self, u: Update) {
        self.shared.send(u);
    }

    fn run(mut self) {
        loop {
            if let Some(cmd) = self.pending.pop_front() {
                if matches!(cmd, Command::Shutdown) {
                    self.disconnect(false);
                    return;
                }
                self.guarded(|w| w.handle(cmd));
                continue;
            }
            let wait = self.wait_time();
            match self.rx.recv_timeout(wait) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    self.disconnect(false);
                    return;
                }
                Ok(cmd) => self.guarded(|w| w.handle(cmd)),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.guarded(Worker::tick);
        }
    }

    /// Runs a step; a panic is reported and the connection reset instead of killing the worker.
    fn guarded(&mut self, f: impl FnOnce(&mut Worker)) {
        if catch_unwind(AssertUnwindSafe(|| f(self))).is_err() {
            self.client = None;
            self.lost = None;
            self.send(Update::Busy(None));
            self.send(Update::Error(
                "Scana hit an internal error and stopped this connection. Connect again; please report it with the session folder."
                    .into(),
            ));
            self.send(Update::State(LinkState::Failed("Internal error: connect again.".into())));
        }
    }

    fn wait_time(&self) -> Duration {
        if self.client.is_some() {
            return Duration::ZERO;
        }
        match self.lost {
            Some((_, at)) => at.saturating_duration_since(Instant::now()).min(Duration::from_millis(200)),
            None => Duration::from_millis(200),
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Connect(src) => self.connect(src),
            Command::Disconnect => self.disconnect(true),
            Command::SetLive(list) => {
                // The second bus has no OBD-II values to poll.
                self.live = if self.bus() == Bus::Second { vec![] } else { list };
                let now = self.client.as_ref().map_or(0, Client::now_ms);
                self.poller.set(&self.live, now);
            }
            // Everything OBD-II needs the main bus. On the second bus these are refused here, and
            // the client would refuse the frames too. Nothing is ever cleared there.
            Command::ReadCodes
            | Command::ReadFreeze
            | Command::ReadReadiness
            | Command::ReadTests
            | Command::ReadInfo
            | Command::HealthCheck
            | Command::ClearCodes(_)
                if self.refuse_on_second_bus() => {}
            Command::ReadCodes => self.one_off("Reading fault codes", Worker::read_codes),
            Command::ReadFreeze => self.one_off("Reading freeze frames", Worker::read_freeze),
            Command::ReadReadiness => self.one_off("Reading readiness", Worker::read_readiness),
            Command::ReadTests => self.one_off("Reading test results", Worker::read_tests),
            Command::ReadInfo => self.one_off("Reading vehicle information", Worker::read_info),
            Command::HealthCheck => self.one_off("Running the health check", |w| w.health_check().map(|_| ())),
            Command::ClearCodes(consent) => self.one_off("Clearing codes", move |w| w.clear(consent)),
            Command::SetMake(make) => {
                self.make_hint = make.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
            }
            Command::Record(on) => {
                self.record_wanted = on;
                if on { self.start_recording() } else { self.stop_recording() }
            }
            Command::Mark(label) => {
                let t = self.client.as_ref().map_or(0, Client::now_ms);
                if let Ok(mut r) = self.shared.rec.lock()
                    && let Some(r) = r.as_mut()
                {
                    r.marker(t, &label);
                }
                self.send(Update::Message(format!("Marked: {label}")));
            }
            Command::SetGaps { alone_ms, shared_ms } => {
                self.gaps = (alone_ms.clamp(20, 2000), shared_ms.clamp(20, 2000));
                if let Some(c) = self.client.as_mut() {
                    c.options.min_gap_ms = self.gaps.0;
                    c.options.shared_gap_ms = self.gaps.1;
                }
            }
            Command::FindModules { parked } => {
                if self.bus() == Bus::Second {
                    self.parked = parked;
                }
                self.one_off("Looking for modules", Worker::find_modules);
            }
            Command::ReadModule(ch) => self.one_off("Reading the module", move |w| w.read_one_module(ch)),
            Command::ScanData { channel, kind, from, to } => {
                self.one_off("Scanning data IDs", move |w| w.scan(channel, kind, from, to));
            }
            Command::Watch(spec) => {
                if spec.is_some() && self.parked_or_refuse().is_err() {
                    self.send(Update::Error(NEEDS_PARKED_APP.into()));
                } else {
                    self.watch = spec.map(|s| (s, 0));
                    self.back_to_listening();
                }
            }
            Command::Console { channel, request } => {
                self.one_off("Sending the request", move |w| w.console(channel, request));
            }
            Command::Stop | Command::Shutdown => {}
        }
    }

    /// The maker family for module names and gateways: from the vehicle profile, else the make.
    fn family(&self) -> Option<&'static str> {
        let conn = self.conn.as_ref()?;
        let from_profile =
            conn.profile_id.as_ref().and_then(|id| scana_db::profile::builtin().iter().find(|p| &p.id == id));
        from_profile
            .and_then(|p| p.family())
            .or_else(|| conn.make.as_deref().and_then(scana_db::modules::family_of_make))
    }

    /// Gives a module the OBD ECU's name from the profile, where it is one.
    fn name_from_conn(&self, info: &mut ModuleInfo) {
        if info.channel.is_obd()
            && let Some(e) = self.conn.as_ref().and_then(|c| c.ecus.iter().find(|e| e.id == info.channel.rx))
            && info.name_from != modules::NameFrom::Module
        {
            info.name = e.name.clone();
            info.name_from = modules::NameFrom::Module;
        }
    }

    fn find_modules(&mut self) -> Result<(), CoreError> {
        let conn = self.conn.clone().ok_or_else(|| CoreError::BadRequest("not connected".into()))?;
        let family = self.family();
        let ecus = self.ecus();
        let bus = self.bus();
        self.open_for_sending()?;
        let mut client = self.client.take().ok_or_else(|| CoreError::BadRequest("not connected".into()))?;
        self.modules.clear();
        self.watch = None;
        self.send(Update::DiscoveryStarted);
        let shared = self.shared.clone();
        let rx = &self.rx;
        let pending = &mut self.pending;
        let mut found = Vec::new();
        let result = (|| -> Result<DiscoverySummary, CoreError> {
            let note = if bus == Bus::Second {
                Some("Scana can't check the car is still on pins 3 and 11. You said it is parked.".to_string())
            } else {
                modules::check_still(&mut client, &ecus)?
            };
            let mut opts = if bus == Bus::Second {
                DiscoverOptions::for_second_bus(family)
            } else {
                DiscoverOptions::for_car(conn.addressing, family)
            };
            opts.still_ecus = ecus.clone();
            let mut last = Instant::now() - Duration::from_secs(1);
            let d = modules::discover(&mut client, &opts, |p| {
                match p {
                    Progress::Listening => shared.send(Update::Busy(Some("Listening to the bus first".into()))),
                    Progress::Probing { done, total, at } => {
                        if last.elapsed() >= Duration::from_millis(250) {
                            last = Instant::now();
                            shared.send(Update::Busy(Some(format!(
                                "Looking for modules: address {at}, {done} of {total}"
                            ))));
                            shared.send(Update::Progress(Some(done as f32 / total.max(1) as f32 * 0.8)));
                        }
                    }
                    Progress::Found(f) => {
                        shared.send(Update::Module(Box::new(ModuleInfo::named_on(f.channel, family, bus))));
                    }
                }
                !stop_requested(rx, pending)
            })?;
            let mut stopped = d.stopped_early;
            for (i, f) in d.found.iter().enumerate() {
                if stop_requested(rx, pending) {
                    stopped = true;
                    break;
                }
                shared.send(Update::Busy(Some(format!(
                    "Reading module {} of {} ({})",
                    i + 1,
                    d.found.len(),
                    f.channel.label()
                ))));
                shared.send(Update::Progress(Some(0.8 + 0.2 * i as f32 / d.found.len().max(1) as f32)));
                found.push(modules::read_module(&mut client, f.channel, Some(f), family)?);
            }
            Ok(DiscoverySummary {
                found: d.found.len(),
                tried: d.tried,
                skipped: d.skipped,
                stopped_early: stopped,
                note,
            })
        })();
        self.client = Some(client);
        for mut m in found {
            self.name_from_conn(&mut m);
            self.send(Update::Module(Box::new(m.clone())));
            self.modules.push(m);
        }
        self.send(Update::Progress(None));
        self.back_to_listening();
        let summary = result?;
        self.write_modules_file();
        self.send(Update::Discovery(summary));
        Ok(())
    }

    fn read_one_module(&mut self, ch: Channel) -> Result<(), CoreError> {
        let family = self.family();
        self.open_for_sending()?;
        let m = modules::read_module(self.client()?, ch, None, family);
        self.back_to_listening();
        let mut m = m?;
        self.name_from_conn(&mut m);
        match self.modules.iter_mut().find(|x| x.channel == ch) {
            Some(x) => *x = m.clone(),
            None => self.modules.push(m.clone()),
        }
        self.send(Update::Module(Box::new(m)));
        Ok(())
    }

    fn scan(&mut self, ch: Channel, kind: DataKind, from: u16, to: u16) -> Result<(), CoreError> {
        let (from, to) = (from.min(to), from.max(to));
        self.watch = None;
        self.open_for_sending()?;
        let mut client = self.client.take().ok_or_else(|| CoreError::BadRequest("not connected".into()))?;
        let shared = self.shared.clone();
        let rx = &self.rx;
        let pending = &mut self.pending;
        let mut complete = true;
        let mut last = Instant::now() - Duration::from_secs(1);
        let result = modules::scan_data(&mut client, ch, kind, from..=to, |done, total| {
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                shared.send(Update::Busy(Some(format!("Scanning {}: {done} of {total}", ch.label()))));
                shared.send(Update::Progress(Some(done as f32 / total.max(1) as f32)));
            }
            let go = !stop_requested(rx, pending);
            complete &= go;
            go
        });
        self.client = Some(client);
        self.send(Update::Progress(None));
        self.back_to_listening();
        let items = result?;
        self.send(Update::DataScan { channel: ch, kind, items, complete });
        Ok(())
    }

    fn console(&mut self, ch: Channel, request: Vec<u8>) -> Result<(), CoreError> {
        self.open_for_sending()?;
        let result = modules::console_request(self.client()?, ch, &request);
        self.back_to_listening();
        let reply = match result {
            Ok(r) => Ok(r),
            Err(e) if modules::is_fatal(&e) && !matches!(e, CoreError::Unsafe(_)) => return Err(e),
            Err(e) => Err(e.to_string()),
        };
        self.send(Update::ConsoleReply { channel: ch, request, reply });
        Ok(())
    }

    /// Reads the next watched data ID, if watching. Returns false when nothing was due.
    fn watch_step(&mut self) -> Option<Result<(), CoreError>> {
        let (spec, idx) = self.watch.as_mut()?;
        if spec.ids.is_empty() {
            return None;
        }
        let id = spec.ids[*idx % spec.ids.len()];
        *idx = (*idx + 1) % spec.ids.len();
        let (ch, kind) = (spec.channel, spec.kind);
        if let Err(e) = self.open_for_sending() {
            return Some(Err(e));
        }
        let client = self.client.as_mut()?;
        match modules::read_data(client, ch, kind, id) {
            Ok(bytes) => {
                let t_ms = self.shared.app_ms(client.now_ms());
                self.send(Update::DataValue { channel: ch, kind, id, bytes, t_ms });
                Some(Ok(()))
            }
            Err(e) if modules::is_fatal(&e) => Some(Err(e)),
            Err(_) => Some(Ok(())),
        }
    }

    /// Saves what discovery found in the session folder, when recording.
    fn write_modules_file(&mut self) {
        if self.modules.is_empty() {
            return;
        }
        let mut t = String::from("# Modules found by Scana's module search. Codes as read at the time.\n");
        for m in &self.modules {
            t.push_str(&format!(
                "\n[[module]]\naddress = \"{}\"\nreply = \"{}\"\nname = \"{}\"\ndialect = \"{}\"\n",
                m.channel.label(),
                m.channel.rx,
                m.display_name().replace('"', "'"),
                m.dialect.name()
            ));
            for (k, v) in &m.identity {
                if k == "VIN" {
                    continue; // kept out of files that might be shared
                }
                t.push_str(&format!("identity.\"{}\" = \"{}\"\n", k.replace('"', "'"), v.replace('"', "'")));
            }
            match &m.codes {
                Ok(c) => t.push_str(&format!(
                    "codes = [{}]\n",
                    c.iter().map(|d| format!("\"{d} {}\"", d.state().words())).collect::<Vec<_>>().join(", ")
                )),
                Err(e) => t.push_str(&format!("codes_error = \"{}\"\n", e.replace('"', "'"))),
            }
        }
        if let Ok(mut g) = self.shared.rec.lock()
            && let Some(r) = g.as_mut()
        {
            let _ = r.write_file("modules.toml", &t);
        }
    }

    fn one_off(&mut self, what: &str, f: impl FnOnce(&mut Worker) -> Result<(), CoreError>) {
        if self.client.is_none() {
            self.send(Update::Error("Not connected to a car.".into()));
            return;
        }
        self.send(Update::Busy(Some(format!("{what}..."))));
        let r = f(self);
        self.send(Update::Busy(None));
        if let Err(e) = r {
            self.on_error(e, false);
        }
    }

    fn ecus(&self) -> Vec<(CanId, Vec<u8>)> {
        self.conn.as_ref().map(|c| c.ecus.iter().map(|e| (e.id, e.supported.clone())).collect()).unwrap_or_default()
    }

    fn open_link(source: &Source) -> Result<Box<dyn Link>, LinkError> {
        Ok(match source {
            Source::Bridge { port, .. } => Box::new(SlcanLink::connect(port.as_deref())?),
            Source::Demo => Box::new(SimLink::new(VirtualCar::hijet().with_dash())),
            Source::DemoCorolla => Box::new(SimLink::new(VirtualCar::corolla_2006())),
            Source::SimCar { name, engine_off, bus } => {
                let mut car = VirtualCar::by_name(name).ok_or_else(|| LinkError::Open {
                    path: format!("simulated car {name}"),
                    reason: format!("no such car. Try one of: {}", VirtualCar::CAR_NAMES.join(", ")),
                })?;
                if *engine_off {
                    car.set_engine_running(false);
                }
                Box::new(SimLink::new(car).on_bus(*bus))
            }
            Source::DemoFaults => {
                let mut car = VirtualCar::hijet_with_faults();
                car.set_engine_running(false);
                Box::new(SimLink::new(car))
            }
        })
    }

    fn connect(&mut self, source: Source) {
        self.disconnect(false);
        self.source = Some(source.clone());
        self.send(Update::State(LinkState::Connecting(match source {
            Source::Bridge { .. } => "Opening the bridge (the Uno resets: about 2 s)".into(),
            _ => "Starting the demo car".into(),
        })));
        self.started = Instant::now();
        match Self::open_link(&source) {
            Ok(link) => {
                let mut client = Client::new(link);
                self.attach(&mut client);
                self.client = Some(client);
                self.find_car(true);
            }
            Err(e) => {
                self.send(Update::State(LinkState::Failed(e.to_string())));
                self.send(Update::Error(e.to_string()));
            }
        }
    }

    /// Detects the car on the open link. `first`: a new connection (else a recovery).
    fn find_car(&mut self, first: bool) {
        // Take the client out while detecting, so errors can be handled with `self` free.
        let Some(mut client) = self.client.take() else { return };
        self.send(Update::State(LinkState::Connecting("Looking for the car".into())));
        let found =
            Self::detect_and_describe(&mut client, self.source.clone().unwrap_or(Source::Demo), self.make_hint.clone());
        self.client = Some(client);
        let (info, profile_live) = match found {
            Ok(x) => x,
            Err(CoreError::NoCar(msg)) => {
                if first {
                    self.send(Update::Message(msg));
                }
                self.lost = Some((Lost::Car, Instant::now() + Duration::from_secs(5)));
                self.send(Update::State(LinkState::Lost(
                    "No car answered yet. Is the OBD plug in and the key on? Trying again every 5 s.".into(),
                )));
                return;
            }
            Err(e) => {
                self.on_error(e, first);
                // Keep trying after an error the handler doesn't retry by itself.
                if !first && self.lost.is_some_and(|(_, at)| at <= Instant::now()) {
                    self.lost = Some((Lost::Car, Instant::now() + Duration::from_secs(5)));
                }
                return;
            }
        };
        if !first {
            self.shared.note(&format!(
                "found the car again: {}, {} ECU{}{}",
                info.vehicle,
                info.ecus.len(),
                if info.ecus.len() == 1 { "" } else { "s" },
                if info.other_tester { ", another tester is on the bus" } else { "" }
            ));
        }
        self.lost = None;
        self.misses = 0;
        if self.bus() == Bus::Second {
            // A new look at the bus: the parked tick is asked for again.
            self.parked = false;
            if !first {
                self.watch = None;
            }
        }
        if first || self.live.is_empty() {
            self.live = profile_live;
        }
        let now = self.client.as_ref().map_or(0, Client::now_ms);
        self.poller.set(&self.live, now);
        let mut info = info;
        info.live = self.live.clone();
        self.conn = Some(info.clone());
        self.send(Update::Connected(info));
        self.send(Update::State(LinkState::Connected));
        // One request, so the top bar can show the check engine light straight away. Not on the
        // second bus: nothing there is asked an OBD-II question.
        if self.bus() == Bus::Main
            && let Err(e) = self.read_readiness()
        {
            self.on_error(e, false);
            return;
        }
        if self.record_wanted && self.shared.rec.lock().is_ok_and(|r| r.is_none()) {
            self.start_recording();
        }
    }

    /// Finds the car, its ECUs and their supported PIDs, and the matching profile.
    fn detect_and_describe(
        client: &mut Client,
        source: Source,
        make_hint: Option<String>,
    ) -> Result<(ConnectionInfo, Vec<(CanId, u8)>), CoreError> {
        if source.bus() == Bus::Second {
            // Listen only: no OBD-II ECUs, no VIN, no live values.
            let d = detect_second_bus(client, SECOND_BUS_LISTEN_MS)?;
            let info = ConnectionInfo {
                source,
                bus: Bus::Second,
                link: client.link().describe(),
                bitrate: d.bitrate,
                addressing: d.addressing,
                profile_id: None,
                vehicle: SECOND_BUS_LABEL.into(),
                make: make_hint,
                other_tester: d.other_tester,
                log: d.log,
                ecus: vec![],
                live: vec![],
            };
            return Ok((info, vec![]));
        }
        let detected = detect(client, &DetectOptions::default())?;
        let mut ecus = Vec::new();
        for id in &detected.ecus {
            let supported = readers::supported_pids(client, *id)?;
            ecus.push(EcuView { id: *id, name: report::ecu_default_name(*id), supported });
        }
        let pid_lists: Vec<(CanId, Vec<u8>)> = ecus.iter().map(|e| (e.id, e.supported.clone())).collect();
        let id = readers::identify(client, &pid_lists)?;
        let profile = id.profile;
        if let Some(p) = profile {
            for e in &mut ecus {
                if let Some(n) = p.ecu_name(u16::try_from(e.id.raw()).unwrap_or(0)) {
                    e.name = n.to_string();
                }
            }
        }
        let info = ConnectionInfo {
            source,
            bus: Bus::Main,
            link: client.link().describe(),
            bitrate: detected.bitrate,
            addressing: detected.addressing,
            profile_id: profile.map(|p| p.id.clone()),
            vehicle: id.name.clone(),
            make: id.make.clone(),
            other_tester: detected.other_tester,
            log: detected.log,
            ecus,
            live: vec![],
        };
        let live = default_live(&info, profile);
        Ok((info, live))
    }

    fn disconnect(&mut self, tell: bool) {
        self.stop_recording();
        self.client = None;
        self.conn = None;
        self.lost = None;
        self.modules.clear();
        self.watch = None;
        self.parked = false;
        if tell {
            self.source = None;
            self.send(Update::State(LinkState::Disconnected));
        }
    }

    fn on_error(&mut self, e: CoreError, during_connect: bool) {
        self.shared.note(&format!("error: {e}"));
        match e {
            CoreError::Link(LinkError::Disconnected | LinkError::NoAnswer(_) | LinkError::Io(_))
            | CoreError::BridgeReset => {
                self.client = None;
                self.lost = Some((Lost::Link, Instant::now() + Duration::from_secs(2)));
                self.send(Update::State(LinkState::Lost(
                    "The bridge was unplugged or restarted. Reconnecting every 2 s.".into(),
                )));
            }
            // Mid-session, a bridge that refuses frames or answers out of turn has almost
            // certainly restarted (its bus is closed). Open it again rather than keep failing.
            CoreError::Link(LinkError::Refused(_) | LinkError::Unexpected(_)) if !during_connect => {
                self.client = None;
                self.lost = Some((Lost::Link, Instant::now() + Duration::from_secs(2)));
                self.send(Update::State(LinkState::Lost(
                    "The bridge stopped taking frames (it probably restarted). Reconnecting every 2 s.".into(),
                )));
            }
            CoreError::NotAcknowledged | CoreError::WatchdogClosed => {
                if let Some(c) = self.client.as_mut() {
                    let _ = c.link().close();
                }
                self.lost = Some((Lost::Car, Instant::now() + Duration::from_secs(5)));
                self.send(Update::State(LinkState::Lost(
                    "No answer from the car: is the key on? Looking again every 5 s.".into(),
                )));
            }
            CoreError::BusOff => {
                if let Some(c) = self.client.as_mut() {
                    let _ = c.link().close();
                }
                self.lost = None;
                let msg = "Bus-off: the CAN controller saw too many errors and stopped. Scana stopped sending. Check the cable, then connect again.";
                self.send(Update::State(LinkState::Failed(msg.into())));
                self.send(Update::Error(msg.into()));
            }
            other => {
                let msg = match other {
                    CoreError::NeedsParked => NEEDS_PARKED_APP.to_string(),
                    other => other.to_string(),
                };
                if during_connect {
                    self.send(Update::State(LinkState::Failed(msg.clone())));
                }
                self.send(Update::Error(msg));
            }
        }
    }

    fn try_recover(&mut self) {
        let Some((kind, _)) = self.lost else { return };
        let Some(source) = self.source.clone() else { return };
        if kind == Lost::Link || self.client.is_none() {
            match Self::open_link(&source) {
                Ok(link) => {
                    let mut client = Client::new(link);
                    self.attach(&mut client);
                    // The new link's clock starts at 0: keep the recording's times going forward.
                    if let Ok(mut r) = self.shared.rec.lock()
                        && let Some(r) = r.as_mut()
                    {
                        r.rebase(client.now_ms());
                        // Opening the port resets the Uno, so this report is about that reset;
                        // its warm start count still shows how many resets came before.
                        let started =
                            client.link().start_report().map_or_else(String::new, |s| format!(" (bridge start: {s})"));
                        r.event(&format!("opened the link again: {}{started}", client.link().describe()));
                    }
                    self.client = Some(client);
                }
                Err(_) => {
                    self.lost = Some((Lost::Link, Instant::now() + Duration::from_secs(2)));
                    return;
                }
            }
        }
        self.find_car(false);
    }

    fn tick(&mut self) {
        if let Some((_, at)) = self.lost {
            if Instant::now() >= at {
                self.try_recover();
            }
            return;
        }
        // The data explorer's watch takes turns with the live values.
        if self.watch.is_some() && (self.watch_turn || self.live.is_empty()) {
            self.watch_turn = false;
            if let Some(r) = self.watch_step() {
                if let Err(e) = r {
                    self.on_error(e, false);
                }
                self.maybe_stats();
                return;
            }
        }
        self.watch_turn = true;
        let Some(client) = self.client.as_mut() else { return };
        let now = client.now_ms();
        self.poller.one_frame = client.other_tester_active();
        let result = match self.poller.next_batch(now) {
            Some((ecu, pids)) => readers::read_pids(client, ecu, &pids).map(|r| Some((ecu, pids, r))),
            None => {
                let until = self.poller.next_due().map_or(now + 20, |d| d.clamp(now + 1, now + 20));
                client.idle_until(until).map(|_| None)
            }
        };
        match result {
            Ok(Some((ecu, _, readings))) => {
                self.misses = 0;
                let t = self.client.as_ref().map_or(0, Client::now_ms);
                if let Ok(mut r) = self.shared.rec.lock()
                    && let Some(r) = r.as_mut()
                {
                    for x in &readings {
                        let main = x.decoded.as_ref().and_then(|d| d.main());
                        let text = x.decoded.as_ref().and_then(|d| d.text.clone()).unwrap_or_default();
                        r.value(t, ecu, x.pid, x.name, main.map(|q| q.value), main.map_or("", |q| q.unit), &text);
                    }
                }
                let t_app = self.shared.app_ms(t);
                let values = readings.into_iter().map(|reading| LiveValue { ecu, t_ms: t_app, reading }).collect();
                self.send(Update::Values(values));
            }
            Ok(None) => {}
            Err(CoreError::NoReply(_)) => {
                // One missed reply happens (for example a clash with another tester).
                self.misses += 1;
                if self.misses >= 3 {
                    self.misses = 0;
                    self.on_error(CoreError::NotAcknowledged, false);
                }
            }
            Err(CoreError::Refused { code, .. }) => {
                // A PID the car turned down: stop asking for that batch's values.
                self.send(Update::Message(format!(
                    "The car refused some live values ({}); Scana stopped asking for them.",
                    obd::negative_code_text(code)
                )));
                let now = self.client.as_ref().map_or(0, Client::now_ms);
                if let Some((ecu, pids)) = self.poller.next_batch(now) {
                    self.live.retain(|(e, p)| !(*e == ecu && pids.contains(p)));
                }
                self.poller.set(&self.live, now);
            }
            Err(e) => self.on_error(e, false),
        }
        self.maybe_stats();
    }

    fn maybe_stats(&mut self) {
        let elapsed = self.stats_at.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let Some(c) = self.client.as_ref() else { return };
        let frames = self.shared.frames.load(Ordering::Relaxed);
        let s = Stats {
            requests_per_s: (c.requests_sent - self.stats_requests.min(c.requests_sent)) as f64 / elapsed.as_secs_f64(),
            frames_per_s: (frames - self.stats_frames.min(frames)) as f64 / elapsed.as_secs_f64(),
            other_tester: c.other_tester_active(),
            bus_errors: self.bus_errors,
        };
        self.stats_requests = c.requests_sent;
        self.stats_frames = frames;
        self.stats_at = Instant::now();
        self.send(Update::Stats(s));
    }

    fn client(&mut self) -> Result<&mut Client, CoreError> {
        self.client.as_mut().ok_or_else(|| CoreError::BadRequest("not connected".into()))
    }

    fn read_codes(&mut self) -> Result<(), CoreError> {
        let c = self.client()?;
        let stored = readers::read_codes(c, obd::STORED_CODES)?;
        let pending = readers::read_codes(c, obd::PENDING_CODES)?;
        let permanent = readers::read_codes(c, obd::PERMANENT_CODES).unwrap_or_default();
        let mut sets: Vec<CodeSet> = Vec::new();
        for (ecu, _) in self.ecus() {
            let get =
                |l: &[(CanId, Vec<Dtc>)]| l.iter().find(|(e, _)| *e == ecu).map(|(_, c)| c.clone()).unwrap_or_default();
            sets.push(CodeSet {
                ecu,
                stored: get(&stored),
                pending: get(&pending),
                permanent: get(&permanent),
                permanent_read: permanent.iter().any(|(e, _)| *e == ecu),
            });
        }
        self.send(Update::Codes(sets));
        Ok(())
    }

    fn read_freeze(&mut self) -> Result<(), CoreError> {
        let mut out = Vec::new();
        for (ecu, _) in self.ecus() {
            if let Some(f) = readers::freeze_frame(self.client()?, ecu)? {
                out.push(f);
            }
        }
        self.send(Update::Freeze(out));
        Ok(())
    }

    fn read_readiness(&mut self) -> Result<(), CoreError> {
        let mut out = Vec::new();
        for (ecu, supported) in self.ecus() {
            if supported.contains(&0x01) {
                out.push((ecu, readers::readiness(self.client()?, ecu)?));
            }
        }
        self.send(Update::Readiness(out));
        Ok(())
    }

    fn read_tests(&mut self) -> Result<(), CoreError> {
        let mut out = Vec::new();
        for (ecu, _) in self.ecus() {
            out.push((ecu, readers::test_results(self.client()?, ecu)?));
        }
        self.send(Update::Tests(out));
        Ok(())
    }

    fn read_info(&mut self) -> Result<(), CoreError> {
        let mut out = Vec::new();
        for (ecu, _) in self.ecus() {
            out.push((ecu, readers::vehicle_info(self.client()?, ecu)?));
        }
        self.send(Update::Info(out));
        Ok(())
    }

    fn start_recording(&mut self) {
        let Some(conn) = self.conn.clone() else { return };
        let now = self.client.as_ref().map_or(0, Client::now_ms);
        let mut guard = match self.shared.rec.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_some() {
            return;
        }
        match Recorder::start(&self.settings.sessions_dir, &conn.vehicle, &conn.link, now) {
            Ok(mut r) => {
                r.summary = SessionSummary {
                    bitrate: conn.bitrate.to_string(),
                    addressing: conn.addressing.name().into(),
                    ecus: conn.ecus.iter().map(|e| format!("{} {}", e.id, e.name)).collect(),
                    profile: conn.profile_id.clone(),
                    ..r.summary.clone()
                };
                r.event(&format!(
                    "recording started: {}, {}, {} {}{}",
                    conn.vehicle,
                    conn.link,
                    conn.bitrate,
                    conn.addressing.name(),
                    if conn.other_tester { ", another tester is on the bus" } else { "" }
                ));
                let dir = r.dir().to_path_buf();
                *guard = Some(r);
                drop(guard);
                self.send(Update::Recording(Some(dir)));
            }
            Err(e) => {
                drop(guard);
                self.send(Update::Error(format!(
                    "Can't start recording in {}: {e}",
                    self.settings.sessions_dir.display()
                )));
            }
        }
    }

    fn stop_recording(&mut self) {
        self.stop_recording_quietly(false);
    }

    /// Ends the session. `quiet`: a health check's own short session, already announced by its report.
    fn stop_recording_quietly(&mut self, quiet: bool) {
        let rec = self.shared.rec.lock().ok().and_then(|mut g| g.take());
        if let Some(r) = rec {
            match r.finish() {
                Ok(dir) if !quiet => self.send(Update::Message(format!("Session saved in {}", dir.display()))),
                Ok(_) => {}
                Err(e) => self.send(Update::Error(format!("The session's summary couldn't be written: {e}"))),
            }
            self.send(Update::Recording(None));
        }
    }

    /// Runs the health check and saves its report in the session folder (a short session is
    /// recorded for it if recording is off).
    fn health_check(&mut self) -> Result<Option<PathBuf>, CoreError> {
        let Some(conn) = self.conn.clone() else { return Err(CoreError::BadRequest("not connected".into())) };
        let temporary = self.shared.rec.lock().is_ok_and(|r| r.is_none());
        if temporary {
            self.start_recording();
        }
        let profile = conn.profile_id.as_ref().and_then(|id| scana_db::profile::builtin().iter().find(|p| &p.id == id));
        let ecus = self.ecus();
        let mut result = run_report(self.client()?, &ecus, profile, &conn);
        // Every module found this connection: read its codes again for the report.
        if result.is_ok() && !self.modules.is_empty() {
            let mut mods = self.modules.clone();
            let mut fatal = None;
            for m in &mut mods {
                m.codes = match modules::read_codes(self.client()?, m.channel, m.dialect) {
                    Ok(c) => Ok(c),
                    Err(e) if modules::is_fatal(&e) => {
                        fatal = Some(e);
                        break;
                    }
                    Err(e) => Err(e.to_string()),
                };
            }
            match fatal {
                Some(e) => result = Err(e),
                None => {
                    if let Ok(report) = result.as_mut() {
                        report.modules = mods;
                    }
                }
            }
        }
        let mut path = None;
        if let Ok(report) = &result
            && let Ok(mut g) = self.shared.rec.lock()
            && let Some(r) = g.as_mut()
        {
            r.summary.codes = report.all_codes();
            path = r.write_file("report.html", &report.to_html()).ok();
        }
        if temporary {
            // Recording was off: the folder stays (it holds the report), but say nothing more.
            self.stop_recording_quietly(true);
        }
        let report = result?;
        self.send(Update::Health { report: Box::new(report), path: path.clone() });
        Ok(path)
    }

    fn clear(&mut self, consent: ClearConsent) -> Result<(), CoreError> {
        // A report first, so there is a record of what was cleared.
        let report_path = self.health_check()?;
        let ecus = self.ecus();
        let outcome = safety::clear_codes(self.client()?, &ecus, consent)?;
        self.send(Update::Cleared { outcome, report_path });
        self.read_codes()?;
        self.read_readiness()
    }
}

fn run_report(
    client: &mut Client,
    ecus: &[(CanId, Vec<u8>)],
    profile: Option<&scana_db::profile::Profile>,
    conn: &ConnectionInfo,
) -> Result<HealthReport, CoreError> {
    report::run_health_check(client, ecus, profile, &conn.vehicle, &conn.protocol())
}

/// The first live values to show: the profile's choice, or common ones the car supports.
fn default_live(info: &ConnectionInfo, profile: Option<&scana_db::profile::Profile>) -> Vec<(CanId, u8)> {
    let Some(ecu) = info.ecus.first() else { return vec![] };
    let wanted: Vec<u8> = match profile {
        Some(p) if !p.default_live.is_empty() => p.default_live.clone(),
        _ => vec![0x0C, 0x0D, 0x05, 0x42, 0x04, 0x11],
    };
    wanted.into_iter().filter(|p| ecu.supported.contains(p)).map(|p| (ecu.id, p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(dir: &std::path::Path, auto_record: bool) -> SessionHandle {
        SessionHandle::spawn(WorkerSettings { sessions_dir: dir.to_path_buf(), auto_record }, || {}).unwrap()
    }

    /// Waits for an update that `want` accepts, up to `secs`.
    fn wait_for<T>(h: &SessionHandle, secs: u64, mut want: impl FnMut(Update) -> Option<T>) -> T {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if let Some(u) = h.wait(Duration::from_millis(100)) {
                if let Update::Error(e) = &u {
                    panic!("error update: {e}");
                }
                if let Some(t) = want(u) {
                    return t;
                }
            }
        }
        panic!("timed out");
    }

    #[test]
    fn demo_connects_polls_and_records() {
        let dir = std::env::temp_dir().join(format!("scana-session-test-{}", std::process::id()));
        let h = handle(&dir, true);
        h.send(Command::Connect(Source::Demo));
        let info = wait_for(&h, 10, |u| if let Update::Connected(i) = u { Some(i) } else { None });
        assert_eq!(info.vehicle, "2024 Daihatsu Hijet Jumbo");
        assert_eq!(info.ecus.len(), 1);
        assert!(info.other_tester);
        let rec = wait_for(&h, 5, |u| if let Update::Recording(Some(p)) = u { Some(p) } else { None });
        // Live values arrive: the profile's six.
        let mut seen = std::collections::HashSet::new();
        wait_for(&h, 10, |u| {
            if let Update::Values(v) = u {
                seen.extend(v.iter().map(|x| x.reading.pid));
            }
            (seen.len() >= 6).then_some(())
        });
        h.send(Command::ReadCodes);
        let codes = wait_for(&h, 10, |u| if let Update::Codes(c) = u { Some(c) } else { None });
        assert!(codes[0].stored.is_empty());
        h.send(Command::Mark("test".into()));
        h.send(Command::Record(false));
        wait_for(&h, 5, |u| matches!(u, Update::Recording(None)).then_some(()));
        let values = std::fs::read_to_string(rec.join("values.csv")).unwrap();
        assert!(values.lines().count() > 6 && values.contains(",MARK,test"));
        assert!(rec.join("session.toml").exists());
        let events = std::fs::read_to_string(rec.join("events.log")).unwrap();
        assert!(events.contains("recording started: 2024 Daihatsu Hijet Jumbo"), "{events}");
        assert!(events.contains("another tester is on the bus"), "{events}");
        // With the demo's dash display on the bus, every live data reply fits one frame.
        let frames = std::fs::read_to_string(rec.join("frames.log")).unwrap();
        let mut asked = 0;
        for line in frames.lines().filter(|l| l.ends_with(" T") && l.contains(" 7E0#")) {
            let hex = line.split('#').nth(1).unwrap().split_whitespace().next().unwrap();
            let bytes: Vec<u8> =
                (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            if bytes[0] & 0xF0 == 0 && bytes.get(1) == Some(&obd::CURRENT_DATA) {
                asked += 1;
                assert!(obd::reply_fits_one_frame(&bytes[2..=usize::from(bytes[0])]), "{line}");
            }
        }
        assert!(asked > 3);
        h.send(Command::Disconnect);
        wait_for(&h, 5, |u| matches!(u, Update::State(LinkState::Disconnected)).then_some(()));
        drop(h);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// After a reconnect (a bridge restart mid-drive) the new link's clock starts at 0. The
    /// app's times must keep counting, or the graphs draw back over themselves.
    #[test]
    fn live_value_times_keep_counting_after_a_reconnect() {
        let dir = std::env::temp_dir().join(format!("scana-session-test-clock-{}", std::process::id()));
        let (ui, updates) = mpsc::channel();
        let shared = Arc::new(Shared {
            ui,
            wake: Arc::new(|| {}),
            rec: Mutex::new(None),
            frames: AtomicU64::new(0),
            offset_ms: AtomicU64::new(0),
        });
        let (_tx, rx) = mpsc::channel();
        let mut w = Worker::new(shared, rx, WorkerSettings { sessions_dir: dir.clone(), auto_record: false });
        w.connect(Source::Demo);
        let times = |w: &mut Worker, ticks: usize| -> Vec<u64> {
            for _ in 0..ticks {
                w.tick();
            }
            updates
                .try_iter()
                .filter_map(|u| if let Update::Values(v) = u { Some(v) } else { None })
                .flatten()
                .map(|v| v.t_ms)
                .collect()
        };
        let before = times(&mut w, 30);
        let last = *before.iter().max().expect("values before the reconnect");
        // The link drops, as after a refused send, and the worker opens a new one.
        w.client = None;
        w.lost = Some((Lost::Link, Instant::now()));
        w.try_recover();
        let after = times(&mut w, 30);
        assert!(!after.is_empty(), "values after the reconnect");
        assert!(after.iter().all(|&t| t >= last), "times went back: last {last}, after {after:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modules_are_found_read_and_reported() {
        let dir = std::env::temp_dir().join(format!("scana-session-test3-{}", std::process::id()));
        let h = handle(&dir, true);
        h.send(Command::Connect(Source::DemoFaults));
        wait_for(&h, 10, |u| matches!(u, Update::Connected(_)).then_some(()));
        h.send(Command::FindModules { parked: false });
        let summary = wait_for(&h, 90, |u| if let Update::Discovery(s) = u { Some(s) } else { None });
        assert_eq!(summary.found, 6, "{summary:?}");
        assert!(!summary.stopped_early);
        // The cluster's U0100 shows in the health report.
        h.send(Command::HealthCheck);
        let report = wait_for(&h, 30, |u| if let Update::Health { report, .. } = u { Some(report) } else { None });
        assert_eq!(report.modules.len(), 6);
        assert!(report.all_codes().iter().any(|c| c.starts_with("U0100-87")));
        assert!(report.summary().iter().any(|(_, l)| l.contains("U0100")));
        // The data explorer: scan the body module, then watch a value.
        let body = Channel::parse("750/40").unwrap();
        h.send(Command::ScanData { channel: body, kind: DataKind::Lid, from: 0, to: 8 });
        let items = wait_for(&h, 30, |u| if let Update::DataScan { items, .. } = u { Some(items) } else { None });
        assert_eq!(items.len(), 3);
        h.send(Command::Watch(Some(WatchSpec { channel: body, kind: DataKind::Lid, ids: vec![1, 2] })));
        wait_for(&h, 10, |u| matches!(u, Update::DataValue { id: 2, .. }).then_some(()));
        h.send(Command::Watch(None));
        // The console refuses a write and answers a read.
        h.send(Command::Console { channel: body, request: vec![0x3B, 0x01, 0x00] });
        let r = wait_for(&h, 10, |u| if let Update::ConsoleReply { reply, .. } = u { Some(reply) } else { None });
        assert!(r.is_err());
        h.send(Command::Console { channel: body, request: vec![0x21, 0x03] });
        let r = wait_for(&h, 10, |u| if let Update::ConsoleReply { reply, .. } = u { Some(reply) } else { None });
        assert!(matches!(r, Ok(Some(Reply::Positive { service: 0x21, .. }))));
        h.send(Command::Disconnect);
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn module_discovery_can_be_stopped() {
        let dir = std::env::temp_dir().join(format!("scana-session-test4-{}", std::process::id()));
        let h = handle(&dir, false);
        h.send(Command::Connect(Source::Demo));
        wait_for(&h, 10, |u| matches!(u, Update::Connected(_)).then_some(()));
        h.send(Command::FindModules { parked: false });
        wait_for(&h, 10, |u| matches!(u, Update::Progress(Some(_))).then_some(()));
        h.send(Command::Stop);
        let summary = wait_for(&h, 20, |u| if let Update::Discovery(s) = u { Some(s) } else { None });
        assert!(summary.stopped_early);
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn faulty_demo_health_check_and_clear() {
        let dir = std::env::temp_dir().join(format!("scana-session-test2-{}", std::process::id()));
        let h = handle(&dir, false);
        h.send(Command::Connect(Source::DemoFaults));
        wait_for(&h, 10, |u| matches!(u, Update::Connected(_)).then_some(()));
        h.send(Command::HealthCheck);
        let (report, path) =
            wait_for(&h, 20, |u| if let Update::Health { report, path } = u { Some((report, path)) } else { None });
        assert!(report.summary().iter().any(|(_, l)| l.contains("P0128")));
        let path = path.unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("P0128"));
        h.send(Command::ClearCodes(ClearConsent::confirmed_by_user()));
        let outcome = wait_for(&h, 30, |u| if let Update::Cleared { outcome, .. } = u { Some(outcome) } else { None });
        assert_eq!(outcome.cleared, vec![CanId::Standard(0x7E8)]);
        let codes = wait_for(&h, 10, |u| if let Update::Codes(c) = u { Some(c) } else { None });
        assert!(codes[0].stored.is_empty() && codes[0].pending.is_empty());
        drop(h);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // The second bus (pins 3 and 11).

    fn second_bus_source() -> Source {
        Source::SimCar { name: "ford-ms-can".into(), engine_off: false, bus: Bus::Second }
    }

    /// Every update up to `secs`, or until `done` says so (it sees the updates so far).
    fn collect(h: &SessionHandle, secs: u64, done: impl Fn(&[Update]) -> bool) -> Vec<Update> {
        let mut all = Vec::new();
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end && !done(&all) {
            if let Some(u) = h.wait(Duration::from_millis(100)) {
                all.push(u);
            }
        }
        all
    }

    fn outgoing(updates: &[Update]) -> Vec<Frame> {
        updates
            .iter()
            .filter_map(|u| if let Update::Frame { frame, outgoing: true, .. } = u { Some(*frame) } else { None })
            .collect()
    }

    fn errors(updates: &[Update]) -> Vec<String> {
        updates.iter().filter_map(|u| if let Update::Error(e) = u { Some(e.clone()) } else { None }).collect()
    }

    fn connect_second_bus(h: &SessionHandle) -> ConnectionInfo {
        h.send(Command::Connect(second_bus_source()));
        wait_for(h, 15, |u| if let Update::Connected(i) = u { Some(i) } else { None })
    }

    #[test]
    fn the_second_bus_connects_by_listening_and_does_nothing_obd() {
        let dir = std::env::temp_dir().join(format!("scana-session-second1-{}", std::process::id()));
        let h = handle(&dir, true);
        let info = connect_second_bus(&h);
        assert_eq!((info.bus, info.bitrate), (Bus::Second, Bitrate::K125));
        assert!(info.ecus.is_empty() && info.live.is_empty() && info.make.is_none());
        assert_eq!(info.vehicle, "Second CAN bus (pins 3 and 11)");
        assert!(info.protocol().contains("pins 3 and 11"), "{}", info.protocol());
        // A few seconds of life: frames come in, and nothing at all goes out. No values, no
        // readiness, no OBD-II of any kind, though the app asks for live values.
        h.send(Command::SetLive(vec![(CanId::Standard(0x7E8), 0x0C)]));
        let seen = collect(&h, 3, |_| false);
        assert!(errors(&seen).is_empty(), "{:?}", errors(&seen));
        assert!(seen.iter().any(|u| matches!(u, Update::Frame { outgoing: false, .. })));
        assert!(outgoing(&seen).is_empty(), "{:?}", outgoing(&seen));
        assert!(!seen.iter().any(|u| matches!(u, Update::Values(_) | Update::Readiness(_))));
        h.send(Command::Disconnect);
        drop(h);
        // The recording holds the frames and the events, and no values.
        let session = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).next().unwrap().path();
        let frames = std::fs::read_to_string(session.join("frames.log")).unwrap();
        assert!(!frames.is_empty() && !frames.lines().any(|l| l.ends_with(" T")));
        assert!(std::fs::read_to_string(session.join("events.log")).unwrap().contains("recording started"));
        let values = std::fs::read_to_string(session.join("values.csv")).unwrap_or_default();
        assert!(values.lines().count() <= 1, "{values}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn obd_commands_are_refused_on_the_second_bus() {
        let dir = std::env::temp_dir().join(format!("scana-session-second2-{}", std::process::id()));
        let h = handle(&dir, false);
        connect_second_bus(&h);
        for c in [
            Command::ReadCodes,
            Command::ReadFreeze,
            Command::ReadReadiness,
            Command::ReadTests,
            Command::ReadInfo,
            Command::HealthCheck,
            Command::ClearCodes(ClearConsent::confirmed_by_user()),
        ] {
            h.send(c);
        }
        let seen = collect(&h, 10, |u| errors(u).len() >= 7);
        assert_eq!(errors(&seen), vec![NEEDS_MAIN_BUS.to_string(); 7]);
        assert_eq!(NEEDS_MAIN_BUS, CoreError::NeedsMainBus.to_string());
        assert!(!seen.iter().any(|u| matches!(u, Update::Cleared { .. } | Update::Codes(_) | Update::Health { .. })));
        assert!(outgoing(&seen).is_empty());
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn module_requests_on_the_second_bus_need_the_parked_tick() {
        let dir = std::env::temp_dir().join(format!("scana-session-second3-{}", std::process::id()));
        let h = handle(&dir, false);
        connect_second_bus(&h);
        // Without the tick: refused, and nothing is sent.
        let cluster = Channel::parse("720").unwrap();
        h.send(Command::FindModules { parked: false });
        h.send(Command::ReadModule(cluster));
        h.send(Command::Console { channel: cluster, request: vec![0x22, 0xF1, 0x87] });
        let seen = collect(&h, 10, |u| errors(u).len() >= 3);
        assert_eq!(errors(&seen), vec![NEEDS_PARKED_APP.to_string(); 3]);
        assert!(!seen.iter().any(|u| matches!(u, Update::DiscoveryStarted | Update::Module(_))));
        assert!(outgoing(&seen).is_empty());
        // With it, and a make: the modules are found, and named from the second-bus entries.
        h.send(Command::SetMake(Some("Ford".into())));
        h.send(Command::Disconnect);
        let info = connect_second_bus(&h);
        assert_eq!(info.make.as_deref(), Some("Ford"));
        h.send(Command::FindModules { parked: true });
        let seen = collect(&h, 90, |u| u.iter().any(|x| matches!(x, Update::Discovery(_))));
        assert!(errors(&seen).is_empty(), "{:?}", errors(&seen));
        let summary = seen.iter().find_map(|u| if let Update::Discovery(s) = u { Some(s.clone()) } else { None });
        assert_eq!(summary.map(|s| s.found), Some(7));
        let named: Vec<String> =
            seen.iter().filter_map(|u| if let Update::Module(m) = u { Some(m.display_name()) } else { None }).collect();
        assert!(named.iter().any(|n| n == "FG Falcon instrument cluster (probably)"), "{named:?}");
        assert!(!named.iter().any(|n| n.contains("Engine computer")), "{named:?}");
        // Whatever was sent, none of it was an OBD-II request.
        let sent = outgoing(&seen);
        assert!(!sent.is_empty());
        assert!(sent.iter().all(|f| !crate::client::Addressing::is_request_id(f.id)), "{sent:?}");
        // The tick holds for later reads on this connection.
        h.send(Command::ReadModule(cluster));
        let seen = collect(&h, 10, |u| u.iter().any(|x| matches!(x, Update::Module(_))));
        assert!(errors(&seen).is_empty());
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quiet_second_bus_fails_with_the_plain_message() {
        let dir = std::env::temp_dir().join(format!("scana-session-second4-{}", std::process::id()));
        let h = handle(&dir, false);
        // A car with nothing on pins 3 and 11: the hybrid's bus is on the main pins.
        h.send(Command::Connect(Source::SimCar { name: "hybrid".into(), engine_off: false, bus: Bus::Second }));
        let seen = collect(&h, 20, |u| !errors(u).is_empty());
        assert_eq!(errors(&seen), vec![CoreError::NothingHeard.to_string()]);
        assert!(seen.iter().any(
            |u| matches!(u, Update::State(LinkState::Failed(m)) if m.starts_with("Nothing heard on pins 3 and 11"))
        ));
        assert!(outgoing(&seen).is_empty());
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
