//! CAN identifiers, frames and bit rates.

use std::fmt;

/// An 11-bit (standard) or 29-bit (extended) CAN identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CanId {
    Standard(u16),
    Extended(u32),
}

impl CanId {
    /// A standard ID, or `None` if it doesn't fit in 11 bits.
    pub fn standard(id: u16) -> Option<Self> {
        (id <= 0x7FF).then_some(CanId::Standard(id))
    }

    /// An extended ID, or `None` if it doesn't fit in 29 bits.
    pub fn extended(id: u32) -> Option<Self> {
        (id <= 0x1FFF_FFFF).then_some(CanId::Extended(id))
    }

    pub fn raw(self) -> u32 {
        match self {
            CanId::Standard(id) => u32::from(id),
            CanId::Extended(id) => id,
        }
    }

    pub fn is_extended(self) -> bool {
        matches!(self, CanId::Extended(_))
    }
}

impl fmt::Display for CanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CanId::Standard(id) => write!(f, "{id:03X}"),
            CanId::Extended(id) => write!(f, "{id:08X}"),
        }
    }
}

/// Which pair of OBD-II pins a connection uses. Most cars have only the main bus. Some Fords,
/// Mazdas and Volvos also put a medium-speed CAN on pins 3 and 11.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bus {
    /// Pins 6 (CAN high) and 14 (CAN low): the OBD-II bus.
    #[default]
    Main,
    /// Pins 3 (CAN high) and 11 (CAN low): body modules on some makes. Scana only listens
    /// there, and sends nothing until the user says the car is parked.
    Second,
}

impl Bus {
    /// "6 and 14" or "3 and 11".
    pub fn pins(self) -> &'static str {
        match self {
            Bus::Main => "6 and 14",
            Bus::Second => "3 and 11",
        }
    }

    /// "main" or "second".
    pub fn name(self) -> &'static str {
        match self {
            Bus::Main => "main",
            Bus::Second => "second",
        }
    }

    /// Parses "6-14" or "3-11" (also "6,14", "main", "second").
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace([',', ' '], "-").as_str() {
            "6-14" | "main" => Some(Bus::Main),
            "3-11" | "second" => Some(Bus::Second),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a CAN frame holds at most 8 data bytes, not {0}")]
    TooLong(usize),
}

/// A classic CAN frame: up to 8 data bytes, or a remote request.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frame {
    pub id: CanId,
    len: u8,
    data: [u8; 8],
    rtr: bool,
}

impl Frame {
    pub fn new(id: CanId, data: &[u8]) -> Result<Self, FrameError> {
        if data.len() > 8 {
            return Err(FrameError::TooLong(data.len()));
        }
        let mut d = [0u8; 8];
        d[..data.len()].copy_from_slice(data);
        Ok(Frame { id, len: data.len() as u8, data: d, rtr: false })
    }

    /// A remote request frame asking for `len` bytes.
    pub fn remote(id: CanId, len: u8) -> Result<Self, FrameError> {
        if len > 8 {
            return Err(FrameError::TooLong(usize::from(len)));
        }
        Ok(Frame { id, len, data: [0; 8], rtr: true })
    }

    /// The data bytes (empty for a remote request).
    pub fn data(&self) -> &[u8] {
        if self.rtr { &[] } else { &self.data[..usize::from(self.len)] }
    }

    /// The length code: data bytes, or bytes asked for by a remote request.
    pub fn len(&self) -> u8 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_remote(&self) -> bool {
        self.rtr
    }
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for Frame {
    /// `7E8#04410C1D4C` like can-utils, or `7E8#R8` for a remote request.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#", self.id)?;
        if self.rtr {
            return write!(f, "R{}", self.len);
        }
        for b in self.data() {
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}

/// CAN bit rates the bridge supports, with their SLCAN `S` codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bitrate {
    K10,
    K20,
    K50,
    K100,
    K125,
    K250,
    K500,
    K800,
    M1,
}

impl Bitrate {
    pub const ALL: [Bitrate; 9] = [
        Bitrate::K10,
        Bitrate::K20,
        Bitrate::K50,
        Bitrate::K100,
        Bitrate::K125,
        Bitrate::K250,
        Bitrate::K500,
        Bitrate::K800,
        Bitrate::M1,
    ];

    pub fn bits_per_second(self) -> u32 {
        match self {
            Bitrate::K10 => 10_000,
            Bitrate::K20 => 20_000,
            Bitrate::K50 => 50_000,
            Bitrate::K100 => 100_000,
            Bitrate::K125 => 125_000,
            Bitrate::K250 => 250_000,
            Bitrate::K500 => 500_000,
            Bitrate::K800 => 800_000,
            Bitrate::M1 => 1_000_000,
        }
    }

    /// The digit in the SLCAN `Sn` command.
    pub fn slcan_code(self) -> u8 {
        match self {
            Bitrate::K10 => 0,
            Bitrate::K20 => 1,
            Bitrate::K50 => 2,
            Bitrate::K100 => 3,
            Bitrate::K125 => 4,
            Bitrate::K250 => 5,
            Bitrate::K500 => 6,
            Bitrate::K800 => 7,
            Bitrate::M1 => 8,
        }
    }

    pub fn from_slcan_code(code: u8) -> Option<Self> {
        Bitrate::ALL.iter().copied().find(|b| b.slcan_code() == code)
    }

    /// Parses "500k", "500000", "1M" and the like.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        let bps: u32 = if let Some(k) = s.strip_suffix('k') {
            k.parse::<u32>().ok()?.checked_mul(1000)?
        } else if let Some(m) = s.strip_suffix('m') {
            m.parse::<u32>().ok()?.checked_mul(1_000_000)?
        } else {
            s.parse().ok()?
        };
        Bitrate::ALL.iter().copied().find(|b| b.bits_per_second() == bps)
    }
}

impl fmt::Display for Bitrate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bps = self.bits_per_second();
        if bps >= 1_000_000 { write!(f, "{} Mbit/s", bps / 1_000_000) } else { write!(f, "{} kbit/s", bps / 1000) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_check_their_range() {
        assert_eq!(CanId::standard(0x7FF), Some(CanId::Standard(0x7FF)));
        assert_eq!(CanId::standard(0x800), None);
        assert_eq!(CanId::extended(0x1FFF_FFFF), Some(CanId::Extended(0x1FFF_FFFF)));
        assert_eq!(CanId::extended(0x2000_0000), None);
        assert_eq!(CanId::Standard(0x7E8).to_string(), "7E8");
        assert_eq!(CanId::Extended(0x18DA_F110).to_string(), "18DAF110");
    }

    #[test]
    fn frames_show_like_can_utils() {
        let f = Frame::new(CanId::Standard(0x7E8), &[0x04, 0x41, 0x0C, 0x1D, 0x4C]).unwrap();
        assert_eq!(f.to_string(), "7E8#04410C1D4C");
        assert_eq!(f.data(), &[0x04, 0x41, 0x0C, 0x1D, 0x4C]);
        let r = Frame::remote(CanId::Standard(0x123), 8).unwrap();
        assert_eq!(r.to_string(), "123#R8");
        assert!(r.data().is_empty());
        assert_eq!(Frame::new(CanId::Standard(1), &[0; 9]), Err(FrameError::TooLong(9)));
    }

    #[test]
    fn bitrates_parse_and_round_trip() {
        assert_eq!(Bitrate::parse("500k"), Some(Bitrate::K500));
        assert_eq!(Bitrate::parse("250000"), Some(Bitrate::K250));
        assert_eq!(Bitrate::parse("1M"), Some(Bitrate::M1));
        assert_eq!(Bitrate::parse("333k"), None);
        for b in Bitrate::ALL {
            assert_eq!(Bitrate::from_slcan_code(b.slcan_code()), Some(b));
        }
        assert_eq!(Bitrate::K500.to_string(), "500 kbit/s");
    }

    #[test]
    fn buses_parse_from_their_pins() {
        assert_eq!(Bus::default(), Bus::Main);
        assert_eq!(Bus::parse("6-14"), Some(Bus::Main));
        assert_eq!(Bus::parse("3-11"), Some(Bus::Second));
        assert_eq!(Bus::parse(" 3,11 "), Some(Bus::Second));
        assert_eq!(Bus::parse("4-5"), None);
        assert_eq!(Bus::Second.pins(), "3 and 11");
    }
}
