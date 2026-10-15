//! ISO-TP (ISO 15765-2): carries messages longer than 7 bytes over several CAN frames.
//!
//! The first data byte of each frame (the PCI) says what it is:
//! single frame `0L`, first frame `1L LL`, consecutive frame `2N`, flow control `3S BS ST`.
//! Time is passed in as milliseconds, so this runs the same in tests and against a car.
//!
//! With extended addressing (ISO 15765-2 address extension, used for example behind a
//! Toyota-family gateway on 0x750) the first data byte of every frame is an address, so each
//! frame carries one byte less: `frame_len` 7 instead of 8. The caller adds and strips that byte.

use thiserror::Error;

/// The byte used to fill frames up to 8 bytes. ISO 15765-2 suggests 0xCC.
pub const DEFAULT_PADDING: u8 = 0xCC;

/// Largest message accepted: 4095 is the classic limit; the escape form allows more,
/// but nothing in OBD-II or UDS reading needs it.
pub const MAX_MESSAGE: usize = 4095;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum IsoTpError {
    #[error("message too long for ISO-TP ({0} bytes)")]
    TooLong(usize),
    #[error("empty message")]
    Empty,
    #[error("a frame arrived out of order (wanted sequence {expected}, got {got})")]
    WrongSequence { expected: u8, got: u8 },
    #[error("the rest of a long reply didn't arrive in time")]
    Timeout,
    #[error("the receiver asked us to stop (flow control overflow)")]
    Overflow,
    #[error("a malformed ISO-TP frame")]
    Malformed,
}

/// What a frame is, read from its PCI byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Single,
    First,
    Consecutive,
    FlowControl,
}

pub fn kind_of(data: &[u8]) -> Option<FrameKind> {
    match data.first()? >> 4 {
        0 => Some(FrameKind::Single),
        1 => Some(FrameKind::First),
        2 => Some(FrameKind::Consecutive),
        3 => Some(FrameKind::FlowControl),
        _ => None,
    }
}

/// Flow control we send after a first frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlowControl {
    /// 0 = send everything without waiting for more flow control.
    pub block_size: u8,
    /// Minimum gap between consecutive frames: 0-127 ms, or 0xF1-0xF9 for 100-900 us.
    pub st_min: u8,
}

impl FlowControl {
    pub fn frame_data(self, padding: Option<u8>) -> Vec<u8> {
        self.frame_data_len(padding, 8)
    }

    /// The flow control payload for frames of `frame_len` bytes (7 with an address byte).
    pub fn frame_data_len(self, padding: Option<u8>, frame_len: usize) -> Vec<u8> {
        pad(vec![0x30, self.block_size, self.st_min], padding, frame_len)
    }
}

fn pad(mut v: Vec<u8>, padding: Option<u8>, frame_len: usize) -> Vec<u8> {
    if let Some(p) = padding
        && v.len() < frame_len
    {
        v.resize(frame_len, p);
    }
    v
}

/// Frame payload sizes allowed: 8 normally, 7 with an address byte. Smaller makes no sense.
fn check_frame_len(frame_len: usize) -> usize {
    frame_len.clamp(4, 8)
}

/// Splits a message into frame payloads. The first is sent at once; if there are more,
/// wait for the receiver's flow control before sending the rest (see [`Sender`]).
pub fn segment(message: &[u8], padding: Option<u8>) -> Result<Vec<Vec<u8>>, IsoTpError> {
    segment_len(message, padding, 8)
}

/// [`segment`] for frames of `frame_len` bytes: 7 when an address byte comes first.
pub fn segment_len(message: &[u8], padding: Option<u8>, frame_len: usize) -> Result<Vec<Vec<u8>>, IsoTpError> {
    let fl = check_frame_len(frame_len);
    let n = message.len();
    if n == 0 {
        return Err(IsoTpError::Empty);
    }
    if n > MAX_MESSAGE {
        return Err(IsoTpError::TooLong(n));
    }
    if n < fl {
        let mut v = vec![n as u8];
        v.extend_from_slice(message);
        return Ok(vec![pad(v, padding, fl)]);
    }
    let first_part = fl - 2;
    let mut frames = Vec::with_capacity(1 + (n - first_part).div_ceil(fl - 1));
    let mut first = vec![0x10 | (n >> 8) as u8, (n & 0xFF) as u8];
    first.extend_from_slice(&message[..first_part]);
    frames.push(first);
    for (i, chunk) in message[first_part..].chunks(fl - 1).enumerate() {
        let mut v = vec![0x20 | ((i + 1) & 0x0F) as u8];
        v.extend_from_slice(chunk);
        frames.push(pad(v, padding, fl));
    }
    Ok(frames)
}

/// Sends a multi-frame message: first frame, then consecutive frames as flow control allows.
#[derive(Debug)]
pub struct Sender {
    frames: Vec<Vec<u8>>,
    next: usize,
    block_left: Option<u8>,
    st_min_us: u32,
    waiting_since_ms: u64,
    waiting_fc: bool,
    /// When the last consecutive frame went out; `None` right after flow control.
    last_cf_ms: Option<u64>,
}

/// What the sender wants done next.
#[derive(Debug, PartialEq, Eq)]
pub enum SendStep {
    /// Send this payload now.
    Send(Vec<u8>),
    /// Nothing to send yet (waiting for flow control or the minimum gap).
    Wait,
    Done,
    Failed(IsoTpError),
}

impl Sender {
    /// Wait this long for flow control (N_Bs).
    pub const FC_TIMEOUT_MS: u64 = 1000;

    pub fn new(message: &[u8], padding: Option<u8>) -> Result<Self, IsoTpError> {
        Self::with_frame_len(message, padding, 8)
    }

    /// A sender for frames of `frame_len` bytes (7 when an address byte comes first).
    pub fn with_frame_len(message: &[u8], padding: Option<u8>, frame_len: usize) -> Result<Self, IsoTpError> {
        let frames = segment_len(message, padding, frame_len)?;
        Ok(Sender {
            frames,
            next: 0,
            block_left: None,
            st_min_us: 0,
            waiting_since_ms: 0,
            waiting_fc: false,
            last_cf_ms: None,
        })
    }

    pub fn is_single_frame(&self) -> bool {
        self.frames.len() == 1
    }

    /// True between sending a first frame (or a full block) and the receiver's flow control.
    pub fn is_waiting_for_flow_control(&self) -> bool {
        self.waiting_fc
    }

    /// Call when a flow-control frame arrives from the receiver.
    pub fn on_flow_control(&mut self, data: &[u8], now_ms: u64) -> Option<IsoTpError> {
        if kind_of(data) != Some(FrameKind::FlowControl) || data.len() < 3 {
            return Some(IsoTpError::Malformed);
        }
        match data[0] & 0x0F {
            0 => {
                self.waiting_fc = false;
                self.block_left = (data[1] != 0).then_some(data[1]);
                self.st_min_us = st_min_to_us(data[2]);
                self.last_cf_ms = None;
                None
            }
            1 => {
                // Wait: the receiver needs more time. Restart the timer.
                self.waiting_since_ms = now_ms;
                None
            }
            2 => Some(IsoTpError::Overflow),
            _ => Some(IsoTpError::Malformed),
        }
    }

    pub fn step(&mut self, now_ms: u64) -> SendStep {
        if self.next >= self.frames.len() {
            return SendStep::Done;
        }
        if self.waiting_fc {
            if now_ms.saturating_sub(self.waiting_since_ms) > Self::FC_TIMEOUT_MS {
                return SendStep::Failed(IsoTpError::Timeout);
            }
            return SendStep::Wait;
        }
        if let Some(t) = self.last_cf_ms
            && now_ms.saturating_sub(t) * 1000 < u64::from(self.st_min_us)
        {
            return SendStep::Wait;
        }
        let payload = self.frames[self.next].clone();
        self.next += 1;
        if self.next > 1 {
            self.last_cf_ms = Some(now_ms);
        }
        if self.next == 1 && self.frames.len() > 1 {
            self.waiting_fc = true;
            self.waiting_since_ms = now_ms;
        } else if self.next > 1
            && let Some(left) = self.block_left.as_mut()
        {
            *left -= 1;
            if *left == 0 && self.next < self.frames.len() {
                self.waiting_fc = true;
                self.waiting_since_ms = now_ms;
            }
        }
        SendStep::Send(payload)
    }
}

/// STmin byte to microseconds. Reserved values count as the maximum, 127 ms.
pub fn st_min_to_us(b: u8) -> u32 {
    match b {
        0x00..=0x7F => u32::from(b) * 1000,
        0xF1..=0xF9 => u32::from(b - 0xF0) * 100,
        _ => 127_000,
    }
}

/// What happened when a frame was given to the [`Receiver`].
#[derive(Debug, PartialEq, Eq)]
pub enum Received {
    /// A whole message.
    Complete(Vec<u8>),
    /// A first frame started a long message: send this flow control to the sender now.
    SendFlowControl(Vec<u8>),
    /// Part of a long message; more to come.
    InProgress,
    /// Not an ISO-TP data frame (for example a flow control), or nothing expected.
    Ignored,
    Failed(IsoTpError),
}

/// Puts a message back together from frames received from one sender.
#[derive(Debug)]
pub struct Receiver {
    flow: FlowControl,
    padding: Option<u8>,
    frame_len: usize,
    buf: Vec<u8>,
    expected: usize,
    next_seq: u8,
    last_ms: u64,
    active: bool,
}

impl Receiver {
    /// Wait this long between consecutive frames (N_Cr).
    pub const CF_TIMEOUT_MS: u64 = 1000;

    pub fn new(flow: FlowControl, padding: Option<u8>) -> Self {
        Self::with_frame_len(flow, padding, 8)
    }

    /// A receiver for frames of `frame_len` bytes (7 when an address byte comes first; the
    /// caller strips that byte before passing the rest in).
    pub fn with_frame_len(flow: FlowControl, padding: Option<u8>, frame_len: usize) -> Self {
        Receiver {
            flow,
            padding,
            frame_len: check_frame_len(frame_len),
            buf: Vec::new(),
            expected: 0,
            next_seq: 1,
            last_ms: 0,
            active: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn on_frame(&mut self, data: &[u8], now_ms: u64) -> Received {
        match kind_of(data) {
            Some(FrameKind::Single) => {
                let len = usize::from(data[0] & 0x0F);
                self.active = false;
                if len == 0 || len >= self.frame_len || data.len() < 1 + len {
                    return Received::Failed(IsoTpError::Malformed);
                }
                Received::Complete(data[1..1 + len].to_vec())
            }
            Some(FrameKind::First) => {
                let fl = self.frame_len;
                if data.len() < fl {
                    return Received::Failed(IsoTpError::Malformed);
                }
                let len = (usize::from(data[0] & 0x0F) << 8) | usize::from(data[1]);
                if len < fl {
                    // Length 0 is the escape form (over 4095 bytes); shorter fits a single frame.
                    return Received::Failed(if len == 0 { IsoTpError::TooLong(len) } else { IsoTpError::Malformed });
                }
                self.buf.clear();
                self.buf.extend_from_slice(&data[2..fl]);
                self.expected = len;
                self.next_seq = 1;
                self.last_ms = now_ms;
                self.active = true;
                Received::SendFlowControl(self.flow.frame_data_len(self.padding, self.frame_len))
            }
            Some(FrameKind::Consecutive) => {
                if !self.active {
                    return Received::Ignored;
                }
                let seq = data[0] & 0x0F;
                if seq != self.next_seq {
                    self.active = false;
                    return Received::Failed(IsoTpError::WrongSequence { expected: self.next_seq, got: seq });
                }
                self.next_seq = (self.next_seq + 1) & 0x0F;
                self.last_ms = now_ms;
                let want = (self.expected - self.buf.len()).min(self.frame_len - 1).min(data.len().saturating_sub(1));
                self.buf.extend_from_slice(&data[1..1 + want]);
                if self.buf.len() >= self.expected {
                    self.active = false;
                    return Received::Complete(std::mem::take(&mut self.buf));
                }
                Received::InProgress
            }
            Some(FrameKind::FlowControl) | None => Received::Ignored,
        }
    }

    /// Call now and then: reports a timeout if a long message stalled.
    pub fn check_timeout(&mut self, now_ms: u64) -> Option<IsoTpError> {
        if self.active && now_ms.saturating_sub(self.last_ms) > Self::CF_TIMEOUT_MS {
            self.active = false;
            return Some(IsoTpError::Timeout);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: Option<u8> = Some(DEFAULT_PADDING);

    #[test]
    fn short_messages_are_one_padded_frame() {
        let f = segment(&[0x01, 0x0C], PAD).unwrap();
        assert_eq!(f, vec![vec![0x02, 0x01, 0x0C, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC]]);
        let f = segment(&[0x09, 0x02], None).unwrap();
        assert_eq!(f, vec![vec![0x02, 0x09, 0x02]]);
        assert_eq!(segment(&[], PAD), Err(IsoTpError::Empty));
        assert_eq!(segment(&[0; 4096], PAD), Err(IsoTpError::TooLong(4096)));
    }

    #[test]
    fn long_messages_split_with_sequence_numbers() {
        let msg: Vec<u8> = (0..20).collect();
        let f = segment(&msg, PAD).unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0], vec![0x10, 20, 0, 1, 2, 3, 4, 5]);
        assert_eq!(f[1], vec![0x21, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(f[2], vec![0x22, 13, 14, 15, 16, 17, 18, 19]);
        // Sequence numbers wrap from F to 0.
        let big = vec![0u8; 6 + 7 * 17];
        let f = segment(&big, PAD).unwrap();
        assert_eq!(f[15][0], 0x2F);
        assert_eq!(f[16][0], 0x20);
    }

    #[test]
    fn a_vin_reply_reassembles() {
        // 49 02 01 + 17 characters = 20 bytes, as a car sends it.
        let mut msg = vec![0x49, 0x02, 0x01];
        msg.extend_from_slice(b"SCANASIMHIJET0001");
        let frames = segment(&msg, PAD).unwrap();
        let mut r = Receiver::new(FlowControl::default(), PAD);
        assert_eq!(
            r.on_frame(&frames[0], 0),
            Received::SendFlowControl(vec![0x30, 0, 0, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC])
        );
        assert_eq!(r.on_frame(&frames[1], 1), Received::InProgress);
        assert_eq!(r.on_frame(&frames[2], 2), Received::Complete(msg));
        assert!(!r.is_active());
    }

    #[test]
    fn receiver_catches_problems() {
        let msg: Vec<u8> = (0..20).collect();
        let frames = segment(&msg, PAD).unwrap();
        let mut r = Receiver::new(FlowControl::default(), PAD);
        r.on_frame(&frames[0], 0);
        assert_eq!(r.on_frame(&frames[2], 1), Received::Failed(IsoTpError::WrongSequence { expected: 1, got: 2 }));
        // A consecutive frame with nothing started is ignored (another tester's reply).
        assert_eq!(r.on_frame(&frames[1], 2), Received::Ignored);
        r.on_frame(&frames[0], 10);
        assert_eq!(r.check_timeout(500), None);
        assert_eq!(r.check_timeout(1011), Some(IsoTpError::Timeout));
        assert_eq!(r.on_frame(&[0x00, 1, 2], 0), Received::Failed(IsoTpError::Malformed));
        assert_eq!(r.on_frame(&[0x05, 1, 2], 0), Received::Failed(IsoTpError::Malformed));
        assert_eq!(r.on_frame(&[0x10, 0x00, 0, 0, 0, 0, 0, 0], 0), Received::Failed(IsoTpError::TooLong(0)));
        assert_eq!(r.on_frame(&[0x30, 0, 0], 0), Received::Ignored);
        assert_eq!(r.on_frame(&[], 0), Received::Ignored);
        assert_eq!(r.on_frame(&[0x10, 20, 1], 0), Received::Failed(IsoTpError::Malformed));
    }

    #[test]
    fn sender_waits_for_flow_control_and_block_size() {
        let msg: Vec<u8> = (0..30).collect();
        let mut s = Sender::new(&msg, PAD).unwrap();
        assert!(!s.is_single_frame());
        assert!(matches!(s.step(0), SendStep::Send(f) if f[0] == 0x10));
        assert_eq!(s.step(1), SendStep::Wait);
        // Block size 2, no gap.
        assert_eq!(s.on_flow_control(&[0x30, 2, 0], 5), None);
        assert!(matches!(s.step(5), SendStep::Send(f) if f[0] == 0x21));
        assert!(matches!(s.step(5), SendStep::Send(f) if f[0] == 0x22));
        assert_eq!(s.step(6), SendStep::Wait);
        assert_eq!(s.on_flow_control(&[0x30, 0, 0], 7), None);
        assert!(matches!(s.step(7), SendStep::Send(f) if f[0] == 0x23));
        assert!(matches!(s.step(7), SendStep::Send(f) if f[0] == 0x24));
        assert_eq!(s.step(7), SendStep::Done);
    }

    #[test]
    fn sender_keeps_the_minimum_gap_and_times_out() {
        let msg: Vec<u8> = (0..20).collect();
        let mut s = Sender::new(&msg, PAD).unwrap();
        s.step(0);
        s.on_flow_control(&[0x30, 0, 10], 0);
        assert!(matches!(s.step(0), SendStep::Send(_)));
        assert_eq!(s.step(5), SendStep::Wait);
        assert!(matches!(s.step(10), SendStep::Send(_)));
        assert_eq!(s.step(10), SendStep::Done);

        let mut s = Sender::new(&msg, PAD).unwrap();
        s.step(0);
        assert_eq!(s.step(1001), SendStep::Failed(IsoTpError::Timeout));
        let mut s = Sender::new(&msg, PAD).unwrap();
        s.step(0);
        assert_eq!(s.on_flow_control(&[0x32, 0, 0], 1), Some(IsoTpError::Overflow));
    }

    #[test]
    fn frames_with_an_address_byte_carry_one_byte_less() {
        // Six bytes still fit one frame; seven need a first frame and a consecutive frame.
        let f = segment_len(&[1, 2, 3, 4, 5, 6], PAD, 7).unwrap();
        assert_eq!(f, vec![vec![0x06, 1, 2, 3, 4, 5, 6]]);
        let f = segment_len(&[1, 2, 3, 4, 5, 6, 7], PAD, 7).unwrap();
        assert_eq!(f, vec![vec![0x10, 7, 1, 2, 3, 4, 5], vec![0x21, 6, 7, 0xCC, 0xCC, 0xCC, 0xCC]]);
        assert_eq!(FlowControl::default().frame_data_len(PAD, 7).len(), 7);
        for n in 1..=100usize {
            let msg: Vec<u8> = (0..n).map(|i| i as u8).collect();
            let frames = segment_len(&msg, PAD, 7).unwrap();
            assert!(frames.iter().all(|f| f.len() <= 7));
            let mut r = Receiver::with_frame_len(FlowControl::default(), PAD, 7);
            let mut done = None;
            for f in &frames {
                if let Received::Complete(m) = r.on_frame(f, 0) {
                    done = Some(m);
                }
            }
            assert_eq!(done.as_deref(), Some(&msg[..]), "length {n}");
        }
        // A 7-byte single frame is too long when an address byte takes a place.
        let mut r = Receiver::with_frame_len(FlowControl::default(), PAD, 7);
        assert_eq!(r.on_frame(&[0x07, 1, 2, 3, 4, 5, 6], 0), Received::Failed(IsoTpError::Malformed));
    }

    #[test]
    fn st_min_values() {
        assert_eq!(st_min_to_us(0), 0);
        assert_eq!(st_min_to_us(10), 10_000);
        assert_eq!(st_min_to_us(0xF5), 500);
        assert_eq!(st_min_to_us(0x80), 127_000);
    }

    #[test]
    fn round_trip_many_lengths() {
        for n in 1..=300usize {
            let msg: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
            let frames = segment(&msg, PAD).unwrap();
            let mut r = Receiver::new(FlowControl::default(), PAD);
            let mut done = None;
            for (t, f) in frames.iter().enumerate() {
                match r.on_frame(f, t as u64) {
                    Received::Complete(m) => done = Some(m),
                    Received::SendFlowControl(_) | Received::InProgress => {}
                    other => panic!("{n}: {other:?}"),
                }
            }
            assert_eq!(done.as_deref(), Some(&msg[..]), "length {n}");
        }
    }
}
