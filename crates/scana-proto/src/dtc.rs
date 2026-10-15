//! Diagnostic trouble codes (SAE J2012, ISO 15031-6): P, C, B and U codes.

use std::fmt;

/// A two-byte trouble code, for example `0x0133` = P0133.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dtc(pub u16);

/// The system a code belongs to, from its first letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Powertrain,
    Chassis,
    Body,
    Network,
}

impl System {
    pub fn letter(self) -> char {
        match self {
            System::Powertrain => 'P',
            System::Chassis => 'C',
            System::Body => 'B',
            System::Network => 'U',
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            System::Powertrain => "Powertrain (engine and gearbox)",
            System::Chassis => "Chassis (brakes, steering, suspension)",
            System::Body => "Body (airbags, lights, comfort)",
            System::Network => "Network (communication between modules)",
        }
    }
}

impl Dtc {
    pub fn from_bytes(hi: u8, lo: u8) -> Self {
        Dtc(u16::from_be_bytes([hi, lo]))
    }

    pub fn system(self) -> System {
        match self.0 >> 14 {
            0 => System::Powertrain,
            1 => System::Chassis,
            2 => System::Body,
            _ => System::Network,
        }
    }

    /// True for maker-specific codes (P1xxx, P30xx-P33xx, B1, B2, C1, C2, U1, U2);
    /// false for the generic codes every maker shares.
    pub fn is_manufacturer_specific(self) -> bool {
        let digit = (self.0 >> 12) & 0x3;
        let second = (self.0 >> 8) & 0xF;
        match self.system() {
            System::Powertrain => digit == 1 || (digit == 3 && second <= 3),
            _ => digit == 1 || digit == 2,
        }
    }

    /// Parses "P0133" (any case).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let mut chars = s.chars();
        let top: u16 = match chars.next()?.to_ascii_uppercase() {
            'P' => 0,
            'C' => 1,
            'B' => 2,
            'U' => 3,
            _ => return None,
        };
        let rest = chars.as_str();
        if rest.len() != 4 || !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let digit = u16::from_str_radix(&rest[..1], 16).ok()?;
        if digit > 3 {
            return None;
        }
        let low = u16::from_str_radix(&rest[1..], 16).ok()?;
        Some(Dtc((top << 14) | (digit << 12) | low))
    }
}

impl fmt::Display for Dtc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}{:03X}", self.system().letter(), (self.0 >> 12) & 0x3, self.0 & 0x0FFF)
    }
}

impl fmt::Debug for Dtc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DtcListError {
    #[error("the code list says {count} codes but only has room for {room}")]
    CountMismatch { count: usize, room: usize },
    #[error("empty code list")]
    Empty,
}

/// Parses the reply to services 03, 07 and 0A on CAN: a count byte, then 2 bytes per code.
/// `data` starts after the service byte (0x43, 0x47 or 0x4A). Zero codes (padding) are skipped.
pub fn parse_dtc_list(data: &[u8]) -> Result<Vec<Dtc>, DtcListError> {
    let (&count, rest) = data.split_first().ok_or(DtcListError::Empty)?;
    let count = usize::from(count);
    let room = rest.len() / 2;
    if count > room {
        return Err(DtcListError::CountMismatch { count, room });
    }
    let (pairs, _) = rest.as_chunks::<2>();
    Ok(pairs.iter().take(count).map(|&[hi, lo]| Dtc::from_bytes(hi, lo)).filter(|d| d.0 != 0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_display_and_parse() {
        assert_eq!(Dtc(0x0133).to_string(), "P0133");
        assert_eq!(Dtc::from_bytes(0xC1, 0x23).to_string(), "U0123");
        assert_eq!(Dtc::from_bytes(0x41, 0x00).to_string(), "C0100");
        assert_eq!(Dtc::from_bytes(0x9A, 0xBC).to_string(), "B1ABC");
        for s in ["P0133", "U0123", "C0100", "B1ABC", "P3400"] {
            assert_eq!(Dtc::parse(s).unwrap().to_string(), s);
        }
        assert_eq!(Dtc::parse("p0420"), Some(Dtc(0x0420)));
        assert_eq!(Dtc::parse("P4000"), None);
        assert_eq!(Dtc::parse("X0100"), None);
        assert_eq!(Dtc::parse("P01"), None);
    }

    #[test]
    fn manufacturer_codes() {
        assert!(!Dtc::parse("P0420").unwrap().is_manufacturer_specific());
        assert!(Dtc::parse("P1135").unwrap().is_manufacturer_specific());
        assert!(!Dtc::parse("P2195").unwrap().is_manufacturer_specific());
        assert!(Dtc::parse("P3000").unwrap().is_manufacturer_specific());
        assert!(!Dtc::parse("P3400").unwrap().is_manufacturer_specific());
        assert!(Dtc::parse("B1000").unwrap().is_manufacturer_specific());
        assert!(!Dtc::parse("U0100").unwrap().is_manufacturer_specific());
    }

    #[test]
    fn lists_parse() {
        assert_eq!(parse_dtc_list(&[0]).unwrap(), vec![]);
        assert_eq!(parse_dtc_list(&[2, 0x01, 0x33, 0x04, 0x20]).unwrap(), vec![Dtc(0x0133), Dtc(0x0420)]);
        // Padding after the codes is fine.
        assert_eq!(parse_dtc_list(&[1, 0x01, 0x71, 0xCC, 0xCC]).unwrap(), vec![Dtc(0x0171)]);
        assert_eq!(parse_dtc_list(&[3, 0x01, 0x33]), Err(DtcListError::CountMismatch { count: 3, room: 1 }));
        assert_eq!(parse_dtc_list(&[]), Err(DtcListError::Empty));
    }
}
