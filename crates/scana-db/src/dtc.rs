//! Fault code descriptions: Scana's own wording for the generic codes (`data/dtc/generic.tsv`),
//! advice for common ones (`data/dtc/advice.toml`), and a description of the code's group
//! for any code not in the list.

use scana_proto::Dtc;
use scana_proto::dtc::System;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Advice for a code: what it usually means and what to check first.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Advice {
    pub codes: Vec<String>,
    pub means: String,
    pub check: Vec<String>,
}

#[derive(Default, Deserialize)]
struct AdviceFile {
    advice: Vec<Advice>,
}

/// A code's meaning on particular makes (`data/dtc/maker.toml`).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct MakerCode {
    pub makes: Vec<String>,
    pub title: String,
    pub sources: Vec<String>,
    #[serde(flatten)]
    pub advice: Advice,
}

#[derive(Default, Deserialize)]
struct MakerFile {
    code: Vec<MakerCode>,
}

static MAKER: LazyLock<Vec<MakerCode>> =
    LazyLock::new(|| toml::from_str::<MakerFile>(include_str!("../data/dtc/maker.toml")).unwrap_or_default().code);

static TITLES: LazyLock<HashMap<u16, &'static str>> = LazyLock::new(|| {
    include_str!("../data/dtc/generic.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let (code, text) = l.split_once('\t')?;
            Some((Dtc::parse(code)?.0, text.trim()))
        })
        .collect()
});

static ADVICE: LazyLock<Vec<Advice>> =
    LazyLock::new(|| toml::from_str::<AdviceFile>(include_str!("../data/dtc/advice.toml")).unwrap_or_default().advice);

/// Everything Scana can say about a code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DtcInfo {
    pub code: Dtc,
    /// Scana's description, for generic codes in its list.
    pub title: Option<&'static str>,
    /// What the code's group covers, worked out from its number.
    pub group: String,
    pub system: System,
    pub manufacturer_specific: bool,
    pub advice: Option<&'static Advice>,
    /// The meaning on the car's make, when Scana has one.
    pub maker: Option<&'static MakerCode>,
}

impl DtcInfo {
    /// The best one-line description: the title, or the group.
    pub fn summary(&self) -> String {
        match self.title {
            Some(t) => t.to_string(),
            None => format!("Not in Scana's list: {}", self.group),
        }
    }
}

pub fn lookup(code: Dtc) -> DtcInfo {
    let text = code.to_string();
    DtcInfo {
        code,
        title: TITLES.get(&code.0).copied(),
        group: group_of(code),
        system: code.system(),
        manufacturer_specific: code.is_manufacturer_specific(),
        advice: ADVICE.iter().find(|a| a.codes.contains(&text)),
        maker: None,
    }
}

/// Like [`lookup`], with the meaning on the car's make if Scana has it.
pub fn lookup_for(code: Dtc, make: Option<&str>) -> DtcInfo {
    let mut info = lookup(code);
    if let Some(m) = make.and_then(|make| maker_meaning(code, make)) {
        info.title = Some(m.title.as_str());
        info.advice = Some(&m.advice);
        info.maker = Some(m);
    }
    info
}

/// The code's meaning on one make, if Scana has it.
pub fn maker_meaning(code: Dtc, make: &str) -> Option<&'static MakerCode> {
    let text = code.to_string();
    MAKER.iter().find(|m| m.advice.codes.contains(&text) && m.makes.iter().any(|x| x.eq_ignore_ascii_case(make)))
}

/// Every make-specific meaning Scana has for a code.
pub fn maker_meanings(code: Dtc) -> Vec<&'static MakerCode> {
    let text = code.to_string();
    MAKER.iter().filter(|m| m.advice.codes.contains(&text)).collect()
}

/// How many codes have Scana descriptions.
pub fn known_count() -> usize {
    TITLES.len()
}

/// What a code's group covers (SAE J2012 code ranges).
pub fn group_of(code: Dtc) -> String {
    let s = code.to_string();
    let b = s.as_bytes();
    let (first, second, third) = (b[0], b[1], b[2]);
    if code.is_manufacturer_specific() {
        return format!(
            "a maker-specific {} code: its meaning depends on the make. See the maker's workshop manual",
            code.system().name().to_lowercase()
        );
    }
    let area = match (first, second, third) {
        (b'P', b'0', b'0') => "fuel and air metering, and emission controls",
        (b'P', b'0', b'1') => "fuel and air metering",
        (b'P', b'0', b'2') => "fuel and air metering: injector circuits",
        (b'P', b'0', b'3') => "the ignition system or a misfire",
        (b'P', b'0', b'4') => "the emission controls (EGR, catalyst, fuel vapour system)",
        (b'P', b'0', b'5') => "vehicle speed, idle control and auxiliary inputs",
        (b'P', b'0', b'6') => "the engine computer and its output circuits",
        (b'P', b'0', b'7' | b'8' | b'9') => "the gearbox",
        (b'P', b'0', b'A') => "the hybrid drive system",
        (b'P', b'2', b'0' | b'1' | b'2') => "fuel and air metering, and emission controls",
        (b'P', b'2', b'3') => "the ignition system",
        (b'P', b'2', b'4') => "the emission controls",
        (b'P', b'2', b'5') => "auxiliary inputs",
        (b'P', b'2', b'6') => "the engine computer and its output circuits",
        (b'P', b'2', b'7' | b'8') => "the gearbox",
        (b'P', b'2', b'9' | b'A') => "fuel and air metering",
        (b'P', b'3', _) => "fuel and air metering, or cylinder deactivation",
        (b'U', b'0', b'0') => "the network wiring",
        (b'U', b'0', b'1' | b'2') => "the network: one module lost communication with another",
        (b'U', b'0', b'3') => "the network: software that doesn't match between modules",
        (b'U', b'0', b'4') => "the network: invalid data received from a module",
        (b'C', b'0', _) => "the chassis (brakes, steering, suspension)",
        (b'B', b'0', _) => "the body (airbags, lights, comfort)",
        _ => "a code group Scana has no description for",
    };
    format!("a generic code about {area}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_parses_and_is_clean() {
        assert!(known_count() >= 350, "{}", known_count());
        let raw = include_str!("../data/dtc/generic.tsv");
        let mut n = 0;
        for line in raw.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let (code, text) = line.split_once('\t').expect("tab");
            let d = Dtc::parse(code).expect(code);
            assert!(!d.is_manufacturer_specific(), "{code} is maker-specific");
            assert!(!text.is_empty() && !text.contains('\u{2014}') && !text.contains('\u{2013}'), "{code}");
            n += 1;
        }
        assert_eq!(n, known_count(), "duplicate codes in generic.tsv");
    }

    #[test]
    fn well_known_codes_say_the_right_thing() {
        let t = |s: &str| lookup(Dtc::parse(s).unwrap()).title.unwrap_or("");
        assert!(t("P0420").starts_with("Catalyst efficiency below threshold (bank 1)"));
        assert!(t("P0300").starts_with("Random or multiple cylinder misfire"));
        assert!(t("P0133").contains("slow response (bank 1 sensor 1)"));
        assert!(t("P0171").contains("too lean (bank 1)"));
        assert!(t("P0128").contains("thermostat"));
        assert!(t("P0303").contains("Cylinder 3 misfire"));
        assert!(t("U0100").contains("engine computer"));
    }

    #[test]
    fn advice_parses_and_points_at_listed_codes() {
        assert!(ADVICE.len() >= 15);
        for a in ADVICE.iter() {
            assert!(!a.means.is_empty() && !a.check.is_empty());
            for c in &a.codes {
                let d = Dtc::parse(c).expect(c);
                assert!(lookup(d).title.is_some(), "advice for {c}, which has no title");
            }
        }
        assert!(lookup(Dtc::parse("P0456").unwrap()).advice.is_some());
    }

    #[test]
    fn maker_codes_need_the_make_and_sources() {
        let code = Dtc::parse("P1603").unwrap();
        let hijet = lookup_for(code, Some("Daihatsu"));
        assert!(hijet.summary().starts_with("Engine stall history"), "{}", hijet.summary());
        assert!(hijet.advice.is_some_and(|a| a.check.iter().any(|c| c.contains("fuel"))));
        // Another make: P1603 means something else there (a memory fault on some), so no meaning.
        assert!(lookup_for(code, Some("Ford")).maker.is_none());
        assert!(lookup_for(code, None).summary().starts_with("Not in Scana's list"));
        for m in MAKER.iter() {
            assert!(m.sources.len() >= 2, "{:?} needs two sources", m.advice.codes);
            assert!(!m.makes.is_empty() && !m.title.is_empty() && !m.advice.means.is_empty());
            for c in &m.advice.codes {
                assert!(Dtc::parse(c).is_some_and(|d| d.is_manufacturer_specific()), "{c}");
            }
        }
    }

    #[test]
    fn unknown_and_maker_codes_still_get_a_group() {
        let i = lookup(Dtc::parse("P0AFF").unwrap());
        assert!(i.title.is_none());
        assert!(i.summary().contains("hybrid"));
        let m = lookup(Dtc::parse("P1135").unwrap());
        assert!(m.manufacturer_specific && m.group.contains("maker-specific"));
        assert!(lookup(Dtc::parse("U0299").unwrap()).group.contains("lost communication"));
        assert!(lookup(Dtc::parse("C0999").unwrap()).group.contains("chassis"));
    }
}
