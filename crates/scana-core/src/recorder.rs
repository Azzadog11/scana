//! Session recording (docs/DESIGN.md section 10). One folder per session:
//!
//! - `frames.log`: every frame, candump format; frames Scana sent end in ` T`.
//! - `values.csv`: every decoded value, and markers. `time_s` counts from the session's start.
//! - `events.log`: what happened on the link and the bus (bridge restarts, bus errors, reconnects).
//! - `session.toml`: a summary, written when the session ends.
//! - `report.html`: the health report, if one was made.
//!
//! Files are flushed at least once a second, so a crash or a pulled cable loses at most 1 s.
//!
//! Each new link starts its clock at 0. The worker calls [`Recorder::rebase`] after a
//! reconnect; a clock that jumps back more than a second is rebased by itself too, so the
//! times in every file keep going forward.

use chrono::Local;
use scana_proto::{CanId, Frame, candump};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Where sessions go: `$XDG_DATA_HOME/scana/sessions`, else `~/.local/share/scana/sessions`.
pub fn sessions_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("scana").join("sessions")
}

/// What `session.toml` holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionSummary {
    pub started: String,
    #[serde(default)]
    pub ended: String,
    pub vehicle: String,
    pub link: String,
    #[serde(default)]
    pub bitrate: String,
    #[serde(default)]
    pub addressing: String,
    #[serde(default)]
    pub ecus: Vec<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub frames: u64,
    #[serde(default)]
    pub values: u64,
    #[serde(default)]
    pub markers: u32,
    #[serde(default)]
    pub events: u32,
    #[serde(default)]
    pub codes: Vec<String>,
    #[serde(default)]
    pub app_version: String,
}

pub struct Recorder {
    dir: PathBuf,
    frames: BufWriter<File>,
    values: BufWriter<File>,
    events: BufWriter<File>,
    /// Wall-clock microseconds when the link's clock was at 0 ms.
    epoch_us: u64,
    /// Wall-clock microseconds when the session started.
    start_us: u64,
    /// The latest link time seen, to notice a link whose clock started again.
    last_ms: u64,
    /// Reads the wall clock in microseconds (a fake one in tests).
    clock: fn() -> u64,
    last_flush: Instant,
    pub summary: SessionSummary,
}

/// Quotes a CSV field if it needs it.
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

fn wall_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "car".into() } else { out.chars().take(40).collect() }
}

impl Recorder {
    /// Starts a session folder under `base`. `link_now_ms` is the link's clock right now.
    pub fn start(base: &Path, vehicle: &str, link: &str, link_now_ms: u64) -> io::Result<Self> {
        Self::start_with_clock(base, vehicle, link, link_now_ms, wall_us)
    }

    fn start_with_clock(
        base: &Path,
        vehicle: &str,
        link: &str,
        link_now_ms: u64,
        clock: fn() -> u64,
    ) -> io::Result<Self> {
        let started = Local::now();
        let mut dir = base.join(format!("{}_{}", started.format("%Y-%m-%d_%H%M%S"), slug(vehicle)));
        let mut n = 2;
        while dir.exists() {
            dir = base.join(format!("{}_{}-{n}", started.format("%Y-%m-%d_%H%M%S"), slug(vehicle)));
            n += 1;
        }
        fs::create_dir_all(&dir)?;
        let now_us = clock();
        let mut values = BufWriter::new(File::create(dir.join("values.csv"))?);
        writeln!(values, "time_s,ecu,pid,name,value,unit,text")?;
        Ok(Recorder {
            frames: BufWriter::new(File::create(dir.join("frames.log"))?),
            values,
            events: BufWriter::new(File::create(dir.join("events.log"))?),
            epoch_us: now_us.saturating_sub(link_now_ms * 1000),
            start_us: now_us,
            last_ms: link_now_ms,
            clock,
            last_flush: Instant::now(),
            summary: SessionSummary {
                started: started.to_rfc3339(),
                vehicle: vehicle.into(),
                link: link.into(),
                app_version: env!("CARGO_PKG_VERSION").into(),
                ..SessionSummary::default()
            },
            dir,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn maybe_flush(&mut self) {
        if self.last_flush.elapsed().as_millis() >= 1000 {
            let _ = self.frames.flush();
            let _ = self.values.flush();
            let _ = self.events.flush();
            self.last_flush = Instant::now();
        }
    }

    /// A new link is open and its clock reads `link_now_ms` now.
    pub fn rebase(&mut self, link_now_ms: u64) {
        self.epoch_us = (self.clock)().saturating_sub(link_now_ms * 1000);
        self.last_ms = link_now_ms;
    }

    /// Wall-clock microseconds for a link time, rebasing first if the link's clock started again.
    fn wall_at(&mut self, at_ms: u64) -> u64 {
        if at_ms + 1000 < self.last_ms {
            self.rebase(at_ms);
        }
        self.last_ms = self.last_ms.max(at_ms);
        self.epoch_us + at_ms * 1000
    }

    /// Seconds since the session started, for a link time.
    fn session_s(&mut self, at_ms: u64) -> f64 {
        self.wall_at(at_ms).saturating_sub(self.start_us) as f64 / 1e6
    }

    pub fn frame(&mut self, frame: &Frame, at_ms: u64, outgoing: bool) {
        let at_us = self.wall_at(at_ms);
        let line = candump::format_line(frame, at_us, "can0");
        let _ = writeln!(self.frames, "{line}{}", if outgoing { " T" } else { "" });
        self.summary.frames += 1;
        self.maybe_flush();
    }

    /// One decoded value. `t_ms` is the link's clock.
    #[allow(clippy::too_many_arguments)]
    pub fn value(&mut self, t_ms: u64, ecu: CanId, pid: u8, name: &str, value: Option<f64>, unit: &str, text: &str) {
        let v = value.map(|x| format!("{x}")).unwrap_or_default();
        let t = self.session_s(t_ms);
        let _ = writeln!(
            self.values,
            "{t:.3},{ecu},{pid:02X},{},{v},{},{}",
            csv_field(name),
            csv_field(unit),
            csv_field(text)
        );
        self.summary.values += 1;
        self.maybe_flush();
    }

    /// A marker the user set ("something happened here").
    pub fn marker(&mut self, t_ms: u64, label: &str) {
        let t = self.session_s(t_ms);
        let _ = writeln!(self.values, "{t:.3},,MARK,{},,,", csv_field(label));
        self.summary.markers += 1;
        let _ = self.values.flush();
    }

    /// Something happened on the link or the bus. One line in `events.log`: the local time,
    /// seconds since the session started, and the text. Written at once, as these often come
    /// just before the link is lost.
    pub fn event(&mut self, text: &str) {
        let now = Local::now();
        let t = (self.clock)().saturating_sub(self.start_us) as f64 / 1e6;
        let _ = writeln!(self.events, "{} {t:>9.3} s  {text}", now.format("%Y-%m-%d %H:%M:%S%.3f"));
        self.summary.events += 1;
        let _ = self.events.flush();
    }

    pub fn write_file(&self, name: &str, content: &str) -> io::Result<PathBuf> {
        let p = self.dir.join(name);
        fs::write(&p, content)?;
        Ok(p)
    }

    /// Ends the session: flushes everything and writes `session.toml`.
    pub fn finish(mut self) -> io::Result<PathBuf> {
        self.frames.flush()?;
        self.values.flush()?;
        self.events.flush()?;
        self.summary.ended = Local::now().to_rfc3339();
        let text = toml::to_string_pretty(&self.summary).map_err(io::Error::other)?;
        fs::write(self.dir.join("session.toml"), text)?;
        Ok(self.dir.clone())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.frames.flush();
        let _ = self.values.flush();
        let _ = self.events.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_finishes() {
        let base = std::env::temp_dir().join(format!("scana-rec-test-{}", std::process::id()));
        let mut r = Recorder::start(&base, "2024 Daihatsu Hijet Jumbo", "simulated car", 1000).unwrap();
        let f = Frame::new(CanId::Standard(0x7E8), &[0x04, 0x41, 0x0C, 0x0D, 0x48]).unwrap();
        r.frame(&f, 1500, false);
        r.frame(&f, 1600, true);
        r.value(1500, CanId::Standard(0x7E8), 0x0C, "Engine speed", Some(850.0), "rpm", "");
        r.value(1500, CanId::Standard(0x7E8), 0x03, "Fuel system status", None, "", "open loop, not warm, yet");
        r.marker(1700, "hesitation");
        let dir = r.finish().unwrap();
        assert!(dir.file_name().unwrap().to_string_lossy().ends_with("2024-daihatsu-hijet-jumbo"));
        let frames = fs::read_to_string(dir.join("frames.log")).unwrap();
        assert_eq!(frames.lines().count(), 2);
        assert!(frames.lines().nth(1).unwrap().ends_with(" T"));
        let values = fs::read_to_string(dir.join("values.csv")).unwrap();
        assert!(values.contains("\"open loop, not warm, yet\""));
        assert!(values.contains(",MARK,hesitation"));
        let s: SessionSummary = toml::from_str(&fs::read_to_string(dir.join("session.toml")).unwrap()).unwrap();
        assert_eq!((s.frames, s.values, s.markers, s.events), (2, 2, 1, 0));
        fs::remove_dir_all(&base).unwrap();
    }

    fn stamps(dir: &Path) -> Vec<f64> {
        let frames = fs::read_to_string(dir.join("frames.log")).unwrap();
        frames.lines().map(|l| l[1..l.find(')').unwrap()].parse().unwrap()).collect()
    }

    use std::sync::atomic::{AtomicU64, Ordering};

    static FAKE_US: AtomicU64 = AtomicU64::new(0);

    fn fake_clock() -> u64 {
        FAKE_US.load(Ordering::SeqCst)
    }

    fn at_s(s: u64) {
        FAKE_US.store(1_760_000_000_000_000 + s * 1_000_000, Ordering::SeqCst);
    }

    #[test]
    fn times_keep_going_after_a_new_link() {
        let base = std::env::temp_dir().join(format!("scana-rec-rebase-{}", std::process::id()));
        let id = CanId::Standard(0x7E8);
        let f = Frame::new(id, &[0x03, 0x41, 0x0D, 0x32]).unwrap();
        at_s(0);
        let mut r = Recorder::start_with_clock(&base, "Demo", "sim", 0, fake_clock).unwrap();
        at_s(60);
        r.frame(&f, 60_000, false);
        r.value(60_000, id, 0x0D, "Vehicle speed", Some(50.0), "km/h", "");
        // The bridge restarts; 2 s later the worker opens a new link, whose clock starts at 0.
        r.event("the bridge restarted, scana-bridge 0.1.1: ... (report: reset=warm step=1 count=1)");
        at_s(62);
        r.rebase(0);
        r.frame(&f, 100, false);
        r.value(100, id, 0x0D, "Vehicle speed", Some(51.0), "km/h", "");
        // A clock that starts again without a rebase is caught too.
        at_s(182);
        r.frame(&f, 120_000, false);
        at_s(185);
        r.frame(&f, 50, false);
        r.value(50, id, 0x0D, "Vehicle speed", Some(52.0), "km/h", "");
        let dir = r.finish().unwrap();
        let t = stamps(&dir);
        assert!(t.windows(2).all(|w| w[1] >= w[0] - 0.01), "frame times go back: {t:?}");
        let values = crate::store::load_values(&dir).unwrap();
        let pts: Vec<f64> = values.series[&("7E8".to_string(), 0x0D)].points.iter().map(|p| p.0).collect();
        assert_eq!(pts, vec![60.0, 62.1, 185.0]);
        assert_eq!(t.first().copied(), Some(1_760_000_060.0));
        let events = fs::read_to_string(dir.join("events.log")).unwrap();
        assert_eq!(events.lines().count(), 1);
        assert!(events.contains("   60.000 s  the bridge restarted, scana-bridge 0.1.1"), "{events}");
        let s: SessionSummary = toml::from_str(&fs::read_to_string(dir.join("session.toml")).unwrap()).unwrap();
        assert_eq!(s.events, 1);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("2024 Daihatsu Hijet Jumbo"), "2024-daihatsu-hijet-jumbo");
        assert_eq!(slug("  !!"), "car");
    }
}
