//! Diagnosis: works through a guide (`scana_db::diag`) for a fault code. The user answers
//! each test (by hand, or Scana measures it on a connected CAN car), and every answer moves
//! the likelihood of each cause. The ranking always shows why.
//!
//! The arithmetic: each cause starts at its weight; an answer's effect of +1 doubles it,
//! +2 multiplies by 4, -1 halves it. Shares are the results scaled to add up to 100 %. It is
//! a guide to what to check first, not proof.
//!
//! Measuring is pure: the caller collects samples (only while the test's hold condition is
//! met) and these functions turn them into a value and an answer.

use crate::readers::FreezeFrame;
use scana_db::diag::{self, Auto, Cause, Guide, Test, TestResult};
use scana_proto::mode06::{self, TestResult as MonitorResult};
use scana_proto::pids::MonitorStatus;
use scana_proto::{CanId, Dtc};
use std::collections::BTreeMap;

/// What a measurement found: a value, a sentence about it, and the answer it points to.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub value: Option<f64>,
    pub text: String,
    /// The test result id it points to, if any.
    pub result: Option<String>,
}

impl Measured {
    fn none(text: impl Into<String>) -> Self {
        Measured { value: None, text: text.into(), result: None }
    }
}

/// One live sample: a PID's main value at a moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub t_ms: u64,
    pub pid: u8,
    pub value: f64,
}

/// A cause with its current share and the answers that moved it.
#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub cause: &'static Cause,
    /// 0 to 1.
    pub share: f64,
    /// "Rear sensor copies the front: twice as likely".
    pub why: Vec<String>,
}

/// A diagnosis in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnosis {
    pub guide: &'static Guide,
    /// The code being chased, if one started it.
    pub code: Option<Dtc>,
    /// Test id -> chosen result id.
    pub answers: BTreeMap<String, String>,
    /// Test id -> what Scana measured.
    pub measured: BTreeMap<String, Measured>,
}

impl Diagnosis {
    pub fn new(guide: &'static Guide, code: Option<Dtc>) -> Self {
        Diagnosis { guide, code, answers: BTreeMap::new(), measured: BTreeMap::new() }
    }

    /// The guide for a code, if Scana has one.
    pub fn for_code(code: Dtc) -> Option<Self> {
        diag::guide_for(code).map(|g| Diagnosis::new(g, Some(code)))
    }

    /// The guide for a code on a known make: generic, or the make's own code numbers.
    pub fn for_code_make(code: Dtc, make: Option<&str>) -> Option<Self> {
        diag::guide_for_make(code, make).map(|g| Diagnosis::new(g, Some(code)))
    }

    pub fn test(&self, id: &str) -> Option<&'static Test> {
        self.guide.test.iter().find(|t| t.id == id)
    }

    /// Records an answer. Unknown test or result ids are ignored.
    pub fn answer(&mut self, test: &str, result: &str) {
        if self.test(test).is_some_and(|t| t.result(result).is_some()) {
            self.answers.insert(test.to_string(), result.to_string());
        }
    }

    pub fn clear(&mut self, test: &str) {
        self.answers.remove(test);
        self.measured.remove(test);
    }

    /// The chosen answer to a test.
    pub fn chosen(&self, test: &str) -> Option<&'static TestResult> {
        let t = self.test(test)?;
        t.result(self.answers.get(test)?)
    }

    /// Every cause, most likely first.
    pub fn ranking(&self) -> Vec<Ranked> {
        let mut scored: Vec<(f64, Ranked)> = self
            .guide
            .cause
            .iter()
            .map(|c| {
                let mut power = 0i32;
                let mut why = Vec::new();
                for t in &self.guide.test {
                    let Some(r) = self.chosen(&t.id) else { continue };
                    if let Some(&e) = r.effect.get(&c.id)
                        && e != 0
                    {
                        power += e;
                        why.push(format!("{}: {}", r.text, effect_words(e)));
                    }
                }
                let score = f64::from(c.weight.max(1)) * 2f64.powi(power.clamp(-12, 12));
                (score, Ranked { cause: c, share: 0.0, why })
            })
            .collect();
        let total: f64 = scored.iter().map(|(s, _)| s).sum();
        for (s, r) in &mut scored {
            r.share = if total > 0.0 { *s / total } else { 0.0 };
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(_, r)| r).collect()
    }

    /// Answers that say "fix this first".
    pub fn stops(&self) -> Vec<(&'static Test, &'static TestResult)> {
        self.guide.test.iter().filter_map(|t| self.chosen(&t.id).filter(|r| r.stop).map(|r| (t, r))).collect()
    }

    /// Codes from `codes` that this guide says to fix first.
    pub fn fix_first_present(&self, codes: &[Dtc]) -> Vec<Dtc> {
        let mut out: Vec<Dtc> = codes
            .iter()
            .copied()
            .filter(|c| Some(*c) != self.code && self.guide.fix_first.iter().any(|p| diag::code_matches(p, *c)))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The next test to do: the first unanswered one, tests needing no tools first.
    pub fn next_test(&self) -> Option<&'static Test> {
        let open = |t: &&Test| !self.answers.contains_key(&t.id);
        self.guide.test.iter().filter(open).find(|t| t.no_tools()).or_else(|| self.guide.test.iter().find(open))
    }

    pub fn answered(&self) -> usize {
        self.answers.len()
    }

    /// A few lines for the report and the command line: the code, the most likely causes,
    /// and what to do.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let stops = self.stops();
        for (t, res) in &stops {
            lines.push(format!("Fix first ({}): {}", t.title, res.note.as_deref().unwrap_or(&res.text)));
        }
        let r = self.ranking();
        if self.answers.is_empty() {
            lines.push("No tests answered yet: the order below is how often each cause is the fault, roughly.".into());
        }
        for x in r.iter().take(3) {
            lines.push(format!("{:>3.0} %  {}", x.share * 100.0, x.cause.name));
        }
        if let Some(top) = r.first() {
            let lead = if stops.is_empty() { "Most likely" } else { "After that, most likely" };
            lines.push(format!("{lead}: {}. {}", top.cause.name, top.cause.fix));
        }
        lines
    }

    /// The diagnosis as one HTML page, to save with the session.
    pub fn to_html(&self, vehicle: &str, when: &str) -> String {
        use crate::report::esc;
        let g = self.guide;
        let mut h = String::new();
        h.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">");
        let code = self.code.map(|c| c.to_string()).unwrap_or_default();
        h.push_str(&format!("<title>Scana diagnosis: {} {}</title>", esc(&code), esc(&g.title)));
        h.push_str(
            "<style>body{font-family:sans-serif;max-width:900px;margin:2em auto;padding:0 1em;color:#222}\
             h1{font-size:1.5em}h2{margin-top:1.4em;border-bottom:1px solid #ccc}table{border-collapse:collapse;width:100%}\
             td,th{border:1px solid #ccc;padding:4px 8px;text-align:left;vertical-align:top}th{background:#f2f2f2}\
             .bar{background:#2f6fb0;height:10px}.small{color:#666;font-size:.9em}.note{color:#8a5a00}</style></head><body>",
        );
        h.push_str(&format!("<h1>Diagnosis: {} {}</h1>", esc(&code), esc(&g.title)));
        h.push_str(&format!("<p class=\"small\">{} &middot; {}</p>", esc(vehicle), esc(when)));
        h.push_str(&format!("<p>{}</p>", esc(&g.summary)));
        h.push_str("<h2>Likely causes</h2><table><tr><th>Cause</th><th>Share</th><th>Why</th><th>Fix</th></tr>");
        for r in self.ranking() {
            h.push_str(&format!(
                "<tr><td>{}</td><td>{:.0} %<div class=\"bar\" style=\"width:{:.0}%\"></div></td><td>{}</td><td>{}</td></tr>",
                esc(&r.cause.name),
                r.share * 100.0,
                r.share * 100.0,
                esc(&r.why.join("; ")),
                esc(&r.cause.fix)
            ));
        }
        h.push_str("</table>");
        for (t, res) in self.stops() {
            h.push_str(&format!(
                "<p class=\"note\">Fix first ({}): {}</p>",
                esc(&t.title),
                esc(res.note.as_deref().unwrap_or(&res.text))
            ));
        }
        h.push_str("<h2>Tests</h2><table><tr><th>Test</th><th>Answer</th><th>Measured by Scana</th></tr>");
        for t in &g.test {
            let ans = self.chosen(&t.id).map_or("not done".to_string(), |r| r.text.clone());
            let m = self.measured.get(&t.id).map(|m| m.text.clone()).unwrap_or_default();
            h.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td></tr>", esc(&t.title), esc(&ans), esc(&m)));
        }
        h.push_str("</table><h2>Sources</h2><ul>");
        for s in &g.sources {
            h.push_str(&format!("<li class=\"small\">{}</li>", esc(s)));
        }
        h.push_str(&format!(
            "</ul><p class=\"small\">Made by Scana {}. The shares are a guide to what to check first, not proof. Check the maker's workshop manual before replacing parts.</p></body></html>\n",
            env!("CARGO_PKG_VERSION")
        ));
        h
    }
}

/// "twice as likely", "a quarter as likely".
pub fn effect_words(e: i32) -> String {
    match e {
        1 => "twice as likely".into(),
        2 => "4 times as likely".into(),
        3 => "8 times as likely".into(),
        -1 => "half as likely".into(),
        -2 => "a quarter as likely".into(),
        -3 => "an eighth as likely".into(),
        e if e > 0 => format!("{} times as likely", 1 << e.min(12)),
        e => format!("1/{} as likely", 1 << (-e).min(12)),
    }
}

/// The PIDs a live test reads (measured ones, then hold and need), in hex.
pub fn live_pids(auto: &Auto) -> Vec<u8> {
    let mut v: Vec<u8> = auto.pids.iter().filter_map(|p| u8::from_str_radix(p, 16).ok()).collect();
    for r in auto.hold.iter().chain(auto.need.iter()) {
        if let Some(p) = r.pid()
            && !v.contains(&p)
        {
            v.push(p);
        }
    }
    v
}

/// True if the car supports every PID a live test needs.
pub fn live_supported(auto: &Auto, supported: &[u8]) -> bool {
    auto.kind == "live" && live_pids(auto).iter().all(|p| supported.contains(p))
}

/// Picks the outcome for a value.
fn outcome_for(auto: &Auto, v: f64) -> Option<String> {
    auto.outcome.iter().find(|o| o.holds(v)).map(|o| o.result.clone())
}

/// Codes: which group of patterns matches the car's codes.
pub fn evaluate_codes(auto: &Auto, codes: &[Dtc]) -> Measured {
    for g in &auto.group {
        let hits: Vec<String> = codes
            .iter()
            .filter(|c| g.patterns.iter().any(|p| diag::code_matches(p, **c)))
            .map(|c| c.to_string())
            .collect();
        if !hits.is_empty() {
            return Measured {
                value: Some(hits.len() as f64),
                text: format!("Found {}", hits.join(", ")),
                result: Some(g.result.clone()),
            };
        }
    }
    let list = if codes.is_empty() {
        "no codes".to_string()
    } else {
        codes.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")
    };
    Measured {
        value: Some(0.0),
        text: format!("Codes read: {list}. None of the ones to fix first."),
        result: auto.absent.clone(),
    }
}

/// Crossings of 0.45 V, with a little hysteresis so noise doesn't count.
fn crossings(values: &[f64]) -> usize {
    let mut n = 0;
    let mut side: Option<bool> = None;
    for &v in values {
        let s = if v > 0.50 {
            Some(true)
        } else if v < 0.40 {
            Some(false)
        } else {
            None
        };
        if let Some(s) = s {
            if side.is_some_and(|old| old != s) {
                n += 1;
            }
            side = Some(s);
        }
    }
    n
}

fn series(samples: &[Sample], pid: u8) -> Vec<f64> {
    let mut v: Vec<&Sample> = samples.iter().filter(|s| s.pid == pid).collect();
    v.sort_by_key(|s| s.t_ms);
    v.into_iter().map(|s| s.value).collect()
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Live: the measure over samples taken while the hold condition was met.
/// Seconds actually measured: samples only come while the test's conditions hold, so a gap of
/// over 1.5 s between readings is a pause (the revs left the range), not measuring time.
fn measured_seconds(times: impl Iterator<Item = u64>) -> f64 {
    let mut ts: Vec<u64> = times.collect();
    ts.sort_unstable();
    ts.dedup();
    let s: u64 = ts.windows(2).map(|w| w[1] - w[0]).filter(|d| *d <= 1500).sum();
    (s as f64 / 1000.0).max(1.0)
}

pub fn evaluate_live(auto: &Auto, samples: &[Sample]) -> Measured {
    let pids: Vec<u8> = auto.pids.iter().filter_map(|p| u8::from_str_radix(p, 16).ok()).collect();
    let Some(&p0) = pids.first() else { return Measured::none("The test names no value to measure.") };
    let a = series(samples, p0);
    if a.is_empty() {
        return Measured::none("No readings were collected.");
    }
    let name = |p: u8| scana_proto::pids::def(p).map_or_else(|| format!("PID {p:02X}"), |d| d.name.to_string());
    let span_s = measured_seconds(samples.iter().filter(|s| s.pid == p0).map(|s| s.t_ms));
    let measure = auto.measure.as_deref().unwrap_or("mean");
    let (value, text) = match measure {
        "mean" | "min" | "max" | "spread" => {
            let lo = a.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = a.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let m = mean(&a).unwrap_or(0.0);
            let v = match measure {
                "min" => lo,
                "max" => hi,
                "spread" => hi - lo,
                _ => m,
            };
            (
                v,
                format!(
                    "{}: average {m:.2}, lowest {lo:.2}, highest {hi:.2} ({} readings in {span_s:.0} s)",
                    name(p0),
                    a.len()
                ),
            )
        }
        "trim" => {
            let b = pids.get(1).map(|&p| series(samples, p)).unwrap_or_default();
            let (ma, mb) = (mean(&a).unwrap_or(0.0), mean(&b).unwrap_or(0.0));
            (ma + mb, format!("Short term {ma:+.1} % + long term {mb:+.1} % = total {:+.1} %", ma + mb))
        }
        "switches" => {
            let n = crossings(&a);
            let per_min = n as f64 * 60.0 / span_s;
            (per_min, format!("{} crossed 0.45 V {n} times in {span_s:.0} s ({per_min:.0} a minute)", name(p0)))
        }
        "switch_ratio" => {
            let Some(&p1) = pids.get(1) else { return Measured::none("The test names only one sensor.") };
            let b = series(samples, p1);
            let (nf, nr) = (crossings(&a), crossings(&b));
            if nf < 3 {
                // Points at the test's "front-flat" answer; callers ignore answers a test lacks.
                return Measured {
                    value: None,
                    text: format!(
                        "The front sensor crossed 0.45 V only {nf} times in {span_s:.0} s: it isn't switching, so the two can't be compared."
                    ),
                    result: Some("front-flat".to_string()),
                };
            }
            let ratio = nr as f64 / nf as f64;
            (
                ratio,
                format!("Front sensor crossed 0.45 V {nf} times, rear {nr} times in {span_s:.0} s: ratio {ratio:.2}"),
            )
        }
        "diff" => {
            let b = pids.get(1).map(|&p| series(samples, p)).unwrap_or_default();
            let (ma, mb) = (mean(&a).unwrap_or(0.0), mean(&b).unwrap_or(0.0));
            (
                ma - mb,
                format!(
                    "{} {ma:.1} minus {} {mb:.1} = {:.1}",
                    name(p0),
                    pids.get(1).map_or(String::new(), |&p| name(p)),
                    ma - mb
                ),
            )
        }
        other => return Measured::none(format!("Scana doesn't know the measure '{other}'.")),
    };
    Measured { value: Some(value), text, result: outcome_for(auto, value) }
}

/// How far a test result is past its limit: over 1.0 is a fail (1.3 is 30 % past the
/// limit); under 1.0 a pass (the nearer to 1.0, the closer to failing).
pub fn limit_ratio(value: f64, min: f64, max: f64) -> f64 {
    let upper = if max > 0.0 { value / max } else { 0.0 };
    let lower = if min > 0.0 { min / value.max(1e-9) } else { 0.0 };
    upper.max(lower)
}

/// Mode 06: the worst result among the named monitors.
pub fn evaluate_mode06(auto: &Auto, results: &[(CanId, Vec<MonitorResult>)]) -> Measured {
    let mids: Vec<u8> =
        auto.mid.as_deref().unwrap_or("").split(',').filter_map(|m| u8::from_str_radix(m.trim(), 16).ok()).collect();
    let mut worst: Option<(f64, &MonitorResult)> = None;
    for (_, list) in results {
        for t in list.iter().filter(|t| mids.contains(&t.mid) && t.scaled) {
            let r = limit_ratio(t.value, t.min, t.max);
            if worst.is_none_or(|(w, _)| r > w) {
                worst = Some((r, t));
            }
        }
    }
    match worst {
        Some((r, t)) => Measured {
            value: Some(r),
            text: format!(
                "{}, {}: {} (limits {}): {}",
                mode06::monitor_name(t.mid),
                mode06::test_name(t.mid, t.tid),
                t.value_text(),
                t.limits_text(),
                if t.passed() {
                    format!("passes, at {:.0} % of the limit", r * 100.0)
                } else {
                    format!("fails, {r:.2} times the limit")
                }
            ),
            result: outcome_for(auto, r),
        },
        None => Measured::none("The car didn't report this test result."),
    }
}

/// Readiness: 1 if the named monitor is complete, 0 if not.
pub fn evaluate_readiness(auto: &Auto, statuses: &[MonitorStatus]) -> Measured {
    let want = auto.monitor.as_deref().unwrap_or("").to_lowercase();
    for s in statuses {
        if let Some(m) = s.monitors.iter().find(|m| m.available && m.name.to_lowercase().contains(&want)) {
            let v = if m.complete { 1.0 } else { 0.0 };
            return Measured {
                value: Some(v),
                text: format!("{} monitor: {}", m.name, if m.complete { "complete" } else { "not complete" }),
                result: outcome_for(auto, v),
            };
        }
    }
    Measured::none("The car doesn't report this monitor.")
}

/// Freeze frame: the first PID's value when the code was stored.
pub fn evaluate_freeze(auto: &Auto, frames: &[FreezeFrame]) -> Measured {
    let Some(pid) = auto.pids.first().and_then(|p| u8::from_str_radix(p, 16).ok()) else {
        return Measured::none("The test names no value.");
    };
    for f in frames {
        if let Some(r) = f.readings.iter().find(|r| r.pid == pid)
            && let Some(v) = r.decoded.as_ref().and_then(|d| d.main()).map(|q| q.value)
        {
            return Measured {
                value: Some(v),
                text: format!("Freeze frame ({}): {} {}", f.dtc, r.name, r.display()),
                result: outcome_for(auto, v),
            };
        }
    }
    Measured::none("No freeze frame holds this value.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalyst() -> Diagnosis {
        Diagnosis::for_code(Dtc(0x0420)).expect("a catalyst guide")
    }

    #[test]
    fn answers_move_the_ranking() {
        let mut d = catalyst();
        let first = d.ranking();
        assert_eq!(first[0].cause.id, "converter");
        let total: f64 = first.iter().map(|r| r.share).sum();
        assert!((total - 1.0).abs() < 1e-9);
        d.answer("leak-check", "found");
        let r = d.ranking();
        assert_eq!(r[0].cause.id, "leak", "{:?}", r.iter().map(|x| (&x.cause.id, x.share)).collect::<Vec<_>>());
        assert!(r[0].why[0].contains("8 times as likely"));
        // Unknown ids are ignored.
        d.answer("leak-check", "nonsense");
        d.answer("nonsense", "found");
        assert_eq!(d.answered(), 1);
        d.clear("leak-check");
        assert_eq!(d.ranking()[0].cause.id, "converter");
    }

    #[test]
    fn stops_and_fix_first() {
        let mut d = catalyst();
        d.answer("other-codes", "misfire");
        assert_eq!(d.stops().len(), 1);
        let codes = [Dtc(0x0420), Dtc(0x0302), Dtc(0x0128)];
        assert_eq!(d.fix_first_present(&codes), vec![Dtc(0x0302)]);
        assert!(d.summary_lines()[0].starts_with("Fix first"));
    }

    #[test]
    fn next_test_prefers_no_tools() {
        let mut d = catalyst();
        for t in &d.guide.test.clone() {
            if t.no_tools() {
                d.answer(&t.id, &t.result[0].id);
            }
        }
        let next = d.next_test().unwrap();
        assert!(!next.no_tools());
    }

    #[test]
    fn code_groups() {
        let d = catalyst();
        let auto = d.test("other-codes").unwrap().auto.as_ref().unwrap();
        assert_eq!(evaluate_codes(auto, &[Dtc(0x0420)]).result.as_deref(), Some("none"));
        assert_eq!(evaluate_codes(auto, &[Dtc(0x0420), Dtc(0x0303)]).result.as_deref(), Some("misfire"));
        assert_eq!(evaluate_codes(auto, &[Dtc(0x0171)]).result.as_deref(), Some("mixture"));
        assert_eq!(evaluate_codes(auto, &[Dtc(0x0141)]).result.as_deref(), Some("sensor"));
    }

    fn o2_samples(front_hz: f64, rear_hz: f64, rear_mid: f64) -> Vec<Sample> {
        let mut v = Vec::new();
        for i in 0..300u64 {
            let t = i * 100;
            let s = t as f64 / 1000.0;
            v.push(Sample { t_ms: t, pid: 0x14, value: 0.45 + 0.4 * (s * front_hz * std::f64::consts::TAU).sin() });
            v.push(Sample { t_ms: t, pid: 0x15, value: rear_mid + 0.35 * (s * rear_hz * std::f64::consts::TAU).sin() });
        }
        v
    }

    #[test]
    fn oxygen_sensor_comparison() {
        let d = catalyst();
        let auto = d.test("rear-compare").unwrap().auto.as_ref().unwrap();
        // Front switching at 1 Hz, rear nearly still: a good converter.
        let m = evaluate_live(auto, &o2_samples(1.0, 0.05, 0.65));
        assert_eq!(m.result.as_deref(), Some("steady"), "{}", m.text);
        // Rear copying the front: worn converter.
        let m = evaluate_live(auto, &o2_samples(1.0, 0.9, 0.45));
        assert_eq!(m.result.as_deref(), Some("copies"), "{}", m.text);
        // Front not switching: can't compare.
        let flat: Vec<Sample> = (0..100).map(|i| Sample { t_ms: i * 100, pid: 0x14, value: 0.7 }).collect();
        let m = evaluate_live(auto, &flat);
        assert_eq!(m.result.as_deref(), Some("front-flat"));
        assert_eq!(evaluate_live(auto, &[]).result, None);
        let level = d.test("rear-level").unwrap().auto.as_ref().unwrap();
        let low: Vec<Sample> = (0..100).map(|i| Sample { t_ms: i * 100, pid: 0x15, value: 0.1 }).collect();
        assert_eq!(evaluate_live(level, &low).result.as_deref(), Some("low"));
        assert!(live_pids(auto).contains(&0x0C) && live_pids(auto).contains(&0x05));
        assert!(live_supported(auto, &[0x05, 0x0C, 0x14, 0x15]));
        assert!(!live_supported(auto, &[0x05, 0x0C, 0x15]));
    }

    #[test]
    fn mode06_limits() {
        assert!((limit_ratio(0.75, 0.0, 0.5) - 1.5).abs() < 1e-9);
        assert!(limit_ratio(0.25, 0.0, 0.5) < 1.0);
        assert!((limit_ratio(0.5, 1.0, 0.0) - 2.0).abs() < 1e-9);
        let d = catalyst();
        let auto = d.test("mode06").unwrap().auto.as_ref().unwrap();
        let fail = MonitorResult {
            mid: 0x21,
            tid: 0x80,
            uasid: 0x20,
            value: 0.75,
            min: 0.0,
            max: 0.6,
            unit: "",
            scaled: true,
            max_open: false,
        };
        let m = evaluate_mode06(auto, &[(CanId::Standard(0x7E8), vec![fail.clone()])]);
        assert_eq!(m.result.as_deref(), Some("just"), "{}", m.text);
        let far = MonitorResult { value: 0.9, ..fail.clone() };
        assert_eq!(evaluate_mode06(auto, &[(CanId::Standard(0x7E8), vec![far])]).result.as_deref(), Some("far"));
        let ok = MonitorResult { value: 0.2, ..fail };
        assert_eq!(evaluate_mode06(auto, &[(CanId::Standard(0x7E8), vec![ok])]).result.as_deref(), Some("pass"));
        assert_eq!(evaluate_mode06(auto, &[]).value, None);
    }

    #[test]
    fn the_report_names_the_causes() {
        let mut d = catalyst();
        d.answer("rattle", "rattle");
        let html = d.to_html("Test car", "2026-10-16 10:00");
        assert!(html.contains("P0420") && html.contains("worn out") && html.contains("It rattles"));
    }

    #[test]
    fn effect_words_read_well() {
        assert_eq!(effect_words(1), "twice as likely");
        assert_eq!(effect_words(-2), "a quarter as likely");
        assert_eq!(effect_words(5), "32 times as likely");
    }

    #[test]
    fn pauses_in_a_live_test_dont_count_as_time() {
        // 10 s of readings, a 50 s pause (the revs left the range), 10 s more.
        let t: Vec<u64> = (0..100).map(|i| i * 100).chain((0..100).map(|i| 60_000 + i * 100)).collect();
        let s = measured_seconds(t.into_iter());
        assert!((19.0..=20.0).contains(&s), "{s}");
        assert_eq!(measured_seconds(std::iter::empty()), 1.0);
    }

    /// The sensors' voltages from a simulated car's engine computer, every 100 ms over `from..to` ms.
    fn car_o2_samples(car: &scana_sim::VirtualCar, from: u64, to: u64) -> Vec<Sample> {
        let ecu = &car.ecus[0];
        let mut out = Vec::new();
        for t in (from..to).step_by(100) {
            for pid in [0x14u8, 0x15] {
                let raw = ecu.pid_value(pid, t).unwrap();
                out.push(Sample { t_ms: t, pid, value: f64::from(raw[0]) / 200.0 });
            }
        }
        out
    }

    #[test]
    fn the_corollas_worn_converter_shows_in_the_sensor_test() {
        let g = scana_db::diag::guide("catalyst").unwrap();
        let auto = g.test.iter().find(|t| t.id == "rear-compare").unwrap().auto.as_ref().unwrap();
        // During the Corolla's 2500 rpm hold (50-80 s), the rear sensor copies the front.
        let m = evaluate_live(auto, &car_o2_samples(&scana_sim::VirtualCar::corolla_2006(), 50_000, 80_000));
        assert_eq!(m.result.as_deref(), Some("copies"), "{}", m.text);
        // The Hijet's good converter keeps the rear sensor steady.
        let m = evaluate_live(auto, &car_o2_samples(&scana_sim::VirtualCar::hijet(), 300_000, 330_000));
        assert_eq!(m.result.as_deref(), Some("steady"), "{}", m.text);
    }
}
