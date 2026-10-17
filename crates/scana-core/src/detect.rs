//! Finding the car's bit rate, addressing and OBD ECUs, gently:
//!
//! 1. Listen-only at each bit rate. Frames with no errors confirm a rate; errors rule it out.
//! 2. On a quiet bus (common: many OBD ports are silent until a tester asks), send one
//!    standard request (01 00) at a time, following ISO 15765-4: 11-bit, then 29-bit, for
//!    each rate not ruled out. The first frame nobody acknowledges ends that rate.
//!
//! The second bus (OBD pins 3 and 11) is found by [`detect_second_bus`], which only listens:
//! 125, then 500, then 250 kbit/s. It never sends anything.

use crate::client::{Addressing, Client, CoreError, Target};
use scana_link::{BusEvent, LinkEvent, OpenMode};
use scana_proto::{Bitrate, Bus, CanId};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct DetectOptions {
    /// How long to listen at each bit rate.
    pub listen_ms: u64,
    /// Bit rates to try, in order. OBD-II on CAN uses 500 or 250 kbit/s.
    pub bitrates: Vec<Bitrate>,
    /// Allowed to send the 01 00 request. Off: listen only.
    pub transmit: bool,
}

impl Default for DetectOptions {
    fn default() -> Self {
        DetectOptions { listen_ms: 800, bitrates: vec![Bitrate::K500, Bitrate::K250], transmit: true }
    }
}

#[derive(Clone, Debug)]
pub struct Detected {
    /// Which pins this was found on.
    pub bus: Bus,
    pub bitrate: Bitrate,
    pub addressing: Addressing,
    /// The IDs the OBD ECUs reply from.
    pub ecus: Vec<CanId>,
    /// Traffic was heard while listening.
    pub traffic_seen: bool,
    /// Another tester (such as a dash display) was heard.
    pub other_tester: bool,
    /// What was tried, in plain words.
    pub log: Vec<String>,
}

struct Heard {
    frames: u32,
    errors: u32,
    extended: u32,
}

fn listen(client: &mut Client, ms: u64) -> Result<Heard, CoreError> {
    let mut h = Heard { frames: 0, errors: 0, extended: 0 };
    let end = client.now_ms() + ms;
    loop {
        let now = client.now_ms();
        if now >= end {
            return Ok(h);
        }
        match client.link().recv(Duration::from_millis(end - now))? {
            Some(LinkEvent::Frame(tf)) => {
                h.frames += 1;
                if tf.frame.id.is_extended() {
                    h.extended += 1;
                }
                client.note_frame(&tf);
            }
            Some(LinkEvent::Bus(BusEvent::ErrorWarning | BusEvent::ErrorPassive | BusEvent::BusOff)) => h.errors += 1,
            Some(LinkEvent::BridgeReady { version, reset, .. }) => {
                return Err(client.bridge_restarted(&version, &reset));
            }
            Some(LinkEvent::Bus(_)) | None => {}
        }
    }
}

/// Finds the car. On success the bus is left open in normal mode, ready for requests.
pub fn detect(client: &mut Client, opts: &DetectOptions) -> Result<Detected, CoreError> {
    let mut log = Vec::new();
    let mut confirmed: Option<(Bitrate, bool)> = None;
    let mut quiet: Vec<Bitrate> = Vec::new();
    let mut traffic_seen = false;
    if client.link().mode().is_some() {
        client.link().close()?;
    }
    for &b in &opts.bitrates {
        client.link().set_bitrate(b)?;
        client.link().open(OpenMode::ListenOnly)?;
        let heard = listen(client, opts.listen_ms);
        client.link().close()?;
        let heard = heard?;
        log.push(format!(
            "Listened at {b} for {} ms: {} frames, {} errors.",
            opts.listen_ms, heard.frames, heard.errors
        ));
        if heard.frames > 0 {
            traffic_seen = true;
        }
        if heard.frames > 0 && heard.errors == 0 {
            confirmed = Some((b, heard.extended > heard.frames / 2));
            break;
        }
        if heard.frames == 0 && heard.errors == 0 {
            quiet.push(b);
        } else {
            log.push(format!("Errors at {b}: not this bit rate."));
        }
    }
    let other_tester = client.foreign_requests > 0;
    if other_tester {
        log.push("Another tester is sending requests (for example a dash display).".into());
    }

    if !opts.transmit {
        return match confirmed {
            Some((bitrate, ext)) => Ok(Detected {
                bus: Bus::Main,
                bitrate,
                addressing: if ext { Addressing::Extended } else { Addressing::Standard },
                ecus: vec![],
                traffic_seen,
                other_tester,
                log,
            }),
            None => Err(CoreError::NoCar(format!("Listen only: no clean traffic heard. {}", log.join(" ")))),
        };
    }

    let mut order: Vec<Bitrate> = confirmed.map(|(b, _)| b).into_iter().collect();
    order.extend(quiet.iter().copied().filter(|q| confirmed.is_none_or(|(b, _)| b != *q)));
    for b in order {
        for addressing in [Addressing::Standard, Addressing::Extended] {
            client.link().set_bitrate(b)?;
            client.link().open(OpenMode::Normal)?;
            client.addressing = addressing;
            client.expected_ecus = None;
            match client.request(Target::AllEcus, &[0x01, 0x00]) {
                Ok(replies) if !replies.is_empty() => {
                    let mut ecus: Vec<CanId> = replies.iter().map(|r| r.ecu).collect();
                    ecus.dedup();
                    log.push(format!(
                        "At {b} with {}: {} ECU{} answered ({}).",
                        addressing.name(),
                        ecus.len(),
                        if ecus.len() == 1 { "" } else { "s" },
                        ecus.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", ")
                    ));
                    client.expected_ecus = Some(ecus.clone());
                    return Ok(Detected {
                        bus: Bus::Main,
                        bitrate: b,
                        addressing,
                        ecus,
                        traffic_seen,
                        other_tester,
                        log,
                    });
                }
                Ok(_) => {
                    log.push(format!(
                        "At {b} with {}: the request was acknowledged, but no ECU answered.",
                        addressing.name()
                    ));
                    client.link().close()?;
                }
                Err(CoreError::NotAcknowledged) => {
                    log.push(format!("At {b}: nobody acknowledged the request, so not this bit rate (or no car)."));
                    client.link().close()?;
                    break;
                }
                Err(e) => {
                    let _ = client.link().close();
                    return Err(e);
                }
            }
        }
    }
    Err(CoreError::NoCar(format!(
        "No car answered. Check that the OBD plug is in, the key is on (ACC or ON), and the DB9 is \
         screwed to the shield. What was tried: {}",
        log.join(" ")
    )))
}

/// The bit rates tried on the second bus, in order: Ford's medium-speed bus is 125 kbit/s;
/// some newer cars may use 500 or 250.
pub const SECOND_BUS_RATES: [Bitrate; 3] = [Bitrate::K125, Bitrate::K500, Bitrate::K250];

/// How long to listen at each rate on the second bus.
pub const SECOND_BUS_LISTEN_MS: u64 = 1000;

/// Finds the bit rate of the second bus (OBD pins 3 and 11) by listening only. It never
/// transmits: not a request, not an acknowledgement of a frame it sent (the bus is only ever
/// open listen-only here). The first rate with clean traffic (frames, no errors) wins, and
/// the bus is left open listen-only at that rate. No OBD ECUs are looked for: nothing on this
/// bus is known to answer OBD-II, and Scana sends no OBD-II request there at all.
pub fn detect_second_bus(client: &mut Client, listen_ms: u64) -> Result<Detected, CoreError> {
    client.set_bus(Bus::Second);
    let mut log = Vec::new();
    let mut any_traffic = false;
    let mut any_errors = false;
    if client.link().mode().is_some() {
        client.link().close()?;
    }
    for b in SECOND_BUS_RATES {
        client.link().set_bitrate(b)?;
        client.link().open(OpenMode::ListenOnly)?;
        let heard = listen(client, listen_ms);
        client.link().close()?;
        let heard = heard?;
        log.push(format!("Listened at {b} for {listen_ms} ms: {} frames, {} errors.", heard.frames, heard.errors));
        any_traffic |= heard.frames > 0;
        any_errors |= heard.errors > 0;
        if heard.frames > 0 && heard.errors == 0 {
            client.link().set_bitrate(b)?;
            client.link().open(OpenMode::ListenOnly)?;
            let other_tester = client.foreign_requests > 0;
            return Ok(Detected {
                bus: Bus::Second,
                bitrate: b,
                addressing: if heard.extended > heard.frames / 2 { Addressing::Extended } else { Addressing::Standard },
                ecus: vec![],
                traffic_seen: true,
                other_tester,
                log,
            });
        }
        if heard.errors > 0 {
            log.push(format!("Errors at {b}: not this bit rate."));
        }
    }
    Err(if any_traffic || any_errors { CoreError::OnlyErrors } else { CoreError::NothingHeard })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_link::sim::SimLink;
    use scana_proto::Frame;
    use scana_sim::VirtualCar;

    fn fast() -> DetectOptions {
        DetectOptions { listen_ms: 150, ..DetectOptions::default() }
    }

    #[test]
    fn finds_the_hijet_on_a_quiet_bus() {
        let mut c = Client::new(Box::new(SimLink::new(VirtualCar::hijet())));
        let d = detect(&mut c, &fast()).unwrap();
        assert_eq!(d.bitrate, Bitrate::K500);
        assert_eq!(d.addressing, Addressing::Standard);
        assert_eq!(d.ecus, vec![CanId::Standard(0x7E8)]);
        assert!(!d.traffic_seen && !d.other_tester);
        assert_eq!(c.link().mode(), Some(OpenMode::Normal));
    }

    #[test]
    fn traffic_confirms_the_rate_and_shows_the_dash() {
        let mut c = Client::new(Box::new(SimLink::new(VirtualCar::hijet().with_dash())));
        let d = detect(&mut c, &fast()).unwrap();
        assert!(d.traffic_seen && d.other_tester);
        assert_eq!(d.bitrate, Bitrate::K500);
    }

    #[test]
    fn finds_250k_and_29_bit() {
        let car = VirtualCar::two_ecus().extended().at_bitrate(Bitrate::K250);
        let mut c = Client::new(Box::new(SimLink::new(car)));
        let d = detect(&mut c, &fast()).unwrap();
        assert_eq!(d.bitrate, Bitrate::K250);
        assert_eq!(d.addressing, Addressing::Extended);
        assert_eq!(d.ecus.len(), 2);
        // 500k was tried once, unacknowledged, and abandoned.
        assert!(d.log.iter().any(|l| l.contains("500 kbit/s: nobody acknowledged")));
    }

    #[test]
    fn busy_bus_at_the_wrong_rate_is_never_transmitted_on() {
        // The car runs at 250k with traffic; listening at 500k shows errors, so no request at 500k.
        let car = VirtualCar::hijet().with_dash().at_bitrate(Bitrate::K250);
        let mut c = Client::new(Box::new(SimLink::new(car)));
        let d = detect(&mut c, &fast()).unwrap();
        assert_eq!(d.bitrate, Bitrate::K250);
        assert!(d.log.iter().any(|l| l.contains("Errors at 500 kbit/s")));
        assert!(!d.log.iter().any(|l| l.contains("500 kbit/s: nobody acknowledged")));
    }

    #[test]
    fn listen_only_never_sends() {
        let mut c = Client::new(Box::new(SimLink::new(VirtualCar::hijet())));
        let opts = DetectOptions { transmit: false, ..fast() };
        assert!(matches!(detect(&mut c, &opts), Err(CoreError::NoCar(_))));
        assert_eq!(c.requests_sent, 0);
    }

    /// A client that keeps every frame it sends, to prove a test transmitted nothing.
    #[derive(Default)]
    struct Outgoing(std::sync::Arc<std::sync::Mutex<Vec<Frame>>>);

    impl crate::client::FrameSink for Outgoing {
        fn frame(&mut self, frame: &Frame, _at_ms: u64, outgoing: bool) {
            if outgoing && let Ok(mut v) = self.0.lock() {
                v.push(*frame);
            }
        }
    }

    fn second_bus_client(car: VirtualCar) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<Frame>>>) {
        let mut c = Client::new(Box::new(SimLink::new(car).on_bus(Bus::Second)));
        let sink = Outgoing::default();
        let sent = sink.0.clone();
        c.set_sink(Box::new(sink));
        (c, sent)
    }

    #[test]
    fn the_second_bus_is_found_at_125k_and_nothing_is_sent() {
        let (mut c, sent) = second_bus_client(VirtualCar::ms_can_ford());
        let d = detect_second_bus(&mut c, 150).unwrap();
        assert_eq!((d.bus, d.bitrate, d.addressing), (Bus::Second, Bitrate::K125, Addressing::Standard));
        assert!(d.ecus.is_empty() && d.traffic_seen);
        // Left open listen-only at the found rate.
        assert_eq!(c.link().mode(), Some(OpenMode::ListenOnly));
        assert_eq!(c.link().bitrate(), Bitrate::K125);
        // Not one frame went out: no request, and the link's own counters agree.
        assert_eq!(c.requests_sent, 0);
        assert!(sent.lock().unwrap().is_empty());
        let h = c.link().health().unwrap().unwrap();
        assert_eq!((h.tx, h.tx_errors, h.bad_commands), (0, 0, 0));
        // The IDs heard are the car's, none of them an OBD-II request or reply.
        assert!(c.seen_ids().len() >= 4);
        assert!(c.seen_ids().iter().all(|id| !Addressing::Standard.is_reply_id(*id)));
    }

    #[test]
    fn a_second_bus_at_500k_is_found_after_trying_125k() {
        let (mut c, sent) = second_bus_client(VirtualCar::ms_can_ford().at_bitrate(Bitrate::K500));
        let d = detect_second_bus(&mut c, 150).unwrap();
        assert_eq!(d.bitrate, Bitrate::K500);
        assert!(d.log.iter().any(|l| l.contains("Errors at 125 kbit/s")));
        assert!(sent.lock().unwrap().is_empty());
    }

    #[test]
    fn a_quiet_second_bus_says_nothing_was_heard() {
        // A car with no bus on pins 3 and 11 (or the wrong pins): silence at every rate.
        let (mut c, sent) = second_bus_client(VirtualCar::hijet());
        let e = detect_second_bus(&mut c, 100).unwrap_err();
        assert!(matches!(e, CoreError::NothingHeard), "{e}");
        assert_eq!(
            e.to_string(),
            "Nothing heard on pins 3 and 11. This car may not have a bus there, the ignition may be off \
             (switch it on), or the cable or switch is set to pins 6 and 14."
        );
        assert!(sent.lock().unwrap().is_empty());
        assert_eq!(c.link().mode(), None);
    }

    #[test]
    fn a_second_bus_with_only_errors_says_so() {
        // 100 kbit/s is none of the three rates tried, so every rate shows errors only.
        let (mut c, sent) = second_bus_client(VirtualCar::ms_can_ford().at_bitrate(Bitrate::K100));
        let e = detect_second_bus(&mut c, 150).unwrap_err();
        assert!(matches!(e, CoreError::OnlyErrors), "{e}");
        assert_eq!(
            e.to_string(),
            "Pins 3 and 11 carry something Scana can't read at 125, 250 or 500 kbit/s. The two wires may be \
             swapped (CAN high is pin 3, CAN low pin 11), or it is another kind of bus (GM's single-wire bus \
             on pin 1 needs other hardware)."
        );
        assert!(sent.lock().unwrap().is_empty());
    }
}
