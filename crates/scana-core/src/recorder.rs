//! Session recording (docs/DESIGN.md section 10). One folder per session:
//!
//! - `frames.log`: every frame, candump format; frames Scana sent end in ` T`.
//! - `values.csv`: every decoded value, and markers.
//! - `session.toml`: a summary, written when the session ends.
//! - `report.html`: the health report, if one was made.
//!
//! Files are flushed at least once a second, so a crash or a pulled cable loses at most 1 s.

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
    pub codes: Vec<String>,
    #[serde(default)]
    pub app_version: String,
}

pub struct Recorder {
    dir: PathBuf,
    frames: BufWriter<File>,
    values: BufWriter<File>,
    /// Wall-clock microseconds when the link's clock was at 0 ms.
    epoch_us: u64,
    last_flush: Instant,
    pub summary: SessionSummary,
}

/// Quotes a CSV field if it needs it.
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
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
        let started = Local::now();
        let mut dir = base.join(format!("{}_{}", started.format("%Y-%m-%d_%H%M%S"), slug(vehicle)));
        let mut n = 2;
        while dir.exists() {
            dir = base.join(format!("{}_{}-{n}", started.format("%Y-%m-%d_%H%M%S"), slug(vehicle)));
            n += 1;
        }
        fs::create_dir_all(&dir)?;
        let now_us = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0);
        let mut values = BufWriter::new(File::create(dir.join("values.csv"))?);
        writeln!(values, "time_s,ecu,pid,name,value,unit,text")?;
        Ok(Recorder {
            frames: BufWriter::new(File::create(dir.join("frames.log"))?),
            values,
            epoch_us: now_us.saturating_sub(link_now_ms * 1000),
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
            self.last_flush = Instant::now();
        }
    }

    pub fn frame(&mut self, frame: &Frame, at_ms: u64, outgoing: bool) {
        let line = candump::format_line(frame, self.epoch_us + at_ms * 1000, "can0");
        let _ = writeln!(self.frames, "{line}{}", if outgoing { " T" } else { "" });
        self.summary.frames += 1;
        self.maybe_flush();
    }

    /// One decoded value. `t_ms` is the link's clock.
    #[allow(clippy::too_many_arguments)]
    pub fn value(&mut self, t_ms: u64, ecu: CanId, pid: u8, name: &str, value: Option<f64>, unit: &str, text: &str) {
        let v = value.map(|x| format!("{x}")).unwrap_or_default();
        let _ = writeln!(
            self.values,
            "{:.3},{ecu},{pid:02X},{},{v},{},{}",
            t_ms as f64 / 1000.0,
            csv_field(name),
            csv_field(unit),
            csv_field(text)
        );
        self.summary.values += 1;
        self.maybe_flush();
    }

    /// A marker the user set ("something happened here").
    pub fn marker(&mut self, t_ms: u64, label: &str) {
        let _ = writeln!(self.values, "{:.3},,MARK,{},,,", t_ms as f64 / 1000.0, csv_field(label));
        self.summary.markers += 1;
        let _ = self.values.flush();
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
        assert_eq!((s.frames, s.values, s.markers), (2, 2, 1));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("2024 Daihatsu Hijet Jumbo"), "2024-daihatsu-hijet-jumbo");
        assert_eq!(slug("  !!"), "car");
    }
}
