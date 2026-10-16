//! The scana-bridge over USB serial: SLCAN text lines at 1 Mbaud plus Scana's `#` commands.
//!
//! A reader thread splits incoming lines: replies go to one channel, frames and events to
//! another. Commands are sent one at a time and each waits for its single reply.

use crate::{Link, LinkError, LinkEvent, SelfTestResult, TimedFrame};
use scana_proto::Bitrate;
use scana_proto::Frame;
use scana_proto::slcan::{BridgeTime, Command, Health, Line, LineReader, OpenMode};
use serialport::{ClearBuffer, SerialPort, SerialPortType};
use std::io::{ErrorKind, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A serial port that might be a bridge.
#[derive(Clone, Debug)]
pub struct PortCandidate {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub product: Option<String>,
    /// An Arduino or a common USB-serial chip: worth trying.
    pub likely_bridge: bool,
}

/// USB serial ports, the likeliest bridges first.
pub fn list_ports() -> Vec<PortCandidate> {
    let mut out: Vec<PortCandidate> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.port_type {
            SerialPortType::UsbPort(u) => Some(PortCandidate {
                likely_bridge: matches!(
                    (u.vid, u.pid),
                    (0x2341, _) | (0x2A03, _) | (0x1A86, 0x7523) | (0x0403, 0x6001) | (0x10C4, 0xEA60)
                ),
                path: p.port_name,
                vid: u.vid,
                pid: u.pid,
                product: u.product,
            }),
            _ => None,
        })
        .collect();
    out.sort_by_key(|p| (!p.likely_bridge, p.vid != 0x2341, p.path.clone()));
    out
}

/// What the bridge said about itself when it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeInfo {
    pub path: String,
    pub version: String,
    /// False if the MCP2515 on the shield didn't answer.
    pub chip_ok: bool,
    pub identity: String,
}

pub struct SlcanLink {
    info: BridgeInfo,
    writer: Box<dyn SerialPort>,
    replies: Receiver<Line>,
    events: Receiver<LinkEvent>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    start: Instant,
    last_command: Instant,
    watchdog_ms: u16,
    bitrate: Bitrate,
    mode: Option<OpenMode>,
}

impl SlcanLink {
    pub const BAUD: u32 = 1_000_000;
    /// How long a command waits for its reply. The self-test takes about 30 ms.
    pub const REPLY_TIMEOUT: Duration = Duration::from_millis(500);
    /// The bridge closes the bus if it hears nothing for this long (`#W`).
    pub const HOST_WATCHDOG_MS: u16 = 2000;

    /// Opens the bridge at `path`, or the first likely port. Resets the Uno so the session
    /// starts clean (bus closed, counters at zero) and waits for its ready line.
    pub fn connect(path: Option<&str>) -> Result<Self, LinkError> {
        let path = match path {
            Some(p) => p.to_string(),
            None => list_ports().into_iter().find(|p| p.likely_bridge).map(|p| p.path).ok_or(LinkError::NotFound)?,
        };
        let mut port = serialport::new(&path, Self::BAUD)
            .timeout(Duration::from_millis(20))
            .open()
            .map_err(|e| open_error(&path, e))?;
        // Pulse DTR: the Uno's auto-reset. Its bootloader then runs for about 1.5 s.
        let _ = port.write_data_terminal_ready(false);
        thread::sleep(Duration::from_millis(50));
        let _ = port.clear(ClearBuffer::All);
        let _ = port.write_data_terminal_ready(true);

        let reader_port = port.try_clone().map_err(|e| open_error(&path, e))?;
        let (reply_tx, replies) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        let reader = {
            let stop = stop.clone();
            thread::Builder::new()
                .name("scana-serial".into())
                .spawn(move || read_loop(reader_port, reply_tx, event_tx, stop, start))
                .map_err(|e| LinkError::Io(e.to_string()))?
        };

        let mut link = SlcanLink {
            info: BridgeInfo { path: path.clone(), version: String::new(), chip_ok: false, identity: String::new() },
            writer: port,
            replies,
            events,
            stop,
            reader: Some(reader),
            start,
            last_command: Instant::now(),
            watchdog_ms: 0,
            bitrate: Bitrate::K500,
            mode: None,
        };

        // The ready line, if the reset worked.
        let deadline = Instant::now() + Duration::from_millis(3000);
        while Instant::now() < deadline {
            match link.events.recv_timeout(Duration::from_millis(50)) {
                Ok(LinkEvent::BridgeReady { version, chip_ok, .. }) => {
                    link.info.version = version;
                    link.info.chip_ok = chip_ok;
                    break;
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(LinkError::Disconnected),
            }
        }

        // Clear any half line in the bridge, then check it really is a bridge.
        link.write_raw(b"\r")?;
        thread::sleep(Duration::from_millis(30));
        while link.replies.try_recv().is_ok() {}
        match link.transact(&Command::Identity) {
            Ok(Line::Identity(id)) if id.starts_with("scana-bridge") => {
                if link.info.version.is_empty() {
                    // No reset happened (no ready line): take the version from the identity.
                    link.info.version = id.split_whitespace().nth(1).unwrap_or("?").to_string();
                    link.info.chip_ok = link.self_test_inner().map(|r| r.is_ok()).unwrap_or(false);
                }
                link.info.identity = id;
            }
            Ok(_) | Err(LinkError::NoAnswer(_)) => return Err(LinkError::NotABridge(path)),
            Err(e) => return Err(e),
        }
        link.expect_ok(&Command::Events(true), "turn on events")?;
        link.expect_ok(&Command::MicrosTimestamps(true), "turn on timestamps")?;
        link.expect_ok(&Command::HostWatchdog(Self::HOST_WATCHDOG_MS), "start the host watchdog")?;
        link.watchdog_ms = Self::HOST_WATCHDOG_MS;
        Ok(link)
    }

    pub fn info(&self) -> &BridgeInfo {
        &self.info
    }

    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        self.writer.write_all(bytes).map_err(io_error)?;
        self.writer.flush().map_err(io_error)?;
        Ok(())
    }

    /// Sends one command and waits for its reply.
    fn transact(&mut self, cmd: &Command) -> Result<Line, LinkError> {
        // A late reply to an earlier command would be taken as this one's: drop any waiting.
        while self.replies.try_recv().is_ok() {}
        self.write_raw(cmd.encode().as_bytes())?;
        self.last_command = Instant::now();
        match self.replies.recv_timeout(Self::REPLY_TIMEOUT) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err(LinkError::NoAnswer(Self::REPLY_TIMEOUT.as_millis() as u64)),
            Err(RecvTimeoutError::Disconnected) => Err(LinkError::Disconnected),
        }
    }

    fn expect_ok(&mut self, cmd: &Command, what: &str) -> Result<(), LinkError> {
        match self.transact(cmd)? {
            Line::Ok => Ok(()),
            Line::Error => Err(LinkError::Refused(what.into())),
            other => Err(LinkError::Unexpected(format!("{other:?}"))),
        }
    }

    /// Keeps the bridge's host watchdog fed while the bus is open.
    fn keepalive(&mut self) -> Result<(), LinkError> {
        if self.watchdog_ms > 0
            && self.mode.is_some()
            && self.last_command.elapsed() > Duration::from_millis(u64::from(self.watchdog_ms) / 3)
        {
            match self.transact(&Command::Ping)? {
                Line::Pong => {}
                other => return Err(LinkError::Unexpected(format!("{other:?}"))),
            }
        }
        Ok(())
    }

    fn self_test_inner(&mut self) -> Result<SelfTestResult, LinkError> {
        match self.transact(&Command::SelfTest)? {
            Line::SelfTest(r) => Ok(r),
            Line::Error => Err(LinkError::Refused("run the self-test (close the bus first)".into())),
            other => Err(LinkError::Unexpected(format!("{other:?}"))),
        }
    }
}

impl Link for SlcanLink {
    fn describe(&self) -> String {
        format!("scana-bridge {} on {}", self.info.version, self.info.path)
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn set_bitrate(&mut self, bitrate: Bitrate) -> Result<(), LinkError> {
        self.expect_ok(&Command::Bitrate(bitrate), "change the bit rate (close the bus first)")?;
        self.bitrate = bitrate;
        Ok(())
    }

    fn bitrate(&self) -> Bitrate {
        self.bitrate
    }

    fn open(&mut self, mode: OpenMode) -> Result<(), LinkError> {
        let what = match mode {
            OpenMode::Normal => "open the bus",
            OpenMode::ListenOnly => "open the bus in listen-only mode",
            OpenMode::Loopback => "open loopback mode",
        };
        if !self.info.chip_ok {
            // The bridge looks for the chip again when asked to open.
            self.expect_ok(&Command::Open(mode), &format!("{what}: the CAN chip on the shield isn't answering"))?;
            self.info.chip_ok = true;
        } else {
            self.expect_ok(&Command::Open(mode), what)?;
        }
        self.mode = Some(mode);
        Ok(())
    }

    fn close(&mut self) -> Result<(), LinkError> {
        self.mode = None;
        self.expect_ok(&Command::Close, "close the bus")
    }

    fn mode(&self) -> Option<OpenMode> {
        self.mode
    }

    fn send(&mut self, frame: &Frame) -> Result<(), LinkError> {
        match self.transact(&Command::Send(*frame))? {
            Line::Queued => Ok(()),
            Line::Error => Err(LinkError::Refused(
                "send a frame (the bus isn't open in normal mode, or the send queue is full)".into(),
            )),
            other => Err(LinkError::Unexpected(format!("{other:?}"))),
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<Option<LinkEvent>, LinkError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.keepalive()?;
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(ev) => {
                    if matches!(ev, LinkEvent::BridgeReady { .. }) {
                        self.mode = None; // it reset: the bus is closed
                    }
                    if matches!(ev, LinkEvent::Bus(scana_proto::slcan::Event::HostWatchdog)) {
                        self.mode = None;
                    }
                    return Ok(Some(ev));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(LinkError::Disconnected),
            }
        }
    }

    fn health(&mut self) -> Result<Option<Health>, LinkError> {
        match self.transact(&Command::Health)? {
            Line::Health(h) => Ok(Some(h)),
            other => Err(LinkError::Unexpected(format!("{other:?}"))),
        }
    }

    fn self_test(&mut self) -> Result<Option<SelfTestResult>, LinkError> {
        let r = self.self_test_inner()?;
        self.info.chip_ok = r.is_ok();
        Ok(Some(r))
    }
}

impl Drop for SlcanLink {
    fn drop(&mut self) {
        if self.mode.is_some() {
            let _ = self.write_raw(Command::Close.encode().as_bytes());
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

fn read_loop(
    mut port: Box<dyn SerialPort>,
    replies: Sender<Line>,
    events: Sender<LinkEvent>,
    stop: Arc<AtomicBool>,
    start: Instant,
) {
    let mut reader = LineReader::new();
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        match port.read(&mut buf) {
            Ok(0) => thread::sleep(Duration::from_millis(5)),
            Ok(n) => {
                for line in reader.push(&buf[..n]) {
                    let sent = match line {
                        Line::Frame { frame, bridge_time } => events
                            .send(LinkEvent::Frame(TimedFrame {
                                frame,
                                at_ms: start.elapsed().as_millis() as u64,
                                bridge_us: match bridge_time {
                                    Some(BridgeTime::Micros(us)) => Some(us),
                                    _ => None,
                                },
                            }))
                            .is_ok(),
                        Line::Event(e) => events.send(LinkEvent::Bus(e)).is_ok(),
                        Line::Ready { version, chip_ok, reset } => {
                            events.send(LinkEvent::BridgeReady { version, chip_ok, reset }).is_ok()
                        }
                        Line::Unknown(_) => true,
                        reply => replies.send(reply).is_ok(),
                    };
                    if !sent {
                        return; // the link was dropped
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
            Err(_) => return, // unplugged: the channels close and the link reports Disconnected
        }
    }
}

fn open_error(path: &str, e: serialport::Error) -> LinkError {
    match e.kind() {
        serialport::ErrorKind::Io(ErrorKind::PermissionDenied) => LinkError::Permission(path.into()),
        serialport::ErrorKind::NoDevice | serialport::ErrorKind::Io(ErrorKind::NotFound) => LinkError::NotFound,
        _ => LinkError::Open { path: path.into(), reason: e.to_string() },
    }
}

fn io_error(e: std::io::Error) -> LinkError {
    match e.kind() {
        ErrorKind::BrokenPipe | ErrorKind::NotConnected => LinkError::Disconnected,
        _ => LinkError::Io(e.to_string()),
    }
}
