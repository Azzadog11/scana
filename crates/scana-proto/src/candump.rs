//! candump log lines, as written by `candump -l` (can-utils) and read by SavvyCAN:
//! `(1760580000.123456) can0 7E8#04410C1D4C000000`.

use crate::can::{CanId, Frame};

/// Formats a frame received `micros` microseconds after the Unix epoch.
pub fn format_line(frame: &Frame, micros: u64, interface: &str) -> String {
    format!("({}.{:06}) {} {}", micros / 1_000_000, micros % 1_000_000, interface, frame)
}

/// Parses a candump log line: (time in microseconds, interface, frame).
pub fn parse_line(line: &str) -> Option<(u64, String, Frame)> {
    let line = line.trim();
    let rest = line.strip_prefix('(')?;
    let (time, rest) = rest.split_once(')')?;
    let (secs, frac) = time.split_once('.')?;
    let secs: u64 = secs.parse().ok()?;
    if frac.is_empty() || frac.len() > 6 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let micros = secs.checked_mul(1_000_000)? + frac.parse::<u64>().ok()? * 10u64.pow(6 - frac.len() as u32);
    let mut parts = rest.split_whitespace();
    let iface = parts.next()?.to_string();
    let body = parts.next()?;
    let (id_s, data_s) = body.split_once('#')?;
    let id = match id_s.len() {
        3 => CanId::standard(u16::from_str_radix(id_s, 16).ok()?)?,
        8 => CanId::extended(u32::from_str_radix(id_s, 16).ok()?)?,
        _ => return None,
    };
    let frame = if let Some(r) = data_s.strip_prefix('R') {
        let len = if r.is_empty() { 0 } else { r.parse().ok()? };
        Frame::remote(id, len).ok()?
    } else {
        if data_s.len() % 2 != 0 || data_s.len() > 16 {
            return None;
        }
        let mut data = Vec::with_capacity(8);
        for i in (0..data_s.len()).step_by(2) {
            data.push(u8::from_str_radix(data_s.get(i..i + 2)?, 16).ok()?);
        }
        Frame::new(id, &data).ok()?
    };
    Some((micros, iface, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let f = Frame::new(CanId::Standard(0x7E8), &[0x04, 0x41, 0x0C, 0x1D, 0x4C, 0, 0, 0]).unwrap();
        let line = format_line(&f, 1_760_580_000_123_456, "can0");
        assert_eq!(line, "(1760580000.123456) can0 7E8#04410C1D4C000000");
        assert_eq!(parse_line(&line), Some((1_760_580_000_123_456, "can0".to_string(), f)));
        let e = Frame::new(CanId::Extended(0x18DA_F110), &[]).unwrap();
        let line = format_line(&e, 5, "scana");
        assert_eq!(parse_line(&line).unwrap().2, e);
        let r = Frame::remote(CanId::Standard(0x123), 8).unwrap();
        assert_eq!(parse_line(&format_line(&r, 0, "x")).unwrap().2, r);
    }

    #[test]
    fn bad_lines() {
        assert!(parse_line("").is_none());
        assert!(parse_line("(1.5) can0 7E8#0").is_none());
        assert!(parse_line("(1.5) can0 7E8#001122334455667788").is_none());
        assert!(parse_line("(x.5) can0 7E8#00").is_none());
        assert!(parse_line("(1.5) can0 7E#00").is_none());
        assert_eq!(parse_line("(1.5) can0 7E8#00").unwrap().0, 1_500_000);
    }
}
