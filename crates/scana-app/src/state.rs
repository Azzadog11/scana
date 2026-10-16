//! What the app knows: the latest of everything the session worker sent, kept for drawing.

use scana_core::Channel;
use scana_core::modules::{DataKind, ModuleInfo};
use scana_core::readers::{FreezeFrame, PidReading, VehicleInfo};
use scana_core::report::HealthReport;
use scana_core::safety::ClearOutcome;
use scana_core::session::{CodeSet, ConnectionInfo, DiscoverySummary, LinkState, Stats, Update};
use scana_db::profile::{self, Profile};
use scana_proto::mode06::TestResult;
use scana_proto::obd::Reply;
use scana_proto::pids::MonitorStatus;
use scana_proto::{CanId, Frame};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

/// Points kept per value for the graphs: 10 minutes at 5 Hz.
pub const HISTORY_POINTS: usize = 3000;
/// Frames kept for the bus monitor.
pub const FRAME_ROWS: usize = 2000;

#[derive(Clone, Debug)]
pub struct Live {
    pub reading: PidReading,
    pub t_ms: u64,
    times: VecDeque<u64>,
}

impl Live {
    /// Updates a second, from the last few arrivals.
    pub fn rate_hz(&self) -> f64 {
        match (self.times.front(), self.times.back()) {
            (Some(a), Some(b)) if b > a && self.times.len() > 1 => {
                (self.times.len() - 1) as f64 * 1000.0 / (b - a) as f64
            }
            _ => 0.0,
        }
    }

    pub fn value(&self) -> Option<f64> {
        self.reading.decoded.as_ref().and_then(|d| d.main()).map(|q| q.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explore_items_count_changes_per_byte() {
        let mut i = ExploreItem::new(vec![1, 2, 3]);
        i.update(vec![1, 9, 3], 100);
        i.update(vec![1, 8, 4], 200);
        assert_eq!(i.changes, vec![0, 2, 1]);
        assert_eq!(i.changed_at, vec![None, Some(200), Some(200)]);
        assert_eq!(i.baseline, vec![1, 2, 3]);
        i.reset();
        assert_eq!(i.changes, vec![0, 0, 0]);
        assert_eq!(i.baseline, vec![1, 8, 4]);
        // A longer reply grows the counters.
        i.update(vec![1, 8, 4, 7], 300);
        assert_eq!(i.changes, vec![0, 0, 0, 1]);
    }

    fn conn(source: scana_core::session::Source, vehicle: &str, pids: Vec<u8>) -> ConnectionInfo {
        ConnectionInfo {
            source,
            link: "simulated".into(),
            bitrate: scana_proto::Bitrate::K500,
            addressing: scana_core::Addressing::Standard,
            ecus: vec![scana_core::session::EcuView {
                id: CanId::Standard(0x7E8),
                name: "Engine".into(),
                supported: pids,
            }],
            profile_id: None,
            vehicle: vehicle.into(),
            make: None,
            other_tester: false,
            log: vec![],
            live: vec![],
        }
    }

    #[test]
    fn a_different_car_starts_clean_and_the_same_car_keeps_its_results() {
        use scana_core::session::Source;
        let health = || HealthReport {
            when: "now".into(),
            vehicle: "Truck".into(),
            link: String::new(),
            protocol: String::new(),
            ecus: vec![],
            modules: vec![],
        };
        let mut m = Model::default();
        m.apply(Update::Connected(conn(Source::Demo, "Truck", vec![0x0C])));
        m.health = Some((health(), None));
        m.codes = Some(vec![]);
        // The same car back after a lost link: results stay.
        m.apply(Update::Connected(conn(Source::Demo, "Truck", vec![0x0C])));
        assert!(m.health.is_some() && m.codes.is_some());
        // Another car: nothing from the truck remains.
        m.apply(Update::Connected(conn(Source::DemoCorolla, "Unknown Toyota", vec![0x0C, 0x3C])));
        assert!(m.health.is_none() && m.codes.is_none());
    }
}

#[derive(Clone, Debug)]
pub struct FrameRow {
    pub frame: Frame,
    pub at_ms: u64,
    pub outgoing: bool,
}

/// One data ID in the data explorer, with which of its bytes have changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExploreItem {
    pub bytes: Vec<u8>,
    /// The bytes when the scan ran or changes were last reset.
    pub baseline: Vec<u8>,
    /// How many times each byte changed.
    pub changes: Vec<u32>,
    /// When each byte last changed (ms on the session clock).
    pub changed_at: Vec<Option<u64>>,
    pub t_ms: u64,
}

impl ExploreItem {
    fn new(bytes: Vec<u8>) -> Self {
        let n = bytes.len();
        ExploreItem { baseline: bytes.clone(), bytes, changes: vec![0; n], changed_at: vec![None; n], t_ms: 0 }
    }

    fn update(&mut self, bytes: Vec<u8>, t_ms: u64) {
        let n = bytes.len().max(self.bytes.len());
        self.changes.resize(n, 0);
        self.changed_at.resize(n, None);
        for i in 0..n {
            if bytes.get(i) != self.bytes.get(i) {
                self.changes[i] += 1;
                self.changed_at[i] = Some(t_ms);
            }
        }
        self.bytes = bytes;
        self.t_ms = t_ms;
    }

    /// Forget the changes seen so far: now is the new starting point.
    pub fn reset(&mut self) {
        self.baseline = self.bytes.clone();
        self.changes.iter_mut().for_each(|c| *c = 0);
        self.changed_at.iter_mut().for_each(|c| *c = None);
    }
}

/// The data explorer: one module's data IDs.
#[derive(Clone, Debug, Default)]
pub struct Explore {
    pub channel: Option<Channel>,
    pub kind: Option<DataKind>,
    pub items: BTreeMap<u16, ExploreItem>,
    /// False if the last scan was stopped before the end.
    pub complete: bool,
    /// The newest time seen, for "changed in the last 2 s".
    pub now_ms: u64,
}

/// One request sent from the console, and what came back.
#[derive(Clone, Debug)]
pub struct ConsoleEntry {
    pub channel: Channel,
    pub request: Vec<u8>,
    pub reply: Result<Option<Reply>, String>,
}

#[derive(Clone, Debug)]
pub struct Banner {
    pub text: String,
    pub error: bool,
    pub since: Instant,
}

#[derive(Default)]
pub struct Model {
    pub state: Option<LinkState>,
    pub conn: Option<ConnectionInfo>,
    /// The live values chosen (also sent to the worker).
    pub selected: Vec<(CanId, u8)>,
    pub live: HashMap<(CanId, u8), Live>,
    /// (seconds, value) per live value, for the graphs.
    pub history: HashMap<(CanId, u8), VecDeque<(f64, f64)>>,
    pub codes: Option<Vec<CodeSet>>,
    pub freeze: Option<Vec<FreezeFrame>>,
    pub readiness: Option<Vec<(CanId, MonitorStatus)>>,
    pub tests: Option<Vec<(CanId, Vec<TestResult>)>>,
    pub info: Option<Vec<(CanId, VehicleInfo)>>,
    pub health: Option<(HealthReport, Option<PathBuf>)>,
    pub cleared: Option<ClearOutcome>,
    pub recording: Option<PathBuf>,
    pub frames: VecDeque<FrameRow>,
    pub frames_paused: bool,
    pub stats: Option<Stats>,
    pub busy: Option<String>,
    pub banners: Vec<Banner>,
    /// Marks set on the live graphs, in seconds.
    pub marks: Vec<f64>,
    /// Stage 3: every module found, with what was read from it.
    pub modules: Vec<ModuleInfo>,
    pub discovery: Option<DiscoverySummary>,
    pub discovering: bool,
    /// A long job's progress, 0 to 1.
    pub progress: Option<f32>,
    pub explore: Explore,
    pub console: Vec<ConsoleEntry>,
}

impl Model {
    pub fn connected(&self) -> bool {
        matches!(self.state, Some(LinkState::Connected)) && self.conn.is_some()
    }

    pub fn profile(&self) -> Option<&'static Profile> {
        let id = self.conn.as_ref()?.profile_id.as_ref()?;
        profile::builtin().iter().find(|p| &p.id == id)
    }

    /// The car's make: from its profile, else from its VIN.
    pub fn make(&self) -> Option<String> {
        self.profile().and_then(|p| p.make.clone()).or_else(|| self.conn.as_ref()?.make.clone())
    }

    pub fn ecu_name(&self, id: CanId) -> String {
        self.conn
            .as_ref()
            .and_then(|c| c.ecus.iter().find(|e| e.id == id))
            .map_or_else(|| scana_core::report::ecu_default_name(id), |e| e.name.clone())
    }

    /// The check engine light, from whatever was read last.
    pub fn mil(&self) -> Option<bool> {
        if let Some(r) = &self.readiness
            && let Some((_, m)) = r.first()
        {
            return Some(m.mil);
        }
        for ((_, pid), l) in &self.live {
            if *pid == 0x01 && l.reading.raw.len() >= 4 {
                return Some(l.reading.raw[0] & 0x80 != 0);
            }
        }
        self.health.as_ref().and_then(|(h, _)| h.ecus.iter().find_map(|e| e.monitors.as_ref().map(|m| m.mil)))
    }

    /// Is the engine running, from live engine speed if it is being read.
    pub fn engine_running(&self) -> bool {
        self.live.iter().any(|((_, pid), l)| *pid == 0x0C && l.value().is_some_and(|v| v > 0.0))
    }

    pub fn banner(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if self.banners.last().is_some_and(|b| b.text == text) {
            return;
        }
        self.banners.push(Banner { text, error, since: Instant::now() });
        if self.banners.len() > 4 {
            self.banners.remove(0);
        }
    }

    fn reset_car(&mut self) {
        self.conn = None;
        self.live.clear();
        self.health = None;
        self.codes = None;
        self.freeze = None;
        self.readiness = None;
        self.tests = None;
        self.info = None;
        self.cleared = None;
        self.stats = None;
        self.busy = None;
        self.modules.clear();
        self.discovery = None;
        self.discovering = false;
        self.progress = None;
        self.explore = Explore::default();
        self.console.clear();
    }

    pub fn apply(&mut self, u: Update) {
        match u {
            Update::State(s) => {
                if matches!(s, LinkState::Disconnected) {
                    self.reset_car();
                }
                self.state = Some(s);
            }
            Update::Connected(info) => {
                // The same car back after a lost link (a bridge restart mid-drive) keeps what was
                // read and the graphs; a different car starts clean, so nothing from the last car
                // shows under the new one's name.
                let same_car = self.conn.as_ref().is_some_and(|c| {
                    c.source == info.source
                        && c.vehicle == info.vehicle
                        && c.ecus.iter().map(|e| &e.supported).eq(info.ecus.iter().map(|e| &e.supported))
                });
                if !same_car {
                    self.selected = info.live.clone();
                    self.history.clear();
                    self.marks.clear();
                    self.reset_car();
                }
                self.conn = Some(info);
            }
            Update::Values(values) => {
                for v in values {
                    let key = (v.ecu, v.reading.pid);
                    let entry = self.live.entry(key).or_insert_with(|| Live {
                        reading: v.reading.clone(),
                        t_ms: v.t_ms,
                        times: VecDeque::new(),
                    });
                    entry.reading = v.reading;
                    entry.t_ms = v.t_ms;
                    entry.times.push_back(v.t_ms);
                    while entry.times.len() > 8 {
                        entry.times.pop_front();
                    }
                    if let Some(val) = entry.value() {
                        let h = self.history.entry(key).or_default();
                        h.push_back((v.t_ms as f64 / 1000.0, val));
                        while h.len() > HISTORY_POINTS {
                            h.pop_front();
                        }
                    }
                }
            }
            Update::Codes(c) => self.codes = Some(c),
            Update::Freeze(f) => self.freeze = Some(f),
            Update::Readiness(r) => self.readiness = Some(r),
            Update::Tests(t) => self.tests = Some(t),
            Update::Info(i) => self.info = Some(i),
            Update::Health { report, path } => {
                if let Some(p) = &path {
                    self.banner(format!("Health report saved: {}", p.display()), false);
                }
                self.health = Some((*report, path));
            }
            Update::Cleared { outcome, .. } => {
                let text = if outcome.refused.is_empty() {
                    format!(
                        "Codes cleared on {} computer(s). The readiness monitors start again.",
                        outcome.cleared.len()
                    )
                } else {
                    format!(
                        "Cleared on {}; refused by {}: {}",
                        outcome.cleared.len(),
                        outcome.refused.len(),
                        outcome.refused.iter().map(|(_, r)| r.as_str()).collect::<Vec<_>>().join("; ")
                    )
                };
                self.banner(text, !outcome.refused.is_empty());
                self.cleared = Some(outcome);
            }
            Update::Recording(r) => self.recording = r,
            Update::Frame { frame, at_ms, outgoing } => {
                if !self.frames_paused {
                    self.frames.push_back(FrameRow { frame, at_ms, outgoing });
                    while self.frames.len() > FRAME_ROWS {
                        self.frames.pop_front();
                    }
                }
            }
            Update::Stats(s) => self.stats = Some(s),
            Update::Busy(b) => {
                if b.is_none() {
                    self.discovering = false;
                }
                self.busy = b;
            }
            Update::Progress(p) => self.progress = p,
            Update::DiscoveryStarted => {
                self.modules.clear();
                self.discovery = None;
                self.discovering = true;
            }
            Update::Discovery(d) => {
                self.discovering = false;
                self.discovery = Some(d);
            }
            Update::Module(m) => match self.modules.iter_mut().find(|x| x.channel == m.channel) {
                Some(x) => *x = *m,
                None => self.modules.push(*m),
            },
            Update::DataScan { channel, kind, items, complete } => {
                self.explore = Explore {
                    channel: Some(channel),
                    kind: Some(kind),
                    items: items.into_iter().map(|i| (i.id, ExploreItem::new(i.bytes))).collect(),
                    complete,
                    now_ms: self.explore.now_ms,
                };
                if !complete {
                    self.banner("The scan was stopped: the list shows what was found before that.", false);
                }
            }
            Update::DataValue { channel, kind, id, bytes, t_ms } => {
                if self.explore.channel == Some(channel) && self.explore.kind == Some(kind) {
                    self.explore.now_ms = self.explore.now_ms.max(t_ms);
                    self.explore.items.entry(id).or_insert_with(|| ExploreItem::new(bytes.clone())).update(bytes, t_ms);
                }
            }
            Update::ConsoleReply { channel, request, reply } => {
                self.console.push(ConsoleEntry { channel, request, reply });
                if self.console.len() > 40 {
                    self.console.remove(0);
                }
            }
            Update::Message(m) => self.banner(m, false),
            Update::Error(e) => self.banner(e, true),
        }
    }
}
