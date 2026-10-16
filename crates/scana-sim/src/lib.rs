//! Virtual vehicles: ECUs that answer OBD-II requests the way a real car does, for tests,
//! the demo car and the command line's `--sim`.
//!
//! [`VirtualCar::hijet`] copies the 2024 Hijet Jumbo's scan of 2026-10-10: 500 kbit/s,
//! 11-bit IDs, one engine ECU on 0x7E0/0x7E8, the same 31 live PIDs, readiness monitors and
//! no codes. Values move over time (idle, revs, warming up) so screens and graphs have
//! something to show. Time is passed in as milliseconds; nothing here reads a clock.
//!
//! Beyond OBD-II, the car carries simulated modules ([`modules`]): brakes, airbags, the
//! instrument cluster, body control and air conditioning, answering UDS or KWP2000 reads.

pub mod modules;

use modules::Module;
use scana_proto::isotp::{self, SendStep, Sender};
use scana_proto::{Bitrate, CanId, Dtc, Frame};
use std::collections::VecDeque;

const OBD_FUNCTIONAL_11: CanId = CanId::Standard(0x7DF);
const OBD_FUNCTIONAL_29: CanId = CanId::Extended(0x18DB_33F1);
/// ECUs pad their frames with this byte (a real ECU may use 0x00, 0x55 or 0xAA).
const ECU_PADDING: u8 = 0xAA;

/// Faults to inject, for testing how the tester copes.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// Drop every Nth reply (0 = never).
    pub drop_every: u32,
    /// Extra delay before each reply, in ms.
    pub extra_delay_ms: u64,
    /// Answer service 09 with "busy, reply coming" (7F 09 78) first, then the real reply 300 ms later.
    pub busy_vehicle_info: bool,
}

/// One simulated ECU.
#[derive(Debug)]
pub struct Ecu {
    pub name: String,
    pub request_id: CanId,
    pub reply_id: CanId,
    /// Live-data PIDs it answers (not counting the 00/20/40 support masks).
    pub pids: Vec<u8>,
    pub stored: Vec<Dtc>,
    pub pending: Vec<Dtc>,
    pub permanent: Vec<Dtc>,
    pub vin: Option<String>,
    pub calibration_id: Option<String>,
    pub ecu_name: Option<String>,
    /// The engine runs: rpm and the rest move; clearing codes is refused (7F 04 22).
    pub engine_running: bool,
    /// The freeze frame: the code that stored it, and the moment (ms) whose values it holds.
    pub freeze: Option<(Dtc, u64)>,
    /// After clearing codes the readiness monitors must run again.
    pub monitors_incomplete: bool,
    /// Service 06 results: (MID, TID, unit-and-scaling ID, value, minimum, maximum), raw.
    pub tests: Vec<(u8, u8, u8, u16, u16, u16)>,
    reply_delay_ms: u64,
    /// A multi-frame reply in progress, and when to look at it next.
    tx: Option<(Sender, u64)>,
}

impl Ecu {
    fn new(name: &str, request_id: CanId, reply_id: CanId, pids: Vec<u8>) -> Self {
        Ecu {
            name: name.into(),
            request_id,
            reply_id,
            pids,
            stored: vec![],
            pending: vec![],
            permanent: vec![],
            vin: None,
            calibration_id: None,
            ecu_name: None,
            engine_running: true,
            freeze: None,
            monitors_incomplete: false,
            tests: vec![],
            reply_delay_ms: 2,
            tx: None,
        }
    }

    /// PIDs stored in the freeze frame: the live ones that make sense as a snapshot.
    fn freeze_pids(&self) -> Vec<u8> {
        self.pids.iter().copied().filter(|p| !matches!(p, 0x01 | 0x13 | 0x1C | 0x21 | 0x41 | 0x4D | 0x4E)).collect()
    }

    fn mask_of(list: &[u8], base: u8) -> [u8; 4] {
        let mut m = [0u8; 4];
        for i in 1u16..=32 {
            let pid = u16::from(base) + i;
            let on = if i == 32 {
                list.iter().any(|&p| u16::from(p) > pid)
            } else {
                u8::try_from(pid).is_ok_and(|p| list.contains(&p))
            };
            if on {
                let bit = usize::from(i - 1);
                m[bit / 8] |= 0x80 >> (bit % 8);
            }
        }
        m
    }

    fn supports_mask(&self, base: u8) -> bool {
        base == 0 || self.pids.iter().any(|&p| p > base)
    }

    fn support_mask(&self, base: u8) -> [u8; 4] {
        let mut m = [0u8; 4];
        for i in 1u16..=32 {
            let pid = u16::from(base) + i;
            let on = if i == 32 {
                self.pids.iter().any(|&p| u16::from(p) > pid)
            } else {
                u8::try_from(pid).is_ok_and(|p| self.pids.contains(&p))
            };
            if on {
                let bit = usize::from(i - 1);
                m[bit / 8] |= 0x80 >> (bit % 8);
            }
        }
        m
    }

    /// The data bytes for a live PID at time `t_ms`, or `None` if unsupported.
    pub fn pid_value(&self, pid: u8, t_ms: u64) -> Option<Vec<u8>> {
        if pid.is_multiple_of(0x20) {
            return self.supports_mask(pid).then(|| self.support_mask(pid).to_vec());
        }
        if !self.pids.contains(&pid) {
            return None;
        }
        Some(self.value_at(pid, t_ms, self.engine_running))
    }

    fn value_at(&self, pid: u8, t_ms: u64, running: bool) -> Vec<u8> {
        let e = Engine::at(t_ms, running, self.stored.len());
        let incomplete = if self.monitors_incomplete { 0x65 } else { 0x00 };
        let u8c = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        let u16c = |v: f64| (v.round().clamp(0.0, 65535.0) as u16).to_be_bytes().to_vec();
        let pct = |v: f64| u8c(v * 255.0 / 100.0);
        let trim = |v: f64| u8c(v * 128.0 / 100.0 + 128.0);
        match pid {
            0x01 | 0x41 => vec![(if e.mil { 0x80 } else { 0 }) | (e.codes.min(0x7F) as u8), 0x07, 0x65, incomplete],
            0x03 => vec![if e.closed_loop { 2 } else { 1 }, 0],
            0x04 => vec![pct(e.load)],
            0x05 => vec![u8c(e.coolant + 40.0)],
            0x06 => vec![trim(e.stft)],
            0x07 => vec![trim(1.6)],
            0x0B => vec![u8c(e.map)],
            0x0C => u16c(e.rpm * 4.0),
            0x0D => vec![0],
            0x0E => vec![u8c((e.timing + 64.0) * 2.0)],
            0x0F => vec![u8c(22.0 + 40.0)],
            0x10 => u16c(e.maf * 100.0),
            0x11 => vec![pct(e.throttle)],
            0x13 => vec![0x03],
            0x14 => vec![u8c(e.o2_front * 200.0), trim(e.stft)],
            0x15 => vec![u8c(e.o2_rear * 200.0), 0xFF],
            0x1C => vec![10],
            0x1F => u16c(e.run_s),
            0x21 | 0x4D => vec![0, 0],
            0x2E => vec![pct(if e.closed_loop { 12.0 } else { 0.0 })],
            0x33 => vec![94],
            0x42 => u16c(e.volts * 1000.0),
            0x45 => vec![pct(e.throttle * 0.3)],
            0x47 => vec![pct(e.throttle)],
            0x49 => vec![pct(15.0 + (e.throttle - 18.0) * 0.8)],
            0x4A => vec![pct(7.5 + (e.throttle - 18.0) * 0.4)],
            0x4C => vec![pct((e.throttle - 8.0).max(0.0))],
            0x4E => u16c(45_060.0 + (t_ms / 60_000) as f64),
            // Scaling limits. The real truck's are not known yet: 0 is a guess.
            0x4F | 0x50 => vec![0, 0, 0, 0],
            // Any other PID in the list: a plain number that changes slowly.
            _ => vec![u8c(100.0 + 20.0 * (t_ms as f64 / 4000.0).sin())],
        }
    }
}

/// Service 06 results for the demo car (raw values), a 3-cylinder engine like the KF.
fn demo_tests(failing_catalyst: bool) -> Vec<(u8, u8, u8, u16, u16, u16)> {
    vec![
        // Oxygen sensor B1S1: switch times in ms (0x10), voltages in mV steps of 1 (0x0B).
        (0x01, 0x05, 0x10, 72, 0, 150),
        (0x01, 0x06, 0x10, 84, 0, 150),
        (0x01, 0x07, 0x0B, 58, 0, 300),
        (0x01, 0x08, 0x0B, 812, 600, 1100),
        // Catalyst B1: a maker's ratio (0x20, 1/256 steps). Below the limit is good.
        (0x21, 0x80, 0x20, if failing_catalyst { 192 } else { 64 }, 0, 154),
        // Oxygen sensor heater B1S1: resistance in ohms (0x14).
        (0x41, 0x81, 0x14, 6, 2, 12),
        // Misfire counts (0x24): general, then cylinders 1-3.
        (0xA1, 0x0B, 0x24, 1, 0, 65535),
        (0xA2, 0x0B, 0x24, 0, 0, 65535),
        (0xA2, 0x0C, 0x24, 0, 0, 65535),
        (0xA3, 0x0B, 0x24, 1, 0, 65535),
        (0xA3, 0x0C, 0x24, 2, 0, 65535),
        (0xA4, 0x0B, 0x24, 0, 0, 65535),
        (0xA4, 0x0C, 0x24, 0, 0, 65535),
    ]
}

/// The Hijet's engine at a moment: a deterministic model, no randomness.
struct Engine {
    rpm: f64,
    load: f64,
    coolant: f64,
    map: f64,
    maf: f64,
    timing: f64,
    throttle: f64,
    stft: f64,
    o2_front: f64,
    o2_rear: f64,
    volts: f64,
    run_s: f64,
    closed_loop: bool,
    mil: bool,
    codes: usize,
}

impl Engine {
    fn at(t_ms: u64, running: bool, codes: usize) -> Self {
        let t = t_ms as f64 / 1000.0;
        if !running {
            return Engine {
                rpm: 0.0,
                load: 0.0,
                coolant: 30.0,
                map: 94.0,
                maf: 0.0,
                timing: 0.0,
                throttle: 13.0,
                stft: 0.0,
                o2_front: 0.45,
                o2_rear: 0.45,
                volts: 12.6,
                run_s: 0.0,
                closed_loop: false,
                mil: codes > 0,
                codes,
            };
        }
        // A blip of the throttle every 20 s, lasting 3 s.
        let cycle = t % 20.0;
        let blip =
            if (10.0..13.0).contains(&cycle) { ((cycle - 10.0) / 3.0 * std::f64::consts::PI).sin() } else { 0.0 };
        let warm = 1.0 - (-(t + 52.0) / 300.0).exp();
        let closed_loop = t > 8.0 && blip < 0.2;
        Engine {
            rpm: 850.0 + 40.0 * (t * 0.7).sin() + 1650.0 * blip,
            load: 27.0 + 30.0 * blip,
            coolant: 30.0 + (88.0 - 30.0) * warm,
            map: 33.0 + 45.0 * blip,
            maf: 2.4 + 0.5 * (t * 0.7).sin().abs() + 9.0 * blip,
            timing: 6.0 + 2.0 * (t * 0.5).sin() + 14.0 * blip,
            throttle: 18.0 + 20.0 * blip,
            stft: if closed_loop { 5.0 * (t * 1.3).sin() - 2.0 } else { 0.0 },
            o2_front: if closed_loop { 0.45 + 0.35 * (t * 6.0).sin() } else { 0.25 },
            o2_rear: 0.035 + 0.6 * warm,
            volts: 14.28 + 0.04 * (t * 0.9).sin(),
            run_s: t + 52.0,
            closed_loop,
            mil: codes > 0,
            codes,
        }
    }
}

/// A dash display sharing the OBD port (for example through a splitter): it polls 0x7DF.
#[derive(Debug)]
struct OtherTester {
    next_ms: u64,
    index: usize,
}

impl OtherTester {
    /// rpm often, the rest less often (like the dash firmware).
    const ORDER: [u8; 8] = [0x0C, 0x0D, 0x0C, 0x05, 0x0C, 0x10, 0x0C, 0x42];
    const PERIOD_MS: u64 = 50;
}

/// A simulated car: one CAN bus with ECUs on it.
#[derive(Debug)]
pub struct VirtualCar {
    pub bitrate: Bitrate,
    pub extended_ids: bool,
    pub ecus: Vec<Ecu>,
    /// Modules that answer enhanced (UDS or KWP) requests.
    pub modules: Vec<Module>,
    pub faults: Faults,
    other_tester: Option<OtherTester>,
    /// Frames waiting to appear on the bus: (time, frame), in time order.
    out: VecDeque<(u64, Frame)>,
    replies: u32,
}

impl VirtualCar {
    /// The 2024 Hijet Jumbo, from its scan of 2026-10-10 and its PID list from the drives of
    /// 2026-10-16.
    pub fn hijet() -> Self {
        let pids = vec![
            0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x13, 0x14, 0x15, 0x1C, 0x1F,
            0x21, 0x2E, 0x33, 0x42, 0x45, 0x47, 0x49, 0x4A, 0x4C, 0x4D, 0x4E, 0x4F, 0x50,
        ];
        let mut ecm = Ecu::new("Engine (simulated Hijet KF)", CanId::Standard(0x7E0), CanId::Standard(0x7E8), pids);
        // A made-up VIN, clearly not a real one. Whether the real Hijet reports a VIN is unknown.
        ecm.vin = Some("SCANASIMHIJET0001".into());
        ecm.calibration_id = Some("SIMCAL-KF-000001".into());
        ecm.ecu_name = Some("ECM-EngineControl".into());
        // Service 06 results, invented for the demo: whether the real Hijet reports them is unknown.
        ecm.tests = demo_tests(false);
        VirtualCar {
            bitrate: Bitrate::K500,
            extended_ids: false,
            ecus: vec![ecm],
            modules: modules::hijet_demo_modules(),
            faults: Faults::default(),
            other_tester: None,
            out: VecDeque::new(),
            replies: 0,
        }
    }

    /// The demo car with faults: the Hijet with a stored code (P0128, thermostat), a pending
    /// one (P0171, running lean), the MIL on, a freeze frame and a failing catalyst test.
    pub fn hijet_with_faults() -> Self {
        let mut car = VirtualCar::hijet();
        let e = &mut car.ecus[0];
        e.stored = vec![Dtc(0x0128)];
        e.pending = vec![Dtc(0x0171)];
        // Stored mid-blip, so the freeze frame shows revs above idle.
        e.freeze = Some((Dtc(0x0128), 11_500));
        e.tests = demo_tests(true);
        car.modules = modules::hijet_demo_modules_with_faults();
        car
    }

    /// The car without its enhanced modules (OBD-II only).
    pub fn without_modules(mut self) -> Self {
        self.modules.clear();
        self
    }

    /// A car with two OBD ECUs (engine 7E8, gearbox 7E9), with one stored and one pending code.
    pub fn two_ecus() -> Self {
        let mut car = VirtualCar::hijet();
        car.ecus[0].stored = vec![Dtc(0x0133)];
        car.ecus[0].pending = vec![Dtc(0x0171)];
        car.ecus[0].freeze = Some((Dtc(0x0133), 11_500));
        let mut tcm = Ecu::new(
            "Gearbox (simulated)",
            CanId::Standard(0x7E1),
            CanId::Standard(0x7E9),
            vec![0x01, 0x05, 0x0D, 0x42],
        );
        tcm.reply_delay_ms = 4;
        car.ecus.push(tcm);
        car
    }

    /// The same car on 29-bit IDs (ECU n: request 18DA(10+n)F1, reply 18DAF1(10+n)). Its
    /// modules move to 29-bit too (module n at 18DA(20+n)F1), without address bytes; the
    /// engine computer's enhanced side is dropped.
    pub fn extended(mut self) -> Self {
        self.extended_ids = true;
        for (i, e) in self.ecus.iter_mut().enumerate() {
            let addr = 0x10 + i as u32;
            e.request_id = CanId::Extended(0x18DA_00F1 | (addr << 8));
            e.reply_id = CanId::Extended(0x18DA_F100 | addr);
        }
        self.modules.retain(|m| m.request_id != CanId::Standard(0x7E0));
        for (i, m) in self.modules.iter_mut().enumerate() {
            let addr = 0x20 + i as u32;
            m.request_id = CanId::Extended(0x18DA_00F1 | (addr << 8));
            m.reply_id = CanId::Extended(0x18DA_F100 | addr);
            m.ext_addr = None;
        }
        self
    }

    pub fn at_bitrate(mut self, b: Bitrate) -> Self {
        self.bitrate = b;
        self
    }

    /// Adds a dash display polling on 0x7DF, sharing the port with Scana.
    pub fn with_dash(mut self) -> Self {
        self.other_tester = Some(OtherTester { next_ms: 0, index: 0 });
        self
    }

    pub fn set_engine_running(&mut self, on: bool) {
        for e in &mut self.ecus {
            e.engine_running = on;
        }
    }

    /// A frame put on the bus by a tester (at the car's bit rate).
    pub fn receive(&mut self, frame: &Frame, now_ms: u64) {
        let functional = if self.extended_ids { OBD_FUNCTIONAL_29 } else { OBD_FUNCTIONAL_11 };
        let data = frame.data();
        // A single-frame request's service byte, if this is one.
        let service = match isotp::kind_of(data) {
            Some(isotp::FrameKind::Single) => data.get(1).copied(),
            _ => None,
        };
        // Enhanced modules. Where one shares its ID with an OBD ECU (the engine computer's
        // enhanced side), OBD services 01-0A go to the ECU and the rest to the module.
        for m in 0..self.modules.len() {
            let Some(payload) = self.modules[m].payload_of(frame) else { continue };
            let payload = payload.to_vec();
            let shared = self.modules[m].ext_addr.is_none()
                && self.ecus.iter().any(|e| e.request_id == self.modules[m].request_id);
            if shared && service.is_some_and(|s| s <= 0x0A) {
                continue;
            }
            for (at, f) in self.modules[m].receive(&payload, now_ms) {
                self.push_out(at + self.faults.extra_delay_ms, f);
            }
        }
        for i in 0..self.ecus.len() {
            let to_me = frame.id == self.ecus[i].request_id;
            if frame.id != functional && !to_me {
                continue;
            }
            let enhanced_here =
                self.modules.iter().any(|m| m.ext_addr.is_none() && m.request_id == self.ecus[i].request_id);
            if to_me && enhanced_here && service.is_some_and(|s| s > 0x0A) {
                continue;
            }
            match isotp::kind_of(data) {
                Some(isotp::FrameKind::FlowControl) if to_me => {
                    if let Some((sender, at)) = self.ecus[i].tx.as_mut() {
                        if sender.on_flow_control(data, now_ms).is_some() {
                            self.ecus[i].tx = None;
                        } else {
                            *at = now_ms;
                        }
                    }
                }
                Some(isotp::FrameKind::Single) => {
                    let len = usize::from(data[0] & 0x0F);
                    if len == 0 || len > 7 || data.len() < 1 + len {
                        continue;
                    }
                    let request = data[1..1 + len].to_vec();
                    for (delay, msg) in self.answer(i, &request, frame.id == functional, now_ms) {
                        self.queue_reply(i, &msg, now_ms + delay);
                    }
                }
                _ => {}
            }
        }
    }

    fn queue_reply(&mut self, ecu: usize, msg: &[u8], at_ms: u64) {
        self.replies += 1;
        if self.faults.drop_every > 0 && self.replies.is_multiple_of(self.faults.drop_every) {
            return;
        }
        let at = at_ms + self.ecus[ecu].reply_delay_ms + self.faults.extra_delay_ms;
        let Ok(mut sender) = Sender::new(msg, Some(ECU_PADDING)) else { return };
        if sender.is_single_frame() {
            if let SendStep::Send(payload) = sender.step(at)
                && let Ok(f) = Frame::new(self.ecus[ecu].reply_id, &payload)
            {
                self.push_out(at, f);
            }
        } else {
            self.ecus[ecu].tx = Some((sender, at));
        }
    }

    fn push_out(&mut self, at: u64, f: Frame) {
        let pos = self.out.iter().position(|(t, _)| *t > at).unwrap_or(self.out.len());
        self.out.insert(pos, (at, f));
    }

    /// Answers a request: a list of (extra delay, reply message).
    fn answer(&mut self, i: usize, req: &[u8], functional: bool, now_ms: u64) -> Vec<(u64, Vec<u8>)> {
        let busy_vehicle_info = self.faults.busy_vehicle_info;
        let ecu = &mut self.ecus[i];
        let service = req[0];
        let silent_or = |code: u8| if functional { vec![] } else { vec![(0, vec![0x7F, service, code])] };
        match service {
            0x01 => {
                if req.len() < 2 {
                    return silent_or(0x13);
                }
                let mut msg = vec![0x41];
                for &pid in &req[1..] {
                    if let Some(v) = ecu.pid_value(pid, now_ms) {
                        msg.push(pid);
                        msg.extend(v);
                    }
                }
                if msg.len() == 1 { silent_or(0x12) } else { vec![(0, msg)] }
            }
            0x02 => {
                let (Some(&pid), frame) = (req.get(1), req.get(2).copied().unwrap_or(0)) else {
                    return silent_or(0x13);
                };
                if frame != 0 {
                    return silent_or(0x12);
                }
                match (ecu.freeze, pid) {
                    (None, 0x02) => vec![(0, vec![0x42, 0x02, 0x00, 0x00, 0x00])],
                    (Some((dtc, _)), 0x02) => vec![(0, [&[0x42, 0x02, 0x00][..], &dtc.0.to_be_bytes()].concat())],
                    (Some(_), p) if p.is_multiple_of(0x20) => {
                        let list = ecu.freeze_pids();
                        if p == 0 || list.iter().any(|&x| x > p) {
                            let mut with_02 = list.clone();
                            with_02.push(0x02);
                            vec![(0, [&[0x42, p, 0x00][..], &Ecu::mask_of(&with_02, p)].concat())]
                        } else {
                            silent_or(0x12)
                        }
                    }
                    (Some((_, t)), p) if ecu.freeze_pids().contains(&p) => {
                        vec![(0, [&[0x42, p, 0x00][..], &ecu.value_at(p, t, true)].concat())]
                    }
                    _ => silent_or(0x12),
                }
            }
            0x06 => {
                let Some(&mid) = req.get(1) else { return silent_or(0x13) };
                let mids: Vec<u8> = ecu.tests.iter().map(|t| t.0).collect();
                if ecu.tests.is_empty() {
                    return silent_or(0x11);
                }
                if mid.is_multiple_of(0x20) {
                    if mid == 0 || mids.iter().any(|&m| m > mid) {
                        return vec![(0, [&[0x46, mid][..], &Ecu::mask_of(&mids, mid)].concat())];
                    }
                    return silent_or(0x12);
                }
                let mut msg = vec![0x46];
                for &(m, tid, uas, v, lo, hi) in ecu.tests.iter().filter(|t| t.0 == mid) {
                    msg.extend_from_slice(&[m, tid, uas]);
                    for x in [v, lo, hi] {
                        msg.extend_from_slice(&x.to_be_bytes());
                    }
                }
                if msg.len() == 1 { silent_or(0x12) } else { vec![(0, msg)] }
            }
            0x03 | 0x07 | 0x0A => {
                let codes = match service {
                    0x03 => &ecu.stored,
                    0x07 => &ecu.pending,
                    _ => &ecu.permanent,
                };
                let mut msg = vec![service + 0x40, codes.len() as u8];
                for d in codes {
                    msg.extend_from_slice(&d.0.to_be_bytes());
                }
                vec![(0, msg)]
            }
            0x04 => {
                if ecu.engine_running {
                    return vec![(0, vec![0x7F, 0x04, 0x22])];
                }
                // Clearing also erases the freeze frame and resets the readiness monitors.
                ecu.stored.clear();
                ecu.pending.clear();
                ecu.freeze = None;
                ecu.monitors_incomplete = true;
                vec![(0, vec![0x44])]
            }
            0x09 => {
                let item = req.get(1).copied().unwrap_or(0xFF);
                let text = |s: &Option<String>, len: usize| {
                    s.as_ref().map(|v| {
                        let mut m = vec![0x49, item, 0x01];
                        let mut b = v.as_bytes().to_vec();
                        b.resize(len, 0);
                        m.extend(b);
                        m
                    })
                };
                let msg = match item {
                    0x00 => {
                        let mut mask = [0u8; 4];
                        for (have, n) in [
                            (ecu.vin.is_some(), 2usize),
                            (ecu.calibration_id.is_some(), 4),
                            (ecu.ecu_name.is_some(), 10),
                        ] {
                            if have {
                                mask[(n - 1) / 8] |= 0x80 >> ((n - 1) % 8);
                            }
                        }
                        Some([&[0x49, 0x00][..], &mask].concat())
                    }
                    0x02 => text(&ecu.vin, 17),
                    0x04 => text(&ecu.calibration_id, 16),
                    0x0A => text(&ecu.ecu_name, 20),
                    _ => None,
                };
                match msg {
                    Some(m) if busy_vehicle_info => vec![(0, vec![0x7F, 0x09, 0x78]), (300, m)],
                    Some(m) => vec![(0, m)],
                    None => silent_or(0x12),
                }
            }
            _ => silent_or(0x11),
        }
    }

    /// The time of the next thing due on the bus, if any.
    pub fn next_due(&self) -> Option<u64> {
        let mut t = self.out.front().map(|(t, _)| *t);
        for e in &self.ecus {
            if let Some((_, at)) = &e.tx
                && *at != u64::MAX
            {
                t = Some(t.map_or(*at, |x| x.min(*at)));
            }
        }
        if let Some(o) = &self.other_tester {
            t = Some(t.map_or(o.next_ms, |x| x.min(o.next_ms)));
        }
        for m in &self.modules {
            if let Some(at) = m.next_due() {
                t = Some(t.map_or(at, |x| x.min(at)));
            }
        }
        t
    }

    /// Frames that appear on the bus up to `now_ms`, in order.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Frame> {
        // The dash's requests: the ECUs see and answer them too.
        while let Some(o) = self.other_tester.as_mut() {
            if o.next_ms > now_ms {
                break;
            }
            let pid = OtherTester::ORDER[o.index % OtherTester::ORDER.len()];
            let at = o.next_ms;
            o.index += 1;
            o.next_ms += OtherTester::PERIOD_MS;
            if let Ok(req) = Frame::new(OBD_FUNCTIONAL_11, &[0x02, 0x01, pid, 0x55, 0x55, 0x55, 0x55, 0x55]) {
                self.push_out(at, req);
                if !self.extended_ids {
                    self.receive(&req, at);
                }
            }
        }
        // Multi-frame replies in progress.
        for i in 0..self.ecus.len() {
            while let Some((sender, at)) = self.ecus[i].tx.as_mut() {
                let t = *at;
                if t > now_ms {
                    break;
                }
                match sender.step(t) {
                    SendStep::Send(p) => {
                        *at = t + 1;
                        if let Ok(f) = Frame::new(self.ecus[i].reply_id, &p) {
                            self.push_out(t, f);
                        }
                    }
                    // Waiting for flow control (until it arrives) or for the gap (1 ms).
                    SendStep::Wait => *at = if sender.is_waiting_for_flow_control() { u64::MAX } else { t + 1 },
                    SendStep::Done | SendStep::Failed(_) => self.ecus[i].tx = None,
                }
            }
        }
        for m in 0..self.modules.len() {
            for (at, f) in self.modules[m].poll(now_ms) {
                self.push_out(at, f);
            }
        }
        let mut bus = Vec::new();
        while self.out.front().is_some_and(|(t, _)| *t <= now_ms) {
            if let Some((_, f)) = self.out.pop_front() {
                bus.push(f);
            }
        }
        bus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_proto::obd;

    fn req(id: u16, data: &[u8]) -> Frame {
        Frame::new(CanId::Standard(id), data).unwrap()
    }

    #[test]
    fn hijet_answers_rpm_and_lists_31_pids() {
        let mut car = VirtualCar::hijet();
        car.receive(&req(0x7DF, &[0x02, 0x01, 0x0C, 0, 0, 0, 0, 0]), 0);
        assert!(car.poll(1).is_empty());
        let f = car.poll(10);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, CanId::Standard(0x7E8));
        assert_eq!(&f[0].data()[..3], &[0x04, 0x41, 0x0C]);
        let mut supported = vec![];
        for base in [0x00u8, 0x20, 0x40] {
            car.receive(&req(0x7E0, &[0x02, 0x01, base]), 100);
            let f = car.poll(200);
            assert_eq!(f.len(), 1, "mask {base:02X}");
            supported.extend(obd::supported_pids(base, &f[0].data()[3..7]));
        }
        let live: Vec<u8> = supported.into_iter().filter(|p| p % 0x20 != 0).collect();
        assert_eq!(live.len(), 31);
    }

    #[test]
    fn vin_comes_in_three_frames_after_flow_control() {
        let mut car = VirtualCar::hijet();
        car.receive(&req(0x7E0, &[0x02, 0x09, 0x02]), 0);
        let f = car.poll(10);
        assert_eq!(f.len(), 1);
        assert_eq!(&f[0].data()[..2], &[0x10, 20]);
        assert!(car.poll(50).is_empty()); // waits for flow control
        car.receive(&req(0x7E0, &[0x30, 0, 0, 0, 0, 0, 0, 0]), 50);
        let f = car.poll(60);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].data()[0], 0x21);
        assert_eq!(f[1].data()[0], 0x22);
    }

    #[test]
    fn unsupported_requests() {
        let mut car = VirtualCar::hijet();
        car.receive(&req(0x7DF, &[0x02, 0x01, 0x5C]), 0); // oil temperature: not on the Hijet
        assert!(car.poll(10).is_empty()); // functional: silent
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x5C]), 20);
        assert_eq!(car.poll(30)[0].data()[..4], [0x03, 0x7F, 0x01, 0x12]);
        car.receive(&req(0x7E0, &[0x01, 0x22]), 40);
        assert_eq!(car.poll(50)[0].data()[..4], [0x03, 0x7F, 0x22, 0x11]);
    }

    #[test]
    fn the_engine_computer_shares_its_id_with_its_enhanced_side() {
        let mut car = VirtualCar::hijet();
        // An OBD request: answered once, by the OBD side.
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x0C]), 0);
        let f = car.poll(10);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].data()[1], 0x41);
        // An enhanced request: answered once, by the enhanced (KWP) side.
        car.receive(&req(0x7E0, &[0x02, 0x21, 0x01]), 20);
        let f = car.poll(30);
        assert_eq!(f.len(), 1);
        assert_eq!(&f[0].data()[..3], &[0x04, 0x61, 0x01]);
        // Without modules, the OBD side refuses it.
        let mut car = VirtualCar::hijet().without_modules();
        car.receive(&req(0x7E0, &[0x02, 0x21, 0x01]), 0);
        assert_eq!(car.poll(10)[0].data()[..4], [0x03, 0x7F, 0x21, 0x11]);
    }

    #[test]
    fn modules_answer_on_the_bus_with_flow_control() {
        let mut car = VirtualCar::hijet();
        car.receive(&req(0x780, &[0x03, 0x22, 0xF1, 0x8A]), 0);
        let f = car.poll(10);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, CanId::Standard(0x788));
        assert_eq!(f[0].data()[0], 0x10);
        car.receive(&req(0x780, &[0x30, 0, 0]), 20);
        // 62 F1 8A + 18 characters = 21 bytes: a first frame (6) and three consecutive frames.
        let f = car.poll(40);
        assert_eq!(f.len(), 3);
        // 29-bit modules.
        let mut car = VirtualCar::hijet().extended();
        assert!(car.modules.iter().all(|m| m.request_id.is_extended()));
        let id = car.modules[0].request_id;
        car.receive(&Frame::new(id, &[0x02, 0x3E, 0x00]).unwrap(), 0);
        assert_eq!(car.poll(10).len(), 1);
    }

    #[test]
    fn codes_and_clearing() {
        let mut car = VirtualCar::two_ecus();
        car.receive(&req(0x7DF, &[0x01, 0x03]), 0);
        let f = car.poll(10);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].data()[..5], [0x04, 0x43, 0x01, 0x01, 0x33]);
        assert_eq!(f[1].data()[..3], [0x02, 0x43, 0x00]);
        car.receive(&req(0x7E0, &[0x01, 0x04]), 20);
        assert_eq!(car.poll(30)[0].data()[..4], [0x03, 0x7F, 0x04, 0x22]); // engine running
        car.set_engine_running(false);
        car.receive(&req(0x7E0, &[0x01, 0x04]), 40);
        assert_eq!(car.poll(50)[0].data()[..2], [0x01, 0x44]);
        assert!(car.ecus[0].stored.is_empty());
    }

    #[test]
    fn dash_polls_every_50_ms_and_the_ecu_answers() {
        let mut car = VirtualCar::hijet().with_dash();
        let frames = car.poll(1000);
        let requests = frames.iter().filter(|f| f.id == CanId::Standard(0x7DF)).count();
        assert_eq!(requests, 21); // 0, 50 ... 1000 ms
        let mut frames = frames;
        frames.extend(car.poll(1010));
        let replies = frames.iter().filter(|f| f.id == CanId::Standard(0x7E8)).count();
        assert_eq!(replies, 21);
    }

    #[test]
    fn values_move_but_stay_believable() {
        let car = VirtualCar::hijet();
        let e = &car.ecus[0];
        for t in (0..120_000).step_by(250) {
            let v = e.pid_value(0x0C, t).unwrap();
            let rpm = u16::from_be_bytes([v[0], v[1]]) / 4;
            assert!((700..=2600).contains(&rpm), "{rpm} at {t}");
            let coolant = i32::from(e.pid_value(0x05, t).unwrap()[0]) - 40;
            assert!((30..=90).contains(&coolant));
        }
    }

    #[test]
    fn faults_drop_and_delay() {
        let mut car = VirtualCar::hijet();
        car.faults.drop_every = 2;
        car.faults.extra_delay_ms = 20;
        car.receive(&req(0x7DF, &[0x02, 0x01, 0x0C]), 0);
        assert!(car.poll(10).is_empty());
        assert_eq!(car.poll(30).len(), 1);
        car.receive(&req(0x7DF, &[0x02, 0x01, 0x0C]), 100);
        assert!(car.poll(200).is_empty()); // dropped
    }

    #[test]
    fn freeze_frame_and_tests_on_the_faulty_demo() {
        let mut car = VirtualCar::hijet_with_faults();
        car.receive(&req(0x7E0, &[0x03, 0x02, 0x02, 0x00]), 0);
        assert_eq!(car.poll(10)[0].data()[..6], [0x05, 0x42, 0x02, 0x00, 0x01, 0x28]);
        car.receive(&req(0x7E0, &[0x03, 0x02, 0x0C, 0x00]), 20);
        let f = car.poll(30);
        assert_eq!(f[0].data()[..4], [0x05, 0x42, 0x0C, 0x00]);
        let rpm = u16::from_be_bytes([f[0].data()[4], f[0].data()[5]]) / 4;
        assert!(rpm > 1500, "freeze frame rpm {rpm}");
        // Service 06: supported monitors, then the catalyst result (fails).
        car.receive(&req(0x7E0, &[0x02, 0x06, 0x00]), 40);
        let m = car.poll(50)[0].data().to_vec();
        assert_eq!(&m[1..3], &[0x46, 0x00]);
        let mids = scana_proto::obd::supported_pids(0x00, &m[3..7]);
        assert_eq!(mids, vec![0x01, 0x20]);
        // One result is 10 bytes (46 + a 9-byte record), so it comes as a first frame.
        car.receive(&req(0x7E0, &[0x02, 0x06, 0x21]), 60);
        let r = car.poll(70)[0].data().to_vec();
        assert_eq!(&r[..6], &[0x10, 0x0A, 0x46, 0x21, 0x80, 0x20]);
    }

    #[test]
    fn clearing_resets_readiness_and_freeze_frame() {
        let mut car = VirtualCar::hijet_with_faults();
        car.set_engine_running(false);
        car.receive(&req(0x7E0, &[0x01, 0x04]), 0);
        assert_eq!(car.poll(10)[0].data()[..2], [0x01, 0x44]);
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x01]), 20);
        assert_eq!(car.poll(30)[0].data()[..7], [0x06, 0x41, 0x01, 0x00, 0x07, 0x65, 0x65]);
        car.receive(&req(0x7E0, &[0x03, 0x02, 0x02, 0x00]), 40);
        assert_eq!(car.poll(50)[0].data()[..6], [0x05, 0x42, 0x02, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn extended_ids() {
        let mut car = VirtualCar::hijet().extended();
        car.receive(&Frame::new(CanId::Extended(0x18DB_33F1), &[0x02, 0x01, 0x00]).unwrap(), 0);
        let f = car.poll(10);
        assert_eq!(f[0].id, CanId::Extended(0x18DA_F110));
        // 11-bit requests are ignored.
        car.receive(&req(0x7DF, &[0x02, 0x01, 0x00]), 20);
        assert!(car.poll(40).is_empty());
    }
}
