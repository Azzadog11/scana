//! The session worker: owns the link and the client on its own thread, so the screen never
//! waits on the car. The app sends [`Command`]s and receives [`Update`]s.
//!
//! It polls live values, runs one-off reads (codes, freeze frames, readiness, tests, vehicle
//! info), the health check and the clear-codes gate, records sessions, and recovers by itself:
//! a pulled USB cable or a bridge reset reconnects every 2 s; a silent car (key off) is looked
//! for again every 5 s.

use crate::client::{Addressing, Client, CoreError, FrameSink};
use crate::detect::{DetectOptions, detect};
use crate::poller::Poller;
use crate::readers::{self, FreezeFrame, PidReading, VehicleInfo};
use crate::recorder::{Recorder, SessionSummary};
use crate::report::{self, HealthReport};
use crate::safety::{self, ClearConsent, ClearOutcome};
use scana_link::serial::SlcanLink;
use scana_link::sim::SimLink;
use scana_link::{Link, LinkError};
use scana_proto::mode06::TestResult;
use scana_proto::pids::MonitorStatus;
use scana_proto::{Bitrate, CanId, Dtc, Frame, obd};
use scana_sim::VirtualCar;
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
    /// The scana-bridge on USB; `None` finds the port.
    Bridge { port: Option<String> },
    /// The simulated Hijet, engine running, with a dash display polling the same port.
    Demo,
    /// The simulated Hijet with faults, key on and engine off, so clearing codes can be tried.
    DemoFaults,
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Bridge { port: Some(p) } => format!("Scana bridge on {p}"),
            Source::Bridge { port: None } => "Scana bridge (USB)".into(),
            Source::Demo => "Demo car".into(),
            Source::DemoFaults => "Demo car with faults".into(),
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
    Shutdown,
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
    pub link: String,
    pub bitrate: Bitrate,
    pub addressing: Addressing,
    pub ecus: Vec<EcuView>,
    pub profile_id: Option<String>,
    pub vehicle: String,
    pub other_tester: bool,
    pub log: Vec<String>,
    /// The live values being polled right after connecting: (ECU, PID).
    pub live: Vec<(CanId, u8)>,
}

impl ConnectionInfo {
    pub fn protocol(&self) -> String {
        format!("ISO 15765-4 CAN, {}, {}", self.addressing.name(), self.bitrate)
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
}

impl Shared {
    fn send(&self, u: Update) {
        let _ = self.ui.send(u);
        (self.wake)();
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
        self.0.send(Update::Frame { frame: *frame, at_ms, outgoing });
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
        let shared = Arc::new(Shared { ui, wake: Arc::new(wake), rec: Mutex::new(None), frames: AtomicU64::new(0) });
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
        }
    }

    fn send(&self, u: Update) {
        self.shared.send(u);
    }

    fn run(mut self) {
        loop {
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
                self.live = list;
                let now = self.client.as_ref().map_or(0, Client::now_ms);
                self.poller.set(&self.live, now);
            }
            Command::ReadCodes => self.one_off("Reading fault codes", Worker::read_codes),
            Command::ReadFreeze => self.one_off("Reading freeze frames", Worker::read_freeze),
            Command::ReadReadiness => self.one_off("Reading readiness", Worker::read_readiness),
            Command::ReadTests => self.one_off("Reading test results", Worker::read_tests),
            Command::ReadInfo => self.one_off("Reading vehicle information", Worker::read_info),
            Command::HealthCheck => self.one_off("Running the health check", |w| w.health_check().map(|_| ())),
            Command::ClearCodes(consent) => self.one_off("Clearing codes", move |w| w.clear(consent)),
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
            Command::Shutdown => {}
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
            Source::Bridge { port } => Box::new(SlcanLink::connect(port.as_deref())?),
            Source::Demo => Box::new(SimLink::new(VirtualCar::hijet().with_dash())),
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
        match Self::open_link(&source) {
            Ok(link) => {
                let mut client = Client::new(link);
                client.set_sink(Box::new(WorkerSink(self.shared.clone())));
                client.options.min_gap_ms = self.gaps.0;
                client.options.shared_gap_ms = self.gaps.1;
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
        let found = Self::detect_and_describe(&mut client, self.source.clone().unwrap_or(Source::Demo));
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
                self.on_error(e, true);
                return;
            }
        };
        self.lost = None;
        self.misses = 0;
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
        // One request, so the top bar can show the check engine light straight away.
        if let Err(e) = self.read_readiness() {
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
    ) -> Result<(ConnectionInfo, Vec<(CanId, u8)>), CoreError> {
        let detected = detect(client, &DetectOptions::default())?;
        let mut ecus = Vec::new();
        for id in &detected.ecus {
            let supported = readers::supported_pids(client, *id)?;
            ecus.push(EcuView { id: *id, name: report::ecu_default_name(*id), supported });
        }
        let profile = ecus.first().and_then(|e| scana_db::profile::find_match(&e.supported));
        if let Some(p) = profile {
            for e in &mut ecus {
                if let Some(n) = p.ecu_name(u16::try_from(e.id.raw()).unwrap_or(0)) {
                    e.name = n.to_string();
                }
            }
        }
        let info = ConnectionInfo {
            source,
            link: client.link().describe(),
            bitrate: detected.bitrate,
            addressing: detected.addressing,
            profile_id: profile.map(|p| p.id.clone()),
            vehicle: profile.map_or_else(|| "Unknown car".to_string(), |p| p.name.clone()),
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
        if tell {
            self.source = None;
            self.send(Update::State(LinkState::Disconnected));
        }
    }

    fn on_error(&mut self, e: CoreError, during_connect: bool) {
        match e {
            CoreError::Link(LinkError::Disconnected | LinkError::NoAnswer(_) | LinkError::Io(_))
            | CoreError::BridgeReset => {
                self.client = None;
                self.lost = Some((Lost::Link, Instant::now() + Duration::from_secs(2)));
                self.send(Update::State(LinkState::Lost(
                    "The bridge was unplugged or restarted. Reconnecting every 2 s.".into(),
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
                let msg = other.to_string();
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
                    client.set_sink(Box::new(WorkerSink(self.shared.clone())));
                    client.options.min_gap_ms = self.gaps.0;
                    client.options.shared_gap_ms = self.gaps.1;
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
        let Some(client) = self.client.as_mut() else { return };
        let now = client.now_ms();
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
                let values = readings.into_iter().map(|reading| LiveValue { ecu, t_ms: t, reading }).collect();
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
            sets.push(CodeSet { ecu, stored: get(&stored), pending: get(&pending), permanent: get(&permanent) });
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
        let result = run_report(self.client()?, &ecus, profile, &conn);
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
        h.send(Command::Disconnect);
        wait_for(&h, 5, |u| matches!(u, Update::State(LinkState::Disconnected)).then_some(()));
        drop(h);
        std::fs::remove_dir_all(&dir).unwrap();
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
}
