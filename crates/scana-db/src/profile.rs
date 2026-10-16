//! Vehicle profiles (`data/vehicles/*.toml`): what Scana knows about one model. Matched by
//! the engine ECU's list of live PIDs.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ProfileEcu {
    pub id: u16,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Note {
    pub pid: u8,
    pub text: String,
}

/// Warning limits for a value. `only_running`: only while the engine runs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Limit {
    pub pid: u8,
    #[serde(default)]
    pub caution_above: Option<f64>,
    #[serde(default)]
    pub alarm_above: Option<f64>,
    #[serde(default)]
    pub alarm_below: Option<f64>,
    #[serde(default)]
    pub only_running: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Caution,
    Alarm,
}

impl Limit {
    pub fn check(&self, value: f64, engine_running: bool) -> Level {
        if self.only_running && !engine_running {
            return Level::Ok;
        }
        if self.alarm_above.is_some_and(|a| value > a) || self.alarm_below.is_some_and(|b| value < b) {
            Level::Alarm
        } else if self.caution_above.is_some_and(|c| value > c) {
            Level::Caution
        } else {
            Level::Ok
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Baseline {
    pub pid: u8,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// The maker ("Daihatsu"), for the module address book.
    #[serde(default)]
    pub make: Option<String>,
    pub model: String,
    pub bitrate: u32,
    pub addressing: String,
    pub match_pids: Vec<u8>,
    #[serde(default)]
    pub default_live: Vec<u8>,
    #[serde(default)]
    pub not_reported: Option<String>,
    #[serde(default)]
    pub ecu: Vec<ProfileEcu>,
    #[serde(default)]
    pub note: Vec<Note>,
    #[serde(default)]
    pub limit: Vec<Limit>,
    #[serde(default)]
    pub baseline: Vec<Baseline>,
}

impl Profile {
    pub fn note(&self, pid: u8) -> Option<&str> {
        self.note.iter().find(|n| n.pid == pid).map(|n| n.text.as_str())
    }
    pub fn limit(&self, pid: u8) -> Option<&Limit> {
        self.limit.iter().find(|l| l.pid == pid)
    }
    pub fn baseline(&self, pid: u8) -> Option<&str> {
        self.baseline.iter().find(|b| b.pid == pid).map(|b| b.value.as_str())
    }
    pub fn ecu_name(&self, id: u16) -> Option<&str> {
        self.ecu.iter().find(|e| e.id == id).map(|e| e.name.as_str())
    }
    /// The diagnostic family for module discovery, from the make ("Daihatsu" -> "toyota").
    pub fn family(&self) -> Option<&'static str> {
        self.make.as_deref().and_then(crate::modules::family_of_make)
    }
}

static BUILTIN: LazyLock<Vec<Profile>> = LazyLock::new(|| {
    [include_str!("../data/vehicles/daihatsu-hijet-s510p-2024.toml")]
        .iter()
        .filter_map(|s| toml::from_str::<Profile>(s).ok())
        .collect()
});

pub fn builtin() -> &'static [Profile] {
    &BUILTIN
}

/// Most PIDs a car's list may differ from a profile's and still match it. A software update,
/// or a profile first written from a handheld tool's list, can be a PID or two out.
pub const MATCH_TOLERANCE: usize = 2;

/// The profile whose PID list best matches the engine ECU's live PIDs (support masks ignored):
/// an exact match, else the closest within [`MATCH_TOLERANCE`] PIDs added or missing.
pub fn find_match(live_pids: &[u8]) -> Option<&'static Profile> {
    let have: std::collections::BTreeSet<u8> = live_pids.iter().copied().filter(|p| !p.is_multiple_of(0x20)).collect();
    BUILTIN
        .iter()
        .filter(|p| p.match_pids.len() > 4 * MATCH_TOLERANCE)
        .map(|p| {
            let want: std::collections::BTreeSet<u8> = p.match_pids.iter().copied().collect();
            (want.symmetric_difference(&have).count(), p)
        })
        .filter(|(diff, _)| *diff <= MATCH_TOLERANCE)
        .min_by_key(|(diff, _)| *diff)
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hijet_profile_parses_and_matches() {
        assert_eq!(builtin().len(), 1);
        let p = &builtin()[0];
        assert_eq!(p.match_pids.len(), 31);
        assert_eq!(p.bitrate, 500_000);
        let mut pids = p.match_pids.clone();
        pids.extend([0x00, 0x20, 0x40]);
        assert_eq!(find_match(&pids).map(|x| x.id.as_str()), Some("daihatsu-hijet-s510p-2024"));
        assert!(find_match(&[0x01, 0x0C]).is_none());
        // Up to two PIDs out still match: the handheld's list (41 extra, 4F and 50 missing) is three.
        let near: Vec<u8> = p.match_pids.iter().copied().filter(|&x| x != 0x50).chain([0x41]).collect();
        assert!(find_match(&near).is_some());
        let handheld: Vec<u8> =
            p.match_pids.iter().copied().filter(|&x| x != 0x50 && x != 0x4F).chain([0x41]).collect();
        assert!(find_match(&handheld).is_none());
        assert!(p.note(0x05).unwrap().contains("101"));
        assert_eq!(p.baseline(0x0C), Some("1874 rpm"));
        assert_eq!(p.ecu_name(0x7E8), Some("Engine (ECM)"));
    }

    #[test]
    fn limits() {
        let p = &builtin()[0];
        let c = p.limit(0x05).unwrap();
        assert_eq!(c.check(95.0, true), Level::Ok);
        assert_eq!(c.check(108.0, true), Level::Caution);
        assert_eq!(c.check(113.0, false), Level::Alarm);
        let v = p.limit(0x42).unwrap();
        assert_eq!(v.check(12.4, false), Level::Ok); // engine off: no charging alarm
        assert_eq!(v.check(12.4, true), Level::Alarm);
        assert_eq!(v.check(14.3, true), Level::Ok);
    }
}
