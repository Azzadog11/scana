//! What the app knows: the latest of everything the session worker sent, kept for drawing.

use scana_core::readers::{FreezeFrame, PidReading, VehicleInfo};
use scana_core::report::HealthReport;
use scana_core::safety::ClearOutcome;
use scana_core::session::{CodeSet, ConnectionInfo, LinkState, Stats, Update};
use scana_db::profile::{self, Profile};
use scana_proto::mode06::TestResult;
use scana_proto::pids::MonitorStatus;
use scana_proto::{CanId, Frame};
use std::collections::{HashMap, VecDeque};
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

#[derive(Clone, Debug)]
pub struct FrameRow {
    pub frame: Frame,
    pub at_ms: u64,
    pub outgoing: bool,
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
}

impl Model {
    pub fn connected(&self) -> bool {
        matches!(self.state, Some(LinkState::Connected)) && self.conn.is_some()
    }

    pub fn profile(&self) -> Option<&'static Profile> {
        let id = self.conn.as_ref()?.profile_id.as_ref()?;
        profile::builtin().iter().find(|p| &p.id == id)
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
        self.codes = None;
        self.freeze = None;
        self.readiness = None;
        self.tests = None;
        self.info = None;
        self.cleared = None;
        self.stats = None;
        self.busy = None;
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
                self.selected = info.live.clone();
                self.history.clear();
                self.marks.clear();
                self.reset_car();
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
            Update::Busy(b) => self.busy = b,
            Update::Message(m) => self.banner(m, false),
            Update::Error(e) => self.banner(e, true),
        }
    }
}
