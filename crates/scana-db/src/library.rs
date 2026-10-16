//! The Library: topics that explain how to read live data, how the car's systems work, and
//! how CAN and diagnostics work (`data/learn/topics/`), with the words that link to them
//! from any text and a search across everything Scana knows.
//!
//! Linking: every topic's title and aliases, and every glossary word, is a phrase. `find_links`
//! finds them in a text, longest first, whole words only, each one once per text. Acronyms (two
//! or more capitals and no small letters, such as CAN or MAP) match only in capitals, so the
//! word "can" stays plain.

use crate::learn::{self, Term};
use scana_proto::Dtc;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Topic {
    pub id: String,
    pub title: String,
    /// `live-data`, `systems` or `protocols`.
    pub group: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// One sentence: shown when the pointer rests on a linked word.
    pub summary: String,
    #[serde(default)]
    pub body: Vec<String>,
    #[serde(default)]
    pub how_to_read: Vec<String>,
    #[serde(default)]
    pub parts: Vec<String>,
    #[serde(default)]
    pub faults: Vec<String>,
    #[serde(default)]
    pub more_detail: Vec<String>,
    /// Links: a topic id, or `term:`, `pid:`, `guide:`, `code:`, `page:` and a value.
    #[serde(default)]
    pub related: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Default, Deserialize)]
struct TopicFile {
    #[serde(default)]
    topic: Vec<Topic>,
    #[serde(default)]
    term: Vec<Term>,
}

/// The groups, in the order the Library shows them: (id, title).
pub const GROUPS: [(&str, &str); 3] =
    [("live-data", "Reading live data"), ("systems", "Car systems and parts"), ("protocols", "CAN and diagnostics")];

const FILES: [&str; 6] = [
    include_str!("../data/learn/topics/live-data-reading.toml"),
    include_str!("../data/learn/topics/live-data-patterns.toml"),
    include_str!("../data/learn/topics/systems-engine.toml"),
    include_str!("../data/learn/topics/systems-other.toml"),
    include_str!("../data/learn/topics/protocols-can.toml"),
    include_str!("../data/learn/topics/protocols-diagnostics.toml"),
];

static PARSED: LazyLock<Vec<TopicFile>> =
    LazyLock::new(|| FILES.iter().map(|f| toml::from_str::<TopicFile>(f).unwrap_or_default()).collect());
static TOPICS: LazyLock<Vec<Topic>> = LazyLock::new(|| PARSED.iter().flat_map(|f| f.topic.clone()).collect());

/// Glossary words that came with the topics (merged into `learn::glossary`).
pub(crate) fn extra_terms() -> Vec<Term> {
    PARSED.iter().flat_map(|f| f.term.clone()).collect()
}

pub fn topics() -> &'static [Topic] {
    &TOPICS
}

pub fn topic(id: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.id == id)
}

pub fn group_title(group: &str) -> &'static str {
    GROUPS.iter().find(|(g, _)| *g == group).map_or("Other", |(_, t)| t)
}

/// Something a word or a related entry can lead to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    Topic(String),
    Term(String),
    Pid(u8),
    Guide(String),
    Code(String),
    Page(String),
}

impl Link {
    /// Reads a `related` entry: a topic id, or `kind:value`. `None` if it names nothing Scana has.
    pub fn parse(s: &str) -> Option<Link> {
        let link = match s.split_once(':') {
            Some(("term", v)) => Link::Term(learn::term(v)?.name.clone()),
            Some(("pid", v)) => Link::Pid(u8::from_str_radix(v, 16).ok()?),
            Some(("guide", v)) => Link::Guide(v.to_string()),
            Some(("code", v)) => Link::Code(Dtc::parse(v)?.to_string()),
            Some(("page", v)) => Link::Page(v.to_string()),
            Some(_) => return None,
            None => Link::Topic(s.to_string()),
        };
        link.exists().then_some(link)
    }

    pub fn exists(&self) -> bool {
        match self {
            Link::Topic(id) => topic(id).is_some(),
            Link::Term(n) => learn::term(n).is_some(),
            Link::Pid(p) => scana_proto::pids::def(*p).is_some(),
            Link::Guide(g) => crate::diag::guide(g).is_some(),
            Link::Code(c) => Dtc::parse(c).is_some(),
            Link::Page(p) => learn::page(p).is_some(),
        }
    }

    /// A short name: the topic's title, the word, "PID 0C Engine speed", the guide's title.
    pub fn title(&self) -> String {
        match self {
            Link::Topic(id) => topic(id).map_or_else(|| id.clone(), |t| t.title.clone()),
            Link::Term(n) => n.clone(),
            Link::Pid(p) => {
                format!("PID {p:02X} {}", scana_proto::pids::def(*p).map_or("", |d| d.name)).trim_end().to_string()
            }
            Link::Guide(g) => crate::diag::guide(g).map_or_else(|| g.clone(), |x| format!("Guide: {}", x.title)),
            Link::Code(c) => c.clone(),
            Link::Page(p) => learn::page(p).map_or_else(|| p.clone(), |x| x.title.clone()),
        }
    }

    /// One or two sentences: shown when the pointer rests on the link.
    pub fn summary(&self) -> Option<String> {
        match self {
            Link::Topic(id) => topic(id).map(|t| t.summary.clone()),
            Link::Term(n) => learn::term(n).map(|t| t.text.clone()),
            Link::Pid(p) => learn::pid(*p).map(|x| x.what.clone()),
            Link::Guide(g) => crate::diag::guide(g).map(|x| x.title.clone()),
            Link::Code(c) => Dtc::parse(c).map(|d| crate::dtc::lookup(d).summary()),
            Link::Page(p) => learn::page(p).map(|x| x.text.clone()),
        }
    }
}

struct Phrase {
    text: String,
    exact_case: bool,
    link: Link,
}

fn is_acronym(s: &str) -> bool {
    s.chars().filter(|c| c.is_ascii_uppercase()).count() >= 2 && !s.chars().any(|c| c.is_lowercase())
}

/// The first word of a phrase, the key phrases are filed under.
fn first_word(s: &str) -> String {
    s.chars().take_while(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

/// Phrases by first word, longest first. Topics win over glossary words with the same phrase.
static PHRASES: LazyLock<HashMap<String, Vec<Phrase>>> = LazyLock::new(|| {
    let mut seen: HashSet<String> = HashSet::new();
    let mut all: Vec<Phrase> = Vec::new();
    for t in topics() {
        for p in std::iter::once(&t.title).chain(t.aliases.iter()) {
            if seen.insert(p.to_lowercase()) {
                all.push(Phrase { text: p.clone(), exact_case: is_acronym(p), link: Link::Topic(t.id.clone()) });
            }
        }
    }
    for term in learn::glossary() {
        if seen.insert(term.name.to_lowercase()) {
            all.push(Phrase {
                text: term.name.clone(),
                exact_case: is_acronym(&term.name),
                link: Link::Term(term.name.clone()),
            });
        }
    }
    let mut map: HashMap<String, Vec<Phrase>> = HashMap::new();
    for p in all.into_iter().filter(|p| !first_word(&p.text).is_empty()) {
        map.entry(first_word(&p.text)).or_default().push(p);
    }
    for v in map.values_mut() {
        v.sort_by_key(|p| std::cmp::Reverse(p.text.len()));
    }
    map
});

fn matches_at(text: &str, at: usize, p: &Phrase) -> Option<usize> {
    let end = at + p.text.len();
    let slice = text.get(at..end)?;
    let same = if p.exact_case { slice == p.text } else { slice.eq_ignore_ascii_case(&p.text) };
    if !same {
        return None;
    }
    let after = |i: usize| text[i..].chars().next();
    match after(end) {
        None => Some(end),
        Some(c) if !c.is_alphanumeric() => Some(end),
        // A plural: "sensors" for "sensor".
        Some('s') | Some('S') if !p.exact_case && after(end + 1).is_none_or(|c| !c.is_alphanumeric()) => Some(end + 1),
        Some(_) => None,
    }
}

/// The known words in `text`: byte ranges and where each leads, in order. Each link appears
/// once (its first mention); links in `skip` (the topic being read) are left plain.
pub fn find_links(text: &str, skip: &[Link]) -> Vec<(Range<usize>, Link)> {
    let mut out: Vec<(Range<usize>, Link)> = Vec::new();
    let mut used: HashSet<Link> = skip.iter().cloned().collect();
    let mut i = 0;
    while i < text.len() {
        let prev_is_word = text[..i].chars().next_back().is_some_and(char::is_alphanumeric);
        let c = text[i..].chars().next().unwrap_or(' ');
        if c.is_alphanumeric() && !prev_is_word {
            let word = first_word(&text[i..]);
            if let Some(cands) = PHRASES.get(&word)
                && let Some((end, link)) = cands.iter().find_map(|p| matches_at(text, i, p).map(|e| (e, &p.link)))
            {
                if used.insert(link.clone()) {
                    out.push((i..end, link.clone()));
                }
                i = end;
                continue;
            }
        }
        i += c.len_utf8();
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    Topic,
    Term,
    Value,
    Code,
    Guide,
    Page,
}

impl HitKind {
    pub fn label(self) -> &'static str {
        match self {
            HitKind::Topic => "Library",
            HitKind::Term => "Word",
            HitKind::Value => "Live value",
            HitKind::Code => "Code",
            HitKind::Guide => "Guide",
            HitKind::Page => "Page",
        }
    }
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub kind: HitKind,
    pub link: Link,
    pub title: String,
    /// A line under the title: the summary or meaning.
    pub detail: String,
    score: u32,
}

/// How well `q` (lower case) matches one name: 100 exact, 80 at the start, 60 inside.
fn name_score(name: &str, q: &str) -> u32 {
    let n = name.to_lowercase();
    if n == q {
        100
    } else if n.starts_with(q) {
        80
    } else if n.contains(q) {
        60
    } else {
        0
    }
}

/// Searches topics, words, live values, codes, guides and pages. Best first, at most `limit`.
pub fn search(query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim().to_lowercase();
    if q.len() < 2 {
        return vec![];
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let has_all = |hay: &str| {
        let h = hay.to_lowercase();
        words.iter().all(|w| h.contains(w))
    };
    let mut hits: Vec<Hit> = Vec::new();
    if let Some(d) = Dtc::parse(query.trim()) {
        hits.push(Hit {
            kind: HitKind::Code,
            link: Link::Code(d.to_string()),
            title: d.to_string(),
            detail: crate::dtc::lookup(d).summary(),
            score: 200,
        });
    }
    for t in topics() {
        let name = std::iter::once(&t.title).chain(t.aliases.iter()).map(|n| name_score(n, &q)).max().unwrap_or(0);
        let score = if name > 0 {
            name + 5
        } else if has_all(&t.summary) {
            40
        } else if t.body.iter().chain(t.how_to_read.iter()).any(|b| has_all(b)) {
            20
        } else {
            0
        };
        if score > 0 {
            hits.push(Hit {
                kind: HitKind::Topic,
                link: Link::Topic(t.id.clone()),
                title: t.title.clone(),
                detail: t.summary.clone(),
                score,
            });
        }
    }
    for term in learn::glossary() {
        let score = match name_score(&term.name, &q) {
            0 if has_all(&term.text) => 25,
            s => s,
        };
        if score > 0 {
            hits.push(Hit {
                kind: HitKind::Term,
                link: Link::Term(term.name.clone()),
                title: term.name.clone(),
                detail: term.text.clone(),
                score,
            });
        }
    }
    for p in 1..=255u8 {
        let Some(def) = scana_proto::pids::def(p) else { continue };
        if scana_proto::pids::is_support_pid(p) {
            continue;
        }
        let hex = format!("{p:02x}");
        let score = if q == hex || q == format!("pid {hex}") { 100 } else { name_score(def.name, &q) };
        if score > 0 {
            hits.push(Hit {
                kind: HitKind::Value,
                link: Link::Pid(p),
                title: format!("{} (PID {p:02X})", def.name),
                detail: learn::pid(p).map(|x| x.what.clone()).unwrap_or_default(),
                score: score.saturating_sub(5),
            });
        }
    }
    for g in crate::diag::search(query.trim()).into_iter().take(8) {
        hits.push(Hit {
            kind: HitKind::Guide,
            link: Link::Guide(g.id.clone()),
            title: g.title.clone(),
            detail: g.summary.clone(),
            score: 30 + name_score(&g.title, &q) / 2,
        });
    }
    for id in PAGE_IDS {
        if let Some(p) = learn::page(id) {
            let score = name_score(&p.title, &q);
            if score > 0 {
                hits.push(Hit {
                    kind: HitKind::Page,
                    link: Link::Page(id.to_string()),
                    title: p.title.clone(),
                    detail: p.text.clone(),
                    score: score.saturating_sub(10),
                });
            }
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.len().cmp(&b.title.len())));
    hits.truncate(limit);
    hits
}

/// The pages a search can open.
const PAGE_IDS: [&str; 16] = [
    "home",
    "health",
    "codes",
    "diagnose",
    "live",
    "graphs",
    "readiness",
    "tests",
    "info",
    "modules",
    "explorer",
    "monitor",
    "sessions",
    "learn",
    "glossary",
    "settings",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn links_in(text: &str) -> Vec<(String, Link)> {
        find_links(text, &[]).into_iter().map(|(r, l)| (text[r].to_string(), l)).collect()
    }

    #[test]
    fn the_library_parses_and_holds_together() {
        assert!(topics().len() >= 120, "{}", topics().len());
        let mut ids = HashSet::new();
        let mut aliases: HashMap<String, &str> = HashMap::new();
        for t in topics() {
            assert!(ids.insert(t.id.as_str()), "topic {} twice", t.id);
            assert!(GROUPS.iter().any(|(g, _)| *g == t.group), "{}: group {}", t.id, t.group);
            assert!(t.summary.split_whitespace().count() <= 25, "{}: summary too long", t.id);
            assert!(!t.body.is_empty() && t.sources.len() >= 2, "{}", t.id);
            for r in &t.related {
                assert!(Link::parse(r).is_some(), "{}: related {r} leads nowhere", t.id);
            }
            for a in std::iter::once(&t.title).chain(t.aliases.iter()) {
                if let Some(other) = aliases.insert(a.to_lowercase(), &t.id) {
                    assert_eq!(other, t.id, "'{a}' links to both {other} and {}", t.id);
                }
            }
            let all = [&t.title, &t.summary]
                .into_iter()
                .chain(t.body.iter())
                .chain(t.how_to_read.iter())
                .chain(t.parts.iter())
                .chain(t.faults.iter())
                .chain(t.more_detail.iter())
                .chain(t.sources.iter());
            for s in all {
                assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "{}: a dash in {s}", t.id);
            }
        }
        // The words that came with the topics are in the glossary, once each.
        let names: Vec<String> = learn::glossary().iter().map(|t| t.name.to_lowercase()).collect();
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(names.len(), unique.len());
        assert!(learn::term("Unmetered air").is_some());
    }

    #[test]
    fn known_words_link_once_and_acronyms_keep_their_case() {
        let l = links_in("The engine computer is in closed loop: it uses the oxygen sensor. Closed loop again.");
        assert_eq!(l.iter().filter(|(w, _)| w.eq_ignore_ascii_case("closed loop")).count(), 1, "{l:?}");
        assert!(l.iter().any(|(w, _)| w == "oxygen sensor"), "{l:?}");
        // "can" is a word; "CAN" is the bus.
        let l = links_in("You can read the CAN bus here.");
        assert!(l.iter().all(|(w, _)| w != "can"), "{l:?}");
        assert!(l.iter().any(|(w, _)| w.starts_with("CAN")), "{l:?}");
        // Plurals link; part of a longer word doesn't.
        assert!(links_in("Two oxygen sensors.").iter().any(|(w, _)| w == "oxygen sensors"));
        assert!(links_in("Rearrange the ruler.").is_empty());
        // The longest phrase wins.
        let l = links_in("A torque converter slips.");
        assert_eq!(l.first().map(|(w, _)| w.as_str()), Some("torque converter"), "{l:?}");
        // The topic being read doesn't link to itself.
        let skip = [Link::Topic("fuel-trim".into())];
        assert!(find_links("Fuel trim is high.", &skip).iter().all(|(_, l)| *l != skip[0]));
    }

    #[test]
    fn search_finds_topics_codes_values_and_guides() {
        let first = |q: &str| search(q, 5).into_iter().next().map(|h| (h.kind, h.link));
        assert_eq!(first("fuel trim"), Some((HitKind::Topic, Link::Topic("fuel-trim".into()))));
        assert_eq!(first("P0420").map(|x| x.0), Some(HitKind::Code));
        assert_eq!(first("0c"), Some((HitKind::Value, Link::Pid(0x0C))));
        assert!(search("vacuum leak", 10).iter().any(|h| h.link == Link::Topic("pattern-vacuum-leak".into())));
        assert!(search("catalytic", 10).iter().any(|h| h.kind == HitKind::Guide));
        assert!(search("x", 10).is_empty());
    }
}
