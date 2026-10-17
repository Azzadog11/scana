//! The bridge's serial protocol: SLCAN (Lawicel) text lines plus Scana's `#` commands.
//! See `docs/DESIGN.md` section 4 and `firmware/scana-bridge/bridge.h`.
//!
//! Every command gets exactly one reply: OK (CR), error (BEL), `z`/`Z` for a queued
//! frame, or one line. Received frames (`t T r R`), events (`#e ...`) and the ready line
//! (`#R ...`) arrive on their own.

use crate::can::{Bitrate, CanId, Frame};

/// How the bridge joins the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Acknowledges frames and may transmit.
    Normal,
    /// Never drives the bus: no acknowledgements, no error frames, no transmits.
    ListenOnly,
    /// The MCP2515's internal loopback: frames sent come straight back; nothing reaches the bus.
    Loopback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Bitrate(Bitrate),
    Open(OpenMode),
    Close,
    Send(Frame),
    StatusFlags,
    Version,
    Identity,
    Health,
    SelfTest,
    Ping,
    /// Close the bus if no command arrives for this many milliseconds; 0 turns it off.
    HostWatchdog(u16),
    MicrosTimestamps(bool),
    Events(bool),
    /// Checked mode (firmware 0.1.2): frame lines both ways end in `;XX`, a CRC-8 of the line.
    Checked(bool),
    /// `None` accepts every frame; `Some((mask, filter))` only standard IDs with `id & mask == filter`.
    Filter(Option<(u16, u16)>),
}

impl Command {
    /// The command line, with its CR.
    pub fn encode(&self) -> String {
        let mut s = match self {
            Command::Bitrate(b) => format!("S{}", b.slcan_code()),
            Command::Open(OpenMode::Normal) => "O".into(),
            Command::Open(OpenMode::ListenOnly) => "L".into(),
            Command::Open(OpenMode::Loopback) => "#K".into(),
            Command::Close => "C".into(),
            Command::Send(f) => encode_frame(f),
            Command::StatusFlags => "F".into(),
            Command::Version => "V".into(),
            Command::Identity => "#I".into(),
            Command::Health => "#H".into(),
            Command::SelfTest => "#T".into(),
            Command::Ping => "#P".into(),
            Command::HostWatchdog(ms) => format!("#W{ms}"),
            Command::MicrosTimestamps(on) => format!("#U{}", u8::from(*on)),
            Command::Events(on) => format!("#E{}", u8::from(*on)),
            Command::Checked(on) => format!("#C{}", u8::from(*on)),
            Command::Filter(None) => "#M".into(),
            Command::Filter(Some((mask, filt))) => format!("#M{:03X},{:03X}", mask & 0x7FF, filt & 0x7FF),
        };
        s.push('\r');
        s
    }

    /// The command line for checked mode: a frame gets its `;XX` check; others are the same.
    pub fn encode_checked(&self) -> String {
        match self {
            Command::Send(f) => {
                let mut s = with_check(&encode_frame(f));
                s.push('\r');
                s
            }
            other => other.encode(),
        }
    }
}

/// CRC-8 (polynomial 0x07, start 0; "123456789" gives 0xF4), as the bridge computes it.
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in bytes {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
        }
    }
    c
}

/// A line with its `;XX` check added (no CR).
pub fn with_check(line: &str) -> String {
    format!("{line};{:02X}", crc8(line.as_bytes()))
}

/// The line without its `;XX` check, if the check is there and matches.
pub fn strip_check(line: &str) -> Option<&str> {
    let (body, check) = line.rsplit_once(';')?;
    let want = u8::from_str_radix(check, 16).ok().filter(|_| check.len() == 2)?;
    (crc8(body.as_bytes()) == want).then_some(body)
}

fn encode_frame(f: &Frame) -> String {
    let kind = match (f.id.is_extended(), f.is_remote()) {
        (false, false) => 't',
        (true, false) => 'T',
        (false, true) => 'r',
        (true, true) => 'R',
    };
    let mut s = String::with_capacity(27);
    s.push(kind);
    s.push_str(&f.id.to_string());
    s.push_str(&format!("{:X}", f.len()));
    for b in f.data() {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// The bridge's counters, from `#H`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub uptime_s: u32,
    /// C closed, N normal, L listen-only, K loopback.
    pub mode: char,
    pub rate_code: u8,
    pub rx: u32,
    pub tx: u32,
    pub tx_errors: u32,
    pub hw_overruns: u32,
    pub bad_commands: u32,
    /// Transmit error counter (MCP2515 TEC).
    pub tec: u8,
    /// Receive error counter (MCP2515 REC).
    pub rec: u8,
    /// MCP2515 EFLG register.
    pub eflg: u8,
}

impl Health {
    pub fn bus_off(&self) -> bool {
        self.eflg & 0x20 != 0
    }
    pub fn error_passive(&self) -> bool {
        self.eflg & 0x18 != 0
    }
}

/// Asynchronous events from the bridge (`#e ...`, after `#E1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    BusOff,
    ErrorPassive,
    ErrorWarning,
    /// Back to error-active.
    Active,
    /// The MCP2515 lost frames: its count so far.
    RxOverrun(u32),
    /// A frame wasn't sent: no acknowledgement or a bus error.
    TxError,
    /// A frame lost arbitration and, in one-shot mode, wasn't retried.
    TxArbitration,
    /// The host watchdog closed the bus.
    HostWatchdog,
    /// The bridge refused a command (firmware 0.1.2): why (NOTOPEN, TXFULL, BADCHECK, BADCMD,
    /// TOOLONG, REFUSED) and the start of the line it got.
    Refused {
        why: String,
        line: String,
    },
    /// Found by Scana, not the bridge: a frame line from the bridge failed its check.
    DamagedLine(String),
    Other(String),
}

impl Event {
    /// The event in plain words, for logs.
    pub fn describe(&self) -> String {
        match self {
            Event::BusOff => "bus-off: the CAN controller stopped after too many errors".into(),
            Event::ErrorPassive => "error-passive: many bus errors".into(),
            Event::ErrorWarning => "error warning: bus errors are building up".into(),
            Event::Active => "back to error-active (errors cleared)".into(),
            Event::RxOverrun(n) => format!("the CAN chip lost received frames ({n} so far)"),
            Event::TxError => "a frame wasn't acknowledged or hit a bus error".into(),
            Event::TxArbitration => "a frame lost arbitration and wasn't retried".into(),
            Event::HostWatchdog => "the bridge's host watchdog closed the bus".into(),
            Event::Refused { why, line } => match why.as_str() {
                "NOTOPEN" => "the bridge refused a frame: its bus isn't open".into(),
                "TXFULL" => "the bridge refused a frame: its send queue is full".into(),
                "BADCHECK" => format!(
                    "the bridge refused a command whose check failed (damaged on the USB cable), so it wasn't sent: {line}"
                ),
                "BADCMD" | "TOOLONG" => {
                    format!("the bridge couldn't read a command (probably damaged on the USB cable): {line}")
                }
                _ => format!("the bridge refused a command: {line}"),
            },
            Event::DamagedLine(l) => {
                format!("a line from the bridge failed its check (damaged on the USB cable) and was dropped: {l}")
            }
            Event::Other(s) => format!("bridge event: {s}"),
        }
    }
}

/// The bridge's reset report (the end of its `#R` line, firmware 0.1.1 and later) in plain
/// words: why it started.
pub fn describe_reset(report: &str) -> String {
    let field = |name: &str| report.split_whitespace().find_map(|w| w.strip_prefix(name)).map(str::to_string);
    let step_name = |s: &str| match s {
        "1" => "between tasks",
        "2" => "reading commands from the laptop",
        "3" => "reading frames from the CAN chip",
        "4" => "sending a frame",
        "5" => "checking the bus status",
        "6" => "passing frames to the laptop",
        "7" => "waiting to write to the laptop",
        "8" => "updating the lights",
        _ => "unknown",
    };
    let mut out = match field("reset=").as_deref() {
        None if report.trim().is_empty() => return "no reset report (firmware before 0.1.1)".into(),
        None => return report.to_string(),
        Some("cold") => "it lost power and started from cold: its supply was cut, or dipped very low".to_string(),
        Some("watchdog") => {
            let at = field("step=").map_or_else(String::new, |s| format!(", while {}", step_name(&s)));
            format!("its watchdog restarted it: the firmware stopped for 250 ms{at}")
        }
        Some("warm") => "it restarted with its power kept on: its reset pin (opening the port pulses it), a short \
             dip in its supply (a brown-out), or electrical noise"
            .to_string(),
        Some(other) => format!("reset {other}"),
    };
    if let Some(flags) = field("flags=") {
        let named: Vec<&str> = flags
            .chars()
            .filter_map(|c| match c {
                'P' => Some("power-on"),
                'E' => Some("reset pin"),
                'B' => Some("brown-out"),
                'W' => Some("watchdog"),
                _ => None,
            })
            .collect();
        if !named.is_empty() {
            out.push_str(&format!("; the chip flagged: {}", named.join(", ")));
        }
    }
    out
}

/// One thing read from the bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// CR: the command worked.
    Ok,
    /// BEL: the command was refused.
    Error,
    /// `z` or `Z`: the frame was queued.
    Queued,
    /// A received frame, with the bridge's timestamp if one was asked for.
    Frame {
        frame: Frame,
        bridge_time: Option<BridgeTime>,
    },
    /// `#R`: the bridge started. `chip_ok` is false if the MCP2515 didn't answer.
    Ready {
        version: String,
        chip_ok: bool,
        /// Why the bridge last reset, as it reports it from firmware 0.1.1
        /// ("reset=warm step=3 count=1 flags=B"); empty from older firmware.
        reset: String,
    },
    Identity(String),
    Health(Health),
    /// `#T`: `Ok(())`, or the failed step and the value read there.
    SelfTest(Result<(), (u8, u8)>),
    Pong,
    StatusFlags(u8),
    Version(String),
    Serial(String),
    Event(Event),
    /// In checked mode, a frame line whose check failed: damaged on the way, so dropped.
    BadCheck(String),
    /// Anything else (noise, an unknown reply).
    Unknown(String),
}

impl Line {
    /// True for lines that answer a command; false for frames, events and the ready line.
    pub fn is_reply(&self) -> bool {
        !matches!(self, Line::Frame { .. } | Line::Event(_) | Line::Ready { .. } | Line::BadCheck(_) | Line::Unknown(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeTime {
    /// Lawicel `Z1`: milliseconds within the minute, 0-59999.
    Millis(u16),
    /// Scana `#U1`: the bridge's microsecond counter (wraps after about 71 minutes).
    Micros(u32),
}

/// Splits the byte stream from the bridge into lines.
#[derive(Default)]
pub struct LineReader {
    buf: Vec<u8>,
    discarded: u64,
    /// Checked mode: frame lines must end in a good `;XX`.
    pub require_check: bool,
}

impl LineReader {
    const MAX_LINE: usize = 160;

    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes thrown away as noise (for example the byte an Uno sends while it resets).
    pub fn discarded(&self) -> u64 {
        self.discarded
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Line> {
        let mut out = Vec::new();
        for &b in bytes {
            match b {
                b'\r' => {
                    let text = String::from_utf8_lossy(&self.buf).into_owned();
                    self.buf.clear();
                    let frame_line = matches!(text.as_bytes().first(), Some(b't' | b'T' | b'r' | b'R'));
                    if self.require_check && frame_line {
                        out.push(match strip_check(&text) {
                            Some(body) => parse_line(body),
                            None => Line::BadCheck(text),
                        });
                    } else {
                        out.push(parse_line(&text));
                    }
                }
                0x07 => {
                    if !self.buf.is_empty() {
                        self.discarded += self.buf.len() as u64;
                        self.buf.clear();
                    }
                    out.push(Line::Error);
                }
                b'\n' => {}
                0x20..=0x7E => {
                    if self.buf.len() < Self::MAX_LINE {
                        self.buf.push(b);
                    } else {
                        self.discarded += 1;
                    }
                }
                _ => self.discarded += 1,
            }
        }
        out
    }
}

/// Parses one line (without its CR).
pub fn parse_line(s: &str) -> Line {
    if s.is_empty() {
        return Line::Ok;
    }
    let first = s.as_bytes()[0];
    match first {
        b't' | b'T' | b'r' | b'R' => parse_frame_line(s).unwrap_or_else(|| Line::Unknown(s.into())),
        b'z' | b'Z' if s.len() == 1 => Line::Queued,
        b'F' if s.len() == 3 => {
            u8::from_str_radix(&s[1..], 16).map(Line::StatusFlags).unwrap_or(Line::Unknown(s.into()))
        }
        b'V' => Line::Version(s[1..].into()),
        b'N' => Line::Serial(s[1..].into()),
        b'#' => parse_extension(&s[1..]).unwrap_or_else(|| Line::Unknown(s.into())),
        _ => Line::Unknown(s.into()),
    }
}

fn hex(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

fn parse_frame_line(s: &str) -> Option<Line> {
    let kind = s.as_bytes()[0];
    let ext = kind == b'T' || kind == b'R';
    let rtr = kind == b'r' || kind == b'R';
    let id_len = if ext { 8 } else { 3 };
    let (body, micros) = match s.split_once(':') {
        Some((b, t)) if t.len() == 8 => (b, Some(hex(t)?)),
        Some(_) => return None,
        None => (s, None),
    };
    let id_raw = hex(body.get(1..1 + id_len)?)?;
    let len = hex(body.get(1 + id_len..2 + id_len)?)? as usize;
    if len > 8 {
        return None;
    }
    let data_len = if rtr { 0 } else { 2 * len };
    let rest = body.get(2 + id_len..)?;
    let (data_hex, millis) = if rest.len() == data_len {
        (rest, None)
    } else if rest.len() == data_len + 4 {
        (&rest[..data_len], Some(hex(&rest[data_len..])? as u16))
    } else {
        return None;
    };
    let id = if ext { CanId::extended(id_raw)? } else { CanId::standard(u16::try_from(id_raw).ok()?)? };
    let frame = if rtr {
        Frame::remote(id, len as u8).ok()?
    } else {
        let mut data = [0u8; 8];
        for (i, byte) in data.iter_mut().enumerate().take(len) {
            *byte = hex(data_hex.get(2 * i..2 * i + 2)?)? as u8;
        }
        Frame::new(id, &data[..len]).ok()?
    };
    let bridge_time = match (micros, millis) {
        (Some(us), _) => Some(BridgeTime::Micros(us)),
        (None, Some(ms)) => Some(BridgeTime::Millis(ms)),
        _ => None,
    };
    Some(Line::Frame { frame, bridge_time })
}

fn parse_extension(s: &str) -> Option<Line> {
    let (tag, rest) = match s.split_once(' ') {
        Some((t, r)) => (t, r),
        None => (s, ""),
    };
    match tag {
        "R" => {
            // "scana-bridge 0.1.0 mcp2515=ok", and from 0.1.1 a reset report after it.
            let mut parts = rest.split_whitespace();
            let _name = parts.next()?;
            let version = parts.next()?.to_string();
            let others: Vec<&str> = parts.collect();
            let chip_ok = others.contains(&"mcp2515=ok");
            let reset = others.iter().filter(|p| !p.starts_with("mcp2515=")).copied().collect::<Vec<_>>().join(" ");
            Some(Line::Ready { version, chip_ok, reset })
        }
        "I" => Some(Line::Identity(rest.into())),
        "P" => Some(Line::Pong),
        "H" => parse_health(rest).map(Line::Health),
        "T" => {
            let mut parts = rest.split_whitespace();
            match parts.next()? {
                "PASS" => Some(Line::SelfTest(Ok(()))),
                "FAIL" => {
                    let step = parts.next()?.parse().ok()?;
                    let detail = parts.next().and_then(hex).unwrap_or(0) as u8;
                    Some(Line::SelfTest(Err((step, detail))))
                }
                _ => None,
            }
        }
        "e" => Some(Line::Event(parse_event(rest))),
        _ => None,
    }
}

fn parse_event(s: &str) -> Event {
    let mut parts = s.split_whitespace();
    match parts.next() {
        Some("BUSOFF") => Event::BusOff,
        Some("ERRPASSIVE") => Event::ErrorPassive,
        Some("ERRWARN") => Event::ErrorWarning,
        Some("ACTIVE") => Event::Active,
        Some("RXOVR") => Event::RxOverrun(parts.next().and_then(|n| n.parse().ok()).unwrap_or(0)),
        Some("TXERR") => Event::TxError,
        Some("TXARB") => Event::TxArbitration,
        Some("HOSTWD") => Event::HostWatchdog,
        Some(why @ ("NOTOPEN" | "TXFULL" | "BADCHECK" | "BADCMD" | "TOOLONG" | "REFUSED")) => {
            Event::Refused { why: why.into(), line: parts.collect::<Vec<_>>().join(" ") }
        }
        _ => Event::Other(s.into()),
    }
}

fn parse_health(s: &str) -> Option<Health> {
    let mut h = Health::default();
    for part in s.split_whitespace() {
        let (k, v) = part.split_once('=')?;
        let num = || v.parse::<u32>().ok();
        match k {
            "up" => h.uptime_s = num()?,
            "mode" => h.mode = v.chars().next()?,
            "rate" => h.rate_code = num()? as u8,
            "rx" => h.rx = num()?,
            "tx" => h.tx = num()?,
            "txerr" => h.tx_errors = num()?,
            "hwovr" => h.hw_overruns = num()?,
            "bad" => h.bad_commands = num()?,
            "tec" => h.tec = num()? as u8,
            "rec" => h.rec = num()? as u8,
            "eflg" => h.eflg = hex(v)? as u8,
            _ => {}
        }
    }
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std(id: u16, data: &[u8]) -> Frame {
        Frame::new(CanId::Standard(id), data).unwrap()
    }

    #[test]
    fn commands_encode() {
        assert_eq!(Command::Bitrate(Bitrate::K500).encode(), "S6\r");
        assert_eq!(Command::Open(OpenMode::ListenOnly).encode(), "L\r");
        assert_eq!(Command::Open(OpenMode::Loopback).encode(), "#K\r");
        assert_eq!(Command::Send(std(0x7DF, &[0x02, 0x01, 0x0C, 0, 0, 0, 0, 0])).encode(), "t7DF802010C0000000000\r");
        let ext = Frame::new(CanId::Extended(0x18DB_33F1), &[0x02, 0x01, 0x00]).unwrap();
        assert_eq!(Command::Send(ext).encode(), "T18DB33F13020100\r");
        assert_eq!(Command::Send(Frame::remote(CanId::Standard(0x7E0), 8).unwrap()).encode(), "r7E08\r");
        assert_eq!(Command::HostWatchdog(2000).encode(), "#W2000\r");
        assert_eq!(Command::Filter(Some((0x7F8, 0x7E8))).encode(), "#M7F8,7E8\r");
    }

    #[test]
    fn frame_lines_parse() {
        let l = parse_line("t7E880441054600000000");
        assert_eq!(l, Line::Frame { frame: std(0x7E8, &[0x04, 0x41, 0x05, 0x46, 0, 0, 0, 0]), bridge_time: None });
        let l = parse_line("t7E8804410C1D4C000000:0012ABCD");
        assert!(matches!(l, Line::Frame { bridge_time: Some(BridgeTime::Micros(0x0012_ABCD)), .. }));
        let l = parse_line("t7E8804410C1D4C00000004D2");
        assert!(matches!(l, Line::Frame { bridge_time: Some(BridgeTime::Millis(1234)), .. }));
        let l = parse_line("T18DAF1104037F2231");
        match l {
            Line::Frame { frame, .. } => {
                assert_eq!(frame.id, CanId::Extended(0x18DA_F110));
                assert_eq!(frame.data(), &[0x03, 0x7F, 0x22, 0x31]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(parse_line("r1232"), Line::Frame { frame, .. } if frame.is_remote() && frame.len() == 2));
        // Bad lines are not frames.
        assert!(matches!(parse_line("t7E8904"), Line::Unknown(_)));
        assert!(matches!(parse_line("t7E8202"), Line::Unknown(_)));
        assert!(matches!(parse_line("t8001AA"), Line::Unknown(_)));
        assert!(matches!(parse_line("t7E81ZZ"), Line::Unknown(_)));
        assert!(matches!(parse_line("t7E801:12"), Line::Unknown(_)));
    }

    #[test]
    fn replies_and_events_parse() {
        assert_eq!(parse_line(""), Line::Ok);
        assert_eq!(parse_line("z"), Line::Queued);
        assert_eq!(parse_line("Z"), Line::Queued);
        assert_eq!(parse_line("F24"), Line::StatusFlags(0x24));
        assert_eq!(parse_line("V0100"), Line::Version("0100".into()));
        assert_eq!(parse_line("#P"), Line::Pong);
        assert_eq!(parse_line("#T PASS"), Line::SelfTest(Ok(())));
        assert_eq!(parse_line("#T FAIL 1 00"), Line::SelfTest(Err((1, 0))));
        assert_eq!(parse_line("#T FAIL 4"), Line::SelfTest(Err((4, 0))));
        assert_eq!(
            parse_line("#R scana-bridge 0.1.0 mcp2515=missing"),
            Line::Ready { version: "0.1.0".into(), chip_ok: false, reset: String::new() }
        );
        assert_eq!(parse_line("#e RXOVR 3"), Line::Event(Event::RxOverrun(3)));
        assert_eq!(parse_line("#e HOSTWD"), Line::Event(Event::HostWatchdog));
        let h = parse_line("#H up=12 mode=L rate=6 rx=100 tx=2 txerr=1 hwovr=0 bad=3 tec=8 rec=0 eflg=15");
        match h {
            Line::Health(h) => {
                assert_eq!((h.uptime_s, h.mode, h.rate_code, h.rx, h.tx, h.tx_errors), (12, 'L', 6, 100, 2, 1));
                assert_eq!((h.bad_commands, h.tec, h.eflg), (3, 8, 0x15));
                assert!(h.error_passive() && !h.bus_off());
            }
            other => panic!("{other:?}"),
        }
        assert!(Line::Ok.is_reply() && Line::Queued.is_reply());
        assert!(!Line::Event(Event::BusOff).is_reply());
    }

    #[test]
    fn line_reader_splits_and_drops_noise() {
        let mut r = LineReader::new();
        // 0xFF is the byte an Uno sends while it resets.
        let lines = r.push(b"\xff#R scana-bridge 0.1.0 mcp2515=ok\r#I scana");
        assert_eq!(lines, vec![Line::Ready { version: "0.1.0".into(), chip_ok: true, reset: String::new() }]);
        assert_eq!(
            parse_line("#R scana-bridge 0.1.1 mcp2515=ok reset=warm step=3 count=2 flags=B"),
            Line::Ready { version: "0.1.1".into(), chip_ok: true, reset: "reset=warm step=3 count=2 flags=B".into() }
        );
        assert_eq!(r.discarded(), 1);
        let lines = r.push(b"-bridge 0.1.0 mcp2515 16MHz\r\x07z\r\r");
        assert_eq!(
            lines,
            vec![Line::Identity("scana-bridge 0.1.0 mcp2515 16MHz".into()), Line::Error, Line::Queued, Line::Ok]
        );
    }

    #[test]
    fn encoded_frames_parse_back() {
        for f in [
            std(0x000, &[]),
            std(0x7FF, &[1, 2, 3, 4, 5, 6, 7, 8]),
            Frame::new(CanId::Extended(0x1FFF_FFFF), &[0xAA]).unwrap(),
            Frame::remote(CanId::Extended(0x0), 0).unwrap(),
        ] {
            let line = Command::Send(f).encode();
            let parsed = parse_line(line.trim_end_matches('\r'));
            assert_eq!(parsed, Line::Frame { frame: f, bridge_time: None });
        }
    }

    #[test]
    fn reset_reports_in_words() {
        assert!(describe_reset("").contains("before 0.1.1"));
        assert!(describe_reset("reset=cold").contains("lost power"));
        let w = describe_reset("reset=watchdog step=7 count=2");
        assert!(w.contains("watchdog") && w.contains("waiting to write to the laptop"), "{w}");
        let b = describe_reset("reset=warm step=1 count=3 flags=B");
        assert!(b.contains("power kept on") && b.ends_with("the chip flagged: brown-out"), "{b}");
        assert_eq!(describe_reset("something new"), "something new");
    }

    #[test]
    fn checks_match_the_bridge() {
        assert_eq!(crc8(b"123456789"), 0xF4); // CRC-8 (0x07) check value, as in the firmware tests
        let line = with_check("t7E0802010C0000000000");
        assert_eq!(strip_check(&line), Some("t7E0802010C0000000000"));
        let damaged = line.replacen("02010C", "12010C", 1); // a read became a reset: caught
        assert_eq!(strip_check(&damaged), None);
        assert_eq!(strip_check("t7E0802010C0000000000"), None);
        let f = Frame::new(CanId::Standard(0x7E0), &[0x02, 0x01, 0x0C, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(Command::Send(f).encode_checked(), format!("{line}\r"));
        assert_eq!(Command::Ping.encode_checked(), "#P\r");
        assert_eq!(Command::Checked(true).encode(), "#C1\r");
    }

    #[test]
    fn checked_mode_drops_damaged_frame_lines() {
        let mut r = LineReader::new();
        r.require_check = true;
        let good = with_check("t7E8504410C1234:000003E8");
        let bad = good.replacen("0C12", "0C13", 1);
        let input = format!("{good}\r{bad}\rz\r#e BADCMD t7E08020\r");
        let lines = r.push(input.as_bytes());
        assert!(matches!(&lines[0], Line::Frame { frame, .. } if frame.data() == [0x04, 0x41, 0x0C, 0x12, 0x34]));
        assert_eq!(lines[1], Line::BadCheck(bad));
        assert_eq!(lines[2], Line::Queued);
        assert_eq!(lines[3], Line::Event(Event::Refused { why: "BADCMD".into(), line: "t7E08020".into() }));
        assert!(!lines[1].is_reply());
        assert!(Event::Refused { why: "BADCHECK".into(), line: "t7E0".into() }.describe().contains("wasn't sent"));
    }
}
