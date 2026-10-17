//! Where modules usually answer, by maker family (`data/modules/addresses.toml`): names to
//! show before a module gives its own, and the gateways to try during module discovery.

use scana_proto::Bus;
use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct KnownAddress {
    pub family: String,
    /// "7B0", "750/40" or "18DA28F1": the request ID, with the address byte after a slash.
    pub at: String,
    pub name: String,
    pub source: String,
    /// "second" for a module on OBD pins 3 and 11. Absent: the main bus (pins 6 and 14).
    #[serde(default)]
    pub bus: Option<String>,
}

impl KnownAddress {
    /// The bus this entry is for.
    pub fn bus(&self) -> Bus {
        if self.bus.as_deref().is_some_and(|b| b.eq_ignore_ascii_case("second")) { Bus::Second } else { Bus::Main }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Gateway {
    pub family: String,
    pub id: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Make {
    pub name: String,
    pub family: String,
}

/// Reply offsets a family uses beyond request + 8.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct FamilyReplies {
    pub name: String,
    /// Reply ID minus request ID (11-bit).
    pub reply_offsets: Vec<i32>,
    pub source: String,
}

#[derive(Default, Deserialize)]
struct File {
    #[serde(default)]
    family: Vec<FamilyReplies>,
    #[serde(default)]
    address: Vec<KnownAddress>,
    #[serde(default)]
    gateway: Vec<Gateway>,
    #[serde(default)]
    make: Vec<Make>,
}

static FILE: LazyLock<File> =
    LazyLock::new(|| toml::from_str(include_str!("../data/modules/addresses.toml")).unwrap_or_default());

pub fn addresses() -> &'static [KnownAddress] {
    &FILE.address
}

/// The usual module at `at` (a channel label such as "7B0" or "750/40") on the main bus, for
/// the family if known, else for any car.
pub fn name_for(family: Option<&str>, at: &str) -> Option<&'static KnownAddress> {
    name_for_bus(family, at, Bus::Main)
}

/// The usual module at `at` on one bus. The main bus uses the family's main entries, then
/// "any". The second bus (pins 3 and 11) uses only entries marked for it, for the family: the
/// OBD-II names of "any" (7E0 "Engine computer") never apply there.
pub fn name_for_bus(family: Option<&str>, at: &str, bus: Bus) -> Option<&'static KnownAddress> {
    let at = at.to_ascii_uppercase();
    let fam = family.map(str::to_ascii_lowercase);
    let on_bus = |a: &&KnownAddress| a.bus() == bus && a.at.eq_ignore_ascii_case(&at);
    FILE.address.iter().filter(on_bus).find(|a| Some(a.family.as_str()) == fam.as_deref()).or_else(|| match bus {
        Bus::Main => FILE.address.iter().filter(on_bus).find(|a| a.family == "any"),
        Bus::Second => None,
    })
}

/// 11-bit gateway IDs to try with address bytes, for a family.
pub fn gateways(family: Option<&str>) -> Vec<u16> {
    let Some(fam) = family else { return vec![] };
    FILE.gateway
        .iter()
        .filter(|g| g.family.eq_ignore_ascii_case(fam))
        .filter_map(|g| u16::from_str_radix(&g.id, 16).ok())
        .collect()
}

/// Extra reply offsets (reply ID minus request ID, 11-bit) for a family, such as -0x280 for
/// Chrysler. Empty for an unknown family.
pub fn reply_offsets(family: Option<&str>) -> Vec<i32> {
    let Some(fam) = family else { return vec![] };
    FILE.family.iter().filter(|f| f.name.eq_ignore_ascii_case(fam)).flat_map(|f| f.reply_offsets.clone()).collect()
}

/// The diagnostic family of a make ("Daihatsu" -> "toyota").
pub fn family_of_make(make: &str) -> Option<&'static str> {
    FILE.make.iter().find(|m| m.name.eq_ignore_ascii_case(make.trim())).map(|m| m.family.as_str())
}

/// World manufacturer identifier prefixes (a VIN's first characters) by make, read from
/// Wikipedia's VIN article, `[wmi]` in `data/modules/addresses.toml` (2026-10-17). Where
/// two prefixes both match, the longest wins. Toyota, Holden, GM and Lexus prefixes are the
/// original table; the rest are new. Cupra shares Seat's prefix (VSS), so it shows as Seat.
/// Prefixes under "GM" (1G, 2G, 3G, KL) stay GM even where a longer one would name Holden.
/// Makes with no `[[make]]` row (Mitsubishi, Isuzu, MG, GWM, BYD) have no address family.
const WMI: &[(&str, &[&str])] = &[
    ("Lexus", &["JTH", "2T2", "58A"]),
    ("Daihatsu", &["JDA", "MHK", "JD1", "JD2", "JD4"]),
    ("Toyota", &["JT", "2T", "4T", "5T", "MR0", "NMT", "SB1", "VNK", "MR1", "MR2", "MMK", "6T1", "AHT"]),
    ("Holden", &["6G1", "6H8", "MMU"]),
    ("Chevrolet", &["MMM", "6G3"]),
    ("GM", &["1G", "2G", "3G", "KL"]),
    (
        "Mazda",
        &[
            "JM0", "JM1", "JM2", "JM3", "JM6", "JM7", "JMZ", "MM0", "MM6", "MM7", "MM8", "MP2", "1YV", "3MD", "3MV",
            "3MZ",
        ],
    ),
    ("Hyundai", &["KMH", "KM8", "KME", "KMF", "KMJ", "5NP", "5NM", "5NT", "2HM", "TMA", "TMC"]),
    ("Genesis", &["KMT", "KMU"]),
    ("Kia", &["KNA", "KNC", "KNE", "KND", "5XX", "U5Y", "U6Y"]),
    (
        "Ford",
        &[
            "6F1", "6FP", "MNA", "MNB", "MNC", "MPB", "AFA", "WF0", "NM0", "1FA", "1FM", "1FT", "1FD", "2FA", "2FM",
            "2FT", "3FA", "3FM", "3FT",
        ],
    ),
    (
        "Nissan",
        &[
            "JN1", "JN3", "JN6", "JN8", "6F4", "MNT", "1N4", "1N6", "3N1", "3N6", "3N8", "5N1", "SJN", "SJK", "VSK",
            "ADN",
        ],
    ),
    ("Infiniti", &["JNK", "JNR", "JNX", "5N3", "3PC"]),
    ("Subaru", &["JF1", "JF2", "JF3", "4S3", "4S4", "MMR", "PLP"]),
    (
        "Honda",
        &[
            "JHM", "JHL", "1HG", "2HG", "2HJ", "2HK", "3HG", "3CZ", "5FN", "5FP", "5J6", "5KB", "7FA", "19X", "MRH",
            "MHR", "SHH", "SHS", "93H",
        ],
    ),
    ("Acura", &["JH4", "2HH", "2HN", "19U", "19V", "5FR", "5J8"]),
    ("Volkswagen", &["WVW", "WVG", "WV1", "WV2", "1VW", "3VW", "3VV", "8AW", "9BW", "AAV"]),
    ("Audi", &["WAU", "WUA", "WU1", "TRU", "99A"]),
    ("Skoda", &["TMB", "TMP", "MEX"]),
    ("Seat", &["VSS"]),
    ("Jeep", &["1J4", "1J8", "ZAC"]),
    ("Chrysler", &["1C3", "1C4", "2C3", "2C4", "3C3", "3C4"]),
    ("Dodge", &["1B3", "1D3", "1D4", "1D7", "2B3", "3B3", "3D3", "3D4", "3D7"]),
    ("Ram", &["1C6", "3C6", "ZFC"]),
    (
        "Mitsubishi",
        &["JA3", "JA4", "JA7", "JMA", "JMB", "JMF", "JMY", "6MM", "MMA", "MMB", "MMT", "ML3", "4A3", "4A4", "XMC"],
    ),
    ("Isuzu", &["JAA", "JAB", "JAC", "JAL", "JAM", "MPA", "MP1", "ACV"]),
    ("MG", &["LSJ", "SDP", "MML", "MZ7"]),
    ("GWM", &["LGW", "Y7C", "92Z"]),
    ("BYD", &["LC0", "LGX", "LPE", "92V"]),
];

/// The make from a VIN's maker code (its first characters), for the makes Scana knows.
pub fn make_of_vin(vin: &str) -> Option<&'static str> {
    let wmi: String = vin.chars().take(3).collect::<String>().to_ascii_uppercase();
    WMI.iter()
        .flat_map(|(make, prefixes)| prefixes.iter().map(move |p| (*p, *make)))
        .filter(|(p, _)| wmi.starts_with(p))
        .max_by_key(|(p, _)| p.len())
        .map(|(_, make)| make)
}

/// The diagnostic family from a VIN's maker code, where Scana knows the make's family.
pub fn family_of_vin(vin: &str) -> Option<&'static str> {
    make_of_vin(vin).and_then(family_of_make)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_book_parses_and_answers() {
        assert!(addresses().len() >= 25);
        assert_eq!(
            name_for(Some("toyota"), "7b0").map(|a| a.name.as_str()),
            Some("Brake control: ABS and stability control")
        );
        assert_eq!(name_for(Some("toyota"), "750/40").map(|a| a.source.as_str()), Some("opendbc"));
        // Family-specific first, then any car.
        assert_eq!(name_for(Some("toyota"), "7E2").map(|a| a.family.as_str()), Some("toyota"));
        assert_eq!(name_for(None, "7E0").map(|a| a.name.as_str()), Some("Engine computer (ECM)"));
        assert_eq!(name_for(None, "7B0"), None);
        assert_eq!(gateways(Some("toyota")), vec![0x750]);
        assert!(gateways(None).is_empty());
        assert_eq!(family_of_make("Daihatsu"), Some("toyota"));
        assert_eq!(family_of_make("Mitsubishi"), None);
        assert_eq!(family_of_vin("JDA00000000000000"), Some("toyota"));
        assert_eq!(family_of_vin("ZZZ00000000000000"), None);
        assert_eq!(make_of_vin("JTDSCANSM60000001"), Some("Toyota"));
        assert_eq!(make_of_vin("JTHBA00000000000"), Some("Lexus"));
        assert_eq!(make_of_vin("6G1ZZZ00000000000"), Some("Holden"));
        assert_eq!(make_of_vin("ZZZ00000000000000"), None);
        assert_eq!(make_of_vin("JTD").and_then(family_of_make), Some("toyota"));
        for a in addresses() {
            assert!(matches!(a.source.as_str(), "iso" | "opendbc" | "toyotadx" | "fgfalcon"), "{a:?}");
        }
    }

    #[test]
    fn the_new_families_answer() {
        let n = |f: &str, at: &str| name_for(Some(f), at).map(|a| a.name.as_str());
        assert_eq!(n("mazda", "760"), Some("Brake control: ABS and stability control"));
        assert_eq!(n("hyundai", "7D0"), Some("Forward radar"));
        assert_eq!(n("ford", "732"), Some("Gear shift module"));
        assert_eq!(n("nissan", "18DAD0F1"), Some("Gateway"));
        assert_eq!(n("subaru", "787"), Some("Forward camera (EyeSight driving assist)"));
        assert_eq!(n("honda", "18da53f1"), Some("Airbag computer (SRS)"));
        assert_eq!(n("vw", "715"), Some("Airbag computer (SRS)"));
        assert_eq!(n("chrysler", "744"), Some("Airbag computer (SRS)"));
        assert_eq!(n("gm", "24B"), Some("Forward camera (replies from 64B, not request plus 8)"));
        // The same 11-bit ID names different modules in different families.
        assert_eq!(n("ford", "730"), Some("Electric power steering"));
        assert_eq!(n("toyota", "730"), Some("Second brake computer (brake booster or parking brake)"));
        // A family without its own entry falls back to "any".
        assert_eq!(n("mazda", "7E2"), Some("Third OBD-II computer (often the hybrid system)"));
        for (make, family) in [
            ("Mazda", "mazda"),
            ("Kia", "hyundai"),
            ("Genesis", "hyundai"),
            ("Ford", "ford"),
            ("Infiniti", "nissan"),
            ("Subaru", "subaru"),
            ("Acura", "honda"),
            ("Cupra", "vw"),
            ("Skoda", "vw"),
            ("Ram", "chrysler"),
            ("Jeep", "chrysler"),
            ("Holden", "gm"),
            ("GM", "gm"),
        ] {
            assert_eq!(family_of_make(make), Some(family), "{make}");
        }
        assert_eq!(family_of_make("Mitsubishi"), None);
        // One sourced prefix per make (Wikipedia, `[wmi]`).
        for (vin, make, family) in [
            ("JM1000000000", "Mazda", Some("mazda")),
            ("MM8000000000", "Mazda", Some("mazda")),
            ("KMH000000000", "Hyundai", Some("hyundai")),
            ("KNA000000000", "Kia", Some("hyundai")),
            ("KMT000000000", "Genesis", Some("hyundai")),
            ("MNA000000000", "Ford", Some("ford")),
            ("6FP000000000", "Ford", Some("ford")),
            ("JN1000000000", "Nissan", Some("nissan")),
            ("JNK000000000", "Infiniti", Some("nissan")),
            ("JF1000000000", "Subaru", Some("subaru")),
            ("JHM000000000", "Honda", Some("honda")),
            ("19U000000000", "Acura", Some("honda")),
            ("WVW000000000", "Volkswagen", Some("vw")),
            ("WAU000000000", "Audi", Some("vw")),
            ("TMB000000000", "Skoda", Some("vw")),
            ("VSS000000000", "Seat", Some("vw")),
            ("1J4000000000", "Jeep", Some("chrysler")),
            ("1C3000000000", "Chrysler", Some("chrysler")),
            ("MMU000000000", "Holden", Some("gm")),
            ("MMM000000000", "Chevrolet", Some("gm")),
            ("1GC000000000", "GM", Some("gm")),
            ("MMA000000000", "Mitsubishi", None),
            ("MPA000000000", "Isuzu", None),
            ("LSJ000000000", "MG", None),
            ("LGW000000000", "GWM", None),
            ("LC0000000000", "BYD", None),
        ] {
            assert_eq!(make_of_vin(vin), Some(make), "{vin}");
            assert_eq!(family_of_vin(vin), family, "{vin}");
        }
        // Lowercase, and too short or unknown.
        assert_eq!(make_of_vin("mnb"), Some("Ford"));
        assert_eq!(make_of_vin("M"), None);
        assert_eq!(make_of_vin(""), None);
        // The original prefixes keep their makes.
        assert_eq!(make_of_vin("JTHBA"), Some("Lexus"));
        assert_eq!(make_of_vin("2T2"), Some("Lexus"));
        assert_eq!(make_of_vin("MHK"), Some("Daihatsu"));
        assert_eq!(make_of_vin("MR0"), Some("Toyota"));
        assert_eq!(make_of_vin("6H8"), Some("Holden"));
        assert_eq!(make_of_vin("KL1"), Some("GM"));
        assert_eq!(family_of_vin("KL1"), Some("gm"));
    }

    #[test]
    fn reply_offsets_are_sourced_by_family() {
        assert_eq!(reply_offsets(Some("chrysler")), vec![-0x280]);
        assert_eq!(reply_offsets(Some("Nissan")), vec![0x20]);
        assert_eq!(reply_offsets(Some("vw")), vec![0x6A]);
        assert_eq!(reply_offsets(Some("gm")), vec![0x400]);
        assert!(reply_offsets(Some("toyota")).is_empty());
        assert!(reply_offsets(None).is_empty());
        let families: Vec<&str> = FILE.make.iter().map(|m| m.family.as_str()).collect();
        for f in &FILE.family {
            assert!(families.contains(&f.name.as_str()), "{f:?}");
            assert_eq!(f.source, "opendbc");
            assert!(!f.reply_offsets.is_empty() && !f.reply_offsets.contains(&0), "{f:?}");
        }
    }

    #[test]
    fn no_wmi_prefix_belongs_to_two_makes() {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for (make, prefixes) in WMI {
            for p in *prefixes {
                assert!(p.len() == 2 || p.len() == 3, "{p}");
                assert!(p.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()), "{p}");
                assert!(!seen.iter().any(|(q, _)| q == p), "{p} listed twice");
                seen.push((p, make));
            }
        }
    }

    #[test]
    fn second_bus_and_main_bus_names_stay_apart() {
        let second = |f: Option<&str>, at: &str| name_for_bus(f, at, Bus::Second).map(|a| a.name.as_str());
        let main = |f: Option<&str>, at: &str| name_for_bus(f, at, Bus::Main).map(|a| a.name.as_str());
        // The FG Falcon's modules are on the second bus only.
        assert_eq!(second(Some("ford"), "720"), Some("FG Falcon instrument cluster"));
        assert_eq!(second(Some("ford"), "7a6"), Some("FG Falcon front display interface module (FDIM)"));
        assert_eq!(main(Some("ford"), "720"), None);
        assert_eq!(name_for(Some("ford"), "720"), None);
        // The second bus never borrows main names, from the family or from "any".
        assert_eq!(second(Some("ford"), "7E0"), None);
        assert_eq!(second(None, "7E0"), None);
        assert_eq!(second(Some("ford"), "732"), None);
        assert_eq!(main(Some("ford"), "7E0"), Some("Engine computer (PCM)"));
        assert_eq!(main(None, "7E0"), Some("Engine computer (ECM)"));
        // Another family has no second-bus names.
        assert_eq!(second(Some("toyota"), "720"), None);
        assert_eq!(second(None, "720"), None);
        let falcon: Vec<_> = addresses().iter().filter(|a| a.source == "fgfalcon").collect();
        assert_eq!(falcon.len(), 7);
        for a in falcon {
            assert_eq!((a.family.as_str(), a.bus()), ("ford", Bus::Second), "{a:?}");
            assert!(a.name.contains("FG Falcon"), "{a:?}");
        }
    }

    fn valid_label(at: &str) -> bool {
        let hex = |s: &str, n: usize| s.len() == n && s.chars().all(|c| matches!(c, '0'..='9' | 'A'..='F'));
        match at.split_once('/') {
            Some((id, ext)) => hex(id, 3) && hex(ext, 2),
            None => hex(at, 3) || hex(at, 8),
        }
    }

    #[test]
    fn every_entry_is_well_formed_and_sourced() {
        let header: String = include_str!("../data/modules/addresses.toml")
            .lines()
            .take_while(|l| l.starts_with('#') || l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let described = |tag: &str| header.contains(&format!("#   [{tag}]"));
        let families: Vec<&str> = FILE.make.iter().map(|m| m.family.as_str()).collect();
        let known = |f: &str| f == "any" || families.contains(&f);
        for a in addresses() {
            assert!(known(&a.family), "{a:?}: family not in [[make]]");
            assert!(valid_label(&a.at), "{a:?}: bad label");
            assert!(described(&a.source), "{a:?}: source not described in the header");
            assert!(!a.name.is_empty(), "{a:?}");
            assert!(a.bus.is_none() || a.bus.as_deref() == Some("second"), "{a:?}: bus is absent or second");
        }
        for g in &FILE.gateway {
            assert!(known(&g.family), "{g:?}");
            assert!(g.id.len() == 3 && valid_label(&g.id), "{g:?}");
        }
        for m in &FILE.make {
            assert!(!m.name.is_empty() && !m.family.is_empty(), "{m:?}");
        }
        // No family names one address twice.
        for (i, a) in addresses().iter().enumerate() {
            for b in &addresses()[i + 1..] {
                assert!(
                    !(a.family == b.family && a.bus() == b.bus() && a.at.eq_ignore_ascii_case(&b.at)),
                    "{a:?} twice"
                );
            }
        }
        assert!(valid_label("750/40") && valid_label("18DA28F1") && valid_label("7B0"));
        assert!(!valid_label("7b0") && !valid_label("750/4") && !valid_label("18DA28") && !valid_label("7B00"));
    }
}
