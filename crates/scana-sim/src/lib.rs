//! Virtual vehicles: ECUs that answer OBD-II requests the way a real car does, for tests,
//! the demo car and the command line's `--sim`.
//!
//! [`VirtualCar::hijet`] copies the 2024 Hijet Jumbo's scan of 2026-10-10: 500 kbit/s,
//! 11-bit IDs, one engine ECU on 0x7E0/0x7E8, the same 31 live PIDs, readiness monitors and
//! no codes. Values move over time (idle, revs, warming up) so screens and graphs have
//! something to show. Time is passed in as milliseconds; nothing here reads a clock.
//!
//! [`VirtualCar::corolla_2006`] is a 2006 Toyota Corolla 1.8 (1ZZ-FE) with a worn catalyst
//! and P0420 stored, built for trying the diagnosis guides. It is simulated on CAN; a real
//! car of that age may use the K-line instead (see its doc).
//!
//! Beyond OBD-II, the car carries simulated modules ([`modules`]): brakes, airbags, the
//! instrument cluster, body control and air conditioning, answering UDS or KWP2000 reads.

pub mod modules;

use modules::Module;
use scana_proto::isotp::{self, SendStep, Sender};
use scana_proto::{Bitrate, Bus, CanId, Dtc, Frame};
use std::collections::VecDeque;

const OBD_FUNCTIONAL_11: CanId = CanId::Standard(0x7DF);
const OBD_FUNCTIONAL_29: CanId = CanId::Extended(0x18DB_33F1);
/// ECUs pad their frames with this byte unless told otherwise (a real ECU may use 0x00, 0x55 or 0xAA).
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

/// Which engine an ECU simulates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    /// The Hijet's KF: 658 cc three-cylinder, started cold, a throttle blip every 20 s.
    HijetKf,
    /// A 2006 Corolla's 1ZZ-FE: 1.8 L four-cylinder, already warm. Every 90 s it idles for
    /// 45 s, then holds about 2500 rpm for 45 s, like a helper holding the revs for a test.
    /// Its catalyst is worn, so the rear oxygen sensor swings almost like the front one.
    Corolla1zz,
    /// An invented 2.2-3.2 L turbo-diesel ute of about 2016-2020. See [`VirtualCar::ute_diesel`].
    DieselUte,
    /// An invented 2015-2019 petrol hatch. See [`VirtualCar::hatch_two_ecus`].
    HatchPetrol,
    /// An invented 2019-2023 petrol hybrid whose engine stops by itself.
    /// See [`VirtualCar::hybrid`].
    Hybrid,
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
    pub engine: EngineKind,
    /// Service 0A (permanent codes) arrived around 2010: older ECUs refuse it.
    pub permanent_supported: bool,
    /// The byte this ECU pads its frames with.
    pub padding: u8,
    /// PID 01 reports real readiness monitors. A gearbox computer's PID 01 reports none (zeros).
    pub monitors_reported: bool,
    /// Freeze frame values that differ from the live model (a code set while driving).
    pub freeze_overrides: Vec<(u8, Vec<u8>)>,
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
            engine: EngineKind::HijetKf,
            permanent_supported: true,
            padding: ECU_PADDING,
            monitors_reported: true,
            freeze_overrides: vec![],
            reply_delay_ms: 2,
            tx: None,
        }
    }

    /// Readiness bytes B, C and D of PID 01 for the newer invented cars (the Hijet and Corolla
    /// keep their original fixed values). After clearing codes every monitor shows not complete.
    /// Bit layout: Wikipedia "OBD-II PIDs", PID 01, read 2026-10-17.
    fn monitor_bytes(&self) -> Option<[u8; 3]> {
        if !self.monitors_reported {
            return Some([0, 0, 0]);
        }
        let (b, c, d) = match self.engine {
            // Compression ignition (B bit 3). C: NMHC catalyst, NOx/SCR, boost pressure, exhaust
            // gas sensor, PM filter, EGR/VVT. D: the particulate filter and NOx monitors not
            // complete yet.
            EngineKind::DieselUte => (0x0F, 0xEB, 0x42),
            // Spark ignition. C: catalyst, evaporative system, oxygen sensor, heater, EGR/VVT.
            EngineKind::HatchPetrol => (0x07, 0xE5, 0x04),
            EngineKind::Hybrid => (0x07, 0xE5, 0x00),
            _ => return None,
        };
        Some(if self.monitors_incomplete { [b | 0x70, c, c] } else { [b, c, d] })
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
        let e = Engine::at(self.engine, t_ms, running, self.stored.len());
        // The Corolla and the newer invented cars follow EOBD and report MIL distance and time.
        let corolla = self.engine != EngineKind::HijetKf;
        let incomplete = if self.monitors_incomplete { 0x65 } else { 0x00 };
        let u8c = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        let u16c = |v: f64| (v.round().clamp(0.0, 65535.0) as u16).to_be_bytes().to_vec();
        let pct = |v: f64| u8c(v * 255.0 / 100.0);
        let trim = |v: f64| u8c(v * 128.0 / 100.0 + 128.0);
        match pid {
            0x01 | 0x41 => {
                let a = (if e.mil { 0x80 } else { 0 }) | (e.codes.min(0x7F) as u8);
                match self.monitor_bytes() {
                    Some([b, c, d]) => vec![a, b, c, d],
                    None => vec![a, 0x07, 0x65, incomplete],
                }
            }
            // A hybrid with its engine stopped has no fuel system status to give.
            0x03 if self.engine == EngineKind::Hybrid && e.rpm == 0.0 => vec![0, 0],
            0x03 => vec![if e.closed_loop { 2 } else { 1 }, 0],
            0x04 => vec![pct(e.load)],
            0x05 => vec![u8c(e.coolant + 40.0)],
            0x06 => vec![trim(e.stft)],
            0x07 => vec![trim(1.6)],
            0x0B => vec![u8c(e.map)],
            0x0C => u16c(e.rpm * 4.0),
            0x0D => vec![u8c(e.speed)],
            0x0E => vec![u8c((e.timing + 64.0) * 2.0)],
            0x0F => vec![u8c(22.0 + 40.0)],
            0x10 => u16c(e.maf * 100.0),
            0x11 => vec![pct(e.throttle)],
            0x13 => vec![0x03],
            0x14 => vec![u8c(e.o2_front * 200.0), trim(e.stft)],
            0x23 => u16c(e.fuel_rail / 10.0),
            // Wide-range oxygen sensor: lambda (2/65536 per step), then volts (8/65536 per step).
            0x24 => [u16c(e.lambda * 32768.0), u16c(2.2 * 8192.0)].concat(),
            0x2C => vec![pct(e.egr)],
            0x2D => vec![trim(if e.egr > 0.0 { 1.5 } else { 0.0 })],
            0x2F => vec![pct(55.0)],
            // Wide-range sensor, current form: lambda, then milliamps (1/256 per step, offset 128).
            0x34 => [u16c(e.lambda * 32768.0), u16c((e.lambda - 1.0) * 6.0 * 256.0 + 128.0 * 256.0)].concat(),
            0x46 => vec![u8c(24.0 + 40.0)],
            0x15 => vec![u8c(e.o2_rear * 200.0), 0xFF],
            // JOBD for the Hijet; EOBD for the Corolla (Australian cars follow ADR 79/01, an
            // estimate for this one).
            0x1C => vec![if corolla { 6 } else { 10 }],
            0x1F => u16c(e.run_s),
            // Distance (km) and time (min) with the MIL on.
            0x21 => u16c(if corolla && e.mil { 340.0 } else { 0.0 }),
            0x4D => u16c(if corolla && e.mil { 410.0 } else { 0.0 }),
            0x2E => vec![pct(if e.closed_loop { 12.0 } else { 0.0 })],
            0x30 => vec![38],
            0x31 => u16c(1250.0),
            0x3C => u16c((e.cat_temp + 40.0) * 10.0),
            0x43 => u16c(e.load * 0.85 * 255.0 / 100.0),
            0x44 if self.engine != EngineKind::HijetKf && self.engine != EngineKind::Corolla1zz => {
                u16c(e.lambda * 32768.0)
            }
            0x44 => u16c(32768.0),
            0x33 => vec![94],
            0x42 => u16c(e.volts * 1000.0),
            0x45 => vec![pct(e.throttle * 0.3)],
            0x47 => vec![pct(e.throttle)],
            0x49 => vec![pct(15.0 + (e.throttle - 18.0) * 0.8)],
            0x4A => vec![pct(7.5 + (e.throttle - 18.0) * 0.4)],
            0x4C => vec![pct((e.throttle - 8.0).max(0.0))],
            0x4E => u16c(45_060.0 + (t_ms / 60_000) as f64),
            0x51 => vec![match self.engine {
                EngineKind::DieselUte => 4,
                EngineKind::Hybrid => 17,
                _ => 1,
            }],
            0x5B => vec![pct(e.hybrid_battery)],
            0x5C => vec![u8c(e.oil_temp + 40.0)],
            0x5E => u16c(e.fuel_rate * 20.0),
            // Exhaust gas temperature bank 1 (0x78) and particulate filter temperature (0x7C): a
            // support byte (sensors 1 and 2), then four temperatures in 0.1 degree steps, offset 40.
            0x78 | 0x7C => {
                let temp = |v: f64| u16c((v + 40.0) * 10.0);
                let mut d = vec![0x03];
                // The filter's inlet is a little hotter than the exhaust manifold side reading.
                let (first, second) =
                    if pid == 0x7C { (e.egt_out + 25.0, e.egt_out - 15.0) } else { (e.egt_in, e.egt_out) };
                for v in [first, second, 0.0, 0.0] {
                    d.extend(temp(v));
                }
                d
            }
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

/// Service 06 results for the Corolla (raw values), a four-cylinder with a worn catalyst.
fn corolla_tests() -> Vec<(u8, u8, u8, u16, u16, u16)> {
    vec![
        // Oxygen sensor B1S1: switch times in ms, voltages in mV.
        (0x01, 0x05, 0x10, 68, 0, 150),
        (0x01, 0x06, 0x10, 79, 0, 150),
        (0x01, 0x07, 0x0B, 61, 0, 300),
        (0x01, 0x08, 0x0B, 824, 600, 1100),
        // Catalyst B1: a maker's ratio (1/256 steps), over its limit: why P0420 set.
        (0x21, 0x80, 0x20, 181, 0, 154),
        // Oxygen sensor heaters B1S1 and B1S2: resistance in ohms.
        (0x41, 0x81, 0x14, 7, 2, 14),
        (0x42, 0x81, 0x14, 9, 2, 16),
        // Misfire counts: general, then cylinders 1-4. None.
        (0xA1, 0x0B, 0x24, 0, 0, 65535),
        (0xA2, 0x0B, 0x24, 0, 0, 65535),
        (0xA3, 0x0B, 0x24, 0, 0, 65535),
        (0xA4, 0x0B, 0x24, 0, 0, 65535),
        (0xA5, 0x0B, 0x24, 0, 0, 65535),
    ]
}

/// An engine at a moment: a deterministic model, no randomness.
#[derive(Default)]
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
    cat_temp: f64,
    closed_loop: bool,
    mil: bool,
    codes: usize,
    /// Vehicle speed, km/h. Zero for the cars that stand still.
    speed: f64,
    /// Fuel rail pressure, kPa (diesel).
    fuel_rail: f64,
    /// Commanded EGR, percent.
    egr: f64,
    /// Exhaust gas temperatures, degrees C: before and after the particulate filter.
    egt_in: f64,
    egt_out: f64,
    /// Air-fuel ratio as lambda (1.0 is stoichiometric); wide-band sensor reading.
    lambda: f64,
    /// Fuel use, L/h.
    fuel_rate: f64,
    oil_temp: f64,
    /// Hybrid battery pack remaining life, percent.
    hybrid_battery: f64,
}

impl Engine {
    fn at(kind: EngineKind, t_ms: u64, running: bool, codes: usize) -> Self {
        let t = t_ms as f64 / 1000.0;
        if running {
            match kind {
                EngineKind::Corolla1zz => return Self::corolla(t, codes),
                EngineKind::DieselUte => return Self::diesel_ute(t, codes),
                EngineKind::HatchPetrol => return Self::hatch(t, codes),
                EngineKind::Hybrid => return Self::hybrid(t, codes),
                EngineKind::HijetKf => {}
            }
        }
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
                cat_temp: 20.0,
                closed_loop: false,
                mil: codes > 0,
                codes,
                lambda: 1.0,
                oil_temp: 30.0,
                fuel_rail: 20_000.0,
                hybrid_battery: 75.0,
                ..Engine::default()
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
            cat_temp: 300.0 + 150.0 * warm + 120.0 * blip,
            closed_loop,
            mil: codes > 0,
            codes,
            lambda: 1.0,
            ..Engine::default()
        }
    }

    /// The Corolla's 1ZZ-FE, warm from a drive to the workshop. Typical values for a warm
    /// 1.8 L four-cylinder (estimates, not measured on a car): about 700 rpm and 3 g/s at idle.
    fn corolla(t: f64, codes: usize) -> Self {
        // 0-45 s idle, 45-90 s about 2500 rpm, with 3 s ramps between.
        let c = t % 90.0;
        let up = if c < 45.0 {
            0.0
        } else if c < 48.0 {
            (c - 45.0) / 3.0
        } else if c < 87.0 {
            1.0
        } else {
            (90.0 - c) / 3.0
        };
        let closed_loop = up == 0.0 || up == 1.0;
        // A narrow-band front sensor swings rich and lean, faster at 2500 rpm.
        let front = if closed_loop { 0.45 + 0.38 * (t * (5.0 + 3.0 * up)).sin() } else { 0.7 };
        // A worn catalyst stores little oxygen: the rear sensor follows the front, a little
        // late and a little smaller. A good catalyst would hold it steady near 0.6-0.7 V.
        let rear = if closed_loop { 0.45 + 0.3 * (t * (5.0 + 3.0 * up) - 0.8).sin() } else { 0.6 };
        Engine {
            rpm: 700.0 + 15.0 * (t * 0.7).sin() + 1800.0 * up + 30.0 * up * (t * 0.4).sin(),
            load: 22.0 + 14.0 * up,
            coolant: 89.0 + 2.0 * (t / 40.0).sin(),
            map: 30.0 + 12.0 * up,
            maf: 3.0 + 0.2 * (t * 0.7).sin().abs() + 6.5 * up,
            timing: 10.0 + 2.0 * (t * 0.5).sin() + 22.0 * up,
            throttle: 14.0 + 7.0 * up,
            stft: if closed_loop { 4.0 * (t * 1.3).sin() + 1.0 } else { 0.0 },
            o2_front: front,
            o2_rear: rear,
            volts: 14.05 + 0.05 * (t * 0.9).sin(),
            run_s: t + 900.0,
            cat_temp: 520.0 + 160.0 * up,
            closed_loop,
            mil: codes > 0,
            codes,
            lambda: 1.0,
            ..Engine::default()
        }
    }

    /// The invented diesel ute, warm. Every 40 s the revs rise to about 1900 rpm for 6 s
    /// (2 s ramps), as if the driver blipped the pedal. Typical diesel figures (estimates, not
    /// measured): lean always (lambda 1.4 to 2.4), no throttle plate to speak of, high rail
    /// pressure.
    fn diesel_ute(t: f64, codes: usize) -> Self {
        let c = t % 40.0;
        let up = if c < 28.0 {
            0.0
        } else if c < 30.0 {
            (c - 28.0) / 2.0
        } else if c < 36.0 {
            1.0
        } else if c < 38.0 {
            (38.0 - c) / 2.0
        } else {
            0.0
        };
        Engine {
            rpm: 780.0 + 10.0 * (t * 0.6).sin() + 1100.0 * up,
            load: 24.0 + 40.0 * up,
            coolant: 87.0 + 2.0 * (t / 50.0).sin(),
            // Boost: the intake manifold pressure is above atmosphere when the turbo works.
            map: 101.0 + 4.0 * (t * 0.5).sin() + 110.0 * up,
            maf: 7.5 + 0.3 * (t * 0.7).sin().abs() + 38.0 * up,
            throttle: 12.0 + 40.0 * up,
            volts: 14.1 + 0.05 * (t * 0.9).sin(),
            run_s: t + 1200.0,
            mil: codes > 0,
            codes,
            fuel_rail: 31_000.0 + 70_000.0 * up + 800.0 * (t * 1.1).sin(),
            egr: 28.0 - 22.0 * up,
            egt_in: 190.0 + 330.0 * up,
            egt_out: 175.0 + 290.0 * up,
            lambda: 1.9 - 0.5 * up,
            fuel_rate: 1.4 + 9.0 * up,
            oil_temp: 91.0 + 2.0 * (t / 50.0).sin(),
            ..Engine::default()
        }
    }

    /// The invented petrol hatch, warm, idling with a short rev every 30 s.
    fn hatch(t: f64, codes: usize) -> Self {
        let c = t % 30.0;
        let up = if (20.0..22.0).contains(&c) {
            (c - 20.0) / 2.0
        } else if (22.0..26.0).contains(&c) {
            1.0
        } else if (26.0..28.0).contains(&c) {
            (28.0 - c) / 2.0
        } else {
            0.0
        };
        let closed_loop = up < 0.5;
        Engine {
            rpm: 680.0 + 12.0 * (t * 0.7).sin() + 1500.0 * up,
            load: 20.0 + 25.0 * up,
            coolant: 90.0 + 1.5 * (t / 40.0).sin(),
            map: 32.0 + 30.0 * up,
            maf: 2.8 + 0.2 * (t * 0.7).sin().abs() + 7.0 * up,
            timing: 12.0 + 2.0 * (t * 0.5).sin() + 14.0 * up,
            throttle: 13.0 + 12.0 * up,
            stft: if closed_loop { 3.0 * (t * 1.3).sin() } else { 0.0 },
            o2_front: 0.45,
            o2_rear: 0.62 + 0.04 * (t * 0.3).sin(),
            volts: 14.2 + 0.05 * (t * 0.9).sin(),
            run_s: t + 600.0,
            cat_temp: 480.0 + 90.0 * up,
            closed_loop,
            mil: codes > 0,
            codes,
            lambda: if closed_loop { 1.0 + 0.015 * (t * 3.0).sin() } else { 0.88 },
            fuel_rate: 0.9 + 4.0 * up,
            oil_temp: 92.0,
            ..Engine::default()
        }
    }

    /// The invented hybrid, "ready" from the start. The engine is off at first (0 rpm, the car
    /// stands), then the car creeps on electric power (0 rpm, up to 25 km/h), then the engine
    /// starts and runs, then stops again. One turn is 90 s. The engine's own values (load,
    /// air flow) are zero while it is off.
    fn hybrid(t: f64, codes: usize) -> Self {
        let c = t % 90.0;
        // Engine run level, 0 to 1, and the road speed.
        let (run, speed) = if c < 20.0 {
            (0.0, 0.0)
        } else if c < 45.0 {
            (0.0, 25.0 * ((c - 20.0) / 5.0).min(1.0))
        } else if c < 48.0 {
            ((c - 45.0) / 3.0, 25.0 + 5.0 * (c - 45.0) / 3.0)
        } else if c < 70.0 {
            (1.0, 30.0 + 20.0 * ((c - 48.0) / 22.0))
        } else if c < 73.0 {
            ((73.0 - c) / 3.0, 50.0 - 10.0 * (c - 70.0) / 3.0)
        } else {
            (0.0, (40.0 - 8.0 * (c - 73.0)).max(0.0))
        };
        let on = run > 0.0;
        Engine {
            rpm: if on { 1000.0 + 1100.0 * run * (0.5 + 0.5 * (t * 0.3).sin()) } else { 0.0 },
            load: if on { 30.0 * run + 5.0 } else { 0.0 },
            coolant: 80.0 + 3.0 * (t / 60.0).sin(),
            map: if on { 40.0 + 20.0 * run } else { 98.0 },
            maf: if on { 2.5 + 6.0 * run } else { 0.0 },
            timing: if on { 18.0 } else { 0.0 },
            throttle: if on { 20.0 + 10.0 * run } else { 12.0 },
            stft: if on { 2.0 * (t * 1.3).sin() } else { 0.0 },
            o2_front: 0.45,
            o2_rear: 0.62,
            volts: 14.0,
            run_s: t + 300.0,
            cat_temp: if on { 520.0 } else { 380.0 },
            closed_loop: on,
            mil: codes > 0,
            codes,
            speed,
            lambda: 1.0,
            fuel_rate: if on { 0.8 + 2.0 * run } else { 0.0 },
            oil_temp: 85.0,
            hybrid_battery: 75.0,
            ..Engine::default()
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

/// A frame a module sends by itself at a fixed period, like the status messages on a real
/// bus. Scana can hear these while it only listens.
#[derive(Debug)]
struct Broadcast {
    id: CanId,
    period_ms: u64,
    next_ms: u64,
    len: u8,
    /// The first bytes of the data; the last byte counts up so the monitor shows change.
    base: [u8; 8],
    count: u8,
}

/// A simulated car: one CAN bus with ECUs on it.
#[derive(Debug)]
pub struct VirtualCar {
    /// Which OBD pins the bus is on. A link on the other pins hears nothing.
    pub bus: Bus,
    pub bitrate: Bitrate,
    pub extended_ids: bool,
    pub ecus: Vec<Ecu>,
    /// Modules that answer enhanced (UDS or KWP) requests.
    pub modules: Vec<Module>,
    pub faults: Faults,
    other_tester: Option<OtherTester>,
    /// Frames modules send on their own, at fixed periods.
    broadcasts: Vec<Broadcast>,
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
            bus: Bus::Main,
            bitrate: Bitrate::K500,
            extended_ids: false,
            ecus: vec![ecm],
            modules: modules::hijet_demo_modules(),
            faults: Faults::default(),
            other_tester: None,
            broadcasts: vec![],
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

    /// A 2006 Toyota Corolla 1.8 (1ZZ-FE, ZZE122) with a worn catalyst: P0420 stored, the MIL
    /// on, a freeze frame from a cruise, and a catalyst test result over its limit. The rear
    /// oxygen sensor swings almost like the front one. The engine idles, then holds about
    /// 2500 rpm, in turns of 45 s (see [`EngineKind::Corolla1zz`]).
    ///
    /// Simulated on CAN at 500 kbit/s with 11-bit IDs, like Toyota's CAN cars. A real 2006
    /// Corolla may use the K-line (ISO 9141-2 or KWP2000) instead, depending on its market and
    /// build: a scan tool's protocol line, or pins 6 and 14 in use, tells. The PID list and the
    /// values are typical for the car, not read from one. No enhanced modules (on a car of that
    /// age they aren't on CAN).
    pub fn corolla_2006() -> Self {
        let pids = vec![
            0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x13, 0x14, 0x15, 0x1C, 0x1F, 0x21,
            0x2E, 0x30, 0x31, 0x33, 0x3C, 0x41, 0x42, 0x43, 0x44, 0x45, 0x47, 0x49, 0x4A, 0x4C, 0x4D,
        ];
        let mut ecm =
            Ecu::new("Engine (simulated 2006 Corolla 1ZZ-FE)", CanId::Standard(0x7E0), CanId::Standard(0x7E8), pids);
        ecm.engine = EngineKind::Corolla1zz;
        // A made-up VIN with Toyota's maker code (JT) and 2006 in place 10. The check digit
        // (place 9) is a letter, so it can't be a real VIN.
        ecm.vin = Some("JTDSCANSM60000001".into());
        ecm.calibration_id = Some("SIMCAL-1ZZ-00001".into());
        ecm.permanent_supported = false;
        ecm.stored = vec![Dtc(0x0420)];
        // P0420 sets on a steady cruise: the freeze frame comes from the 2500 rpm part, at 72 km/h.
        ecm.freeze = Some((Dtc(0x0420), 60_000));
        ecm.freeze_overrides = vec![(0x0D, vec![72])];
        ecm.tests = corolla_tests();
        VirtualCar {
            bus: Bus::Main,
            bitrate: Bitrate::K500,
            extended_ids: false,
            ecus: vec![ecm],
            modules: vec![],
            faults: Faults::default(),
            other_tester: None,
            broadcasts: vec![],
            out: VecDeque::new(),
            replies: 0,
        }
    }

    fn bare(ecus: Vec<Ecu>) -> Self {
        VirtualCar {
            bus: Bus::Main,
            bitrate: Bitrate::K500,
            extended_ids: false,
            ecus,
            modules: vec![],
            faults: Faults::default(),
            other_tester: None,
            broadcasts: vec![],
            out: VecDeque::new(),
            replies: 0,
        }
    }

    /// An INVENTED 2016-2020 turbo-diesel ute (the kind sold in Australia with a 2.2 to 3.2 L
    /// engine), made up for testing. The values and the PID list are NOT taken from any real
    /// car's data. The engine computer (7E0/7E8) and the gearbox computer (7E1/7E9) both
    /// answer functional (7DF) requests. 11-bit IDs, 500 kbit/s, no dash display.
    ///
    /// Facts used, from Wikipedia "OBD-II PIDs" (read 2026-10-17): PID 01 byte B bit 3 set means
    /// compression ignition, and bytes C and D then hold the diesel monitors (NMHC catalyst,
    /// NOx/SCR, boost pressure, exhaust gas sensor, PM filter, EGR/VVT); PID 0x23 is fuel rail
    /// pressure (10 kPa per step); 0x34 is a wide-range oxygen sensor with lambda and current;
    /// 0x78 is exhaust gas temperature and 0x7C is particulate filter temperature; 0x51 value 4
    /// is diesel. There are no narrow-band oxygen sensor PIDs (0x13 to 0x1B). One code is
    /// stored: P2002 (particulate filter efficiency below threshold), with a freeze frame.
    pub fn ute_diesel() -> Self {
        let pids = vec![
            0x01, 0x04, 0x05, 0x0B, 0x0C, 0x0D, 0x0F, 0x10, 0x11, 0x1C, 0x1F, 0x21, 0x23, 0x2C, 0x2D, 0x2F, 0x30, 0x31,
            0x33, 0x34, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x49, 0x4C, 0x4D, 0x4E, 0x51, 0x5C, 0x5E, 0x78, 0x7C,
        ];
        let mut ecm = Ecu::new("Engine (simulated diesel ute)", CanId::Standard(0x7E0), CanId::Standard(0x7E8), pids);
        ecm.engine = EngineKind::DieselUte;
        ecm.vin = Some("MNCSIMUTED0000001".into());
        ecm.calibration_id = Some("SIMCAL-UTE-ENG01".into());
        ecm.stored = vec![Dtc(0x2002)];
        // Stored on a steady run at 85 km/h.
        ecm.freeze = Some((Dtc(0x2002), 32_000));
        ecm.freeze_overrides = vec![(0x0D, vec![85])];
        let mut tcm = Ecu::new(
            "Gearbox (simulated diesel ute)",
            CanId::Standard(0x7E1),
            CanId::Standard(0x7E9),
            vec![0x01, 0x05, 0x0C, 0x0D, 0x42],
        );
        tcm.engine = EngineKind::DieselUte;
        tcm.calibration_id = Some("SIMCAL-UTE-TCM01".into());
        tcm.reply_delay_ms = 4;
        tcm.monitors_reported = false;
        VirtualCar::bare(vec![ecm, tcm])
    }

    /// An INVENTED 2015-2019 petrol hatch (Mazda3 or Hyundai i30 class), made up for testing.
    /// Values and PID lists are NOT taken from any real car's data. The engine computer
    /// (7E0/7E8) and the gearbox computer (7E1/7E9) both answer engine speed, vehicle speed and
    /// coolant temperature, and both pad their frames with 0x00 (not 0xAA). The engine
    /// computer reports a VIN and a calibration ID, the gearbox computer a calibration ID.
    /// Permanent codes are supported (none stored). One code is pending, not stored: P0455 (an
    /// evaporative system leak), so the MIL is off.
    ///
    /// PID formulas and the PID 01 layout: Wikipedia "OBD-II PIDs", read 2026-10-17.
    pub fn hatch_two_ecus() -> Self {
        let pids = vec![
            0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x13, 0x15, 0x1C, 0x1F, 0x21,
            0x24, 0x2E, 0x2F, 0x30, 0x31, 0x33, 0x3C, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x49, 0x4A, 0x4C, 0x4D,
            0x4E, 0x51,
        ];
        let mut ecm = Ecu::new("Engine (simulated hatch)", CanId::Standard(0x7E0), CanId::Standard(0x7E8), pids);
        ecm.engine = EngineKind::HatchPetrol;
        ecm.padding = 0x00;
        ecm.vin = Some("JM0SIMHATCH000001".into());
        ecm.calibration_id = Some("SIMCAL-HATCH-E01".into());
        ecm.pending = vec![Dtc(0x0455)];
        let mut tcm = Ecu::new(
            "Gearbox (simulated hatch)",
            CanId::Standard(0x7E1),
            CanId::Standard(0x7E9),
            vec![0x01, 0x05, 0x0C, 0x0D, 0x1C, 0x42],
        );
        tcm.engine = EngineKind::HatchPetrol;
        tcm.padding = 0x00;
        tcm.calibration_id = Some("SIMCAL-HATCH-T01".into());
        tcm.reply_delay_ms = 4;
        tcm.monitors_reported = false;
        VirtualCar::bare(vec![ecm, tcm])
    }

    /// An INVENTED 2019-2023 petrol hybrid (Toyota Corolla or RAV4 hybrid class), made up for
    /// testing. Values and PID lists are NOT taken from any real car's data. The engine
    /// computer (7E0/7E8) and a hybrid computer (7E2/7EA) answer. The engine stops and starts
    /// by itself: its speed reads 0 rpm while the car is ready, also while it creeps on
    /// electric power, then the engine runs again (a 90 s turn, see [`EngineKind::Hybrid`]).
    /// The hybrid computer answers 0x0D (vehicle speed), 0x42 and 0x5B (hybrid battery pack
    /// remaining life). The engine computer answers 0x51 (fuel type, 17 is hybrid gasoline) and
    /// 0x5E (fuel rate). PID meanings: Wikipedia "OBD-II PIDs", read 2026-10-17.
    pub fn hybrid() -> Self {
        let pids = vec![
            0x01, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x11, 0x1C, 0x1F, 0x21, 0x24, 0x2E, 0x2F,
            0x30, 0x31, 0x33, 0x3C, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x49, 0x4A, 0x4C, 0x4D, 0x4E, 0x51, 0x5C,
            0x5E,
        ];
        let mut ecm = Ecu::new("Engine (simulated hybrid)", CanId::Standard(0x7E0), CanId::Standard(0x7E8), pids);
        ecm.engine = EngineKind::Hybrid;
        ecm.vin = Some("JTDSIMHYB00000001".into());
        ecm.calibration_id = Some("SIMCAL-HYB-ENG01".into());
        let mut hv = Ecu::new(
            "Hybrid system (simulated)",
            CanId::Standard(0x7E2),
            CanId::Standard(0x7EA),
            vec![0x0D, 0x42, 0x5B],
        );
        hv.engine = EngineKind::Hybrid;
        hv.calibration_id = Some("SIMCAL-HYB-HV01".into());
        hv.reply_delay_ms = 4;
        hv.monitors_reported = false;
        VirtualCar::bare(vec![ecm, hv])
    }

    /// A simulated car by its `--sim-car` name: `ute-diesel`, `hatch-two-ecus`, `hybrid` or
    /// `ford-ms-can` (the second bus, pins 3 and 11).
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "ute-diesel" => Some(VirtualCar::ute_diesel()),
            "hatch-two-ecus" => Some(VirtualCar::hatch_two_ecus()),
            "hybrid" => Some(VirtualCar::hybrid()),
            "ford-ms-can" => Some(VirtualCar::ms_can_ford()),
            _ => None,
        }
    }

    /// The names [`VirtualCar::by_name`] knows.
    pub const CAR_NAMES: [&'static str; 4] = ["ute-diesel", "hatch-two-ecus", "hybrid", "ford-ms-can"];

    /// An INVENTED medium-speed CAN bus, like the one some Fords put on OBD pins 3 and 11,
    /// made up for testing. Nothing here is read from a real car. 125 kbit/s, 11-bit IDs.
    /// Several IDs send status frames by themselves (so listening alone hears traffic), and
    /// seven body modules answer UDS or KWP2000 read requests on the request and reply IDs
    /// the FG Falcon's medium-speed modules use (github.com/jakka351/FG-Falcon, read
    /// 2026-10-17; low to medium confidence): 720/728, 726/72E, 727/72F, 736/73E, 781/789,
    /// 767/76F and 7A6/7AE. It has no OBD-II computer: nothing answers 0x7DF. It sits on
    /// [`Bus::Second`]: a link on pins 6 and 14 hears nothing from it.
    pub fn ms_can_ford() -> Self {
        let mut car = VirtualCar::bare(vec![]);
        car.bus = Bus::Second;
        car.bitrate = Bitrate::K125;
        car.modules = modules::ford_ms_can_modules();
        // (ID, period ms, length, first bytes): invented status frames.
        for (id, period_ms, len, head) in [
            (0x1C0u16, 20u64, 8u8, [0x00u8, 0x64, 0x00, 0x00]),
            (0x230, 50, 8, [0x40, 0x00, 0x80, 0x00]),
            (0x250, 100, 6, [0x01, 0x00, 0x00, 0x00]),
            (0x3A0, 100, 8, [0x00, 0x00, 0x7F, 0x00]),
            (0x4B0, 200, 4, [0x40, 0x00, 0x00, 0x00]),
        ] {
            let mut base = [0u8; 8];
            base[..4].copy_from_slice(&head);
            car.broadcasts.push(Broadcast { id: CanId::Standard(id), period_ms, next_ms: 0, len, base, count: 0 });
        }
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
        tcm.monitors_reported = false;
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
        let Ok(mut sender) = Sender::new(msg, Some(self.ecus[ecu].padding)) else { return };
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
                        let value = ecu
                            .freeze_overrides
                            .iter()
                            .find(|(q, _)| *q == p)
                            .map_or_else(|| ecu.value_at(p, t, true), |(_, v)| v.clone());
                        vec![(0, [&[0x42, p, 0x00][..], &value].concat())]
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
            0x0A if !ecu.permanent_supported => silent_or(0x11),
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
        for b in &self.broadcasts {
            t = Some(t.map_or(b.next_ms, |x| x.min(b.next_ms)));
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
        // Frames modules send by themselves. A bus nobody listened to for a while doesn't
        // pile up a burst: the periods start again from now.
        let mut sent = Vec::new();
        for b in &mut self.broadcasts {
            if b.next_ms + 1000 < now_ms {
                b.next_ms = now_ms;
            }
            while b.next_ms <= now_ms {
                let mut d = b.base;
                d[usize::from(b.len).saturating_sub(1)] = b.count;
                b.count = b.count.wrapping_add(1);
                if let Ok(f) = Frame::new(b.id, &d[..usize::from(b.len)]) {
                    sent.push((b.next_ms, f));
                }
                b.next_ms += b.period_ms;
            }
        }
        for (at, f) in sent {
            self.push_out(at, f);
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

    #[test]
    fn the_2006_corolla_has_p0420_and_no_permanent_codes() {
        let mut car = VirtualCar::corolla_2006();
        // Stored codes: P0420.
        car.receive(&req(0x7E0, &[0x01, 0x03]), 100);
        assert_eq!(car.poll(200)[0].data()[..5], [0x04, 0x43, 0x01, 0x04, 0x20]);
        // Permanent codes: refused (service not supported), as on cars before about 2010.
        car.receive(&req(0x7E0, &[0x01, 0x0A]), 300);
        assert_eq!(car.poll(400)[0].data()[..4], [0x03, 0x7F, 0x0A, 0x11]);
        // The freeze frame was stored on a cruise: 72 km/h, though the car stands still now.
        car.receive(&req(0x7E0, &[0x03, 0x02, 0x0D, 0x00]), 500);
        assert_eq!(car.poll(600)[0].data()[..5], [0x04, 0x42, 0x0D, 0x00, 72]);
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x0D]), 700);
        assert_eq!(car.poll(800)[0].data()[..4], [0x03, 0x41, 0x0D, 0x00]);
        // It idles at about 700 rpm, then holds about 2500 rpm.
        let rpm = |t: u64| {
            let d = car.ecus[0].pid_value(0x0C, t).unwrap();
            f64::from(u16::from_be_bytes([d[0], d[1]])) / 4.0
        };
        assert!((650.0..760.0).contains(&rpm(20_000)));
        assert!((2400.0..2600.0).contains(&rpm(60_000)));
    }

    /// Sends a functional request and returns the first frame from each answering ID, sorted.
    fn ask_all(car: &mut VirtualCar, data: &[u8], at: u64) -> Vec<Frame> {
        car.receive(&req(0x7DF, data), at);
        let mut f = car.poll(at + 50);
        f.sort_by_key(|x| x.id.raw());
        f
    }

    fn rpm_of(car: &VirtualCar, ecu: usize, t: u64) -> f64 {
        let d = car.ecus[ecu].pid_value(0x0C, t).unwrap();
        f64::from(u16::from_be_bytes([d[0], d[1]])) / 4.0
    }

    #[test]
    fn the_diesel_ute_has_two_ecus_diesel_monitors_and_a_dpf_code() {
        let mut car = VirtualCar::ute_diesel();
        let f = ask_all(&mut car, &[0x02, 0x01, 0x00], 0);
        assert_eq!(f.iter().map(|x| x.id).collect::<Vec<_>>(), vec![CanId::Standard(0x7E8), CanId::Standard(0x7E9)]);
        // Fuel type: diesel. Both computers answer 0x05 functionally.
        let f = ask_all(&mut car, &[0x02, 0x01, 0x05], 100);
        assert_eq!(f.len(), 2);
        let f = ask_all(&mut car, &[0x02, 0x01, 0x51], 200);
        assert_eq!(f[0].data()[..4], [0x03, 0x41, 0x51, 0x04]);
        // PID 01: B bit 3 set (compression ignition), C and D hold the diesel monitors.
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x01]), 300);
        let d = car.poll(310)[0].data().to_vec();
        assert_eq!(d[1..3], [0x41, 0x01]);
        assert_eq!(d[3], 0x81); // MIL on, one code
        assert_eq!(d[4] & 0x08, 0x08);
        assert_eq!((d[5], d[6]), (0xEB, 0x42));
        // No oxygen sensor voltage PIDs (0x13 to 0x1B), but fuel rail pressure and DPF temperature.
        let pids = &car.ecus[0].pids;
        assert!((0x13..=0x1B).all(|p| !pids.contains(&p)));
        assert!(pids.contains(&0x23) && pids.contains(&0x7C) && pids.contains(&0x78));
        // Fuel rail pressure 0x23: 10 kPa per step, about 31000 kPa at idle.
        let v = car.ecus[0].pid_value(0x23, 5000).unwrap();
        let kpa = f64::from(u16::from_be_bytes([v[0], v[1]])) * 10.0;
        assert!((29_000.0..33_000.0).contains(&kpa), "{kpa}");
        // 0x7C: nine data bytes (support flags, then four temperatures).
        assert_eq!(car.ecus[0].pid_value(0x7C, 5000).unwrap().len(), 9);
        // The stored code is P2002, with a freeze frame from 85 km/h.
        car.receive(&req(0x7E0, &[0x01, 0x03]), 400);
        assert_eq!(car.poll(410)[0].data()[..5], [0x04, 0x43, 0x01, 0x20, 0x02]);
        car.receive(&req(0x7E0, &[0x03, 0x02, 0x0D, 0x00]), 500);
        assert_eq!(car.poll(510)[0].data()[..5], [0x04, 0x42, 0x0D, 0x00, 85]);
        // Idles at about 780 rpm.
        assert!((700.0..900.0).contains(&rpm_of(&car, 0, 5000)));
    }

    #[test]
    fn the_hatch_has_two_ecus_answering_the_same_pids_with_zero_padding() {
        let mut car = VirtualCar::hatch_two_ecus();
        let f = ask_all(&mut car, &[0x02, 0x01, 0x0D], 0);
        assert_eq!(f.len(), 2);
        for fr in &f {
            // 03 41 0D 00, then padding of 0x00 (not 0xAA).
            assert_eq!(fr.data(), [0x03, 0x41, 0x0D, 0x00, 0, 0, 0, 0]);
        }
        let f = ask_all(&mut car, &[0x02, 0x01, 0x05], 100);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].data()[3], f[1].data()[3]);
        // Calibration IDs from both, a VIN from the engine computer.
        for (id, want) in [(0x7E0, "SIMCAL-HATCH-E01"), (0x7E1, "SIMCAL-HATCH-T01")] {
            car.receive(&req(id, &[0x02, 0x09, 0x04]), 200);
            let first = car.poll(210)[0].data().to_vec();
            assert_eq!(first[0], 0x10);
            car.receive(&req(id, &[0x30, 0, 0]), 220);
            let rest: Vec<u8> = car.poll(240).iter().flat_map(|x| x.data()[1..].to_vec()).collect();
            let text: Vec<u8> = [&first[5..], &rest[..]].concat();
            assert!(String::from_utf8_lossy(&text).starts_with(want), "{id:X}");
        }
        car.receive(&req(0x7E0, &[0x02, 0x09, 0x02]), 300);
        assert_eq!(car.poll(310)[0].data()[..2], [0x10, 20]);
        car.receive(&req(0x7E0, &[0x30, 0, 0]), 320);
        car.poll(340);
        // A pending code (P0455), none stored, permanent codes supported (none).
        car.receive(&req(0x7E0, &[0x01, 0x07]), 400);
        assert_eq!(car.poll(410)[0].data()[..5], [0x04, 0x47, 0x01, 0x04, 0x55]);
        car.receive(&req(0x7E0, &[0x01, 0x03]), 420);
        assert_eq!(car.poll(430)[0].data()[..3], [0x02, 0x43, 0x00]);
        car.receive(&req(0x7E0, &[0x01, 0x0A]), 440);
        assert_eq!(car.poll(450)[0].data()[..3], [0x02, 0x4A, 0x00]);
        // Spark ignition monitors: B bit 3 clear.
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x01]), 500);
        let d = car.poll(510)[0].data().to_vec();
        assert_eq!((d[3], d[4] & 0x08, d[5]), (0x00, 0x00, 0xE5));
    }

    #[test]
    fn the_hybrid_reads_0_rpm_while_ready_and_the_engine_comes_and_goes() {
        let mut car = VirtualCar::hybrid();
        let f = ask_all(&mut car, &[0x02, 0x01, 0x00], 0);
        assert_eq!(f.iter().map(|x| x.id).collect::<Vec<_>>(), vec![CanId::Standard(0x7E8), CanId::Standard(0x7EA)]);
        // Ready and standing: 0 rpm, 0 km/h. Creeping on electric power: 0 rpm, but moving.
        assert_eq!(rpm_of(&car, 0, 10_000), 0.0);
        assert_eq!(rpm_of(&car, 0, 35_000), 0.0);
        let speed = |t| car.ecus[1].pid_value(0x0D, t).unwrap()[0];
        assert_eq!(speed(10_000), 0);
        assert!(speed(35_000) > 20);
        // Then the engine runs, and stops again.
        assert!(rpm_of(&car, 0, 60_000) > 900.0);
        assert_eq!(rpm_of(&car, 0, 85_000), 0.0);
        // The hybrid computer answers its own PIDs: 0x5B battery life, not on the engine computer.
        car.receive(&req(0x7E2, &[0x02, 0x01, 0x5B]), 100);
        let f = car.poll(110);
        assert_eq!((f[0].id, f[0].data()[..4].to_vec()), (CanId::Standard(0x7EA), vec![0x03, 0x41, 0x5B, 0xBF]));
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x5B]), 200);
        assert!(car.poll(210)[0].data()[..4] == [0x03, 0x7F, 0x01, 0x12]);
        // Fuel type 17 (hybrid gasoline), and fuel rate 0 L/h with the engine stopped.
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x51]), 300);
        assert_eq!(car.poll(310)[0].data()[..4], [0x03, 0x41, 0x51, 17]);
        assert_eq!(car.ecus[0].pid_value(0x5E, 10_000).unwrap(), vec![0, 0]);
        // Ready, so the engine computer refuses to clear codes even at 0 rpm.
        car.receive(&req(0x7E0, &[0x01, 0x04]), 400);
        assert_eq!(car.poll(410)[0].data()[..4], [0x03, 0x7F, 0x04, 0x22]);
    }

    #[test]
    fn the_second_bus_car_talks_by_itself_and_answers_no_obd() {
        let mut car = VirtualCar::ms_can_ford();
        assert_eq!((car.bus, car.bitrate), (Bus::Second, Bitrate::K125));
        assert!(car.ecus.is_empty());
        // Frames from several IDs, with nothing sent to it.
        let heard = car.poll(300);
        let mut ids: Vec<CanId> = heard.iter().map(|f| f.id).collect();
        ids.sort();
        ids.dedup();
        assert!(ids.len() >= 4, "{ids:?}");
        assert!(heard.len() > 20);
        // Nothing answers a functional or an engine-computer request.
        car.receive(&req(0x7DF, &[0x02, 0x01, 0x00]), 400);
        car.receive(&req(0x7E0, &[0x02, 0x01, 0x00]), 400);
        assert!(car.poll(450).iter().all(|f| !(0x7E8..=0x7EF).contains(&f.id.raw())));
        // The instrument cluster answers a tester-present on 720 from 728.
        car.receive(&req(0x720, &[0x02, 0x3E, 0x00]), 500);
        let f = car.poll(520);
        assert!(f.iter().any(|f| f.id == CanId::Standard(0x728) && f.data()[..3] == [0x02, 0x7E, 0x00]));
    }

    #[test]
    fn a_long_quiet_spell_does_not_pile_up_frames() {
        let mut car = VirtualCar::ms_can_ford();
        car.poll(10);
        // Nobody polled for a minute: the next poll is not a burst of thousands of frames.
        assert!(car.poll(60_000).len() < 40);
    }

    #[test]
    fn cars_are_found_by_name() {
        for name in VirtualCar::CAR_NAMES {
            assert!(VirtualCar::by_name(name).is_some(), "{name}");
        }
        assert!(VirtualCar::by_name("nope").is_none());
    }
}
