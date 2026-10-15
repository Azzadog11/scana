//! Simulated modules beyond OBD-II: brakes, airbags, the instrument cluster, body control and
//! air conditioning, answering UDS (ISO 14229) or KWP2000 (ISO 14230) read requests.
//!
//! The demo layout follows the common Toyota-family pattern (modules on request ID + 8, body
//! modules behind a gateway on 0x750 with an address byte). It is invented for testing: the
//! real Hijet's modules are unknown until Scana's module discovery runs on the truck. Part
//! numbers and names say SIM so nobody mistakes them for real ones.

use scana_proto::isotp::{self, SendStep, Sender};
use scana_proto::{CanId, Dtc, Frame};

/// Modules pad frames with this byte.
const PADDING: u8 = 0x00;

/// Which diagnostic dialect a module speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// UDS: `22` data by ID, `19` fault codes, `3E 00` answered `7E 00`.
    Uds,
    /// KWP2000: `21` local IDs, `1A` identity, `18` fault codes; `3E 00` refused (`7F 3E 12`),
    /// `3E 01` or a bare `3E` answered.
    Kwp,
}

/// A data value that changes with time: given milliseconds, returns the bytes.
pub type DataFn = fn(u64) -> Vec<u8>;

/// One simulated module.
#[derive(Debug)]
pub struct Module {
    pub name: String,
    pub request_id: CanId,
    pub reply_id: CanId,
    /// The address byte in front of every frame (extended addressing), if any.
    pub ext_addr: Option<u8>,
    pub dialect: Dialect,
    /// UDS identity DIDs, or KWP `1A` options (as u16), with their bytes.
    pub identity: Vec<(u16, Vec<u8>)>,
    /// Toyota-style KWP identity: `1A 88 00` lists these sub-IDs; `1A 88 nn` returns each.
    pub kwp88: Vec<(u8, Vec<u8>)>,
    /// Fault codes: (code, UDS failure type, status byte in this module's dialect).
    pub dtcs: Vec<(Dtc, u8, u8)>,
    /// Data: UDS DIDs or KWP local IDs (as u16).
    pub data: Vec<(u16, DataFn)>,
    tx: Option<(Sender, u64)>,
}

impl Module {
    pub fn new(name: &str, request_id: CanId, reply_id: CanId, ext_addr: Option<u8>, dialect: Dialect) -> Self {
        Module {
            name: name.into(),
            request_id,
            reply_id,
            ext_addr,
            dialect,
            identity: vec![],
            kwp88: vec![],
            dtcs: vec![],
            data: vec![],
            tx: None,
        }
    }

    fn frame_len(&self) -> usize {
        if self.ext_addr.is_some() { 7 } else { 8 }
    }

    /// The ISO-TP payload of a frame meant for this module, or `None` if it is not for it.
    pub fn payload_of<'a>(&self, frame: &'a Frame) -> Option<&'a [u8]> {
        if frame.id != self.request_id {
            return None;
        }
        let data = frame.data();
        match self.ext_addr {
            None => Some(data),
            Some(a) => (data.first() == Some(&a)).then(|| &data[1..]),
        }
    }

    fn wrap(&self, payload: &[u8]) -> Option<Frame> {
        let mut d = Vec::with_capacity(8);
        if let Some(a) = self.ext_addr {
            d.push(a);
        }
        d.extend_from_slice(payload);
        Frame::new(self.reply_id, &d).ok()
    }

    /// Handles a frame for this module; returns frames to put on the bus at `now_ms + delay`.
    pub fn receive(&mut self, payload: &[u8], now_ms: u64) -> Vec<(u64, Frame)> {
        match isotp::kind_of(payload) {
            Some(isotp::FrameKind::FlowControl) => {
                if let Some((sender, at)) = self.tx.as_mut() {
                    if sender.on_flow_control(payload, now_ms).is_some() {
                        self.tx = None;
                    } else {
                        *at = now_ms;
                    }
                }
                vec![]
            }
            Some(isotp::FrameKind::Single) => {
                let len = usize::from(payload[0] & 0x0F);
                if len == 0 || len >= self.frame_len() || payload.len() < 1 + len {
                    return vec![];
                }
                let request = payload[1..1 + len].to_vec();
                let Some(reply) = self.answer(&request, now_ms) else { return vec![] };
                let Ok(mut sender) = Sender::with_frame_len(&reply, Some(PADDING), self.frame_len()) else {
                    return vec![];
                };
                let at = now_ms + 3;
                if sender.is_single_frame() {
                    if let SendStep::Send(p) = sender.step(at)
                        && let Some(f) = self.wrap(&p)
                    {
                        return vec![(at, f)];
                    }
                    vec![]
                } else {
                    self.tx = Some((sender, at));
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    /// Frames of a multi-frame reply due by `now_ms`.
    pub fn poll(&mut self, now_ms: u64) -> Vec<(u64, Frame)> {
        let mut out = Vec::new();
        while let Some((sender, at)) = self.tx.as_mut() {
            let t = *at;
            if t > now_ms {
                break;
            }
            match sender.step(t) {
                SendStep::Send(p) => {
                    *at = t + 1;
                    let mut d = Vec::with_capacity(8);
                    if let Some(a) = self.ext_addr {
                        d.push(a);
                    }
                    d.extend_from_slice(&p);
                    if let Ok(f) = Frame::new(self.reply_id, &d) {
                        out.push((t, f));
                    }
                }
                SendStep::Wait => *at = if sender.is_waiting_for_flow_control() { u64::MAX } else { t + 1 },
                SendStep::Done | SendStep::Failed(_) => self.tx = None,
            }
        }
        out
    }

    /// When a multi-frame reply next needs attention.
    pub fn next_due(&self) -> Option<u64> {
        self.tx.as_ref().map(|(_, at)| *at).filter(|t| *t != u64::MAX)
    }

    fn answer(&mut self, req: &[u8], now_ms: u64) -> Option<Vec<u8>> {
        let service = req[0];
        let refuse = |code: u8| Some(vec![0x7F, service, code]);
        match (self.dialect, service) {
            (_, 0x3E) => match (self.dialect, req.get(1).copied()) {
                (_, Some(s)) if s & 0x80 != 0 => None, // "no reply wanted"
                (Dialect::Uds, Some(0x00)) => Some(vec![0x7E, 0x00]),
                (Dialect::Uds, _) => refuse(0x12),
                (Dialect::Kwp, None) => Some(vec![0x7E]),
                (Dialect::Kwp, Some(0x01)) => Some(vec![0x7E, 0x01]),
                (Dialect::Kwp, Some(_)) => refuse(0x12),
            },
            (Dialect::Uds, 0x10) => match req.get(1).map(|s| s & 0x7F) {
                Some(0x01) => Some(vec![0x50, 0x01, 0x00, 0x32, 0x01, 0xF4]),
                _ => refuse(0x12),
            },
            (Dialect::Uds, 0x22) => {
                if req.len() != 3 {
                    return refuse(0x13);
                }
                let did = u16::from_be_bytes([req[1], req[2]]);
                let value = self
                    .identity
                    .iter()
                    .find(|(d, _)| *d == did)
                    .map(|(_, v)| v.clone())
                    .or_else(|| self.data.iter().find(|(d, _)| *d == did).map(|(_, f)| f(now_ms)));
                match value {
                    Some(v) => Some([&[0x62, req[1], req[2]][..], &v].concat()),
                    None => refuse(0x31),
                }
            }
            (Dialect::Uds, 0x19) => match (req.get(1).copied(), req.get(2).copied()) {
                (Some(0x01), Some(mask)) => {
                    let n = self.dtcs.iter().filter(|(_, _, st)| st & mask != 0).count() as u16;
                    Some([&[0x59, 0x01, 0xFF, 0x01][..], &n.to_be_bytes()].concat())
                }
                (Some(sub @ (0x02 | 0x0A)), mask) => {
                    let mask = if sub == 0x0A { 0xFF } else { mask.unwrap_or(0xFF) };
                    let mut m = vec![0x59, sub, 0xFF];
                    for (dtc, ft, st) in &self.dtcs {
                        if sub == 0x0A || st & mask != 0 {
                            m.extend_from_slice(&dtc.0.to_be_bytes());
                            m.extend_from_slice(&[*ft, *st]);
                        }
                    }
                    Some(m)
                }
                _ => refuse(0x12),
            },
            (Dialect::Uds, 0x14) => {
                self.dtcs.clear();
                Some(vec![0x54])
            }
            (Dialect::Kwp, 0x1A) => {
                let opt = *req.get(1)?;
                if opt == 0x88 {
                    let sub = *req.get(2)?;
                    if sub == 0x00 {
                        let mut m = vec![0x5A, 0x88, 0x00];
                        m.extend(self.kwp88.iter().map(|(s, _)| *s));
                        return Some(m);
                    }
                    return match self.kwp88.iter().find(|(s, _)| *s == sub) {
                        Some((_, v)) => Some([&[0x5A, 0x88, sub][..], v].concat()),
                        None => refuse(0x31),
                    };
                }
                match self.identity.iter().find(|(d, _)| *d == u16::from(opt)) {
                    Some((_, v)) => Some([&[0x5A, opt][..], v].concat()),
                    None => refuse(0x12),
                }
            }
            (Dialect::Kwp, 0x21) => {
                let lid = *req.get(1)?;
                match self.data.iter().find(|(d, _)| *d == u16::from(lid)) {
                    Some((_, f)) => Some([&[0x61, lid][..], &f(now_ms)].concat()),
                    None => refuse(0x31),
                }
            }
            (Dialect::Kwp, 0x18) => {
                if req.len() < 4 {
                    return refuse(0x13);
                }
                let mut m = vec![0x58, self.dtcs.len() as u8];
                for (dtc, _, st) in &self.dtcs {
                    m.extend_from_slice(&dtc.0.to_be_bytes());
                    m.push(*st);
                }
                Some(m)
            }
            (Dialect::Kwp, 0x14) => {
                self.dtcs.clear();
                Some(vec![0x54, 0xFF, 0x00])
            }
            _ => refuse(0x11),
        }
    }
}

/// A slow wave between `lo` and `hi`, period in seconds.
fn wave(t_ms: u64, lo: f64, hi: f64, period_s: f64) -> f64 {
    let x = (t_ms as f64 / 1000.0 / period_s * std::f64::consts::TAU).sin();
    lo + (hi - lo) * (x + 1.0) / 2.0
}

/// True for `on_s` seconds out of every `every_s`, starting at `offset_s`.
fn duty(t_ms: u64, every_s: u64, on_s: u64, offset_s: u64) -> bool {
    ((t_ms / 1000 + every_s - offset_s % every_s) % every_s) < on_s
}

fn bit(on: bool, n: u8) -> u8 {
    if on { 1 << n } else { 0 }
}

fn ascii(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

// Data generators for the demo modules. Byte layouts are invented.

fn abs_wheel_speeds(_t: u64) -> Vec<u8> {
    // Parked: four wheels at 0 km/h (km/h x 2, one byte each).
    vec![0, 0, 0, 0]
}

fn abs_switches(t: u64) -> Vec<u8> {
    // Bit 0: brake pedal (pressed 4 s of every 15 s). Bit 3: parking brake on.
    vec![bit(duty(t, 15, 4, 5), 0) | bit(true, 3), 0x00]
}

fn abs_voltage(t: u64) -> Vec<u8> {
    vec![(wave(t, 14.1, 14.3, 9.0) * 10.0).round() as u8]
}

fn abs_pressure(t: u64) -> Vec<u8> {
    // Master cylinder pressure, rises when the pedal is pressed.
    vec![if duty(t, 15, 4, 5) { 62 } else { 0 }]
}

fn srs_buckles(t: u64) -> Vec<u8> {
    // Bit 0: driver's belt buckled (unbuckled 5 s of every 40 s). Bit 1: passenger seat occupied.
    vec![bit(!duty(t, 40, 5, 20), 0)]
}

fn meter_odometer(t: u64) -> Vec<u8> {
    let km = 45_060u32 + (t / 60_000) as u32;
    km.to_be_bytes()[1..].to_vec()
}

fn meter_fuel(t: u64) -> Vec<u8> {
    vec![(wave(t, 61.0, 63.0, 60.0) * 2.55).round() as u8]
}

fn meter_lamps(t: u64) -> Vec<u8> {
    // Bit 0 and 1: turn signals (the left blinks 10 s of every 30 s, 1 Hz). Bit 4: headlamps.
    let blinking = duty(t, 30, 10, 0) && (t / 500).is_multiple_of(2);
    vec![bit(blinking, 0) | bit(duty(t, 60, 20, 30), 4)]
}

fn meter_dimmer(_t: u64) -> Vec<u8> {
    vec![0xC0]
}

fn body_switches(t: u64) -> Vec<u8> {
    // Byte 0: bit 0 driver door open (8 s of every 45 s), bit 1 passenger door.
    // Byte 1: bit 2 headlamp switch (20 s of every 60 s, as the meter shows), bit 5 tail lamps.
    let head = duty(t, 60, 20, 30);
    vec![bit(duty(t, 45, 8, 12), 0), bit(head, 2) | bit(head, 5), 0x00]
}

fn body_voltage(t: u64) -> Vec<u8> {
    vec![(wave(t, 14.0, 14.3, 11.0) * 10.0).round() as u8]
}

fn body_key(_t: u64) -> Vec<u8> {
    // 0 off, 1 accessory, 2 on, 3 start: on.
    vec![0x02]
}

fn ac_temps(t: u64) -> Vec<u8> {
    // Cabin and evaporator temperature, °C + 40.
    vec![(wave(t, 23.5, 24.5, 120.0) + 40.0).round() as u8, (wave(t, 3.0, 5.0, 25.0) + 40.0).round() as u8]
}

fn ac_blower(_t: u64) -> Vec<u8> {
    vec![0x02]
}

fn ecm_counter(t: u64) -> Vec<u8> {
    // An enhanced engine value: idle air control step, moves with the idle.
    vec![(wave(t, 28.0, 34.0, 7.0)).round() as u8, 0x00]
}

fn ecm_fixed(_t: u64) -> Vec<u8> {
    vec![0x01, 0x10]
}

/// The demo car's modules, healthy. See the module comment: the layout is invented.
pub fn hijet_demo_modules() -> Vec<Module> {
    let std = CanId::Standard;
    let mut ecm = Module::new("Engine computer, enhanced (simulated)", std(0x7E0), std(0x7E8), None, Dialect::Kwp);
    ecm.kwp88 = vec![(0x01, ascii("89661-SIM01     "))];
    ecm.data = vec![(0x01, ecm_counter), (0x02, ecm_fixed)];

    let mut abs = Module::new("Brake control, ABS (simulated)", std(0x7B0), std(0x7B8), None, Dialect::Kwp);
    abs.kwp88 = vec![(0x01, ascii("89541-SIM01     "))];
    abs.identity = vec![(0x97, ascii("SIM ABS"))];
    abs.data = vec![(0x01, abs_wheel_speeds), (0x02, abs_switches), (0x03, abs_voltage), (0x41, abs_pressure)];

    let mut srs = Module::new("Airbag, SRS (simulated)", std(0x780), std(0x788), None, Dialect::Uds);
    srs.identity = vec![
        (0xF197, ascii("SRS AIRBAG SIM")),
        (0xF187, ascii("89170-SIM01")),
        (0xF18A, ascii("SIMULATED SUPPLIER")),
        (0xF189, ascii("01.02")),
    ];
    srs.data = vec![(0x0101, srs_buckles)];

    let mut meter = Module::new("Instrument cluster (simulated)", std(0x7C0), std(0x7C8), None, Dialect::Uds);
    meter.identity = vec![(0xF197, ascii("COMBINATION METER SIM")), (0xF187, ascii("83800-SIM01"))];
    meter.data = vec![(0x1001, meter_odometer), (0x1002, meter_fuel), (0x1003, meter_lamps), (0x1004, meter_dimmer)];

    let mut ac = Module::new("Air conditioning (simulated)", std(0x7C4), std(0x7CC), None, Dialect::Uds);
    ac.identity = vec![(0xF197, ascii("AIR CONDITIONER SIM")), (0xF187, ascii("88650-SIM01"))];
    ac.data = vec![(0x0201, ac_temps), (0x0202, ac_blower)];

    let mut body = Module::new("Body control (simulated)", std(0x750), std(0x758), Some(0x40), Dialect::Kwp);
    body.kwp88 = vec![(0x81, ascii("89221-SIM01     "))];
    body.data = vec![(0x01, body_switches), (0x02, body_voltage), (0x03, body_key)];

    vec![ecm, abs, srs, meter, ac, body]
}

/// The demo modules with faults: the brake computer and the cluster both remember losing
/// contact with the engine computer (U0100), as often happens after battery work.
pub fn hijet_demo_modules_with_faults() -> Vec<Module> {
    let mut m = hijet_demo_modules();
    for module in &mut m {
        if module.request_id == CanId::Standard(0x7B0) {
            // KWP status: stored, not present now (bits 5-6 = 01).
            module.dtcs = vec![(Dtc(0xC100), 0x00, 0x20)];
        }
        if module.request_id == CanId::Standard(0x7C0) {
            // UDS status 0x28: confirmed, failed since clear, not failing now. Failure type 87: message missing.
            module.dtcs = vec![(Dtc(0xC100), 0x87, 0x28)];
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: u16, data: &[u8]) -> Frame {
        Frame::new(CanId::Standard(id), data).unwrap()
    }

    fn ask(m: &mut Module, frame: Frame, t: u64) -> Vec<Frame> {
        let Some(p) = m.payload_of(&frame) else { return vec![] };
        let p = p.to_vec();
        m.receive(&p, t).into_iter().map(|(_, f)| f).collect()
    }

    #[test]
    fn uds_module_answers_reads_and_refuses_the_rest() {
        let mut mods = hijet_demo_modules();
        let srs = mods.iter_mut().find(|m| m.request_id == CanId::Standard(0x780)).unwrap();
        let r = ask(srs, f(0x780, &[0x02, 0x3E, 0x00]), 0);
        assert_eq!(r[0].id, CanId::Standard(0x788));
        assert_eq!(&r[0].data()[..3], &[0x02, 0x7E, 0x00]);
        let r = ask(srs, f(0x780, &[0x03, 0x22, 0x01, 0x01]), 10);
        assert_eq!(&r[0].data()[..4], &[0x04, 0x62, 0x01, 0x01]);
        let r = ask(srs, f(0x780, &[0x03, 0x22, 0x12, 0x34]), 20);
        assert_eq!(&r[0].data()[..4], &[0x03, 0x7F, 0x22, 0x31]);
        let r = ask(srs, f(0x780, &[0x02, 0x21, 0x01]), 30);
        assert_eq!(&r[0].data()[..4], &[0x03, 0x7F, 0x21, 0x11]);
        // A long identity string comes as a first frame.
        let r = ask(srs, f(0x780, &[0x03, 0x22, 0xF1, 0x8A]), 40);
        assert!(r.is_empty());
        let r: Vec<Frame> = srs.poll(50).into_iter().map(|(_, f)| f).collect();
        assert_eq!(r[0].data()[0], 0x10);
    }

    #[test]
    fn kwp_module_behind_the_gateway_uses_its_address_byte() {
        let mut mods = hijet_demo_modules();
        let body = mods.iter_mut().find(|m| m.ext_addr == Some(0x40)).unwrap();
        // Not for it: another address byte.
        assert!(ask(body, f(0x750, &[0x41, 0x02, 0x3E, 0x00]), 0).is_empty());
        // 3E 00 is refused by KWP, which still shows it is there.
        let r = ask(body, f(0x750, &[0x40, 0x02, 0x3E, 0x00]), 0);
        assert_eq!(r[0].id, CanId::Standard(0x758));
        assert_eq!(&r[0].data()[..5], &[0x40, 0x03, 0x7F, 0x3E, 0x12]);
        let r = ask(body, f(0x750, &[0x40, 0x02, 0x21, 0x02]), 10);
        assert_eq!(&r[0].data()[..4], &[0x40, 0x03, 0x61, 0x02]);
        let r = ask(body, f(0x750, &[0x40, 0x03, 0x1A, 0x88, 0x00]), 20);
        assert_eq!(&r[0].data()[..6], &[0x40, 0x04, 0x5A, 0x88, 0x00, 0x81]);
    }

    #[test]
    fn faulty_demo_modules_report_u0100() {
        let mut mods = hijet_demo_modules_with_faults();
        let meter = mods.iter_mut().find(|m| m.request_id == CanId::Standard(0x7C0)).unwrap();
        let r = ask(meter, f(0x7C0, &[0x03, 0x19, 0x02, 0xFF]), 0);
        assert_eq!(&r[0].data()[..8], &[0x07, 0x59, 0x02, 0xFF, 0xC1, 0x00, 0x87, 0x28]);
        let abs = mods.iter_mut().find(|m| m.request_id == CanId::Standard(0x7B0)).unwrap();
        let r = ask(abs, f(0x7B0, &[0x04, 0x18, 0x00, 0xFF, 0x00]), 0);
        assert_eq!(&r[0].data()[..6], &[0x05, 0x58, 0x01, 0xC1, 0x00, 0x20]);
    }

    #[test]
    fn values_change_over_time() {
        assert_ne!(abs_switches(5_000), abs_switches(10_000));
        assert_ne!(body_switches(12_000), body_switches(25_000));
        assert_eq!(body_switches(30_000)[1], 0x24);
    }
}
