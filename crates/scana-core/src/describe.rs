//! Short plain-English readings of frames, for the bus monitor and the command line.

use crate::client::Addressing;
use scana_proto::{Frame, obd, pids};

/// What an OBD-II frame says, in a few words. Empty if Scana can't tell.
pub fn frame_meaning(f: &Frame) -> String {
    let d = f.data();
    let Some(&pci) = d.first() else { return String::new() };
    if pci >> 4 != 0 {
        return match pci >> 4 {
            1 => "start of a long message".into(),
            2 => "part of a long message".into(),
            3 => "flow control: go ahead".into(),
            _ => String::new(),
        };
    }
    let len = usize::from(pci & 0x0F);
    let Some(msg) = d.get(1..1 + len) else { return String::new() };
    let is_request = Addressing::is_request_id(f.id);
    match (is_request, msg) {
        (true, [0x01, list @ ..]) if !list.is_empty() => {
            let names: Vec<&str> = list.iter().map(|p| pids::def(*p).map_or("?", |x| x.name)).collect();
            format!("asks: {}", names.join(", "))
        }
        (true, [s, ..]) => format!("asks: {}", obd::service_name(*s)),
        (false, [0x41, rest @ ..]) => obd::split_current_data(rest)
            .map(|parts| {
                parts
                    .iter()
                    .map(|(p, v)| match pids::decode(*p, v) {
                        Some(dec) => format!("{} {}", pids::def(*p).map_or("?", |x| x.name), dec.display()),
                        None => format!("PID {p:02X}"),
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default(),
        (false, [0x7F, s, code, ..]) => {
            format!("refused {}: {}", obd::service_name(*s), obd::negative_code_text(*code))
        }
        (false, [s, ..]) if *s >= 0x40 => format!("answers: {}", obd::service_name(s - 0x40)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_proto::CanId;

    #[test]
    fn meanings() {
        let f = |id, d: &[u8]| Frame::new(CanId::Standard(id), d).unwrap();
        assert_eq!(frame_meaning(&f(0x7DF, &[0x02, 0x01, 0x0C, 0x55])), "asks: Engine speed");
        assert_eq!(frame_meaning(&f(0x7E8, &[0x04, 0x41, 0x0C, 0x0D, 0x48])), "Engine speed 850 rpm");
        assert_eq!(
            frame_meaning(&f(0x7E8, &[0x03, 0x7F, 0x01, 0x12])),
            "refused live data: this module doesn't support that request"
        );
        assert_eq!(frame_meaning(&f(0x7E0, &[0x30, 0, 0])), "flow control: go ahead");
        assert_eq!(frame_meaning(&f(0x7E8, &[0x02, 0x43, 0x00])), "answers: stored codes");
        assert_eq!(frame_meaning(&f(0x123, &[])), "");
    }
}
