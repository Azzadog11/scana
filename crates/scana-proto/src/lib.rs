//! Scana's protocol code: CAN frames, the bridge's SLCAN text protocol, ISO-TP
//! (ISO 15765-2), OBD-II services (SAE J1979) and fault codes (SAE J2012).
//!
//! Nothing here does I/O, starts threads or reads a clock. Time comes in as an
//! argument, so the same code runs in tests, in the simulator and against a car.

pub mod can;
pub mod candump;
pub mod dtc;
pub mod isotp;
pub mod mode06;
pub mod obd;
pub mod pids;
pub mod slcan;

pub use can::{Bitrate, CanId, Frame, FrameError};
pub use dtc::Dtc;
