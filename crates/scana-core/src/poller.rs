//! Which live values to ask for next. Each value has a wanted period: engine speed and other
//! fast-changing values every 200 ms, temperatures every second, things that rarely change every
//! 5 s. Due values for one ECU are packed 6 to a request (service 01 on CAN allows 6).
//! While another tester (a dash display) is on the bus, only as many as fit a one-frame
//! reply: the ECU can cut off a longer reply to answer the other tester.

use scana_proto::{CanId, obd, pids};

#[derive(Clone, Debug)]
struct Entry {
    ecu: CanId,
    pid: u8,
    period_ms: u64,
    next_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Poller {
    entries: Vec<Entry>,
    /// Pack only what fits a one-frame reply (set while another tester is active).
    pub one_frame: bool,
}

/// How often to ask for a PID, in ms.
pub fn period_for(pid: u8) -> u64 {
    match pid {
        0x04
        | 0x06
        | 0x08
        | 0x0B
        | 0x0C
        | 0x0D
        | 0x0E
        | 0x10
        | 0x11
        | 0x14..=0x1B
        | 0x24..=0x2B
        | 0x34..=0x3B
        | 0x43..=0x45
        | 0x47..=0x4C
        | 0x5A
        | 0x5E
        | 0x61
        | 0x62 => 200,
        0x01 | 0x13 | 0x1C | 0x1D | 0x21 | 0x30 | 0x31 | 0x41 | 0x4D | 0x4E | 0x4F | 0x50 | 0x51 | 0x5F => 5000,
        _ => 1000,
    }
}

impl Poller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the list of values to poll. Support masks are ignored.
    pub fn set(&mut self, list: &[(CanId, u8)], now_ms: u64) {
        self.entries = list
            .iter()
            .filter(|(_, p)| !pids::is_support_pid(*p))
            .map(|&(ecu, pid)| Entry { ecu, pid, period_ms: period_for(pid), next_ms: now_ms })
            .collect();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn list(&self) -> Vec<(CanId, u8)> {
        self.entries.iter().map(|e| (e.ecu, e.pid)).collect()
    }

    /// When the next value falls due.
    pub fn next_due(&self) -> Option<u64> {
        self.entries.iter().map(|e| e.next_ms).min()
    }

    /// The next request: one ECU and up to 6 of its PIDs that are due, most overdue first.
    /// A PID Scana doesn't know the length of goes alone (its reply can't be split otherwise).
    pub fn next_batch(&mut self, now_ms: u64) -> Option<(CanId, Vec<u8>)> {
        let first = self.entries.iter().filter(|e| e.next_ms <= now_ms).min_by_key(|e| e.next_ms)?;
        let ecu = first.ecu;
        let unknown_first = pids::def(first.pid).is_none();
        let mut due: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.entries[i].ecu == ecu && self.entries[i].next_ms <= now_ms)
            .collect();
        due.sort_by_key(|&i| self.entries[i].next_ms);
        let chosen: Vec<usize> = if unknown_first {
            due.into_iter().take(1).collect()
        } else if self.one_frame {
            let mut pick: Vec<usize> = Vec::new();
            let mut pids_so_far: Vec<u8> = Vec::new();
            for i in due.into_iter().filter(|&i| pids::def(self.entries[i].pid).is_some()) {
                pids_so_far.push(self.entries[i].pid);
                if pick.is_empty() || obd::reply_fits_one_frame(&pids_so_far) {
                    pick.push(i);
                } else {
                    pids_so_far.pop();
                }
            }
            pick
        } else {
            due.into_iter()
                .filter(|&i| pids::def(self.entries[i].pid).is_some())
                .take(obd::MAX_PIDS_PER_REQUEST)
                .collect()
        };
        let mut batch = Vec::with_capacity(chosen.len());
        for i in chosen {
            let e = &mut self.entries[i];
            batch.push(e.pid);
            // Keep a steady rhythm, but don't try to catch up a long backlog.
            e.next_ms = (e.next_ms + e.period_ms).max(now_ms + e.period_ms / 2);
        }
        Some((ecu, batch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECU: CanId = CanId::Standard(0x7E8);

    #[test]
    fn packs_six_and_respects_periods() {
        let mut p = Poller::new();
        let list: Vec<(CanId, u8)> =
            [0x0C, 0x0D, 0x05, 0x42, 0x04, 0x06, 0x0F, 0x00].iter().map(|&x| (ECU, x)).collect();
        p.set(&list, 0);
        assert_eq!(p.list().len(), 7); // the 00 mask is dropped
        let (ecu, first) = p.next_batch(0).unwrap();
        assert_eq!(ecu, ECU);
        assert_eq!(first.len(), 6);
        let (_, second) = p.next_batch(0).unwrap();
        assert_eq!(second.len(), 1);
        assert!(p.next_batch(0).is_none());
        // At 200 ms only the fast ones are due again.
        let (_, fast) = p.next_batch(200).unwrap();
        assert!(fast.iter().all(|x| period_for(*x) == 200), "{fast:?}");
        assert!(fast.contains(&0x0C));
        assert!(!fast.contains(&0x05));
        assert_eq!(p.next_due(), Some(400));
    }

    #[test]
    fn rates_over_ten_seconds() {
        let mut p = Poller::new();
        p.set(&[(ECU, 0x0C), (ECU, 0x05), (ECU, 0x4E)], 0);
        let mut counts = std::collections::HashMap::new();
        let mut t = 0;
        while t < 10_000 {
            while let Some((_, b)) = p.next_batch(t) {
                for x in b {
                    *counts.entry(x).or_insert(0) += 1;
                }
            }
            t += 50;
        }
        assert_eq!(counts[&0x0C], 50);
        assert_eq!(counts[&0x05], 10);
        assert_eq!(counts[&0x4E], 2);
    }

    #[test]
    fn one_frame_replies_while_another_tester_is_on() {
        let mut p = Poller::new();
        let list: Vec<(CanId, u8)> = [0x0C, 0x0D, 0x05, 0x42, 0x04, 0x11, 0x4F].iter().map(|&x| (ECU, x)).collect();
        p.set(&list, 0);
        p.one_frame = true;
        let mut seen = Vec::new();
        while let Some((_, b)) = p.next_batch(0) {
            assert!(obd::reply_fits_one_frame(&b), "{b:02X?}");
            seen.extend(b);
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![0x04, 0x05, 0x0C, 0x0D, 0x11, 0x42, 0x4F]);
    }

    #[test]
    fn unknown_pids_go_alone() {
        let mut p = Poller::new();
        p.set(&[(ECU, 0x9D), (ECU, 0x0C)], 0);
        let (_, a) = p.next_batch(0).unwrap();
        let (_, b) = p.next_batch(0).unwrap();
        assert!(a.len() == 1 && b.len() == 1);
    }
}
