//! A link to a simulated car ([`scana_sim::VirtualCar`]). It behaves like the bridge:
//! the bus starts closed, listen-only never transmits, loopback echoes, and a wrong bit
//! rate gives bus errors instead of frames.

use crate::{BusEvent, Health, Link, LinkError, LinkEvent, OpenMode, SelfTestResult, TimedFrame};
use scana_proto::{Bitrate, Frame};
use scana_sim::VirtualCar;
use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant};

pub struct SimLink {
    car: VirtualCar,
    start: Instant,
    bitrate: Bitrate,
    mode: Option<OpenMode>,
    queue: VecDeque<LinkEvent>,
    last_error_event_ms: Option<u64>,
    rx: u32,
    tx: u32,
    tx_errors: u32,
}

impl SimLink {
    pub fn new(car: VirtualCar) -> Self {
        SimLink {
            car,
            start: Instant::now(),
            bitrate: Bitrate::K500,
            mode: None,
            queue: VecDeque::new(),
            last_error_event_ms: None,
            rx: 0,
            tx: 0,
            tx_errors: 0,
        }
    }

    pub fn car_mut(&mut self) -> &mut VirtualCar {
        &mut self.car
    }

    fn pump(&mut self, now: u64) {
        let frames = self.car.poll(now);
        match self.mode {
            Some(OpenMode::Normal | OpenMode::ListenOnly) if self.bitrate == self.car.bitrate => {
                for frame in frames {
                    self.rx += 1;
                    self.queue.push_back(LinkEvent::Frame(TimedFrame { frame, at_ms: now, bridge_us: None }));
                }
            }
            // Wrong bit rate: the controller sees garbage and counts receive errors.
            Some(OpenMode::Normal | OpenMode::ListenOnly)
                if !frames.is_empty() && self.last_error_event_ms.is_none_or(|t| now >= t + 100) =>
            {
                self.last_error_event_ms = Some(now);
                self.queue.push_back(LinkEvent::Bus(BusEvent::ErrorPassive));
            }
            _ => {}
        }
    }
}

impl Link for SimLink {
    fn describe(&self) -> String {
        format!(
            "simulated car ({} ECU{}, {}, {})",
            self.car.ecus.len(),
            if self.car.ecus.len() == 1 { "" } else { "s" },
            self.car.bitrate,
            if self.car.extended_ids { "29-bit IDs" } else { "11-bit IDs" }
        )
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn set_bitrate(&mut self, bitrate: Bitrate) -> Result<(), LinkError> {
        if self.mode.is_some() {
            return Err(LinkError::Refused("change the bit rate (close the bus first)".into()));
        }
        self.bitrate = bitrate;
        Ok(())
    }

    fn bitrate(&self) -> Bitrate {
        self.bitrate
    }

    fn open(&mut self, mode: OpenMode) -> Result<(), LinkError> {
        if self.mode.is_some() {
            return Err(LinkError::Refused("open the bus (it is already open)".into()));
        }
        self.mode = Some(mode);
        self.last_error_event_ms = None;
        Ok(())
    }

    fn close(&mut self) -> Result<(), LinkError> {
        self.mode = None;
        Ok(())
    }

    fn mode(&self) -> Option<OpenMode> {
        self.mode
    }

    fn send(&mut self, frame: &Frame) -> Result<(), LinkError> {
        let now = self.now_ms();
        match self.mode {
            Some(OpenMode::Normal) => {
                self.pump(now);
                if self.bitrate == self.car.bitrate {
                    self.tx += 1;
                    self.car.receive(frame, now);
                } else {
                    // Nobody acknowledges a frame at the wrong bit rate.
                    self.tx_errors += 1;
                    self.queue.push_back(LinkEvent::Bus(BusEvent::TxError));
                }
                Ok(())
            }
            Some(OpenMode::Loopback) => {
                self.tx += 1;
                self.queue.push_back(LinkEvent::Frame(TimedFrame { frame: *frame, at_ms: now, bridge_us: None }));
                Ok(())
            }
            _ => Err(LinkError::Refused(
                "send a frame (the bus isn't open in normal mode, or the send queue is full)".into(),
            )),
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<LinkEvent>, LinkError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            let now = self.now_ms();
            self.pump(now);
            if !self.queue.is_empty() {
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let until_due =
                self.car.next_due().map(|t| Duration::from_millis(t.saturating_sub(now).max(1))).unwrap_or(left);
            thread::sleep(left.min(until_due).min(Duration::from_millis(20)));
        }
    }

    fn health(&mut self) -> Result<Option<Health>, LinkError> {
        Ok(Some(Health {
            uptime_s: (self.now_ms() / 1000) as u32,
            mode: match self.mode {
                None => 'C',
                Some(OpenMode::Normal) => 'N',
                Some(OpenMode::ListenOnly) => 'L',
                Some(OpenMode::Loopback) => 'K',
            },
            rate_code: self.bitrate.slcan_code(),
            rx: self.rx,
            tx: self.tx,
            tx_errors: self.tx_errors,
            ..Health::default()
        }))
    }

    fn self_test(&mut self) -> Result<Option<SelfTestResult>, LinkError> {
        Ok(Some(Ok(())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_proto::CanId;

    fn rpm_request() -> Frame {
        Frame::new(CanId::Standard(0x7DF), &[0x02, 0x01, 0x0C, 0, 0, 0, 0, 0]).unwrap()
    }

    #[test]
    fn closed_and_listen_only_refuse_to_send() {
        let mut l = SimLink::new(VirtualCar::hijet());
        assert!(l.send(&rpm_request()).is_err());
        l.open(OpenMode::ListenOnly).unwrap();
        assert!(l.send(&rpm_request()).is_err());
        assert!(l.set_bitrate(Bitrate::K250).is_err());
    }

    #[test]
    fn normal_mode_gets_a_reply() {
        let mut l = SimLink::new(VirtualCar::hijet());
        l.open(OpenMode::Normal).unwrap();
        l.send(&rpm_request()).unwrap();
        match l.recv(Duration::from_millis(200)).unwrap() {
            Some(LinkEvent::Frame(f)) => assert_eq!(f.frame.id, CanId::Standard(0x7E8)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wrong_bitrate_gives_errors() {
        let mut l = SimLink::new(VirtualCar::hijet());
        l.set_bitrate(Bitrate::K250).unwrap();
        l.open(OpenMode::Normal).unwrap();
        l.send(&rpm_request()).unwrap();
        assert_eq!(l.recv(Duration::from_millis(100)).unwrap(), Some(LinkEvent::Bus(BusEvent::TxError)));
        assert_eq!(l.recv(Duration::from_millis(50)).unwrap(), None);
        // Listen-only at the wrong rate on a busy bus: receive errors.
        let mut l = SimLink::new(VirtualCar::hijet().with_dash());
        l.set_bitrate(Bitrate::K250).unwrap();
        l.open(OpenMode::ListenOnly).unwrap();
        assert_eq!(l.recv(Duration::from_millis(200)).unwrap(), Some(LinkEvent::Bus(BusEvent::ErrorPassive)));
    }

    #[test]
    fn loopback_echoes() {
        let mut l = SimLink::new(VirtualCar::hijet());
        l.open(OpenMode::Loopback).unwrap();
        l.send(&rpm_request()).unwrap();
        match l.recv(Duration::from_millis(50)).unwrap() {
            Some(LinkEvent::Frame(f)) => assert_eq!(f.frame, rpm_request()),
            other => panic!("{other:?}"),
        }
    }
}
