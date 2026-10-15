//! OBD-II service requests and replies (SAE J1979, ISO 15031-5), as ISO-TP messages.

use crate::pids;
use thiserror::Error;

pub const CURRENT_DATA: u8 = 0x01;
pub const FREEZE_FRAME: u8 = 0x02;
pub const STORED_CODES: u8 = 0x03;
pub const CLEAR_CODES: u8 = 0x04;
pub const TEST_RESULTS: u8 = 0x06;
pub const PENDING_CODES: u8 = 0x07;
pub const VEHICLE_INFO: u8 = 0x09;
pub const PERMANENT_CODES: u8 = 0x0A;

/// Most PIDs one service 01 request may ask for on CAN (ISO 15031-5).
pub const MAX_PIDS_PER_REQUEST: usize = 6;

pub fn service_name(service: u8) -> &'static str {
    match service {
        CURRENT_DATA => "live data",
        FREEZE_FRAME => "freeze frame",
        STORED_CODES => "stored codes",
        CLEAR_CODES => "clear codes",
        TEST_RESULTS => "on-board test results",
        PENDING_CODES => "pending codes",
        VEHICLE_INFO => "vehicle information",
        PERMANENT_CODES => "permanent codes",
        _ => "unknown service",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ObdError {
    #[error("ask for 1 to {MAX_PIDS_PER_REQUEST} PIDs at a time, not {0}")]
    PidCount(usize),
    #[error("empty reply")]
    Empty,
    #[error("malformed reply: {0}")]
    Malformed(&'static str),
    #[error("PID {0:02X} has an unknown length, so the rest of the reply can't be split")]
    UnknownPid(u8),
}

/// Service 01 for up to 6 PIDs.
pub fn current_data_request(pids: &[u8]) -> Result<Vec<u8>, ObdError> {
    if pids.is_empty() || pids.len() > MAX_PIDS_PER_REQUEST {
        return Err(ObdError::PidCount(pids.len()));
    }
    let mut v = vec![CURRENT_DATA];
    v.extend_from_slice(pids);
    Ok(v)
}

/// Service 02: a PID from freeze frame `frame` (usually 0).
pub fn freeze_frame_request(pid: u8, frame: u8) -> Vec<u8> {
    vec![FREEZE_FRAME, pid, frame]
}

/// Service 06: results for one monitor, or the supported-monitors mask for 00, 20, 40 ...
pub fn test_results_request(mid: u8) -> Vec<u8> {
    vec![TEST_RESULTS, mid]
}

/// Splits a service 02 reply (after 0x42) for one PID: (PID, frame number, data).
pub fn split_freeze_frame(data: &[u8]) -> Result<(u8, u8, Vec<u8>), ObdError> {
    if data.len() < 3 {
        return Err(ObdError::Malformed("freeze frame reply shorter than 3 bytes"));
    }
    let (pid, frame) = (data[0], data[1]);
    let rest = &data[2..];
    let bytes = match pids::def(pid) {
        Some(d) if rest.len() >= usize::from(d.len) => rest[..usize::from(d.len)].to_vec(),
        Some(_) => return Err(ObdError::Malformed("freeze frame reply shorter than its PID needs")),
        None => rest.to_vec(),
    };
    Ok((pid, frame, bytes))
}

/// Service 09: vehicle information item (02 VIN, 04 calibration IDs, 0A ECU name).
pub fn vehicle_info_request(item: u8) -> Vec<u8> {
    vec![VEHICLE_INFO, item]
}

/// A reply: positive (service + 0x40) or negative (`7F service code`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `data` is everything after the service byte.
    Positive {
        service: u8,
        data: Vec<u8>,
    },
    Negative {
        service: u8,
        code: u8,
    },
}

pub fn parse_reply(msg: &[u8]) -> Result<Reply, ObdError> {
    let (&first, rest) = msg.split_first().ok_or(ObdError::Empty)?;
    if first == 0x7F {
        if rest.len() < 2 {
            return Err(ObdError::Malformed("negative reply shorter than 3 bytes"));
        }
        return Ok(Reply::Negative { service: rest[0], code: rest[1] });
    }
    if first < 0x40 {
        return Err(ObdError::Malformed("not a reply (service byte below 0x40)"));
    }
    Ok(Reply::Positive { service: first - 0x40, data: rest.to_vec() })
}

/// True if `reply` answers `request`: same service, and for services 01, 02 and 09 the
/// same first PID. Replies to another tester's requests (such as a dash display) fail this.
pub fn reply_matches(request: &[u8], reply: &Reply) -> bool {
    let Some(&service) = request.first() else { return false };
    match reply {
        Reply::Negative { service: s, .. } => *s == service,
        Reply::Positive { service: s, data } => {
            if *s != service {
                return false;
            }
            match service {
                CURRENT_DATA | FREEZE_FRAME | VEHICLE_INFO => {
                    request.get(1).is_some_and(|pid| data.first() == Some(pid))
                }
                _ => true,
            }
        }
    }
}

/// Splits a service 01 reply (after 0x41) into (PID, data) pairs. Needs every PID's
/// length, so only ask several PIDs at once when all are known (see [`pids::def`]).
pub fn split_current_data(data: &[u8]) -> Result<Vec<(u8, Vec<u8>)>, ObdError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let pid = data[i];
        let Some(def) = pids::def(pid) else {
            if out.is_empty() {
                // A single unknown PID: everything after it is its data.
                out.push((pid, data[i + 1..].to_vec()));
                return Ok(out);
            }
            // Probably padding after the last known PID.
            return Ok(out);
        };
        let len = usize::from(def.len);
        if i + 1 + len > data.len() {
            if out.is_empty() {
                return Err(ObdError::Malformed("reply shorter than its PID needs"));
            }
            return Ok(out);
        }
        out.push((pid, data[i + 1..i + 1 + len].to_vec()));
        i += 1 + len;
    }
    Ok(out)
}

/// The PIDs a "PIDs supported" reply lists. `base` is 0x00, 0x20, 0x40 ... ; the mask is 4 bytes.
pub fn supported_pids(base: u8, mask: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (byte_i, &byte) in mask.iter().take(4).enumerate() {
        for bit in 0..8 {
            if byte & (0x80 >> bit) != 0 {
                let pid = u16::from(base) + 1 + (byte_i * 8 + bit) as u16;
                if let Ok(p) = u8::try_from(pid) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// The VIN from a service 09 item 02 reply (after 0x49 0x02): a count byte, then 17 characters.
/// Some ECUs pad the front with zero bytes; those are dropped.
pub fn parse_vin(data: &[u8]) -> Option<String> {
    let text: String = data.iter().skip(1).filter(|b| b.is_ascii_alphanumeric()).map(|&b| char::from(b)).collect();
    (text.len() == 17).then_some(text)
}

/// Text items such as the ECU name (service 09 item 0A): a count byte, then ASCII.
pub fn parse_text_item(data: &[u8]) -> String {
    data.iter()
        .skip(1)
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|&b| char::from(b))
        .collect::<String>()
        .trim()
        .to_string()
}

/// What a negative reply code means, in plain words (ISO 14229-1 table).
pub fn negative_code_text(code: u8) -> &'static str {
    match code {
        0x10 => "refused (general reject)",
        0x11 => "this module doesn't support that service",
        0x12 => "this module doesn't support that request",
        0x13 => "the request had the wrong length",
        0x14 => "the answer would be too long",
        0x21 => "busy: ask again",
        0x22 => "conditions not right (for example the engine is running)",
        0x24 => "requests in the wrong order",
        0x31 => "value not supported (out of range)",
        0x33 => "locked: security access needed",
        0x35 => "wrong security key",
        0x36 => "too many security attempts",
        0x37 => "wait before trying again",
        0x78 => "working on it: the answer is coming",
        0x7E => "not supported in the current session",
        0x7F => "service not supported in the current session",
        _ => "unknown reason",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_build() {
        assert_eq!(current_data_request(&[0x0C]).unwrap(), vec![0x01, 0x0C]);
        assert_eq!(current_data_request(&[0x0C, 0x0D, 0x05, 0x0F, 0x10, 0x42]).unwrap().len(), 7);
        assert_eq!(current_data_request(&[]), Err(ObdError::PidCount(0)));
        assert_eq!(current_data_request(&[1; 7]), Err(ObdError::PidCount(7)));
        assert_eq!(freeze_frame_request(0x0C, 0), vec![0x02, 0x0C, 0x00]);
        assert_eq!(vehicle_info_request(0x02), vec![0x09, 0x02]);
    }

    #[test]
    fn replies_parse_and_match() {
        let r = parse_reply(&[0x41, 0x0C, 0x1D, 0x48]).unwrap();
        assert_eq!(r, Reply::Positive { service: 0x01, data: vec![0x0C, 0x1D, 0x48] });
        assert!(reply_matches(&[0x01, 0x0C], &r));
        assert!(!reply_matches(&[0x01, 0x05], &r)); // another tester asked for rpm
        assert!(!reply_matches(&[0x09, 0x02], &r));
        let n = parse_reply(&[0x7F, 0x01, 0x31]).unwrap();
        assert_eq!(n, Reply::Negative { service: 0x01, code: 0x31 });
        assert!(reply_matches(&[0x01, 0x64], &n));
        assert!(reply_matches(&[0x03], &parse_reply(&[0x43, 0x00]).unwrap()));
        assert_eq!(parse_reply(&[]), Err(ObdError::Empty));
        assert!(parse_reply(&[0x7F, 0x01]).is_err());
        assert!(parse_reply(&[0x01, 0x0C]).is_err());
    }

    #[test]
    fn multi_pid_replies_split() {
        // 41 0C 1D48 0D 00 05 46 42 37C8
        let data = [0x0C, 0x1D, 0x48, 0x0D, 0x00, 0x05, 0x46, 0x42, 0x37, 0xC8];
        let parts = split_current_data(&data).unwrap();
        assert_eq!(
            parts,
            vec![(0x0C, vec![0x1D, 0x48]), (0x0D, vec![0x00]), (0x05, vec![0x46]), (0x42, vec![0x37, 0xC8])]
        );
        // Padding after the last PID is ignored.
        let parts = split_current_data(&[0x05, 0x46, 0xCC, 0xCC]).unwrap();
        assert_eq!(parts, vec![(0x05, vec![0x46])]);
        assert!(split_current_data(&[0x0C, 0x1D]).is_err());
        // A single unknown PID keeps all its bytes.
        assert_eq!(split_current_data(&[0x9D, 1, 2, 3]).unwrap(), vec![(0x9D, vec![1, 2, 3])]);
    }

    #[test]
    fn supported_masks_decode() {
        // BE 1F A8 13: the classic example from SAE J1979 (01-20).
        let s = supported_pids(0x00, &[0xBE, 0x1F, 0xA8, 0x13]);
        assert_eq!(
            s,
            vec![0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x13, 0x15, 0x1C, 0x1F, 0x20]
        );
        assert_eq!(supported_pids(0xE0, &[0, 0, 0, 0x02]), vec![0xFF]);
        // The last bit of the E0 mask would be PID 0x100, which does not exist.
        assert_eq!(supported_pids(0xE0, &[0, 0, 0, 0x01]), Vec::<u8>::new());
    }

    #[test]
    fn freeze_frames_split() {
        assert_eq!(split_freeze_frame(&[0x02, 0x00, 0x01, 0x28]).unwrap(), (0x02, 0, vec![0x01, 0x28]));
        assert_eq!(split_freeze_frame(&[0x0C, 0x00, 0x1D, 0x48, 0xCC]).unwrap(), (0x0C, 0, vec![0x1D, 0x48]));
        assert!(split_freeze_frame(&[0x0C, 0x00, 0x1D]).is_err());
        assert!(split_freeze_frame(&[0x0C]).is_err());
        assert_eq!(test_results_request(0x21), vec![0x06, 0x21]);
    }

    #[test]
    fn vin_and_text_items() {
        let mut d = vec![0x01];
        d.extend_from_slice(b"SCANASIMHIJET0001");
        assert_eq!(parse_vin(&d).as_deref(), Some("SCANASIMHIJET0001"));
        let mut padded = vec![0x01, 0x00, 0x00, 0x00];
        padded.extend_from_slice(b"SCANASIMHIJET0001");
        assert_eq!(parse_vin(&padded).as_deref(), Some("SCANASIMHIJET0001"));
        assert_eq!(parse_vin(&[0x01, b'A']), None);
        let mut name = vec![0x01];
        name.extend_from_slice(b"ECM\0-EngineControl\0\0");
        assert_eq!(parse_text_item(&name), "ECM-EngineControl");
    }
}
