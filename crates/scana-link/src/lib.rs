//! Links to a CAN bus. Every link looks the same to the rest of Scana ([`Link`]):
//!
//! - [`serial::SlcanLink`]: the scana-bridge (Arduino Uno + SparkFun CAN-BUS Shield) over USB.
//! - [`sim::SimLink`]: a simulated car, for tests, the demo car and `--sim`.

pub mod serial;
pub mod sim;

use scana_proto::{Bitrate, Frame};
use std::time::Duration;

pub use scana_proto::slcan::{Event as BusEvent, Health, OpenMode};

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("no Scana bridge found. Is the Arduino plugged in by USB?")]
    NotFound,
    #[error(
        "permission denied opening {0}. Add yourself to the uucp group \
         (sudo usermod -aG uucp $USER), then log out and back in."
    )]
    Permission(String),
    #[error("can't open {path}: {reason}")]
    Open { path: String, reason: String },
    #[error("{0} answered, but not like a Scana bridge. Is scana-bridge flashed on the Uno?")]
    NotABridge(String),
    #[error("the bridge didn't answer within {0} ms")]
    NoAnswer(u64),
    #[error("the bridge refused to {0}")]
    Refused(String),
    #[error("the bridge was unplugged")]
    Disconnected,
    #[error("unexpected reply from the bridge: {0}")]
    Unexpected(String),
    #[error("serial port error: {0}")]
    Io(String),
}

/// The bridge's self-test: `Ok(())`, or the failed step and the value read there.
pub type SelfTestResult = Result<(), (u8, u8)>;

/// A frame from the bus, with when it arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedFrame {
    pub frame: Frame,
    /// Milliseconds since the link was opened (host clock).
    pub at_ms: u64,
    /// The bridge's own microsecond timestamp, if it sent one.
    pub bridge_us: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkEvent {
    Frame(TimedFrame),
    /// Bus trouble reported by the bridge: errors, bus-off, a frame not acknowledged.
    Bus(BusEvent),
    /// The bridge (re)started. Mid-session this means it reset: the bus is closed again.
    BridgeReady {
        version: String,
        chip_ok: bool,
    },
}

/// A connection to a CAN bus.
pub trait Link: Send {
    /// What this link is, for messages: "scana-bridge 0.1.0 on /dev/ttyACM0".
    fn describe(&self) -> String;
    /// Milliseconds since the link was opened.
    fn now_ms(&self) -> u64;
    /// Only while the bus is closed.
    fn set_bitrate(&mut self, bitrate: Bitrate) -> Result<(), LinkError>;
    fn bitrate(&self) -> Bitrate;
    fn open(&mut self, mode: OpenMode) -> Result<(), LinkError>;
    fn close(&mut self) -> Result<(), LinkError>;
    fn mode(&self) -> Option<OpenMode>;
    /// Queues a frame. Only in normal or loopback mode.
    fn send(&mut self, frame: &Frame) -> Result<(), LinkError>;
    /// The next frame or event, waiting at most `timeout`.
    fn recv(&mut self, timeout: Duration) -> Result<Option<LinkEvent>, LinkError>;
    /// The bridge's counters, if this link has them.
    fn health(&mut self) -> Result<Option<Health>, LinkError> {
        Ok(None)
    }
    /// The bridge's self-test (bus must be closed): `Ok(())` or the failed step and value read.
    fn self_test(&mut self) -> Result<Option<SelfTestResult>, LinkError> {
        Ok(None)
    }
}
