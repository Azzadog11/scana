//! Learn content: what each live value means, the glossary, guided jobs, page help and the
//! "How OBD-II works" lesson. All from `data/learn/`.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PidLearn {
    pub id: u8,
    pub what: String,
    #[serde(default)]
    pub normal: Option<String>,
    #[serde(default)]
    pub read_with: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Term {
    pub name: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Step {
    pub text: String,
    #[serde(default)]
    pub page: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub title: String,
    pub why: String,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PageHelp {
    pub id: String,
    pub title: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub text: String,
}

#[derive(Default, Deserialize)]
struct PidFile {
    pid: Vec<PidLearn>,
}
#[derive(Default, Deserialize)]
struct TermFile {
    term: Vec<Term>,
}
#[derive(Default, Deserialize)]
struct JobFile {
    job: Vec<Job>,
}
#[derive(Default, Deserialize)]
struct PageFile {
    page: Vec<PageHelp>,
}
#[derive(Default, Deserialize)]
struct LessonFile {
    section: Vec<Section>,
}

static PIDS: LazyLock<Vec<PidLearn>> =
    LazyLock::new(|| toml::from_str::<PidFile>(include_str!("../data/learn/pids.toml")).unwrap_or_default().pid);
static TERMS: LazyLock<Vec<Term>> = LazyLock::new(|| {
    let mut t = toml::from_str::<TermFile>(include_str!("../data/learn/glossary.toml")).unwrap_or_default().term;
    t.sort_by_key(|x| x.name.to_lowercase());
    t
});
static JOBS: LazyLock<Vec<Job>> =
    LazyLock::new(|| toml::from_str::<JobFile>(include_str!("../data/learn/jobs.toml")).unwrap_or_default().job);
static PAGES: LazyLock<Vec<PageHelp>> =
    LazyLock::new(|| toml::from_str::<PageFile>(include_str!("../data/learn/pages.toml")).unwrap_or_default().page);
static LESSON: LazyLock<Vec<Section>> = LazyLock::new(|| {
    toml::from_str::<LessonFile>(include_str!("../data/learn/lesson.toml")).unwrap_or_default().section
});

pub fn pid(id: u8) -> Option<&'static PidLearn> {
    PIDS.iter().find(|p| p.id == id)
}

/// Glossary terms, sorted by name.
pub fn glossary() -> &'static [Term] {
    &TERMS
}

pub fn term(name: &str) -> Option<&'static Term> {
    TERMS.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

pub fn jobs() -> &'static [Job] {
    &JOBS
}

pub fn page(id: &str) -> Option<&'static PageHelp> {
    PAGES.iter().find(|p| p.id == id)
}

pub fn lesson() -> &'static [Section] {
    &LESSON
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE_IDS: [&str; 13] = [
        "home",
        "health",
        "codes",
        "live",
        "graphs",
        "readiness",
        "tests",
        "info",
        "modules",
        "monitor",
        "sessions",
        "learn",
        "settings",
    ];

    fn no_dashes(s: &str) -> bool {
        !s.contains('\u{2014}') && !s.contains('\u{2013}')
    }

    #[test]
    fn every_file_parses() {
        assert!(PIDS.len() >= 30, "pids: {}", PIDS.len());
        assert!(TERMS.len() >= 30, "terms: {}", TERMS.len());
        assert!(JOBS.len() >= 5);
        assert_eq!(PAGES.len(), PAGE_IDS.len());
        assert!(LESSON.len() >= 5);
    }

    #[test]
    fn the_hijets_pids_all_have_explanations() {
        for p in [
            0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x13, 0x14, 0x15, 0x1C, 0x1F,
            0x21, 0x2E, 0x33, 0x41, 0x42, 0x45, 0x47, 0x49, 0x4A, 0x4C, 0x4D, 0x4E,
        ] {
            assert!(pid(p).is_some(), "no explanation for PID {p:02X}");
        }
    }

    #[test]
    fn pages_and_job_steps_point_at_real_pages() {
        for id in PAGE_IDS {
            assert!(page(id).is_some(), "{id}");
        }
        for j in jobs() {
            for s in &j.steps {
                if let Some(p) = &s.page {
                    assert!(PAGE_IDS.contains(&p.as_str()), "job {} points at {p}", j.id);
                }
            }
        }
    }

    #[test]
    fn plain_text_rules() {
        for p in PIDS.iter() {
            assert!(no_dashes(&p.what) && p.normal.as_deref().is_none_or(no_dashes));
        }
        for t in glossary() {
            assert!(no_dashes(&t.text), "{}", t.name);
        }
        assert!(term("mil").is_some());
    }
}
