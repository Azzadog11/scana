//! Past sessions: listing them, and loading their values back for the graphs.

use crate::recorder::SessionSummary;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub dir: PathBuf,
    pub name: String,
    pub summary: Option<SessionSummary>,
    pub has_report: bool,
}

/// Sessions under `base`, newest first.
pub fn list_sessions(base: &Path) -> Vec<SessionInfo> {
    let Ok(rd) = fs::read_dir(base) else { return vec![] };
    let mut out: Vec<SessionInfo> = rd
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let dir = e.path();
            let summary = fs::read_to_string(dir.join("session.toml"))
                .ok()
                .and_then(|t| toml::from_str::<SessionSummary>(&t).ok());
            SessionInfo {
                name: e.file_name().to_string_lossy().into_owned(),
                has_report: dir.join("report.html").exists(),
                summary,
                dir,
            }
        })
        .collect();
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

/// One value's history from a session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub name: String,
    pub unit: String,
    /// (seconds, value)
    pub points: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedSession {
    /// Keyed by (ECU, PID).
    pub series: BTreeMap<(String, u8), Series>,
    pub markers: Vec<(f64, String)>,
}

/// Splits one CSV line, handling quoted fields.
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if field.is_empty() => quoted = true,
            (',', false) => out.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    out.push(field);
    out
}

/// Loads `values.csv` from a session folder.
pub fn load_values(dir: &Path) -> io::Result<LoadedSession> {
    let text = fs::read_to_string(dir.join("values.csv"))?;
    let mut s = LoadedSession::default();
    for line in text.lines().skip(1) {
        let f = split_csv(line);
        if f.len() < 7 {
            continue;
        }
        let Ok(t) = f[0].parse::<f64>() else { continue };
        if f[2] == "MARK" {
            s.markers.push((t, f[3].clone()));
            continue;
        }
        let (Ok(pid), Ok(v)) = (u8::from_str_radix(&f[2], 16), f[4].parse::<f64>()) else { continue };
        let entry = s.series.entry((f[1].clone(), pid)).or_insert_with(|| Series {
            name: f[3].clone(),
            unit: f[5].clone(),
            points: vec![],
        });
        entry.points.push((t, v));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::Recorder;
    use scana_proto::CanId;

    #[test]
    fn csv_round_trip() {
        assert_eq!(split_csv("1,\"a, b\",\"say \"\"hi\"\"\","), vec!["1", "a, b", "say \"hi\"", ""]);
    }

    #[test]
    fn record_list_and_load() {
        let base = std::env::temp_dir().join(format!("scana-store-test-{}", std::process::id()));
        let mut r = Recorder::start(&base, "Demo", "sim", 0).unwrap();
        for i in 0..5u64 {
            r.value(i * 200, CanId::Standard(0x7E8), 0x0C, "Engine speed", Some(800.0 + i as f64), "rpm", "");
        }
        r.marker(500, "bump");
        r.finish().unwrap();
        let list = list_sessions(&base);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].summary.as_ref().unwrap().values, 5);
        let loaded = load_values(&list[0].dir).unwrap();
        let s = &loaded.series[&("7E8".to_string(), 0x0C)];
        assert_eq!(s.points.len(), 5);
        assert_eq!(s.points[4], (0.8, 804.0));
        assert_eq!(loaded.markers, vec![(0.5, "bump".to_string())]);
        fs::remove_dir_all(&base).unwrap();
    }
}
