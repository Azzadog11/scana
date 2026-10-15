//! The OBD-II client: sends a request (as ISO-TP), collects the replies that answer it, and
//! keeps to the bus manners: a gap between requests, wider when another tester (such as a
//! dash display) is on the bus, and a stop at the first transmit error.

use scana_link::{BusEvent, Link, LinkError, LinkEvent, TimedFrame};
use scana_proto::isotp::{self, FlowControl, Received, Receiver, SendStep, Sender};
use scana_proto::obd::{self, Reply};
use scana_proto::{CanId, Frame};
use std::collections::{HashMap, HashSet};
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

    /// Every received frame goes through here: the log, and spotting other testers.
    pub(crate) fn note_frame(&mut self, tf: &TimedFrame) {
        if let Some(s) = self.sink.as_mut() {
            s.frame(&tf.frame, tf.at_ms, false);
        }
        // The bridge doesn't echo our own frames, so any request we see came from someone else.
        if Addressing::is_request_id(tf.frame.id) {
            self.foreign_requests += 1;
            self.last_foreign_ms = Some(tf.at_ms);
        }
    }

    fn send_frame(&mut self, id: CanId, payload: &[u8]) -> Result<(), CoreError> {
        let frame = Frame::new(id, payload).map_err(|e| CoreError::BadRequest(e.to_string()))?;
        self.link.send(&frame)?;
        let now = self.link.now_ms();
        if let Some(s) = self.sink.as_mut() {
            s.frame(&frame, now, true);
        }
        Ok(())
    }

    /// Turns bus events into errors where Scana must stop.
    fn check_bus_event(e: &BusEvent) -> Result<(), CoreError> {
        match e {
            BusEvent::TxError | BusEvent::TxArbitration => Err(CoreError::NotAcknowledged),
            BusEvent::BusOff => Err(CoreError::BusOff),
            BusEvent::HostWatchdog => Err(CoreError::WatchdogClosed),
            _ => Ok(()),
        }
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
                Some(LinkEvent::Bus(e)) => Self::check_bus_event(&e)?,
                Some(LinkEvent::BridgeReady { .. }) => return Err(CoreError::BridgeReset),
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
                    Self::check_bus_event(&e)?;
                    continue;
                }
                LinkEvent::BridgeReady { .. } => return Err(CoreError::BridgeReset),
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
                    if let Reply::Negative { code: 0x78, .. } = reply {
                        waiting.insert(id);
                        deadline = deadline.max(tf.at_ms + self.options.pending_timeout_ms);
                        continue;
                    }
                    waiting.remove(&id);
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
        results.sort_by_key(|r| r.ecu);
        Ok(results)
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
