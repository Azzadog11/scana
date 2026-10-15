//! Enhanced diagnostics beyond OBD-II: UDS (ISO 14229-1) and KWP2000 on CAN (ISO 14230-3),
//! the two ways a car's other modules (brakes, airbags, body) answer a scan tool.
//!
//! Only reading is built: identity, fault codes and data values. Every service has a
//! [`Level`] so the core can refuse anything that changes a module (`docs/DESIGN.md` 11).
//! Replies have the same shape as OBD-II replies (service + 0x40, or `7F service code`), so
//! [`crate::obd::parse_reply`] reads them.

use crate::dtc::Dtc;
use crate::obd::Reply;
use std::fmt;

// UDS services (ISO 14229-1).
pub const DIAGNOSTIC_SESSION: u8 = 0x10;
pub const ECU_RESET: u8 = 0x11;
pub const CLEAR_DTC: u8 = 0x14;
pub const READ_DTC_INFO: u8 = 0x19;
pub const READ_DATA_BY_ID: u8 = 0x22;
pub const READ_MEMORY: u8 = 0x23;
pub const SECURITY_ACCESS: u8 = 0x27;
pub const COMMUNICATION_CONTROL: u8 = 0x28;
pub const WRITE_DATA_BY_ID: u8 = 0x2E;
pub const IO_CONTROL: u8 = 0x2F;
pub const ROUTINE_CONTROL: u8 = 0x31;
pub const TESTER_PRESENT: u8 = 0x3E;
pub const CONTROL_DTC_SETTING: u8 = 0x85;

// KWP2000 services (ISO 14230-3) that differ from UDS.
pub const KWP_READ_FREEZE_FRAME: u8 = 0x12;
pub const KWP_READ_DTC: u8 = 0x13;
pub const KWP_READ_DTC_STATUS: u8 = 0x17;
pub const KWP_READ_DTC_BY_STATUS: u8 = 0x18;
pub const KWP_READ_ECU_ID: u8 = 0x1A;
pub const KWP_READ_LOCAL_ID: u8 = 0x21;
pub const KWP_IO_CONTROL_LOCAL: u8 = 0x30;
pub const KWP_WRITE_LOCAL_ID: u8 = 0x3B;

/// UDS `19` sub-functions Scana uses.
pub const DTC_BY_STATUS_MASK: u8 = 0x02;
pub const DTC_COUNT_BY_STATUS_MASK: u8 = 0x01;
pub const DTC_SUPPORTED: u8 = 0x0A;

/// The tester-present probe used to find modules: `3E 00`, answered by `7E 00`, or by a
/// refusal `7F 3E ..` from a module that speaks another dialect. Either way, someone is there.
pub const PROBE: [u8; 2] = [TESTER_PRESENT, 0x00];

/// How far a service goes. The levels match `docs/DESIGN.md` section 11.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Reads only: allowed.
    Read,
    /// Clears codes: behind a confirmation and the engine-off check.
    Clear,
    /// Makes a module do something (actuator tests, routines, resets, other sessions): stage 4.
    Change,
    /// Programming, security access, writing data, switching off communication: never.
    Never,
}

impl Level {
    /// Why Scana won't send a request at this level, as the end of a sentence.
    pub fn refusal(self) -> &'static str {
        match self {
            Level::Read => "it only reads",
            Level::Clear => "it clears fault codes, which needs the clear-codes checks (Fault codes page)",
            Level::Change => "it makes a module do something, which comes in stage 4 with its own safety checks",
            Level::Never => "it programs, unlocks or writes to a module, which Scana never does",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Read => "read only",
            Level::Clear => "clears codes",
            Level::Change => "changes how a module behaves",
            Level::Never => "never allowed in Scana",
        }
    }
}

/// The level of a request (its first byte is the service). OBD-II services are included.
pub fn level_of(request: &[u8]) -> Level {
    let Some(&service) = request.first() else { return Level::Never };
    let sub = request.get(1).copied();
    match service {
        // OBD-II reads.
        0x01 | 0x02 | 0x03 | 0x05 | 0x06 | 0x07 | 0x09 | 0x0A => Level::Read,
        0x04 => Level::Clear,
        // OBD-II service 08 runs an on-board test (for example an EVAP leak check).
        0x08 => Level::Change,
        // The default session changes nothing; any other session unlocks more.
        DIAGNOSTIC_SESSION => {
            if sub.map(|s| s & 0x7F) == Some(0x01) {
                Level::Read
            } else {
                Level::Change
            }
        }
        READ_DTC_INFO | READ_DATA_BY_ID | TESTER_PRESENT => Level::Read,
        KWP_READ_FREEZE_FRAME
        | KWP_READ_DTC
        | KWP_READ_DTC_STATUS
        | KWP_READ_DTC_BY_STATUS
        | KWP_READ_ECU_ID
        | KWP_READ_LOCAL_ID => Level::Read,
        CLEAR_DTC => Level::Clear,
        ECU_RESET | IO_CONTROL | ROUTINE_CONTROL | KWP_IO_CONTROL_LOCAL => Level::Change,
        // Security access, writes, communication control, DTC setting, programming
        // (0x34-0x37), memory access (0x23, 0x3D), dynamic and periodic data (0x2A, 0x2C),
        // responses on event (0x86), link control (0x87) and anything unknown.
        _ => Level::Never,
    }
}

/// A short name for a service, UDS, KWP or OBD-II.
pub fn service_name(service: u8) -> &'static str {
    match service {
        0x01..=0x0A => crate::obd::service_name(service),
        DIAGNOSTIC_SESSION => "diagnostic session",
        ECU_RESET => "module reset",
        KWP_READ_FREEZE_FRAME => "freeze frame (KWP)",
        KWP_READ_DTC => "fault codes (KWP)",
        CLEAR_DTC => "clear codes",
        KWP_READ_DTC_STATUS => "fault code status (KWP)",
        KWP_READ_DTC_BY_STATUS => "fault codes by status (KWP)",
        READ_DTC_INFO => "fault codes",
        KWP_READ_ECU_ID => "module identity (KWP)",
        KWP_READ_LOCAL_ID => "data by local ID (KWP)",
        READ_DATA_BY_ID => "data by ID",
        READ_MEMORY => "read memory",
        SECURITY_ACCESS => "security access",
        COMMUNICATION_CONTROL => "communication control",
        WRITE_DATA_BY_ID => "write data",
        IO_CONTROL => "actuator control",
        ROUTINE_CONTROL => "routine",
        KWP_IO_CONTROL_LOCAL => "actuator control (KWP)",
        0x34..=0x37 => "programming",
        KWP_WRITE_LOCAL_ID => "write data (KWP)",
        TESTER_PRESENT => "tester present",
        CONTROL_DTC_SETTING => "fault code setting",
        _ => "unknown service",
    }
}

/// True if `reply` answers `request`: same service, and the same identifier for services
/// that echo one (`22` DID, `21` local ID, `1A` option, `19` sub-function, `3E`, OBD PIDs).
pub fn reply_matches(request: &[u8], reply: &Reply) -> bool {
    let Some(&service) = request.first() else { return false };
    match reply {
        Reply::Negative { service: s, .. } => *s == service,
        Reply::Positive { service: s, data } => {
            if *s != service {
                return false;
            }
            match service {
                READ_DATA_BY_ID => request.len() >= 3 && data.len() >= 2 && data[..2] == request[1..3],
                KWP_READ_LOCAL_ID | KWP_READ_ECU_ID | READ_DTC_INFO | DIAGNOSTIC_SESSION => {
                    request.get(1).is_none_or(|b| data.first() == Some(b))
                }
                // `3E 00` is answered `7E 00`; a short `3E` by a bare `7E`.
                TESTER_PRESENT => true,
                _ => crate::obd::reply_matches(request, reply),
            }
        }
    }
}

/// `22 DID`: read one data identifier.
pub fn read_did_request(did: u16) -> Vec<u8> {
    let [hi, lo] = did.to_be_bytes();
    vec![READ_DATA_BY_ID, hi, lo]
}

/// The value bytes from a `62` reply to `22 did` (after the 0x62), or `None` if it is for another DID.
pub fn did_value(did: u16, data: &[u8]) -> Option<Vec<u8>> {
    (data.len() >= 2 && data[..2] == did.to_be_bytes()).then(|| data[2..].to_vec())
}

/// `21 LID`: read a KWP local identifier.
pub fn read_lid_request(lid: u8) -> Vec<u8> {
    vec![KWP_READ_LOCAL_ID, lid]
}

/// `19 02 mask`: every code whose status matches the mask (0xFF asks for all).
pub fn dtc_by_status_request(mask: u8) -> Vec<u8> {
    vec![READ_DTC_INFO, DTC_BY_STATUS_MASK, mask]
}

/// Standard identification DIDs (ISO 14229-1 annex C), with plain names.
pub const IDENTITY_DIDS: &[(u16, &str)] = &[
    (0xF197, "System name"),
    (0xF187, "Part number"),
    (0xF18A, "Supplier"),
    (0xF191, "Hardware number"),
    (0xF192, "Supplier hardware number"),
    (0xF193, "Supplier hardware version"),
    (0xF181, "Application software"),
    (0xF188, "Software number"),
    (0xF189, "Software version"),
    (0xF194, "Supplier software number"),
    (0xF195, "Supplier software version"),
    (0xF18B, "Manufacturing date"),
    (0xF18C, "Serial number"),
    (0xF190, "VIN"),
];

/// A plain name for a data identifier, where the standard defines one.
pub fn did_name(did: u16) -> Option<String> {
    if let Some((_, n)) = IDENTITY_DIDS.iter().find(|(d, _)| *d == did) {
        return Some((*n).to_string());
    }
    let named = match did {
        0xF180 => "Boot software",
        0xF182 => "Application data",
        0xF183 => "Boot software fingerprint",
        0xF184 => "Application software fingerprint",
        0xF185 => "Application data fingerprint",
        0xF186 => "Active session",
        0xF18D => "Supported functional units",
        0xF18E => "Kit assembly part number",
        0xF196 => "Type approval number",
        0xF198 => "Repair shop code",
        0xF199 => "Programming date",
        0xF19A => "Calibration repair shop code",
        0xF19B => "Calibration date",
        0xF19D => "Installation date",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    match did >> 8 {
        // OBD-II data mapped into UDS: F4xx is service 01 PID xx, F6xx a service 06 monitor,
        // F8xx a service 09 item.
        0xF4 => Some(format!("OBD-II live value, PID {:02X}", did & 0xFF)),
        0xF6 => Some(format!("OBD-II test result, monitor {:02X}", did & 0xFF)),
        0xF8 => Some(format!("OBD-II vehicle information, item {:02X}", did & 0xFF)),
        _ => None,
    }
}

/// KWP `1A` identification options that most modules support, with plain names
/// (ISO 14230-3; makers differ, so these are hints).
pub const KWP_IDENTITY_OPTIONS: &[(u8, &str)] = &[
    (0x97, "System name"),
    (0x87, "Part number"),
    (0x8A, "Supplier"),
    (0x92, "Supplier hardware number"),
    (0x94, "Supplier software number"),
    (0x95, "Supplier software version"),
    (0x90, "VIN"),
];

/// Bytes as text when they are mostly printable ASCII, else `None`. Trailing zeros, spaces
/// and padding are dropped.
pub fn text_of(bytes: &[u8]) -> Option<String> {
    let trimmed: &[u8] = {
        let end =
            bytes.iter().rposition(|b| !matches!(b, 0x00 | 0x20 | 0xFF | 0xAA | 0x55 | 0xCC)).map_or(0, |i| i + 1);
        let start = bytes[..end].iter().position(|b| !matches!(b, 0x00 | 0x20)).unwrap_or(end);
        &bytes[start..end]
    };
    if trimmed.is_empty() {
        return None;
    }
    let printable = trimmed.iter().filter(|b| b.is_ascii_graphic() || **b == b' ').count();
    (printable * 10 >= trimmed.len() * 9)
        .then(|| trimmed.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { char::from(b) } else { '.' }).collect())
}

/// Bytes as spaced hex: `1A 2B 3C`.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// Parses hex like `22 F1 90`, `22F190` or `0x22,0xF1`. `None` if anything else is in it.
pub fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let cleaned: String =
        s.replace("0x", "").replace("0X", "").chars().filter(|c| !matches!(c, ' ' | ',' | ':' | '-' | '\t')).collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) || !cleaned.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..cleaned.len()).step_by(2).map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok()).collect()
}

/// Which dialect a module's fault codes came in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeFormat {
    /// UDS: three bytes (the code and a failure type) and a UDS status byte.
    Uds,
    /// KWP2000: two bytes and a KWP status byte.
    Kwp,
    /// OBD-II services 03, 07, 0A: two bytes, no status.
    Obd,
}

/// A fault code read from a module, with whatever status came with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleDtc {
    pub code: Dtc,
    /// UDS failure type byte (the third byte), for example 0x13 "circuit open".
    pub failure_type: Option<u8>,
    pub status: Option<u8>,
    pub format: CodeFormat,
}

impl ModuleDtc {
    /// What the status says, in short words: "active now", "stored", "pending", "earlier".
    pub fn state(&self) -> DtcState {
        let Some(s) = self.status else {
            return DtcState { active: false, stored: true, pending: false, earlier: false, warning_light: false };
        };
        match self.format {
            CodeFormat::Uds => DtcState {
                active: s & 0x01 != 0,
                stored: s & 0x08 != 0,
                pending: s & 0x04 != 0,
                earlier: s & 0x01 == 0 && s & 0x20 != 0,
                warning_light: s & 0x80 != 0,
            },
            // ISO 14230-3: bits 5-6 storage state (11 present now, 10 maturing, 01 stored
            // but not present), bit 7 warning lamp requested. Makers vary.
            CodeFormat::Kwp => {
                let storage = (s >> 5) & 0x03;
                DtcState {
                    active: storage == 0x03,
                    stored: storage == 0x01 || storage == 0x03,
                    pending: storage == 0x02,
                    earlier: storage == 0x01,
                    warning_light: s & 0x80 != 0,
                }
            }
            CodeFormat::Obd => {
                DtcState { active: false, stored: true, pending: false, earlier: false, warning_light: false }
            }
        }
    }
}

impl fmt::Display for ModuleDtc {
    /// `C0035-11` with a failure type, else `C0035`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.failure_type {
            Some(ft) if ft != 0 => write!(f, "{}-{ft:02X}", self.code),
            _ => write!(f, "{}", self.code),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DtcState {
    /// Failing at the moment of reading.
    pub active: bool,
    /// Confirmed and stored.
    pub stored: bool,
    /// Seen once; not yet confirmed.
    pub pending: bool,
    /// Failed some time since codes were last cleared, but not now.
    pub earlier: bool,
    /// The module asks for its warning light.
    pub warning_light: bool,
}

impl DtcState {
    pub fn words(&self) -> String {
        let mut w = Vec::new();
        if self.active {
            w.push("active now");
        }
        if self.stored {
            w.push("stored");
        }
        if self.pending {
            w.push("pending");
        }
        if self.earlier && !self.active {
            w.push("happened earlier");
        }
        if self.warning_light {
            w.push("warning light on");
        }
        if w.is_empty() {
            w.push("recorded");
        }
        w.join(", ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodeListError {
    #[error("the code list is too short")]
    TooShort,
    #[error("the code list says {count} codes but has room for {room}")]
    CountMismatch { count: usize, room: usize },
}

/// Parses a UDS `59 02` (or `59 0A`) reply, after the 0x59: sub-function, availability mask,
/// then 4 bytes per code. Codes with status 0 (nothing happened) are dropped from `02` replies.
pub fn parse_uds_dtcs(data: &[u8]) -> Result<Vec<ModuleDtc>, CodeListError> {
    if data.len() < 2 {
        return Err(CodeListError::TooShort);
    }
    let sub = data[0];
    let records = &data[2..];
    let (chunks, _) = records.as_chunks::<4>();
    Ok(chunks
        .iter()
        .filter(|[a, b, c, _]| (*a, *b, *c) != (0, 0, 0))
        .filter(|[_, _, _, st]| sub != DTC_BY_STATUS_MASK || *st != 0)
        .map(|&[hi, lo, ft, st]| ModuleDtc {
            code: Dtc::from_bytes(hi, lo),
            failure_type: Some(ft),
            status: Some(st),
            format: CodeFormat::Uds,
        })
        .collect())
}

/// Parses a KWP `58` reply (to `18`), after the 0x58: a count, then 3 bytes per code
/// (2-byte code, status). Some modules send fewer records than the count; extra room is padding.
pub fn parse_kwp_dtcs(data: &[u8]) -> Result<Vec<ModuleDtc>, CodeListError> {
    let (&count, rest) = data.split_first().ok_or(CodeListError::TooShort)?;
    let count = usize::from(count);
    let room = rest.len() / 3;
    if count > room {
        return Err(CodeListError::CountMismatch { count, room });
    }
    let (chunks, _) = rest.as_chunks::<3>();
    Ok(chunks
        .iter()
        .take(count)
        .filter(|[hi, lo, _]| (*hi, *lo) != (0, 0))
        .map(|&[hi, lo, st]| ModuleDtc {
            code: Dtc::from_bytes(hi, lo),
            failure_type: None,
            status: Some(st),
            format: CodeFormat::Kwp,
        })
        .collect())
}

/// Parses a KWP `53` reply (to `13`), after the 0x53: a count, then 2 bytes per code.
pub fn parse_kwp13_dtcs(data: &[u8]) -> Result<Vec<ModuleDtc>, CodeListError> {
    crate::dtc::parse_dtc_list(data)
        .map(|list| {
            list.into_iter()
                .map(|code| ModuleDtc { code, failure_type: None, status: None, format: CodeFormat::Kwp })
                .collect()
        })
        .map_err(|e| match e {
            crate::dtc::DtcListError::CountMismatch { count, room } => CodeListError::CountMismatch { count, room },
            crate::dtc::DtcListError::Empty => CodeListError::TooShort,
        })
}

/// What a UDS failure type byte means, in Scana's own words (the categories of SAE J2012 /
/// ISO 15031-6; 0xF0-0xFF are each maker's own).
pub fn failure_type_text(ft: u8) -> &'static str {
    match ft {
        0x00 => "no more detail given",
        0x01 => "an electrical fault",
        0x02 => "a signal fault",
        0x03 => "a fault in a pulsed (frequency or PWM) signal",
        0x04 => "a fault inside the module",
        0x05 => "a programming or software fault",
        0x06 => "the module's own checks found a fault",
        0x07 => "a mechanical fault",
        0x08 => "a fault in a network message",
        0x09 => "a component fault",
        0x11 => "the circuit is shorted to ground",
        0x12 => "the circuit is shorted to battery voltage",
        0x13 => "the circuit is open (a broken wire or loose plug)",
        0x14 => "the circuit is open or shorted to ground",
        0x15 => "the circuit is open or shorted to battery voltage",
        0x16 => "the voltage is too low",
        0x17 => "the voltage is too high",
        0x18 => "the current is too low",
        0x19 => "the current is too high",
        0x1A => "the resistance is too low",
        0x1B => "the resistance is too high",
        0x1C => "the voltage is outside its normal range",
        0x1D => "the current is outside its normal range",
        0x1E => "the resistance is outside its normal range",
        0x1F => "the circuit comes and goes (intermittent)",
        0x21 => "the signal is too weak",
        0x22 => "the signal is too strong",
        0x23 => "the signal is stuck low",
        0x24 => "the signal is stuck high",
        0x25 => "the signal has the wrong shape",
        0x26 => "the signal changes too slowly",
        0x27 => "the signal changes too fast",
        0x28 => "the signal's zero point is off (needs adjusting)",
        0x29 => "the signal is not valid",
        0x2A => "the signal is stuck at one value",
        0x2B => "the signal is mixed up with another one",
        0x2F => "the signal jumps about (erratic)",
        0x31 => "there is no signal",
        0x32 => "the signal's low time is too short",
        0x33 => "the signal's low time is too long",
        0x34 => "the signal's high time is too short",
        0x35 => "the signal's high time is too long",
        0x36 => "the signal's frequency is too low",
        0x37 => "the signal's frequency is too high",
        0x38 => "the signal's frequency is wrong",
        0x39 => "too few pulses",
        0x3A => "too many pulses",
        0x41 => "a checksum fault in the module's memory",
        0x42 => "a memory fault in the module",
        0x43 => "a fault in a special memory area",
        0x44 => "a data memory fault",
        0x45 => "a program memory fault",
        0x46 => "a calibration memory fault",
        0x47 => "the module's watchdog or safety processor tripped",
        0x48 => "the module's supervising software found a fault",
        0x49 => "an internal electronic fault",
        0x4A => "the wrong part is fitted",
        0x4B => "too hot",
        0x51 => "not programmed",
        0x52 => "not activated",
        0x53 => "switched off (deactivated)",
        0x54 => "the calibration is missing",
        0x55 => "not set up (not configured)",
        0x61 => "a calculated value is wrong",
        0x62 => "two signals that should agree don't",
        0x63 => "a protection time limit ran out",
        0x64 => "the signal doesn't make sense with the others (plausibility)",
        0x65 => "the signal changes too few times",
        0x66 => "the signal changes too many times",
        0x67 => "the signal is wrong after an event",
        0x68 => "an event was recorded",
        0x71 => "an actuator is stuck",
        0x72 => "an actuator is stuck open",
        0x73 => "an actuator is stuck closed",
        0x74 => "an actuator is slipping",
        0x75 => "the emergency position can't be reached",
        0x76 => "fitted in the wrong position",
        0x77 => "the commanded position can't be reached",
        0x78 => "the alignment or adjustment is wrong",
        0x79 => "a mechanical linkage fault",
        0x7A => "a fluid leak or seal fault",
        0x7B => "the fluid level is low",
        0x81 => "a network message arrived with bad data",
        0x82 => "a network message's counter is wrong or not updating",
        0x83 => "a network message's protection value is wrong",
        0x84 => "a network signal is below its allowed range",
        0x85 => "a network signal is above its allowed range",
        0x86 => "a network signal is not valid",
        0x87 => "a network message is missing",
        0x88 => "the network wiring went bus-off",
        0x8F => "a network signal jumps about (erratic)",
        0x91 => "a measured value is out of its limits",
        0x92 => "the part doesn't work as well as it should",
        0x93 => "the part doesn't work at all",
        0x94 => "the part works when it shouldn't",
        0x95 => "put together wrongly",
        0x96 => "a fault inside the part",
        0x97 => "the part is blocked or obstructed",
        0x98 => "the part or system is too hot",
        0xF0..=0xFF => "a maker-specific failure type (see the workshop manual)",
        _ => "a failure type Scana has no description for",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obd::parse_reply;

    #[test]
    fn levels_keep_scana_read_only() {
        assert_eq!(level_of(&[0x22, 0xF1, 0x90]), Level::Read);
        assert_eq!(level_of(&[0x19, 0x02, 0xFF]), Level::Read);
        assert_eq!(level_of(&[0x3E, 0x00]), Level::Read);
        assert_eq!(level_of(&[0x10, 0x01]), Level::Read);
        assert_eq!(level_of(&[0x10, 0x81]), Level::Read);
        assert_eq!(level_of(&[0x10, 0x03]), Level::Change);
        assert_eq!(level_of(&[0x21, 0x01]), Level::Read);
        assert_eq!(level_of(&[0x1A, 0x88, 0x01]), Level::Read);
        assert_eq!(level_of(&[0x18, 0x00, 0xFF, 0x00]), Level::Read);
        assert_eq!(level_of(&[0x01, 0x0C]), Level::Read);
        assert_eq!(level_of(&[0x04]), Level::Clear);
        assert_eq!(level_of(&[0x14, 0xFF, 0xFF, 0xFF]), Level::Clear);
        assert_eq!(level_of(&[0x2F, 0x01, 0x02, 0x03]), Level::Change);
        assert_eq!(level_of(&[0x31, 0x01, 0xFF, 0x00]), Level::Change);
        assert_eq!(level_of(&[0x11, 0x01]), Level::Change);
        for never in [0x27u8, 0x2E, 0x28, 0x85, 0x34, 0x35, 0x36, 0x37, 0x3B, 0x3D, 0x23, 0x2C, 0x2A, 0x86, 0x99] {
            assert_eq!(level_of(&[never, 0x01]), Level::Never, "{never:02X}");
        }
        assert_eq!(level_of(&[]), Level::Never);
    }

    #[test]
    fn replies_match_their_identifier() {
        let r = parse_reply(&[0x62, 0xF1, 0x90, b'A']).unwrap();
        assert!(reply_matches(&[0x22, 0xF1, 0x90], &r));
        assert!(!reply_matches(&[0x22, 0xF1, 0x87], &r));
        let r = parse_reply(&[0x61, 0x01, 0x10]).unwrap();
        assert!(reply_matches(&[0x21, 0x01], &r));
        assert!(!reply_matches(&[0x21, 0x02], &r));
        assert!(reply_matches(&[0x3E, 0x00], &parse_reply(&[0x7E, 0x00]).unwrap()));
        assert!(reply_matches(&[0x3E, 0x00], &parse_reply(&[0x7E]).unwrap()));
        assert!(reply_matches(&[0x3E, 0x00], &parse_reply(&[0x7F, 0x3E, 0x12]).unwrap()));
        assert!(!reply_matches(&[0x3E, 0x00], &parse_reply(&[0x41, 0x0C, 0, 0]).unwrap()));
        assert!(reply_matches(&[0x01, 0x0C], &parse_reply(&[0x41, 0x0C, 0, 0]).unwrap()));
        assert_eq!(did_value(0xF190, &[0xF1, 0x90, 1, 2]), Some(vec![1, 2]));
        assert_eq!(did_value(0xF190, &[0xF1, 0x87, 1, 2]), None);
    }

    #[test]
    fn uds_codes_parse_with_failure_type_and_status() {
        // 59 02 FF | C0035-11 status 2F | U0100-87 status 28 | empty record
        let data = [0x02, 0xFF, 0x40, 0x35, 0x11, 0x2F, 0xC1, 0x00, 0x87, 0x28, 0, 0, 0, 0];
        let codes = parse_uds_dtcs(&data).unwrap();
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[0].to_string(), "C0035-11");
        let s = codes[0].state();
        assert!(s.active && s.stored && s.pending && !s.warning_light);
        assert_eq!(codes[1].to_string(), "U0100-87");
        let s = codes[1].state();
        assert!(!s.active && s.stored && s.earlier);
        assert_eq!(s.words(), "stored, happened earlier");
        // A status of 0 means nothing happened: dropped.
        assert!(parse_uds_dtcs(&[0x02, 0xFF, 0x01, 0x23, 0x00, 0x00]).unwrap().is_empty());
        assert_eq!(parse_uds_dtcs(&[0x02]), Err(CodeListError::TooShort));
        // A failure type of 0 shows as the plain code.
        let c = parse_uds_dtcs(&[0x02, 0xFF, 0x04, 0x20, 0x00, 0x08]).unwrap();
        assert_eq!(c[0].to_string(), "P0420");
    }

    #[test]
    fn kwp_codes_parse() {
        // 58 02 | C0210 status E0 (present, lamp) | B1000 status 20 (stored, not present)
        let codes = parse_kwp_dtcs(&[0x02, 0x42, 0x10, 0xE0, 0x90, 0x00, 0x20]).unwrap();
        assert_eq!(codes.len(), 2);
        assert_eq!(codes[0].to_string(), "C0210");
        let s = codes[0].state();
        assert!(s.active && s.warning_light);
        let s = codes[1].state();
        assert!(!s.active && s.stored && s.earlier);
        assert!(parse_kwp_dtcs(&[0x00]).unwrap().is_empty());
        assert!(parse_kwp_dtcs(&[0x02, 0x42, 0x10, 0xE0]).is_err());
        assert_eq!(parse_kwp13_dtcs(&[0x01, 0x01, 0x33]).unwrap()[0].code.to_string(), "P0133");
    }

    #[test]
    fn text_and_hex() {
        assert_eq!(text_of(b"89170-B5010\0\0\0").as_deref(), Some("89170-B5010"));
        assert_eq!(text_of(&[0x00, 0x00]), None);
        assert_eq!(text_of(&[0x01, 0x02, 0x03, 0x04]), None);
        assert_eq!(hex(&[0x22, 0xF1, 0x90]), "22 F1 90");
        assert_eq!(parse_hex("22 F1 90"), Some(vec![0x22, 0xF1, 0x90]));
        assert_eq!(parse_hex("0x22,0xF1"), Some(vec![0x22, 0xF1]));
        assert_eq!(parse_hex("22F"), None);
        assert_eq!(parse_hex("zz"), None);
        assert_eq!(parse_hex(""), None);
        assert_eq!(did_name(0xF190).as_deref(), Some("VIN"));
        assert_eq!(did_name(0xF40C).as_deref(), Some("OBD-II live value, PID 0C"));
        assert_eq!(did_name(0x1234), None);
        assert_eq!(failure_type_text(0x13), "the circuit is open (a broken wire or loose plug)");
        assert_eq!(service_name(0x22), "data by ID");
        assert_eq!(service_name(0x03), "stored codes");
    }
}
