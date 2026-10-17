//! Stress the bridge's serial link in both directions at once, as on a busy bus: loopback
//! frames stream back while the next commands go out. Counts refused commands.
//! `cargo run --release -p scana-link --example duplex_stress -- /dev/ttyACM0 3000`
use scana_link::serial::SlcanLink;
use scana_link::{Link, LinkError, LinkEvent};
use scana_proto::slcan::OpenMode;
use scana_proto::{CanId, Frame};
use std::time::{Duration, Instant};

fn main() -> Result<(), LinkError> {
    let args: Vec<String> = std::env::args().collect();
    let port = args.get(1).map(String::as_str);
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3000);
    let mut link = SlcanLink::connect(port)?;
    link.open(OpenMode::Loopback)?;
    let Some(id) = CanId::standard(0x7E0) else { return Err(LinkError::Io("bad CAN ID".into())) };
    let (mut refused, mut other, mut back) = (0, 0, 0);
    let start = Instant::now();
    for i in 0..count {
        let f = Frame::new(id, &[0x03, 0x01, (i % 256) as u8, 0x0D, 0xCC, 0xCC, 0xCC, 0xCC])
            .map_err(|e| LinkError::Io(e.to_string()))?;
        match link.send(&f) {
            Ok(()) => {}
            Err(LinkError::Refused(_)) => refused += 1,
            Err(e) => {
                other += 1;
                eprintln!("send {i}: {e}");
            }
        }
        // Take whatever came back, without waiting: the next command overlaps the echoes.
        while let Ok(Some(ev)) = link.recv(Duration::ZERO) {
            if matches!(ev, LinkEvent::Frame(_)) {
                back += 1;
            }
        }
    }
    while let Ok(Some(ev)) = link.recv(Duration::from_millis(200)) {
        if matches!(ev, LinkEvent::Frame(_)) {
            back += 1;
        }
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "sent {count} in {secs:.1} s ({:.0}/s): refused {refused}, other errors {other}, frames back {back}",
        count as f64 / secs
    );
    println!("bridge: {:?}", link.health()?);
    Ok(())
}
