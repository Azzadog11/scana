//! Talking to a car over any [`scana_link::Link`]: OBD-II requests and replies, finding the
//! bit rate and ECUs, and the bus manners in `docs/DESIGN.md` section 7.

pub mod client;
pub mod describe;
pub mod detect;
pub mod poller;
pub mod readers;
pub mod recorder;
pub mod report;
pub mod safety;
pub mod session;
pub mod store;

pub use client::{Addressing, Client, CoreError, EcuReply, FrameSink, Options, Target};
pub use detect::{DetectOptions, Detected, detect};
