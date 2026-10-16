//! Where modules usually answer, by maker family (`data/modules/addresses.toml`): names to
//! show before a module gives its own, and the gateways to try during module discovery.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct KnownAddress {
    pub family: String,
    /// "7B0", "750/40" or "18DA28F1": the request ID, with the address byte after a slash.
    pub at: String,
    pub name: String,
    pub source: String,
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

#[derive(Default, Deserialize)]
struct File {
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

/// The usual module at `at` (a channel label such as "7B0" or "750/40"), for the family if
/// known, else for any car.
pub fn name_for(family: Option<&str>, at: &str) -> Option<&'static KnownAddress> {
    let at = at.to_ascii_uppercase();
    let fam = family.map(str::to_ascii_lowercase);
    FILE.address
        .iter()
        .find(|a| Some(a.family.as_str()) == fam.as_deref() && a.at.eq_ignore_ascii_case(&at))
        .or_else(|| FILE.address.iter().find(|a| a.family == "any" && a.at.eq_ignore_ascii_case(&at)))
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

/// The diagnostic family of a make ("Daihatsu" -> "toyota").
pub fn family_of_make(make: &str) -> Option<&'static str> {
    FILE.make.iter().find(|m| m.name.eq_ignore_ascii_case(make.trim())).map(|m| m.family.as_str())
}

/// The family from a VIN's maker code (its first three characters), where Scana knows it.
pub fn family_of_vin(vin: &str) -> Option<&'static str> {
    let wmi: String = vin.chars().take(3).collect::<String>().to_ascii_uppercase();
    // Toyota (JT, 2T, 4T, 5T, MR0, NMT, SB1, VNK), Lexus (JTH, 2T2), Daihatsu (JDA, MHK).
    let toyota = ["JT", "2T", "4T", "5T", "MR0", "NMT", "SB1", "VNK", "JDA", "MHK"];
    // GM brands, including Holden (6G1, 6H8).
    let gm = ["1G", "2G", "3G", "KL", "6G1", "6H8"];
    if toyota.iter().any(|p| wmi.starts_with(p)) {
        Some("toyota")
    } else if gm.iter().any(|p| wmi.starts_with(p)) {
        Some("gm")
    } else {
        None
    }
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
        assert_eq!(family_of_make("Ford"), None);
        assert_eq!(family_of_vin("JDA00000000000000"), Some("toyota"));
        assert_eq!(family_of_vin("WVW00000000000000"), None);
        for a in addresses() {
            assert!(matches!(a.source.as_str(), "iso" | "opendbc" | "toyotadx"), "{a:?}");
        }
    }
}
