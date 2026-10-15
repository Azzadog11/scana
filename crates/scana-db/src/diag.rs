//! Diagnosis guides (`data/diag/*.toml`): for a fault code, what it means, how the system
//! works, the likely causes, and tests that tell the causes apart. Each test's answer moves
//! the likelihood of each cause. Tests can be answered by hand (eyes and ears, a meter, or
//! another scan tool's readings); some Scana can also measure itself on a CAN car (`auto`).
//!
//! Also the tools a test may need (`data/diag/tools.toml`), so a guide can say "an infrared
//! thermometer could check this" without assuming the user owns one.
//!
//! Written in simplified technical English: short sentences, one instruction per step.

use scana_proto::Dtc;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// One fault code family's guide.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Guide {
    pub id: String,
    pub title: String,
    /// Codes this guide covers: "P0420", or a range "P0300-P0312".
    pub codes: Vec<String>,
    /// What the code means, in a few plain sentences.
    pub summary: String,
    #[serde(default)]
    pub how_it_works: Vec<String>,
    /// Glossary words used in the guide.
    #[serde(default)]
    pub terms: Vec<String>,
    /// Codes to fix before this one ("P0300-P0312"): their causes often cause this code.
    #[serde(default)]
    pub fix_first: Vec<String>,
    #[serde(default)]
    pub safety: Vec<String>,
    /// Where the facts come from.
    pub sources: Vec<String>,
    /// Other guides worth reading.
    #[serde(default)]
    pub related: Vec<String>,
    pub cause: Vec<Cause>,
    pub test: Vec<Test>,
    /// What is known for particular makes and models.
    #[serde(default)]
    pub note: Vec<MakeNote>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Cause {
    pub id: String,
    pub name: String,
    pub detail: String,
    /// How often this is the cause, relative to the guide's other causes (an estimate from
    /// the sources; it only sets the starting order).
    pub weight: u32,
    /// What fixes it.
    pub fix: String,
    /// Parts cost class: "low", "medium" or "high" (no prices: they change).
    #[serde(default)]
    pub cost: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Test {
    pub id: String,
    pub title: String,
    /// What the test tells you.
    pub why: String,
    /// Tool ids from `tools.toml`. Empty: no tools (eyes, ears, hands). "scan-tool": live
    /// data from Scana or another scan tool.
    #[serde(default)]
    pub needs: Vec<String>,
    pub steps: Vec<String>,
    #[serde(default)]
    pub safety: Vec<String>,
    /// How Scana can measure it on a connected CAN car.
    #[serde(default)]
    pub auto: Option<Auto>,
    pub result: Vec<TestResult>,
}

impl Test {
    pub fn result(&self, id: &str) -> Option<&TestResult> {
        self.result.iter().find(|r| r.id == id)
    }

    /// True if the test needs no tools beyond eyes and ears (or a scan tool).
    pub fn no_tools(&self) -> bool {
        self.needs.iter().all(|n| n == "scan-tool")
    }
}

/// One possible answer to a test.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TestResult {
    pub id: String,
    pub text: String,
    /// How the answer moves each cause: +1 makes it twice as likely, +2 four times,
    /// -1 half as likely, and so on.
    #[serde(default)]
    pub effect: BTreeMap<String, i32>,
    /// Shown when this answer is chosen.
    #[serde(default)]
    pub note: Option<String>,
    /// Another guide to go to (for example a misfire found while chasing P0420).
    #[serde(default)]
    pub goto: Option<String>,
    /// Fix this first: the rest of the guide waits for it.
    #[serde(default)]
    pub stop: bool,
}

/// How Scana measures a test by itself.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Auto {
    /// "codes", "live", "mode06", "readiness" or "freeze".
    pub kind: String,
    /// codes: groups of code patterns, each giving a result if any code matches.
    #[serde(default)]
    pub group: Vec<CodeGroup>,
    /// codes: the result when no group matches.
    #[serde(default)]
    pub absent: Option<String>,
    /// live: what to calculate. "mean", "min", "max", "spread" of the first PID; "trim":
    /// the mean of the first two added (short and long term fuel trim); "switches": how
    /// often the first PID crosses 0.45 V, per minute; "switch_ratio": the second PID's
    /// switches divided by the first's (rear sensor over front); "diff": mean of the first
    /// minus mean of the second.
    #[serde(default)]
    pub measure: Option<String>,
    /// live and freeze: PIDs in hex ("0C").
    #[serde(default)]
    pub pids: Vec<String>,
    /// live: only measure while this value is in range (for example engine speed held at 2500 rpm).
    #[serde(default)]
    pub hold: Option<Range>,
    /// live: this must be true before measuring (for example a warm engine).
    #[serde(default)]
    pub need: Option<Range>,
    /// live: seconds of good samples to collect.
    #[serde(default)]
    pub seconds: Option<u32>,
    /// mode06: the monitor ID in hex ("21" catalyst bank 1).
    #[serde(default)]
    pub mid: Option<String>,
    /// readiness: part of the monitor's name ("Catalyst").
    #[serde(default)]
    pub monitor: Option<String>,
    /// Thresholds: the first outcome whose range holds the measured value gives the result.
    /// mode06 measures how far the result is past its limit: over 1.0 is a fail.
    /// readiness gives 1 for complete, 0 for not complete.
    #[serde(default)]
    pub outcome: Vec<Outcome>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct CodeGroup {
    pub patterns: Vec<String>,
    pub result: String,
}

/// A value range on a PID: "engine speed between 2300 and 2700 rpm".
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Range {
    pub pid: String,
    #[serde(default)]
    pub from: Option<f64>,
    #[serde(default)]
    pub to: Option<f64>,
    /// What to tell the user: "Hold the engine at 2500 rpm".
    pub text: String,
}

impl Range {
    pub fn pid(&self) -> Option<u8> {
        u8::from_str_radix(self.pid.trim(), 16).ok()
    }

    pub fn holds(&self, v: f64) -> bool {
        self.from.is_none_or(|f| v >= f) && self.to.is_none_or(|t| v <= t)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Outcome {
    #[serde(default)]
    pub above: Option<f64>,
    #[serde(default)]
    pub below: Option<f64>,
    pub result: String,
}

impl Outcome {
    pub fn holds(&self, v: f64) -> bool {
        self.above.is_none_or(|a| v > a) && self.below.is_none_or(|b| v <= b)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct MakeNote {
    pub make: String,
    #[serde(default)]
    pub models: Option<String>,
    #[serde(default)]
    pub years: Option<String>,
    pub text: String,
    pub source: String,
}

/// A tool a test may need.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub what: String,
    /// How to use it for car diagnosis, briefly.
    #[serde(rename = "use")]
    pub how: String,
    /// "low", "medium" or "high".
    pub cost: String,
    #[serde(default)]
    pub safety: Option<String>,
}

#[derive(Default, Deserialize)]
struct ToolFile {
    tool: Vec<Tool>,
}

/// The guide files, built into the program.
const FILES: &[(&str, &str)] = &[
    ("catalyst", include_str!("../data/diag/catalyst.toml")),
    ("misfire", include_str!("../data/diag/misfire.toml")),
    ("lean", include_str!("../data/diag/lean.toml")),
    ("rich", include_str!("../data/diag/rich.toml")),
    ("o2-sensor", include_str!("../data/diag/o2-sensor.toml")),
];

static GUIDES: LazyLock<Vec<Guide>> =
    LazyLock::new(|| FILES.iter().filter_map(|(_, text)| toml::from_str::<Guide>(text).ok()).collect());

static TOOLS: LazyLock<Vec<Tool>> =
    LazyLock::new(|| toml::from_str::<ToolFile>(include_str!("../data/diag/tools.toml")).unwrap_or_default().tool);

pub fn guides() -> &'static [Guide] {
    &GUIDES
}

pub fn guide(id: &str) -> Option<&'static Guide> {
    GUIDES.iter().find(|g| g.id == id)
}

pub fn tools() -> &'static [Tool] {
    &TOOLS
}

pub fn tool(id: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.id == id)
}

/// True if `code` matches a pattern: an exact code ("P0420") or a range ("P0300-P0312").
pub fn code_matches(pattern: &str, code: Dtc) -> bool {
    match pattern.split_once('-') {
        Some((a, b)) => match (Dtc::parse(a), Dtc::parse(b)) {
            (Some(a), Some(b)) => a.0 <= code.0 && code.0 <= b.0,
            _ => false,
        },
        None => Dtc::parse(pattern) == Some(code),
    }
}

/// The guide for a code, if Scana has one.
pub fn guide_for(code: Dtc) -> Option<&'static Guide> {
    GUIDES.iter().find(|g| g.codes.iter().any(|p| code_matches(p, code)))
}

/// Guides whose title, codes or summary contain every word of `query` (any case).
pub fn search(query: &str) -> Vec<&'static Guide> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return vec![];
    }
    if let Some(code) = Dtc::parse(query.trim()) {
        return guide_for(code).into_iter().collect();
    }
    GUIDES
        .iter()
        .filter(|g| {
            let hay = format!("{} {} {}", g.title, g.codes.join(" "), g.summary).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_dashes(s: &str) -> bool {
        !s.contains('\u{2014}') && !s.contains('\u{2013}')
    }

    #[test]
    fn every_guide_file_parses() {
        for (name, text) in FILES {
            if let Err(e) = toml::from_str::<Guide>(text) {
                panic!("{name}.toml: {e}");
            }
        }
        assert_eq!(GUIDES.len(), FILES.len());
        assert!(TOOLS.len() >= 10, "tools: {}", TOOLS.len());
    }

    #[test]
    fn guides_are_consistent() {
        let mut ids = std::collections::HashSet::new();
        for g in guides() {
            assert!(ids.insert(g.id.clone()), "duplicate guide id {}", g.id);
            assert!(!g.sources.is_empty(), "{}: no sources", g.id);
            assert!(g.cause.len() >= 2, "{}: fewer than 2 causes", g.id);
            assert!(!g.test.is_empty(), "{}: no tests", g.id);
            for c in &g.codes {
                assert!(
                    code_matches(c, Dtc::parse(c.split('-').next().unwrap_or("")).unwrap_or(Dtc(0xFFFF))),
                    "{}: bad code pattern {c}",
                    g.id
                );
            }
            for c in &g.fix_first {
                assert!(Dtc::parse(c.split('-').next().unwrap_or("")).is_some(), "{}: bad fix_first {c}", g.id);
            }
            let causes: Vec<&str> = g.cause.iter().map(|c| c.id.as_str()).collect();
            let mut test_ids = std::collections::HashSet::new();
            for t in &g.test {
                assert!(test_ids.insert(t.id.clone()), "{}: duplicate test {}", g.id, t.id);
                assert!(t.result.len() >= 2, "{}/{}: fewer than 2 results", g.id, t.id);
                for n in &t.needs {
                    assert!(n == "scan-tool" || tool(n).is_some(), "{}/{}: unknown tool {n}", g.id, t.id);
                }
                for r in &t.result {
                    for k in r.effect.keys() {
                        assert!(causes.contains(&k.as_str()), "{}/{}/{}: unknown cause {k}", g.id, t.id, r.id);
                    }
                    if let Some(to) = &r.goto {
                        assert!(guide(to).is_some() || to == &g.id, "{}/{}/{}: no guide {to}", g.id, t.id, r.id);
                    }
                }
                if let Some(a) = &t.auto {
                    assert!(
                        matches!(a.kind.as_str(), "codes" | "live" | "mode06" | "readiness" | "freeze"),
                        "{}/{}: auto kind {}",
                        g.id,
                        t.id,
                        a.kind
                    );
                    let mut want: Vec<&str> = a.outcome.iter().map(|o| o.result.as_str()).collect();
                    want.extend(a.group.iter().map(|x| x.result.as_str()));
                    want.extend(a.absent.as_deref());
                    for w in want {
                        assert!(t.result(w).is_some(), "{}/{}: auto points at unknown result {w}", g.id, t.id);
                    }
                    for p in a.pids.iter().chain(a.hold.iter().map(|h| &h.pid)).chain(a.need.iter().map(|h| &h.pid)) {
                        assert!(u8::from_str_radix(p, 16).is_ok(), "{}/{}: bad PID {p}", g.id, t.id);
                    }
                    if a.kind == "live" {
                        let m = a.measure.as_deref().unwrap_or("");
                        assert!(
                            matches!(
                                m,
                                "mean" | "min" | "max" | "spread" | "trim" | "switches" | "switch_ratio" | "diff"
                            ),
                            "{}/{}: measure {m}",
                            g.id,
                            t.id
                        );
                        assert!(a.seconds.is_some_and(|s| (5..=300).contains(&s)), "{}/{}: seconds", g.id, t.id);
                    }
                }
            }
            for term in &g.terms {
                assert!(crate::learn::term(term).is_some(), "{}: glossary has no '{term}'", g.id);
            }
            for rel in &g.related {
                assert!(guide(rel).is_some(), "{}: related guide {rel} missing", g.id);
            }
            let all_text = format!("{g:?}");
            assert!(no_dashes(&all_text), "{}: has an em or en dash", g.id);
        }
        for t in tools() {
            assert!(matches!(t.cost.as_str(), "low" | "medium" | "high"), "{}", t.id);
            assert!(no_dashes(&t.what) && no_dashes(&t.how));
        }
    }

    #[test]
    fn codes_find_their_guides() {
        assert!(code_matches("P0300-P0312", Dtc(0x0304)));
        assert!(!code_matches("P0300-P0312", Dtc(0x0313)));
        assert!(code_matches("P0420", Dtc(0x0420)));
        assert_eq!(guide_for(Dtc(0x0420)).map(|g| g.id.as_str()), Some("catalyst"));
        assert_eq!(search("P0420").len(), 1);
        assert!(!search("catalytic").is_empty());
        assert!(search("").is_empty());
    }
}
