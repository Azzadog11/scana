//! Reading things from the car: supported PIDs, live values, fault codes, vehicle info.

use crate::client::{Client, CoreError, Target};
use scana_proto::mode06::{self, TestResult};
use scana_proto::obd::{self, Reply};
use scana_proto::pids::{self, Decoded, MonitorStatus};
use scana_proto::{CanId, Dtc, dtc};

/// The live-data PIDs an ECU supports (service 01), following the 00, 20, 40 ... chain.
/// Includes the support-mask PIDs themselves.
pub fn supported_pids(client: &mut Client, ecu: CanId) -> Result<Vec<u8>, CoreError> {
    let mut all = Vec::new();
    let mut base: u8 = 0x00;
    loop {
        let data = client.request_one(ecu, &[obd::CURRENT_DATA, base])?;
        if data.len() < 5 || data[0] != base {
            return Err(CoreError::BadRequest(format!("PIDs-supported reply for {base:02X} was malformed")));
        }
        all.push(base);
        let found = obd::supported_pids(base, &data[1..5]);
        let next = base.checked_add(0x20);
        all.extend(found.iter().copied().filter(|p| !pids::is_support_pid(*p)));
        match next {
            Some(n) if found.contains(&n) => base = n,
            _ => break,
        }
    }
    all.sort_unstable();
    all.dedup();
    Ok(all)
}

/// One live value.
#[derive(Clone, Debug, PartialEq)]
pub struct PidReading {
    pub pid: u8,
    pub name: &'static str,
    pub raw: Vec<u8>,
    pub decoded: Option<Decoded>,
}

impl PidReading {
    pub fn display(&self) -> String {
        match &self.decoded {
            Some(d) => d.display(),
            None => self.raw.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
        }
    }
}

/// Reads live PIDs from one ECU. Known PIDs go 6 to a request; unknown ones one at a time.
/// Only ask for PIDs the ECU supports: a refusal can upset another tester polling the same ECU.
pub fn read_pids(client: &mut Client, ecu: CanId, wanted: &[u8]) -> Result<Vec<PidReading>, CoreError> {
    let (known, unknown): (Vec<u8>, Vec<u8>) =
        wanted.iter().copied().filter(|p| !pids::is_support_pid(*p)).partition(|p| pids::def(*p).is_some());
    let mut groups: Vec<Vec<u8>> = known.chunks(obd::MAX_PIDS_PER_REQUEST).map(|c| c.to_vec()).collect();
    groups.extend(unknown.into_iter().map(|p| vec![p]));
    let mut out = Vec::new();
    for group in groups {
        let req = obd::current_data_request(&group)?;
        let data = client.request_one(ecu, &req)?;
        for (pid, raw) in obd::split_current_data(&data)? {
            if !group.contains(&pid) {
                continue;
            }
            out.push(PidReading {
                pid,
                name: pids::def(pid).map_or("Unknown PID", |d| d.name),
                decoded: pids::decode(pid, &raw),
                raw,
            });
        }
    }
    out.sort_by_key(|r| wanted.iter().position(|p| *p == r.pid).unwrap_or(usize::MAX));
    Ok(out)
}

/// Codes from every ECU for service 03 (stored), 07 (pending) or 0A (permanent).
pub fn read_codes(client: &mut Client, service: u8) -> Result<Vec<(CanId, Vec<Dtc>)>, CoreError> {
    let replies = client.request(Target::AllEcus, &[service])?;
    let mut out = Vec::new();
    for r in replies {
        match r.reply {
            Reply::Positive { data, .. } => {
                let codes = dtc::parse_dtc_list(&data)
                    .map_err(|e| CoreError::BadRequest(format!("code list from {}: {e}", r.ecu)))?;
                out.push((r.ecu, codes));
            }
            Reply::Negative { .. } => out.push((r.ecu, vec![])),
        }
    }
    Ok(out)
}

/// The freeze frame: the code that stored it and the values at that moment.
#[derive(Clone, Debug, PartialEq)]
pub struct FreezeFrame {
    pub ecu: CanId,
    pub dtc: Dtc,
    pub readings: Vec<PidReading>,
}

/// Follows a "supported" mask chain (00, 20, 40 ...) for service 01, 02 or 06.
fn mask_chain(client: &mut Client, ecu: CanId, request: impl Fn(u8) -> Vec<u8>) -> Result<Vec<u8>, CoreError> {
    let mut found_all = Vec::new();
    let mut base: u8 = 0;
    loop {
        let req = request(base);
        let data = client.request_one(ecu, &req)?;
        // Service 02 replies carry a frame number after the PID: skip it.
        let skip = if req[0] == obd::FREEZE_FRAME { 2 } else { 1 };
        let Some(mask) = data.get(skip..skip + 4) else {
            return Err(CoreError::BadRequest(format!("supported mask {base:02X} was too short")));
        };
        let found = obd::supported_pids(base, mask);
        found_all.extend(found.iter().copied().filter(|p| !p.is_multiple_of(0x20)));
        match base.checked_add(0x20) {
            Some(n) if found.contains(&n) => base = n,
            _ => break,
        }
    }
    Ok(found_all)
}

/// Reads freeze frame 0, if the ECU has one. `None` when no code stored it.
pub fn freeze_frame(client: &mut Client, ecu: CanId) -> Result<Option<FreezeFrame>, CoreError> {
    let data = match client.request_one(ecu, &obd::freeze_frame_request(0x02, 0)) {
        Ok(d) => d,
        Err(CoreError::Refused { .. } | CoreError::NoReply(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let (_, _, bytes) = obd::split_freeze_frame(&data)?;
    let dtc = Dtc::from_bytes(bytes.first().copied().unwrap_or(0), bytes.get(1).copied().unwrap_or(0));
    if dtc.0 == 0 {
        return Ok(None);
    }
    let supported = mask_chain(client, ecu, |base| obd::freeze_frame_request(base, 0)).unwrap_or_default();
    let mut readings = Vec::new();
    for pid in supported.into_iter().filter(|p| *p != 0x02) {
        let Ok(d) = client.request_one(ecu, &obd::freeze_frame_request(pid, 0)) else { continue };
        let Ok((p, _, raw)) = obd::split_freeze_frame(&d) else { continue };
        if p != pid {
            continue;
        }
        readings.push(PidReading {
            pid,
            name: pids::def(pid).map_or("Unknown PID", |x| x.name),
            decoded: pids::decode(pid, &raw),
            raw,
        });
    }
    Ok(Some(FreezeFrame { ecu, dtc, readings }))
}

/// The check engine light, code count and readiness monitors (PID 01).
pub fn readiness(client: &mut Client, ecu: CanId) -> Result<MonitorStatus, CoreError> {
    let r = read_pids(client, ecu, &[0x01])?;
    match r.first() {
        Some(x) if x.raw.len() >= 4 => Ok(MonitorStatus::parse(&x.raw)),
        _ => Err(CoreError::BadRequest("PID 01 reply was too short".into())),
    }
}

/// Service 06 results from one ECU. Empty if the ECU doesn't support service 06.
pub fn test_results(client: &mut Client, ecu: CanId) -> Result<Vec<TestResult>, CoreError> {
    let mids = match mask_chain(client, ecu, obd::test_results_request) {
        Ok(m) => m,
        Err(CoreError::Refused { .. } | CoreError::NoReply(_)) => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for mid in mids {
        let Ok(d) = client.request_one(ecu, &obd::test_results_request(mid)) else { continue };
        if let Ok(results) = mode06::parse_results(&d) {
            out.extend(results.into_iter().filter(|r| r.mid == mid));
        }
    }
    Ok(out)
}

/// Service 09 items from one ECU.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VehicleInfo {
    pub vin: Option<String>,
    pub calibration_id: Option<String>,
    pub ecu_name: Option<String>,
    /// Items the ECU says it supports.
    pub supported: Vec<u8>,
}

pub fn vehicle_info(client: &mut Client, ecu: CanId) -> Result<VehicleInfo, CoreError> {
    let mut info = VehicleInfo::default();
    let mask = match client.request_one(ecu, &obd::vehicle_info_request(0x00)) {
        Ok(d) if d.len() >= 5 => d,
        Ok(_) | Err(CoreError::Refused { .. }) | Err(CoreError::NoReply(_)) => return Ok(info),
        Err(e) => return Err(e),
    };
    info.supported = obd::supported_pids(0x00, &mask[1..5]);
    if info.supported.contains(&0x02) {
        let d = client.request_one(ecu, &obd::vehicle_info_request(0x02))?;
        info.vin = obd::parse_vin(d.get(1..).unwrap_or_default());
    }
    if info.supported.contains(&0x04) {
        let d = client.request_one(ecu, &obd::vehicle_info_request(0x04))?;
        info.calibration_id = Some(obd::parse_text_item(d.get(1..).unwrap_or_default()));
    }
    if info.supported.contains(&0x0A) {
        let d = client.request_one(ecu, &obd::vehicle_info_request(0x0A))?;
        info.ecu_name = Some(obd::parse_text_item(d.get(1..).unwrap_or_default()));
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{DetectOptions, detect};
    use scana_link::sim::SimLink;
    use scana_sim::VirtualCar;

    fn connected(car: VirtualCar) -> (Client, Vec<CanId>) {
        let mut c = Client::new(Box::new(SimLink::new(car)));
        let d = detect(&mut c, &DetectOptions { listen_ms: 100, ..DetectOptions::default() }).unwrap();
        (c, d.ecus)
    }

    #[test]
    fn hijet_pids_match_the_scan() {
        let (mut c, ecus) = connected(VirtualCar::hijet());
        let pids = supported_pids(&mut c, ecus[0]).unwrap();
        let live: Vec<u8> = pids.iter().copied().filter(|p| !pids::is_support_pid(*p)).collect();
        assert_eq!(live.len(), 30);
        let readings = read_pids(&mut c, ecus[0], &live).unwrap();
        assert_eq!(readings.len(), 30);
        let rpm = readings.iter().find(|r| r.pid == 0x0C).unwrap();
        let v = rpm.decoded.as_ref().unwrap().main().unwrap().value;
        assert!((700.0..2600.0).contains(&v));
        let volts = readings.iter().find(|r| r.pid == 0x42).unwrap();
        assert!((volts.decoded.as_ref().unwrap().main().unwrap().value - 14.28).abs() < 0.1);
        // 30 PIDs in requests of up to 6 (several PIDs are 2-4 bytes, but all fit in one reply).
        assert!(c.requests_sent < 20);
    }

    #[test]
    fn codes_from_two_ecus() {
        let (mut c, _) = connected(VirtualCar::two_ecus());
        let stored = read_codes(&mut c, obd::STORED_CODES).unwrap();
        assert_eq!(stored, vec![(CanId::Standard(0x7E8), vec![Dtc(0x0133)]), (CanId::Standard(0x7E9), vec![])]);
        let pending = read_codes(&mut c, obd::PENDING_CODES).unwrap();
        assert_eq!(pending[0].1, vec![Dtc(0x0171)]);
    }

    #[test]
    fn vehicle_info_reads_the_vin() {
        let (mut c, ecus) = connected(VirtualCar::hijet());
        let info = vehicle_info(&mut c, ecus[0]).unwrap();
        assert_eq!(info.vin.as_deref(), Some("SCANASIMHIJET0001"));
        assert_eq!(info.calibration_id.as_deref(), Some("SIMCAL-KF-000001"));
        assert_eq!(info.ecu_name.as_deref(), Some("ECM-EngineControl"));
    }

    #[test]
    fn freeze_frame_readiness_and_tests_from_the_faulty_demo() {
        let (mut c, ecus) = connected(VirtualCar::hijet_with_faults());
        let ff = freeze_frame(&mut c, ecus[0]).unwrap().unwrap();
        assert_eq!(ff.dtc, Dtc(0x0128));
        assert!(ff.readings.len() >= 20);
        let rpm = ff.readings.iter().find(|r| r.pid == 0x0C).unwrap();
        assert!(rpm.decoded.as_ref().unwrap().main().unwrap().value > 1500.0);
        let m = readiness(&mut c, ecus[0]).unwrap();
        assert!(m.mil && m.dtc_count == 1);
        let t = test_results(&mut c, ecus[0]).unwrap();
        assert_eq!(t.len(), 13);
        let cat = t.iter().find(|r| r.mid == 0x21).unwrap();
        assert!(!cat.passed());
        // The healthy demo has no freeze frame.
        let (mut c, ecus) = connected(VirtualCar::hijet());
        assert!(freeze_frame(&mut c, ecus[0]).unwrap().is_none());
    }

    #[test]
    fn readings_with_the_dash_polling() {
        let (mut c, ecus) = connected(VirtualCar::hijet().with_dash());
        for _ in 0..3 {
            let r = read_pids(&mut c, ecus[0], &[0x05, 0x0C, 0x0D, 0x42]).unwrap();
            assert_eq!(r.iter().map(|x| x.pid).collect::<Vec<_>>(), vec![0x05, 0x0C, 0x0D, 0x42]);
        }
        assert!(c.other_tester_active());
    }
}
