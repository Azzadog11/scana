//! Safety level 1: clearing fault codes. Clearing also erases freeze frames and resets
//! the readiness monitors, so it needs the user's go-ahead, and Scana checks the engine is off
//! and the car is still before it sends anything.

use crate::client::{Client, CoreError, Target};
use crate::readers;
use scana_proto::obd::{self, Reply};
use scana_proto::{CanId, Dtc};

/// What clearing codes loses, for the confirmation the user reads first.
pub const CLEAR_WARNING: &[&str] = &[
    "Every stored and pending code, on every engine-related computer.",
    "The freeze frames: the record of the conditions when each code was stored.",
    "The readiness monitors: they reset to not complete until the car has been driven enough.",
    "Permanent codes stay until the car's own tests pass again.",
];

/// The user's go-ahead to clear codes. Only made after showing [`CLEAR_WARNING`].
#[derive(Debug)]
pub struct ClearConsent {
    _private: (),
}

impl ClearConsent {
    /// Call only when the user has read the warning and confirmed.
    pub fn confirmed_by_user() -> Self {
        ClearConsent { _private: () }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClearOutcome {
    /// ECUs that confirmed the clear.
    pub cleared: Vec<CanId>,
    /// ECUs that refused, with the reason in plain words.
    pub refused: Vec<(CanId, String)>,
    /// Stored codes read again afterwards.
    pub codes_after: Vec<(CanId, Vec<Dtc>)>,
}

/// Checks the engine is off and the car is still, using only PIDs each ECU supports.
pub fn check_engine_off(client: &mut Client, ecus: &[(CanId, Vec<u8>)]) -> Result<(), CoreError> {
    let mut checked = false;
    for (ecu, supported) in ecus {
        let want: Vec<u8> = [0x0C, 0x0D].into_iter().filter(|p| supported.contains(p)).collect();
        if want.is_empty() {
            continue;
        }
        for r in readers::read_pids(client, *ecu, &want)? {
            let v = r.decoded.as_ref().and_then(|d| d.main()).map_or(0.0, |q| q.value);
            if r.pid == 0x0C && v > 0.0 {
                return Err(CoreError::Unsafe(format!(
                    "The engine is running ({v:.0} rpm). Turn the engine off, leave the key on, and try again."
                )));
            }
            if r.pid == 0x0D && v > 0.0 {
                return Err(CoreError::Unsafe(format!("The car is moving ({v:.0} km/h). Stop first.")));
            }
            checked = true;
        }
    }
    if !checked {
        return Err(CoreError::Unsafe(
            "Scana can't confirm the engine is off: no ECU reports engine speed. Codes were not cleared.".into(),
        ));
    }
    Ok(())
}

/// Clears codes on every OBD ECU, after the checks. `ecus` lists each ECU with its supported PIDs.
pub fn clear_codes(
    client: &mut Client,
    ecus: &[(CanId, Vec<u8>)],
    _consent: ClearConsent,
) -> Result<ClearOutcome, CoreError> {
    check_engine_off(client, ecus)?;
    let replies = client.request(Target::AllEcus, &[obd::CLEAR_CODES])?;
    let mut out = ClearOutcome { cleared: vec![], refused: vec![], codes_after: vec![] };
    for r in replies {
        match r.reply {
            Reply::Positive { .. } => out.cleared.push(r.ecu),
            Reply::Negative { code, .. } => out.refused.push((r.ecu, obd::negative_code_text(code).to_string())),
        }
    }
    out.codes_after = readers::read_codes(client, obd::STORED_CODES)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{DetectOptions, detect};
    use scana_link::sim::SimLink;
    use scana_sim::VirtualCar;

    fn connected(car: VirtualCar) -> (Client, Vec<(CanId, Vec<u8>)>) {
        let mut c = Client::new(Box::new(SimLink::new(car)));
        let d = detect(&mut c, &DetectOptions { listen_ms: 100, ..DetectOptions::default() }).unwrap();
        let ecus = d.ecus.iter().map(|e| (*e, readers::supported_pids(&mut c, *e).unwrap())).collect();
        (c, ecus)
    }

    #[test]
    fn refuses_with_the_engine_running() {
        let (mut c, ecus) = connected(VirtualCar::hijet_with_faults());
        let e = clear_codes(&mut c, &ecus, ClearConsent::confirmed_by_user()).unwrap_err();
        assert!(matches!(e, CoreError::Unsafe(ref m) if m.contains("engine is running")), "{e}");
        // Nothing was sent: the code is still there.
        assert_eq!(readers::read_codes(&mut c, obd::STORED_CODES).unwrap()[0].1, vec![Dtc(0x0128)]);
    }

    #[test]
    fn clears_with_the_engine_off() {
        let mut car = VirtualCar::hijet_with_faults();
        car.set_engine_running(false);
        let (mut c, ecus) = connected(car);
        let out = clear_codes(&mut c, &ecus, ClearConsent::confirmed_by_user()).unwrap();
        assert_eq!(out.cleared, vec![CanId::Standard(0x7E8)]);
        assert!(out.refused.is_empty());
        assert_eq!(out.codes_after, vec![(CanId::Standard(0x7E8), vec![])]);
        let m = readers::readiness(&mut c, CanId::Standard(0x7E8)).unwrap();
        assert!(!m.mil && m.monitors.iter().any(|x| x.available && !x.complete));
    }

    #[test]
    fn refuses_when_nothing_reports_engine_speed() {
        let (mut c, _) = connected(VirtualCar::hijet_with_faults());
        let fake = vec![(CanId::Standard(0x7E8), vec![0x05])];
        assert!(matches!(check_engine_off(&mut c, &fake), Err(CoreError::Unsafe(_))));
    }
}
