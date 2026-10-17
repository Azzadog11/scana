//! The OBD-II client: sends a request (as ISO-TP), collects the replies that answer it, and
//! keeps to the bus manners: a gap between requests, wider when another tester (such as a
//! dash display) is on the bus, and a stop at the first transmit error.

use scana_link::{BusEvent, Link, LinkError, LinkEvent, TimedFrame};
use scana_proto::isotp::{self, FlowControl, Received, Receiver, SendStep, Sender};
use scana_proto::obd::{self, Reply};
use scana_proto::{CanId, Frame, uds};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error("{0}")]
    NoCar(String),
    #[error(
        "the frame wasn't acknowledged: no car on the bus, the wrong bit rate, or the key is off. \
         Scana stopped sending"
    )]
    NotAcknowledged,
    #[error("bus-off: the CAN controller saw too many errors and stopped. Scana stopped sending")]
    BusOff,
    #[error("the bridge restarted, so the bus is closed. Connect again")]
    BridgeReset,
    #[error("the bridge closed the bus because Scana went quiet for too long")]
    WatchdogClosed,
    #[error("{ecu} refused the {service} request: {text}")]
    Refused { ecu: CanId, service: &'static str, code: u8, text: &'static str },
    #[error("no reply from {0}")]
    NoReply(String),
    /// A safety check said no (safety level 1 and up).
    #[error("{0}")]
    Unsafe(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error(transparent)]
    IsoTp(#[from] isotp::IsoTpError),
    #[error(transparent)]
    Obd(#[from] obd::ObdError),
}

/// 11-bit or 29-bit OBD-II addressing (ISO 15765-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressing {
    Standard,
    Extended,
}

impl Addressing {
    /// "Every ECU": 0x7DF or 0x18DB33F1.
    pub fn functional_id(self) -> CanId {
        match self {
            Addressing::Standard => CanId::Standard(0x7DF),
            Addressing::Extended => CanId::Extended(0x18DB_33F1),
        }
    }

    /// True for IDs ECUs reply from: 0x7E8-0x7EF or 0x18DAF1xx.
    pub fn is_reply_id(self, id: CanId) -> bool {
        match (self, id) {
            (Addressing::Standard, CanId::Standard(x)) => (0x7E8..=0x7EF).contains(&x),
            (Addressing::Extended, CanId::Extended(x)) => x & 0xFFFF_FF00 == 0x18DA_F100,
            _ => false,
        }
    }

    /// True for IDs testers send requests on: 0x7DF, 0x7E0-0x7E7, 0x18DB33F1, 0x18DAxxF1.
    pub fn is_request_id(id: CanId) -> bool {
        match id {
            CanId::Standard(x) => x == 0x7DF || (0x7E0..=0x7E7).contains(&x),
            CanId::Extended(x) => x == 0x18DB_33F1 || x & 0xFFFF_00FF == 0x18DA_00F1,
        }
    }

    /// The ID to send to for the ECU that replies from `reply`.
    pub fn request_id_for(reply: CanId) -> Option<CanId> {
        match reply {
            CanId::Standard(x) if (0x7E8..=0x7EF).contains(&x) => Some(CanId::Standard(x - 8)),
            CanId::Extended(x) if x & 0xFFFF_FF00 == 0x18DA_F100 => {
                Some(CanId::Extended(0x18DA_00F1 | ((x & 0xFF) << 8)))
            }
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Addressing::Standard => "11-bit IDs",
            Addressing::Extended => "29-bit IDs",
        }
    }
}

/// Where one module listens and answers: the request ID, the reply ID, and the address byte
/// in front of every frame when the module sits behind a gateway (ISO 15765-2 extended
/// addressing; Toyota-family cars use 0x750 with an address byte, replies from 0x758).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Channel {
    pub tx: CanId,
    pub rx: CanId,
    pub ext: Option<u8>,
}

impl Channel {
    pub fn new(tx: CanId, rx: CanId, ext: Option<u8>) -> Self {
        Channel { tx, rx, ext }
    }

    /// The usual reply ID for a request ID: request + 8 for 11-bit IDs, or source and target
    /// swapped for 29-bit `18DAttss` IDs.
    pub fn conventional(tx: CanId, ext: Option<u8>) -> Option<Self> {
        let rx = match tx {
            CanId::Standard(x) if x <= 0x7F7 => CanId::Standard(x + 8),
            CanId::Extended(x) if x & 0xFFFF_0000 == 0x18DA_0000 => {
                CanId::Extended(0x18DA_0000 | ((x & 0xFF) << 8) | ((x >> 8) & 0xFF))
            }
            _ => return None,
        };
        Some(Channel { tx, rx, ext })
    }

    /// The channel of an OBD-II ECU, named by the ID it replies from (0x7E8 ...).
    pub fn obd(reply: CanId) -> Option<Self> {
        Addressing::request_id_for(reply).map(|tx| Channel { tx, rx: reply, ext: None })
    }

    /// True for an OBD-II ECU's channel (7E0-7E7, or 18DAxxF1 with its usual reply).
    pub fn is_obd(&self) -> bool {
        self.ext.is_none() && Addressing::request_id_for(self.rx) == Some(self.tx)
    }

    /// Bytes per frame for ISO-TP: one less with an address byte.
    pub fn frame_len(&self) -> usize {
        if self.ext.is_some() { 7 } else { 8 }
    }

    /// "7B0", "750/40" or "18DA28F1": the request ID, and the address byte after a slash.
    pub fn label(&self) -> String {
        match self.ext {
            Some(a) => format!("{}/{a:02X}", self.tx),
            None => self.tx.to_string(),
        }
    }

    /// Parses "7B0", "0x7B0", "750/40", "18DA28F1", or "7B0:7B8" with an explicit reply ID.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_uppercase().replace("0X", "");
        let (ids, ext) = match s.split_once('/') {
            Some((a, b)) => (a.to_string(), Some(u8::from_str_radix(b, 16).ok()?)),
            None => (s.clone(), None),
        };
        let id_of = |t: &str| -> Option<CanId> {
            let v = u32::from_str_radix(t, 16).ok()?;
            if t.len() <= 3 { CanId::standard(u16::try_from(v).ok()?) } else { CanId::extended(v) }
        };
        match ids.split_once(':') {
            Some((tx, rx)) => Some(Channel { tx: id_of(tx)?, rx: id_of(rx)?, ext }),
            None => Channel::conventional(id_of(&ids)?, ext),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// Who a request goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every OBD ECU (functional request). Must fit in one frame.
    AllEcus,
    /// One ECU, named by the ID it replies from (0x7E8 ...).
    Ecu(CanId),
}

/// A reply that answers our request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EcuReply {
    /// The ID the ECU replied from.
    pub ecu: CanId,
    pub reply: Reply,
    pub at_ms: u64,
}

/// Something that wants to see every frame: a log file, the bus monitor.
pub trait FrameSink: Send {
    fn frame(&mut self, frame: &Frame, at_ms: u64, outgoing: bool);
    /// Something happened on the link or the bus (a bridge restart, a bus error), in words.
    fn event(&mut self, _text: &str, _at_ms: u64) {}
}

/// Timings. The defaults follow ISO 15765-4 with some slack.
#[derive(Clone, Debug)]
pub struct Options {
    /// How long to collect replies to a request sent to every ECU.
    pub functional_wait_ms: u64,
    /// How long to wait for one ECU's reply (P2CAN is 50 ms).
    pub physical_timeout_ms: u64,
    /// After "busy, reply coming" (7F xx 78), wait up to this long.
    pub pending_timeout_ms: u64,
    /// Gap between requests when Scana is alone: 50 ms is 20 requests a second.
    pub min_gap_ms: u64,
    /// Gap when another tester is on the bus.
    pub shared_gap_ms: u64,
    /// Pad request frames to 8 bytes with this byte.
    pub padding: Option<u8>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            functional_wait_ms: 100,
            physical_timeout_ms: 150,
            pending_timeout_ms: 5000,
            min_gap_ms: 50,
            shared_gap_ms: 100,
            padding: Some(isotp::DEFAULT_PADDING),
        }
    }
}

/// How long after the last foreign request another tester still counts as active.
const OTHER_TESTER_MEMORY_MS: u64 = 5000;

pub struct Client {
    link: Box<dyn Link>,
    pub addressing: Addressing,
    pub options: Options,
    /// The ECUs found by detection: a request to every ECU ends early once all have replied.
    pub expected_ecus: Option<Vec<CanId>>,
    sink: Option<Box<dyn FrameSink>>,
    last_request_ms: Option<u64>,
    last_foreign_ms: Option<u64>,
    /// Requests seen from other testers since the client started.
    pub foreign_requests: u64,
    /// Requests Scana sent.
    pub requests_sent: u64,
    /// Every CAN ID heard on the bus. Module discovery never sends on one of these: it
    /// belongs to someone else.
    seen_ids: HashSet<CanId>,
}

impl Client {
    pub fn new(link: Box<dyn Link>) -> Self {
        Client {
            link,
            addressing: Addressing::Standard,
            options: Options::default(),
            expected_ecus: None,
            sink: None,
            last_request_ms: None,
            last_foreign_ms: None,
            foreign_requests: 0,
            requests_sent: 0,
            seen_ids: HashSet::new(),
        }
    }

    pub fn set_sink(&mut self, sink: Box<dyn FrameSink>) {
        self.sink = Some(sink);
    }

    pub fn link(&mut self) -> &mut dyn Link {
        self.link.as_mut()
    }

    pub fn now_ms(&self) -> u64 {
        self.link.now_ms()
    }

    /// True if another tester sent a request in the last 5 seconds.
    pub fn other_tester_active(&self) -> bool {
        self.last_foreign_ms.is_some_and(|t| self.now_ms().saturating_sub(t) < OTHER_TESTER_MEMORY_MS)
    }

    /// Every CAN ID heard since the client started.
    pub fn seen_ids(&self) -> &HashSet<CanId> {
        &self.seen_ids
    }

    /// Every received frame goes through here: the log, and spotting other testers.
    pub(crate) fn note_frame(&mut self, tf: &TimedFrame) {
        if let Some(s) = self.sink.as_mut() {
            s.frame(&tf.frame, tf.at_ms, false);
        }
        self.seen_ids.insert(tf.frame.id);
        // The bridge doesn't echo our own frames, so any request we see came from someone else.
        if Addressing::is_request_id(tf.frame.id) {
            self.foreign_requests += 1;
            self.last_foreign_ms = Some(tf.at_ms);
        }
    }

    fn send_frame(&mut self, id: CanId, payload: &[u8]) -> Result<(), CoreError> {
        let frame = Frame::new(id, payload).map_err(|e| CoreError::BadRequest(e.to_string()))?;
        match self.link.send(&frame) {
            Ok(()) => {}
            Err(e @ (LinkError::Refused(_) | LinkError::Unexpected(_))) => {
                let refused = matches!(e, LinkError::Refused(_));
                self.check_still_open(e)?;
                // A refused frame was not queued, so sending it again can't double it. After an
                // unexpected reply it may have gone out: the caller's wait for an answer decides.
                if refused && let Err(e) = self.link.send(&frame) {
                    return Err(self.send_failed(e));
                }
            }
            Err(e) => return Err(self.send_failed(e)),
        }
        let now = self.link.now_ms();
        if let Some(s) = self.sink.as_mut() {
            s.frame(&frame, now, true);
        }
        Ok(())
    }

    /// Tells the sink (the session's events log) something happened.
    pub fn note_event(&mut self, text: &str) {
        let now = self.link.now_ms();
        if let Some(s) = self.sink.as_mut() {
            s.event(text, now);
        }
    }

    /// Logs a bus event, and turns it into an error where Scana must stop.
    fn bus_event(&mut self, e: &BusEvent) -> Result<(), CoreError> {
        self.note_event(&e.describe());
        match e {
            BusEvent::TxError | BusEvent::TxArbitration => Err(CoreError::NotAcknowledged),
            BusEvent::BusOff => Err(CoreError::BusOff),
            BusEvent::HostWatchdog => Err(CoreError::WatchdogClosed),
            _ => Ok(()),
        }
    }

    /// Logs a bridge restart (with its reset report) and returns the error for it.
    pub(crate) fn bridge_restarted(&mut self, version: &str, reset: &str) -> CoreError {
        let why = scana_proto::slcan::describe_reset(reset);
        // Scana keeps the port open, so a warm restart now didn't come from opening it.
        let note = if reset.contains("reset=warm") { ". Not the port opening: it was already open" } else { "" };
        let raw = if reset.is_empty() { String::new() } else { format!(" (report: {reset})") };
        self.note_event(&format!("the bridge restarted, scana-bridge {version}: {why}{note}{raw}"));
        CoreError::BridgeReset
    }

    /// After a refused send or an unexpected reply: did the bridge restart, or is its bus still
    /// open? On a drive the bridge sometimes gets a command garbled on the USB cable and refuses
    /// it; the bus is fine, and reopening the port (which resets the Uno) cost 4 s each time.
    /// Ok means the bus is open and Scana can carry on.
    fn check_still_open(&mut self, e: LinkError) -> Result<(), CoreError> {
        // A restart line arrives before the bridge's reply to anything after it.
        while let Ok(Some(ev)) = self.link.recv(Duration::ZERO) {
            match ev {
                LinkEvent::BridgeReady { version, reset, .. } => return Err(self.bridge_restarted(&version, &reset)),
                LinkEvent::Frame(tf) => self.note_frame(&tf),
                LinkEvent::Bus(b) => self.bus_event(&b)?,
            }
        }
        match self.link.health() {
            Ok(Some(h)) if h.mode == 'N' || h.mode == 'K' => {
                self.note_event(&format!(
                    "the bridge didn't take a command ({e}), but its bus is still open: the command was \
                     probably garbled on the USB cable. Carrying on (bad commands so far: {})",
                    h.bad_commands
                ));
                Ok(())
            }
            _ => Err(self.send_failed(e)),
        }
    }

    /// A send that fails often means the bridge restarted: its bus is closed until Scana opens
    /// it again, and its restart line (with the reset report) is waiting behind the frames
    /// before it. Looks for that line, so the report reaches the log before the link is dropped.
    fn send_failed(&mut self, e: LinkError) -> CoreError {
        let look_ms = match e {
            LinkError::Refused(_) | LinkError::Unexpected(_) => 300,
            // A bridge that is restarting doesn't answer; after a reset by its pin it waits
            // about a second in the boot loader before it starts.
            LinkError::NoAnswer(_) => 2000,
            _ => return e.into(),
        };
        let until = self.link.now_ms() + look_ms;
        loop {
            let now = self.link.now_ms();
            if now >= until {
                break;
            }
            match self.link.recv(Duration::from_millis(until - now)) {
                Ok(Some(LinkEvent::BridgeReady { version, reset, .. })) => {
                    return self.bridge_restarted(&version, &reset);
                }
                Ok(Some(LinkEvent::Frame(tf))) => self.note_frame(&tf),
                Ok(Some(LinkEvent::Bus(b))) => self.note_event(&b.describe()),
                Ok(None) | Err(_) => break,
            }
        }
        self.note_event(&format!("sending failed: {e}"));
        e.into()
    }

    /// Handles frames and events until `until_ms`, without sending anything.
    pub fn idle_until(&mut self, until_ms: u64) -> Result<(), CoreError> {
        loop {
            let now = self.link.now_ms();
            if now >= until_ms {
                return Ok(());
            }
            match self.link.recv(Duration::from_millis(until_ms - now))? {
                Some(LinkEvent::Frame(tf)) => self.note_frame(&tf),
                Some(LinkEvent::Bus(e)) => self.bus_event(&e)?,
                Some(LinkEvent::BridgeReady { version, reset, .. }) => {
                    return Err(self.bridge_restarted(&version, &reset));
                }
                None => {}
            }
        }
    }

    /// Waits out the gap since the last request.
    fn pace(&mut self) -> Result<(), CoreError> {
        let gap = if self.other_tester_active() { self.options.shared_gap_ms } else { self.options.min_gap_ms };
        if let Some(last) = self.last_request_ms {
            self.idle_until(last + gap)?;
        }
        Ok(())
    }

    /// Sends a request and returns the replies that answer it (possibly none).
    pub fn request(&mut self, target: Target, payload: &[u8]) -> Result<Vec<EcuReply>, CoreError> {
        self.pace()?;
        let padding = self.options.padding;
        let to = match target {
            Target::AllEcus => self.addressing.functional_id(),
            Target::Ecu(reply_id) => Addressing::request_id_for(reply_id)
                .ok_or_else(|| CoreError::BadRequest(format!("{reply_id} is not an OBD reply ID")))?,
        };
        let mut sender = Sender::new(payload, padding)?;
        if target == Target::AllEcus && !sender.is_single_frame() {
            return Err(CoreError::BadRequest("a request to every ECU must fit in one frame".into()));
        }
        let start = self.link.now_ms();
        if let SendStep::Send(first) = sender.step(start) {
            self.send_frame(to, &first)?;
        }
        self.requests_sent += 1;
        self.last_request_ms = Some(start);

        let mut deadline = start
            + match target {
                Target::AllEcus => self.options.functional_wait_ms,
                Target::Ecu(_) => self.options.physical_timeout_ms,
            };
        let mut receivers: HashMap<CanId, Receiver> = HashMap::new();
        let mut waiting: HashSet<CanId> = HashSet::new();
        let mut results: Vec<EcuReply> = Vec::new();
        // Answers that start right but lack some of the PIDs asked for, per ECU: kept in case
        // the full one never comes.
        let mut partial: HashMap<CanId, EcuReply> = HashMap::new();
        let mut sending = !sender.is_single_frame();

        loop {
            let now = self.link.now_ms();
            if sending {
                match sender.step(now) {
                    SendStep::Send(p) => self.send_frame(to, &p)?,
                    SendStep::Wait => {}
                    SendStep::Done => sending = false,
                    SendStep::Failed(e) => return Err(e.into()),
                }
            }
            if now >= deadline {
                break;
            }
            let wait = if sending { 2 } else { deadline - now };
            let ev = match self.link.recv(Duration::from_millis(wait))? {
                Some(ev) => ev,
                None => continue,
            };
            let tf = match ev {
                LinkEvent::Frame(tf) => tf,
                LinkEvent::Bus(e) => {
                    self.bus_event(&e)?;
                    continue;
                }
                LinkEvent::BridgeReady { version, reset, .. } => return Err(self.bridge_restarted(&version, &reset)),
            };
            self.note_frame(&tf);
            let id = tf.frame.id;
            if !self.addressing.is_reply_id(id) {
                continue;
            }
            if let Target::Ecu(want) = target
                && id != want
            {
                continue;
            }
            let data = tf.frame.data();
            if isotp::kind_of(data) == Some(isotp::FrameKind::FlowControl) {
                if sending && let Some(e) = sender.on_flow_control(data, tf.at_ms) {
                    return Err(e.into());
                }
                continue;
            }
            let rx = receivers.entry(id).or_insert_with(|| Receiver::new(FlowControl::default(), padding));
            match rx.on_frame(data, tf.at_ms) {
                Received::Complete(msg) => {
                    let Ok(reply) = obd::parse_reply(&msg) else { continue };
                    if !obd::reply_matches(payload, &reply) {
                        continue; // an answer to another tester
                    }
                    if !obd::reply_complete(payload, &reply) {
                        // Most likely another tester's answer for the first PID alone (a dash
                        // display asking for rpm). Ours should follow within a few ms.
                        partial.entry(id).or_insert(EcuReply { ecu: id, reply, at_ms: tf.at_ms });
                        deadline = deadline.min(tf.at_ms + self.options.physical_timeout_ms).max(now);
                        continue;
                    }
                    if let Reply::Negative { code: 0x78, .. } = reply {
                        waiting.insert(id);
                        deadline = deadline.max(tf.at_ms + self.options.pending_timeout_ms);
                        continue;
                    }
                    waiting.remove(&id);
                    partial.remove(&id);
                    results.push(EcuReply { ecu: id, reply, at_ms: tf.at_ms });
                    if matches!(target, Target::Ecu(_)) {
                        break;
                    }
                    if let Some(expected) = &self.expected_ecus
                        && waiting.is_empty()
                        && expected.iter().all(|e| results.iter().any(|r| r.ecu == *e))
                    {
                        break;
                    }
                }
                Received::SendFlowControl(fc) => {
                    if let Some(req_id) = Addressing::request_id_for(id) {
                        self.send_frame(req_id, &fc)?;
                        deadline = deadline.max(tf.at_ms + Receiver::CF_TIMEOUT_MS);
                    }
                }
                Received::InProgress => deadline = deadline.max(tf.at_ms + Receiver::CF_TIMEOUT_MS),
                Received::Ignored | Received::Failed(_) => {}
            }
        }
        // An ECU that only ever gave part of the answer (it may have dropped ours to answer the
        // other tester): what it gave is still today's data.
        for (ecu, r) in partial {
            if !results.iter().any(|x| x.ecu == ecu) {
                results.push(r);
            }
        }
        results.sort_by_key(|r| r.ecu);
        Ok(results)
    }

    /// Sends a request to one module on its channel and returns its answer, if any.
    /// Handles long messages both ways, the address byte, and "busy, reply coming".
    pub fn request_on(&mut self, ch: Channel, payload: &[u8]) -> Result<Option<EcuReply>, CoreError> {
        self.pace()?;
        let padding = self.options.padding;
        let fl = ch.frame_len();
        let mut sender = Sender::with_frame_len(payload, padding, fl)?;
        let start = self.link.now_ms();
        if let SendStep::Send(first) = sender.step(start) {
            self.send_on(ch, &first)?;
        }
        self.requests_sent += 1;
        self.last_request_ms = Some(start);
        let mut deadline = start + self.options.physical_timeout_ms;
        let mut receiver = Receiver::with_frame_len(FlowControl::default(), padding, fl);
        let mut sending = !sender.is_single_frame();
        loop {
            let now = self.link.now_ms();
            if sending {
                match sender.step(now) {
                    SendStep::Send(p) => self.send_on(ch, &p)?,
                    SendStep::Wait => {}
                    SendStep::Done => sending = false,
                    SendStep::Failed(e) => return Err(e.into()),
                }
            }
            if now >= deadline {
                return Ok(None);
            }
            let wait = if sending { 2 } else { deadline - now };
            let tf = match self.link.recv(Duration::from_millis(wait))? {
                Some(LinkEvent::Frame(tf)) => tf,
                Some(LinkEvent::Bus(e)) => {
                    self.bus_event(&e)?;
                    continue;
                }
                Some(LinkEvent::BridgeReady { version, reset, .. }) => {
                    return Err(self.bridge_restarted(&version, &reset));
                }
                None => continue,
            };
            self.note_frame(&tf);
            if tf.frame.id != ch.rx {
                continue;
            }
            let data = match ch.ext {
                None => tf.frame.data(),
                Some(a) if tf.frame.data().first() == Some(&a) => &tf.frame.data()[1..],
                Some(_) => continue,
            };
            if isotp::kind_of(data) == Some(isotp::FrameKind::FlowControl) {
                if sending && let Some(e) = sender.on_flow_control(data, tf.at_ms) {
                    return Err(e.into());
                }
                continue;
            }
            match receiver.on_frame(data, tf.at_ms) {
                Received::Complete(msg) => {
                    let Ok(reply) = obd::parse_reply(&msg) else { continue };
                    if !uds::reply_matches(payload, &reply) {
                        continue;
                    }
                    if let Reply::Negative { code: 0x78, .. } = reply {
                        deadline = deadline.max(tf.at_ms + self.options.pending_timeout_ms);
                        continue;
                    }
                    return Ok(Some(EcuReply { ecu: ch.rx, reply, at_ms: tf.at_ms }));
                }
                Received::SendFlowControl(fc) => {
                    self.send_on(ch, &fc)?;
                    deadline = deadline.max(tf.at_ms + Receiver::CF_TIMEOUT_MS);
                }
                Received::InProgress => deadline = deadline.max(tf.at_ms + Receiver::CF_TIMEOUT_MS),
                Received::Ignored | Received::Failed(_) => {}
            }
        }
    }

    /// One module's answer on its channel: positive data (after the service byte), a refusal
    /// as an error, or no answer as an error.
    pub fn request_one_on(&mut self, ch: Channel, payload: &[u8]) -> Result<Vec<u8>, CoreError> {
        match self.request_on(ch, payload)? {
            Some(EcuReply { reply: Reply::Positive { data, .. }, .. }) => Ok(data),
            Some(EcuReply { reply: Reply::Negative { service, code }, .. }) => Err(CoreError::Refused {
                ecu: ch.rx,
                service: uds::service_name(service),
                code,
                text: obd::negative_code_text(code),
            }),
            None => Err(CoreError::NoReply(format!(
                "{} to {}",
                ch.label(),
                uds::service_name(payload.first().copied().unwrap_or(0))
            ))),
        }
    }

    /// Sends one single-frame request to `tx` (with an address byte, if given) and returns
    /// every frame heard in the next `window_ms`. Module discovery uses it: the caller decides
    /// which frame, if any, answered.
    pub fn probe(
        &mut self,
        tx: CanId,
        ext: Option<u8>,
        payload: &[u8],
        window_ms: u64,
    ) -> Result<Vec<TimedFrame>, CoreError> {
        self.pace()?;
        let ch = Channel { tx, rx: tx, ext };
        let frames = isotp::segment_len(payload, self.options.padding, ch.frame_len())?;
        let [first] = frames.as_slice() else {
            return Err(CoreError::BadRequest("a probe must fit in one frame".into()));
        };
        let start = self.link.now_ms();
        self.send_on(ch, first)?;
        self.requests_sent += 1;
        self.last_request_ms = Some(start);
        let mut heard = Vec::new();
        loop {
            let now = self.link.now_ms();
            if now >= start + window_ms {
                return Ok(heard);
            }
            match self.link.recv(Duration::from_millis(start + window_ms - now))? {
                Some(LinkEvent::Frame(tf)) => {
                    self.note_frame(&tf);
                    heard.push(tf);
                }
                Some(LinkEvent::Bus(e)) => self.bus_event(&e)?,
                Some(LinkEvent::BridgeReady { version, reset, .. }) => {
                    return Err(self.bridge_restarted(&version, &reset));
                }
                None => {}
            }
        }
    }

    fn send_on(&mut self, ch: Channel, payload: &[u8]) -> Result<(), CoreError> {
        match ch.ext {
            None => self.send_frame(ch.tx, payload),
            Some(a) => {
                let mut d = Vec::with_capacity(8);
                d.push(a);
                d.extend_from_slice(payload);
                self.send_frame(ch.tx, &d)
            }
        }
    }

    /// One ECU's reply: positive data, a refusal as an error, or no reply as an error.
    pub fn request_one(&mut self, ecu: CanId, payload: &[u8]) -> Result<Vec<u8>, CoreError> {
        let replies = self.request(Target::Ecu(ecu), payload)?;
        match replies.into_iter().next() {
            Some(EcuReply { reply: Reply::Positive { data, .. }, .. }) => Ok(data),
            Some(EcuReply { reply: Reply::Negative { service, code }, .. }) => Err(CoreError::Refused {
                ecu,
                service: obd::service_name(service),
                code,
                text: obd::negative_code_text(code),
            }),
            None => Err(CoreError::NoReply(format!(
                "{ecu} to {}",
                obd::service_name(payload.first().copied().unwrap_or(0))
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_link::OpenMode;
    use scana_link::sim::SimLink;
    use scana_sim::VirtualCar;

    fn client(car: VirtualCar) -> Client {
        let mut link = SimLink::new(car);
        link.open(OpenMode::Normal).unwrap();
        Client::new(Box::new(link))
    }

    /// Keeps what the client tells its sink.
    struct Events(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl FrameSink for Events {
        fn frame(&mut self, _: &Frame, _: u64, _: bool) {}
        fn event(&mut self, text: &str, _: u64) {
            self.0.lock().unwrap().push(text.to_string());
        }
    }

    #[test]
    fn a_refused_send_finds_the_bridge_restart_and_logs_it() {
        let mut link = SimLink::new(VirtualCar::hijet());
        link.open(OpenMode::Normal).unwrap();
        link.restart_bridge("reset=warm step=3 count=1 flags=B");
        let mut c = Client::new(Box::new(link));
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        c.set_sink(Box::new(Events(log.clone())));
        let e = c.request(Target::AllEcus, &[0x01, 0x0C]).unwrap_err();
        assert!(matches!(e, CoreError::BridgeReset), "{e:?}");
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1, "{log:?}");
        assert!(log[0].contains("the bridge restarted") && log[0].contains("report: reset=warm"), "{log:?}");
    }

    /// A command garbled on the USB cable is refused, but the bus stays open: Scana sends it
    /// again and carries on, instead of reopening the port (which resets the Uno).
    #[test]
    fn a_garbled_command_is_sent_again_without_reconnecting() {
        let mut link = SimLink::new(VirtualCar::hijet());
        link.open(OpenMode::Normal).unwrap();
        link.garble_sends(1);
        let mut c = Client::new(Box::new(link));
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        c.set_sink(Box::new(Events(log.clone())));
        let r = c.request(Target::AllEcus, &[0x01, 0x0C]).expect("the second try is answered");
        assert_eq!(r.len(), 1);
        let log = log.lock().unwrap();
        assert!(log.iter().any(|l| l.contains("garbled") && l.contains("bad commands so far: 1")), "{log:?}");
        assert!(!log.iter().any(|l| l.starts_with("sending failed")), "{log:?}");
    }

    /// Two in a row: the second refusal is a real failure, as before.
    #[test]
    fn a_command_refused_twice_still_fails() {
        let mut link = SimLink::new(VirtualCar::hijet());
        link.open(OpenMode::Normal).unwrap();
        link.garble_sends(2);
        let mut c = Client::new(Box::new(link));
        let e = c.request(Target::AllEcus, &[0x01, 0x0C]).unwrap_err();
        assert!(matches!(e, CoreError::Link(LinkError::Refused(_))), "{e:?}");
    }

    #[test]
    fn a_refused_send_without_a_restart_is_logged() {
        let link = SimLink::new(VirtualCar::hijet()); // never opened
        let mut c = Client::new(Box::new(link));
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        c.set_sink(Box::new(Events(log.clone())));
        let e = c.request(Target::AllEcus, &[0x01, 0x0C]).unwrap_err();
        assert!(matches!(e, CoreError::Link(LinkError::Refused(_))), "{e:?}");
        assert!(log.lock().unwrap()[0].starts_with("sending failed"));
    }

    #[test]
    fn functional_request_collects_every_ecu() {
        let mut c = client(VirtualCar::two_ecus());
        let r = c.request(Target::AllEcus, &[0x01, 0x00]).unwrap();
        let ecus: Vec<CanId> = r.iter().map(|x| x.ecu).collect();
        assert_eq!(ecus, vec![CanId::Standard(0x7E8), CanId::Standard(0x7E9)]);
    }

    #[test]
    fn vin_needs_flow_control_and_arrives_whole() {
        let mut c = client(VirtualCar::hijet());
        let data = c.request_one(CanId::Standard(0x7E8), &[0x09, 0x02]).unwrap();
        assert_eq!(obd::parse_vin(&data[1..]).as_deref(), Some("SCANASIMHIJET0001"));
    }

    #[test]
    fn refusals_and_silence() {
        let mut c = client(VirtualCar::hijet());
        let e = c.request_one(CanId::Standard(0x7E8), &[0x01, 0x5C]).unwrap_err();
        assert!(matches!(e, CoreError::Refused { code: 0x12, .. }), "{e}");
        // Nothing at 0x7EF.
        assert!(matches!(c.request_one(CanId::Standard(0x7EF), &[0x01, 0x00]), Err(CoreError::NoReply(_))));
    }

    #[test]
    fn busy_then_reply() {
        let mut car = VirtualCar::hijet();
        car.faults.busy_vehicle_info = true;
        let mut c = client(car);
        let data = c.request_one(CanId::Standard(0x7E8), &[0x09, 0x0A]).unwrap();
        assert_eq!(obd::parse_text_item(&data[1..]), "ECM-EngineControl");
    }

    #[test]
    fn other_tester_is_noticed_and_its_replies_ignored() {
        let mut c = client(VirtualCar::hijet().with_dash());
        let start = c.now_ms();
        c.idle_until(start + 200).unwrap();
        assert!(c.other_tester_active());
        assert!(c.foreign_requests >= 3);
        // Ask for coolant while the dash asks for rpm: only the coolant answer counts.
        for _ in 0..5 {
            let data = c.request_one(CanId::Standard(0x7E8), &[0x01, 0x05]).unwrap();
            assert_eq!(data[0], 0x05);
        }
    }

    #[test]
    fn wrong_bitrate_stops_at_once() {
        let mut link = SimLink::new(VirtualCar::hijet().at_bitrate(scana_proto::Bitrate::K250));
        link.open(OpenMode::Normal).unwrap();
        let mut c = Client::new(Box::new(link));
        assert!(matches!(c.request(Target::AllEcus, &[0x01, 0x00]), Err(CoreError::NotAcknowledged)));
    }

    #[test]
    fn channels_parse_and_pair() {
        let c = Channel::parse("7B0").unwrap();
        assert_eq!((c.tx, c.rx, c.ext), (CanId::Standard(0x7B0), CanId::Standard(0x7B8), None));
        assert_eq!(c.label(), "7B0");
        let c = Channel::parse("750/40").unwrap();
        assert_eq!((c.rx, c.ext, c.frame_len()), (CanId::Standard(0x758), Some(0x40), 7));
        assert_eq!(c.to_string(), "750/40");
        let c = Channel::parse("18DA28F1").unwrap();
        assert_eq!(c.rx, CanId::Extended(0x18DA_F128));
        let c = Channel::parse("0x7E0:0x7E8").unwrap();
        assert!(c.is_obd());
        assert!(!Channel::parse("7B0").unwrap().is_obd());
        assert_eq!(Channel::parse("7FF"), None);
        assert_eq!(Channel::parse("zz"), None);
        assert_eq!(Channel::obd(CanId::Standard(0x7E9)).unwrap().tx, CanId::Standard(0x7E1));
    }

    #[test]
    fn modules_answer_on_their_channels() {
        let mut c = client(VirtualCar::hijet());
        // UDS airbag module: a long identity string, with flow control.
        let srs = Channel::parse("780").unwrap();
        let data = c.request_one_on(srs, &uds::read_did_request(0xF18A)).unwrap();
        assert_eq!(uds::did_value(0xF18A, &data).and_then(|v| uds::text_of(&v)).as_deref(), Some("SIMULATED SUPPLIER"));
        // KWP body module behind the 750 gateway.
        let body = Channel::parse("750/40").unwrap();
        let data = c.request_one_on(body, &[0x1A, 0x88, 0x81]).unwrap();
        assert_eq!(uds::text_of(&data[2..]).as_deref(), Some("89221-SIM01"));
        // A refusal comes back as an error with the service named.
        let e = c.request_one_on(srs, &[0x21, 0x01]).unwrap_err();
        assert!(matches!(e, CoreError::Refused { code: 0x11, service: "data by local ID (KWP)", .. }), "{e}");
        // Nothing at 7D0.
        assert!(matches!(c.request_one_on(Channel::parse("7D0").unwrap(), &uds::PROBE), Err(CoreError::NoReply(_))));
    }

    #[test]
    fn probes_return_what_was_heard() {
        let mut c = client(VirtualCar::hijet());
        let heard = c.probe(CanId::Standard(0x7B0), None, &uds::PROBE, 60).unwrap();
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].frame.id, CanId::Standard(0x7B8));
        assert!(c.seen_ids().contains(&CanId::Standard(0x7B8)));
        let heard = c.probe(CanId::Standard(0x7D0), None, &uds::PROBE, 60).unwrap();
        assert!(heard.is_empty());
    }

    #[test]
    fn requests_are_spaced() {
        let mut c = client(VirtualCar::hijet());
        c.expected_ecus = Some(vec![CanId::Standard(0x7E8)]);
        let t0 = c.now_ms();
        for _ in 0..5 {
            c.request(Target::AllEcus, &[0x01, 0x0C]).unwrap();
        }
        // 4 gaps of at least 50 ms.
        assert!(c.now_ms() - t0 >= 200);
    }
}
