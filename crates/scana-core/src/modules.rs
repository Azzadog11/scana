//! Stage 3: finding every module in the car and reading it, read-only.
//!
//! **Discovery** sends one tester-present request (`3E 00`) to each likely request ID and
//! listens for any answer: `7E` from a UDS module, or a refusal `7F 3E ..` from a module that
//! speaks another dialect. Both mean someone is there. The rules (`docs/DESIGN.md` 7):
//!
//! - Only while the car is still, when Scana can read its speed.
//! - Listen first, and never send on an ID heard on the bus: it belongs to someone else.
//! - Never send on the OBD-II functional ID (0x7DF) or the OBD-II reply IDs (0x7E8-0x7EF).
//! - One request at a time, at the usual pace (20 a second, 10 with another tester), and stop
//!   at the first bus error.
//!
//! **Reading a module**: its identity (UDS data identifiers, or KWP `1A`), its fault codes (UDS
//! `19 02`, KWP `18` or `13`, or OBD-II `03`/`07`), and for the data explorer, the data IDs it
//! answers (UDS `22`, KWP `21`).

use crate::client::{Addressing, Channel, Client, CoreError};
use crate::readers;
use scana_proto::obd::{self, Reply};
use scana_proto::uds::{self, CodeFormat, ModuleDtc};
use scana_proto::{CanId, isotp};

/// How module discovery searches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoverOptions {
    /// Listen this long before sending anything.
    pub listen_ms: u64,
    /// Wait this long for an answer to each probe (P2 is 50 ms).
    pub window_ms: u64,
    /// Try 11-bit request IDs 0x700-0x7F7.
    pub standard: bool,
    /// Try 29-bit request IDs 0x18DAxxF1.
    pub extended: bool,
    /// 11-bit gateway IDs to try with every address byte (Toyota family: 0x750).
    pub gateways: Vec<u16>,
    /// Extra reply offsets (reply ID minus request ID) for 11-bit probes: a reply from
    /// request + offset counts even outside 0x700-0x7FF (Chrysler family: -0x280).
    pub reply_offsets: Vec<i32>,
    /// Stop after this many modules: more means something answers everything.
    pub max_modules: usize,
    /// The OBD ECUs and their PIDs, to check the car is still every [`STILL_CHECK_EVERY`] probes
    /// (discovery takes tens of seconds, and a hybrid may creep off by itself). Empty: no checks.
    pub still_ecus: Vec<(CanId, Vec<u8>)>,
}

/// Discovery looks at vehicle speed again after this many probes.
pub const STILL_CHECK_EVERY: usize = 64;

impl Default for DiscoverOptions {
    fn default() -> Self {
        DiscoverOptions {
            listen_ms: 1000,
            window_ms: 60,
            standard: true,
            extended: false,
            gateways: vec![],
            reply_offsets: vec![],
            max_modules: 48,
            still_ecus: vec![],
        }
    }
}

impl DiscoverOptions {
    /// The search for a car: its own addressing, and the gateways its maker family uses.
    pub fn for_car(addressing: Addressing, family: Option<&str>) -> Self {
        DiscoverOptions {
            standard: addressing == Addressing::Standard,
            extended: addressing == Addressing::Extended,
            gateways: if addressing == Addressing::Standard { scana_db::modules::gateways(family) } else { vec![] },
            reply_offsets: if addressing == Addressing::Standard {
                scana_db::modules::reply_offsets(family)
            } else {
                vec![]
            },
            ..DiscoverOptions::default()
        }
    }
}

/// A module that answered discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub channel: Channel,
    /// Its answer to `3E 00` (after ISO-TP): `7E 00`, `7E`, or `7F 3E code`.
    pub answer: Vec<u8>,
}

impl Found {
    /// A guess at the dialect from the probe's answer: UDS answers `7E 00`.
    pub fn looks_like_uds(&self) -> bool {
        self.answer.first() == Some(&0x7E)
    }
}

/// How far discovery has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Listening,
    Probing { done: usize, total: usize, at: String },
    Found(Found),
}

/// What discovery found, and what it skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub found: Vec<Found>,
    /// IDs heard on the bus and so not used.
    pub skipped: Vec<CanId>,
    /// How many request IDs were tried.
    pub tried: usize,
    /// True if the user stopped it, or it stopped at `max_modules`.
    pub stopped_early: bool,
}

/// The request IDs (with address bytes) discovery will try, leaving out every ID in `seen`.
pub fn candidates(opts: &DiscoverOptions, seen: &std::collections::HashSet<CanId>) -> Vec<(CanId, Option<u8>)> {
    let mut out = Vec::new();
    if opts.standard {
        for id in 0x700u16..=0x7F7 {
            let can = CanId::Standard(id);
            let obd_request = (0x7E0..=0x7E7).contains(&id);
            if id == 0x7DF || (0x7E8..=0x7EF).contains(&id) || opts.gateways.contains(&id) {
                continue;
            }
            // Scana already uses the OBD-II request IDs; any other ID heard is someone else's.
            if seen.contains(&can) && !obd_request {
                continue;
            }
            out.push((can, None));
        }
    }
    for &gw in &opts.gateways {
        let can = CanId::Standard(gw);
        if seen.contains(&can) {
            continue;
        }
        for addr in 0u8..=0xFF {
            out.push((can, Some(addr)));
        }
    }
    if opts.extended {
        for target in 0u32..=0xFF {
            if target == 0xF1 {
                continue;
            }
            let can = CanId::Extended(0x18DA_00F1 | (target << 8));
            if seen.contains(&can) {
                continue;
            }
            out.push((can, None));
        }
    }
    out
}

/// The tester-present answer in a frame heard after probing `tx` (with `ext`), if it is one.
fn answer_in(frame: &scana_proto::Frame, tx: CanId, ext: Option<u8>, offsets: &[i32]) -> Option<Vec<u8>> {
    let plausible_id = match (tx, frame.id) {
        (CanId::Standard(t), CanId::Standard(r)) => {
            let family_reply = offsets.iter().any(|o| i32::from(t) + o == i32::from(r));
            ((0x700..=0x7FF).contains(&r) || family_reply) && frame.id != tx && r != 0x7DF
        }
        (CanId::Extended(t), CanId::Extended(r)) => {
            // 18DAF1xx from target xx.
            r & 0xFFFF_FF00 == 0x18DA_F100 && (r & 0xFF) == ((t >> 8) & 0xFF)
        }
        _ => false,
    };
    if !plausible_id {
        return None;
    }
    let data = match ext {
        None => frame.data(),
        Some(a) if frame.data().first() == Some(&a) => &frame.data()[1..],
        Some(_) => return None,
    };
    if isotp::kind_of(data) != Some(isotp::FrameKind::Single) {
        return None;
    }
    let len = usize::from(data[0] & 0x0F);
    let msg = data.get(1..1 + len)?;
    match msg {
        [0x7E, ..] | [0x7F, uds::TESTER_PRESENT, _, ..] => Some(msg.to_vec()),
        _ => None,
    }
}

/// Finds modules: listens, then probes each candidate in turn. `progress` hears about each
/// step and returns `false` to stop early.
pub fn discover(
    client: &mut Client,
    opts: &DiscoverOptions,
    mut progress: impl FnMut(Progress) -> bool,
) -> Result<Discovery, CoreError> {
    if !progress(Progress::Listening) {
        return Ok(Discovery { stopped_early: true, ..Discovery::default() });
    }
    let start = client.now_ms();
    client.idle_until(start + opts.listen_ms)?;
    let seen = client.seen_ids().clone();
    let list = candidates(opts, &seen);
    let mut out = Discovery {
        skipped: {
            let mut v: Vec<CanId> = seen.iter().copied().filter(|id| !Addressing::is_request_id(*id)).collect();
            v.sort();
            v
        },
        ..Discovery::default()
    };
    let total = list.len();
    for (i, (tx, ext)) in list.into_iter().enumerate() {
        let label = match ext {
            Some(a) => format!("{tx}/{a:02X}"),
            None => tx.to_string(),
        };
        if !progress(Progress::Probing { done: i, total, at: label }) {
            out.stopped_early = true;
            break;
        }
        if i > 0 && i % STILL_CHECK_EVERY == 0 && !opts.still_ecus.is_empty() {
            check_still(client, &opts.still_ecus)?;
        }
        let heard = client.probe(tx, ext, &uds::PROBE, opts.window_ms)?;
        out.tried += 1;
        let mut answers: Vec<(CanId, Vec<u8>)> = heard
            .iter()
            .filter_map(|tf| answer_in(&tf.frame, tx, ext, &opts.reply_offsets).map(|a| (tf.frame.id, a)))
            .collect();
        answers.dedup_by_key(|(id, _)| *id);
        // More than one answer to one probe: prefer the usual reply ID.
        let conventional = Channel::conventional(tx, ext).map(|c| c.rx);
        if answers.len() > 1
            && let Some(pos) = answers.iter().position(|(id, _)| Some(*id) == conventional)
        {
            answers = vec![answers.swap_remove(pos)];
        }
        for (rx, answer) in answers {
            let f = Found { channel: Channel::new(tx, rx, ext), answer };
            if !out.found.iter().any(|x| x.channel == f.channel) {
                progress(Progress::Found(f.clone()));
                out.found.push(f);
            }
        }
        if out.found.len() >= opts.max_modules {
            out.stopped_early = true;
            break;
        }
    }
    Ok(out)
}

/// Refuses while the car moves, using vehicle speed (PID 0D) where an ECU reports it.
/// Returns a note when no ECU reports speed, so the caller can say discovery ran unchecked.
pub fn check_still(client: &mut Client, ecus: &[(CanId, Vec<u8>)]) -> Result<Option<String>, CoreError> {
    let mut checked = false;
    // Every computer that reports speed: one may read 0 while another does not.
    for (ecu, supported) in ecus {
        if !supported.contains(&0x0D) {
            continue;
        }
        if let Some(r) = readers::read_pids(client, *ecu, &[0x0D])?.first() {
            let v = r.decoded.as_ref().and_then(|d| d.main()).map_or(0.0, |q| q.value);
            if v > 0.0 {
                return Err(CoreError::Unsafe(format!(
                    "The car is moving ({v:.0} km/h). Scana only looks for modules while the car is still."
                )));
            }
            checked = true;
        }
    }
    if checked {
        return Ok(None);
    }
    Ok(Some("No computer reports vehicle speed, so Scana couldn't check the car is still.".into()))
}

/// The dialect a module speaks, as far as Scana could tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialect {
    Uds,
    Kwp,
    /// Only OBD-II services.
    Obd,
    Unknown,
}

impl Dialect {
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Uds => "UDS",
            Dialect::Kwp => "KWP2000",
            Dialect::Obd => "OBD-II only",
            Dialect::Unknown => "unknown",
        }
    }
}

/// Where a module's name came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameFrom {
    /// The module said it (UDS F197 or KWP 1A 97), or the vehicle profile names it.
    Module,
    /// The usual module at this address for the make (`data/modules/addresses.toml`).
    Address,
    Unknown,
}

/// What Scana read from one module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInfo {
    pub channel: Channel,
    pub name: String,
    pub name_from: NameFrom,
    pub dialect: Dialect,
    /// Identity lines: ("Part number", "89170-...").
    pub identity: Vec<(String, String)>,
    /// Fault codes, or why they couldn't be read.
    pub codes: Result<Vec<ModuleDtc>, String>,
}

impl ModuleInfo {
    /// A name for a module before anything is read from it.
    pub fn named(channel: Channel, family: Option<&str>) -> Self {
        let (name, from) = match scana_db::modules::name_for(family, &channel.label()) {
            Some(a) => (a.name.clone(), NameFrom::Address),
            None => (format!("Module at {}", channel.label()), NameFrom::Unknown),
        };
        ModuleInfo {
            channel,
            name,
            name_from: from,
            dialect: Dialect::Unknown,
            identity: vec![],
            codes: Err("not read yet".into()),
        }
    }

    /// The name with a hint of how sure Scana is.
    pub fn display_name(&self) -> String {
        match self.name_from {
            NameFrom::Address => format!("{} (probably)", self.name),
            _ => self.name.clone(),
        }
    }
}

fn refused_code(e: &CoreError) -> Option<u8> {
    match e {
        CoreError::Refused { code, .. } => Some(*code),
        _ => None,
    }
}

/// True for errors that end the whole job (bus trouble, the bridge gone), as opposed to one
/// module refusing or staying quiet.
pub fn is_fatal(e: &CoreError) -> bool {
    !matches!(e, CoreError::Refused { .. } | CoreError::NoReply(_) | CoreError::IsoTp(_) | CoreError::Obd(_))
}

/// Reads a module's identity and fault codes. `found` is its discovery answer, if any.
pub fn read_module(
    client: &mut Client,
    channel: Channel,
    found: Option<&Found>,
    family: Option<&str>,
) -> Result<ModuleInfo, CoreError> {
    let mut info = ModuleInfo::named(channel, family);
    let try_uds_first = found.is_none_or(Found::looks_like_uds);
    let order = if try_uds_first { [Dialect::Uds, Dialect::Kwp] } else { [Dialect::Kwp, Dialect::Uds] };
    for d in order {
        let ok = match d {
            Dialect::Uds => uds_identity(client, channel, &mut info.identity)?,
            _ => kwp_identity(client, channel, &mut info.identity)?,
        };
        if ok {
            info.dialect = d;
            break;
        }
    }
    if info.dialect == Dialect::Unknown && channel.is_obd() {
        info.dialect = Dialect::Obd;
        if let Ok(data) = client.request_one_on(channel, &obd::vehicle_info_request(0x0A)) {
            let name = obd::parse_text_item(data.get(1..).unwrap_or(&[]));
            if !name.is_empty() {
                info.identity.push(("Name (OBD-II)".into(), name));
            }
        }
    }
    if let Some((_, n)) = info.identity.iter().find(|(k, _)| k == "System name")
        && !n.is_empty()
    {
        info.name = n.clone();
        info.name_from = NameFrom::Module;
    }
    info.codes = match read_codes(client, channel, info.dialect) {
        Ok(c) => Ok(c),
        Err(e) if is_fatal(&e) => return Err(e),
        Err(e) => Err(e.to_string()),
    };
    Ok(info)
}

/// UDS identity DIDs. Returns false if the module doesn't speak UDS (`7F 22 11`, or no
/// answer to the first request).
fn uds_identity(client: &mut Client, ch: Channel, out: &mut Vec<(String, String)>) -> Result<bool, CoreError> {
    let mut spoke = false;
    for (i, (did, name)) in uds::IDENTITY_DIDS.iter().enumerate() {
        match client.request_one_on(ch, &uds::read_did_request(*did)) {
            Ok(data) => {
                spoke = true;
                if let Some(v) = uds::did_value(*did, &data)
                    && !v.is_empty()
                {
                    out.push(((*name).to_string(), uds::text_of(&v).unwrap_or_else(|| uds::hex(&v))));
                }
            }
            Err(e) if refused_code(&e) == Some(0x11) => return Ok(spoke),
            Err(e) if refused_code(&e).is_some() => spoke = true,
            Err(CoreError::NoReply(_)) if i == 0 => return Ok(false),
            Err(CoreError::NoReply(_)) => {}
            Err(e) if is_fatal(&e) => return Err(e),
            Err(_) => {}
        }
    }
    Ok(spoke)
}

/// KWP identity: Toyota-style `1A 88` first, then the common `1A` options. Returns false if
/// the module refuses `1A` altogether.
fn kwp_identity(client: &mut Client, ch: Channel, out: &mut Vec<(String, String)>) -> Result<bool, CoreError> {
    let mut spoke = false;
    match client.request_one_on(ch, &[uds::KWP_READ_ECU_ID, 0x88, 0x00]) {
        Ok(data) => {
            spoke = true;
            for (n, &sub) in data.iter().skip(2).take(8).enumerate() {
                if let Ok(v) = client.request_one_on(ch, &[uds::KWP_READ_ECU_ID, 0x88, sub]) {
                    let bytes = v.get(2..).unwrap_or(&[]);
                    let text = uds::text_of(bytes).unwrap_or_else(|| uds::hex(bytes));
                    let label =
                        if n == 0 { "Part and software number".to_string() } else { format!("Number {}", n + 1) };
                    out.push((label, text));
                }
            }
        }
        Err(e) if refused_code(&e) == Some(0x11) => return Ok(false),
        Err(e) if refused_code(&e).is_some() => spoke = true,
        Err(CoreError::NoReply(_)) => return Ok(false),
        Err(e) if is_fatal(&e) => return Err(e),
        Err(_) => {}
    }
    for (opt, name) in uds::KWP_IDENTITY_OPTIONS {
        match client.request_one_on(ch, &[uds::KWP_READ_ECU_ID, *opt]) {
            Ok(v) => {
                spoke = true;
                let bytes = v.get(1..).unwrap_or(&[]);
                if !bytes.is_empty() {
                    out.push(((*name).to_string(), uds::text_of(bytes).unwrap_or_else(|| uds::hex(bytes))));
                }
            }
            Err(e) if refused_code(&e) == Some(0x11) => break,
            Err(e) if is_fatal(&e) => return Err(e),
            Err(_) => {}
        }
    }
    Ok(spoke)
}

/// The status bit that marks an OBD-II pending code (service 07) in a [`ModuleDtc`].
const OBD_PENDING_BIT: u8 = 0x04;

/// A module's fault codes, trying the ways its dialect allows.
pub fn read_codes(client: &mut Client, ch: Channel, dialect: Dialect) -> Result<Vec<ModuleDtc>, CoreError> {
    let mut last_err = None;
    let attempts: &[&[u8]] = match dialect {
        Dialect::Uds => &[&[0x19, 0x02, 0xFF], &[0x19, 0x02, 0x08]],
        Dialect::Kwp => &[&[0x18, 0x00, 0xFF, 0x00], &[0x18, 0x02, 0xFF, 0x00], &[0x13]],
        Dialect::Obd | Dialect::Unknown => &[],
    };
    let mut out = Vec::new();
    let mut enhanced_ok = false;
    for req in attempts {
        match client.request_one_on(ch, req) {
            Ok(data) => {
                let parsed = match req[0] {
                    uds::READ_DTC_INFO => uds::parse_uds_dtcs(&data),
                    uds::KWP_READ_DTC_BY_STATUS => uds::parse_kwp_dtcs(&data),
                    _ => uds::parse_kwp13_dtcs(&data),
                };
                out = parsed
                    .map_err(|e| CoreError::BadRequest(format!("the module's code list didn't make sense: {e}")))?;
                enhanced_ok = true;
                break;
            }
            Err(e) if is_fatal(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    // An OBD-II computer: add its OBD-II stored and pending codes, which its enhanced side
    // may not list.
    if ch.is_obd() || dialect == Dialect::Obd {
        if enhanced_ok {
            last_err = None;
        }
        for service in [obd::STORED_CODES, obd::PENDING_CODES] {
            if let Ok(data) = client.request_one_on(ch, &[service]) {
                for code in scana_proto::dtc::parse_dtc_list(&data).unwrap_or_default() {
                    if !out.iter().any(|d: &ModuleDtc| d.code == code) {
                        // A pending code is not stored yet: mark it so (the UDS pending bit).
                        let status = (service == obd::PENDING_CODES).then_some(OBD_PENDING_BIT);
                        out.push(ModuleDtc { code, failure_type: None, status, format: CodeFormat::Obd });
                    }
                }
                last_err = None;
            }
        }
        if last_err.is_none() {
            return Ok(out);
        }
    } else if enhanced_ok {
        return Ok(out);
    }
    Err(last_err
        .unwrap_or_else(|| CoreError::BadRequest("Scana doesn't know how to read codes from this module".into())))
}

/// Which kind of data identifier to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataKind {
    /// UDS `22`: 16-bit data identifiers.
    Did,
    /// KWP `21`: 8-bit local identifiers.
    Lid,
}

impl DataKind {
    pub fn request(self, id: u16) -> Vec<u8> {
        match self {
            DataKind::Did => uds::read_did_request(id),
            DataKind::Lid => uds::read_lid_request((id & 0xFF) as u8),
        }
    }

    /// The value bytes from a positive reply (after the service byte), without the echoed ID.
    pub fn value(self, id: u16, data: &[u8]) -> Option<Vec<u8>> {
        match self {
            DataKind::Did => uds::did_value(id, data),
            DataKind::Lid => (data.first() == Some(&((id & 0xFF) as u8))).then(|| data[1..].to_vec()),
        }
    }

    /// "DID F190" or "LID 01".
    pub fn label(self, id: u16) -> String {
        match self {
            DataKind::Did => format!("DID {id:04X}"),
            DataKind::Lid => format!("LID {:02X}", id & 0xFF),
        }
    }

    /// The kind that suits a dialect.
    pub fn for_dialect(d: Dialect) -> Self {
        if d == Dialect::Kwp { DataKind::Lid } else { DataKind::Did }
    }
}

/// One data identifier and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataItem {
    pub id: u16,
    pub bytes: Vec<u8>,
}

/// Reads one data identifier.
pub fn read_data(client: &mut Client, ch: Channel, kind: DataKind, id: u16) -> Result<Vec<u8>, CoreError> {
    let data = client.request_one_on(ch, &kind.request(id))?;
    kind.value(id, &data).ok_or_else(|| CoreError::BadRequest(format!("{} answered for another ID", ch.label())))
}

/// Reads every ID in `ids` and keeps the ones the module answers. `progress(done, total)`
/// returns false to stop. Stops with an error if the module refuses the service itself, or
/// stops answering (5 silences in a row).
pub fn scan_data(
    client: &mut Client,
    ch: Channel,
    kind: DataKind,
    ids: std::ops::RangeInclusive<u16>,
    mut progress: impl FnMut(usize, usize) -> bool,
) -> Result<Vec<DataItem>, CoreError> {
    let total = usize::from(ids.end().saturating_sub(*ids.start())) + 1;
    let mut out = Vec::new();
    let mut silent = 0;
    for (i, id) in ids.enumerate() {
        if !progress(i, total) {
            break;
        }
        match read_data(client, ch, kind, id) {
            Ok(bytes) => {
                silent = 0;
                out.push(DataItem { id, bytes });
            }
            Err(e) if refused_code(&e) == Some(0x11) => {
                return Err(CoreError::BadRequest(format!(
                    "{} doesn't support this kind of read ({}). Try the other kind.",
                    ch.label(),
                    match kind {
                        DataKind::Did => "UDS data identifiers",
                        DataKind::Lid => "KWP local identifiers",
                    }
                )));
            }
            Err(CoreError::NoReply(_)) => {
                silent += 1;
                if silent >= 5 {
                    return Err(CoreError::NoReply(format!("{}: it stopped answering", ch.label())));
                }
            }
            Err(e) if is_fatal(&e) => return Err(e),
            Err(_) => silent = 0,
        }
    }
    Ok(out)
}

/// A request typed by the user (the console): checked against the safety levels first. Only
/// reads are allowed in stage 3.
pub fn console_request(client: &mut Client, ch: Channel, request: &[u8]) -> Result<Option<Reply>, CoreError> {
    let level = uds::level_of(request);
    if level != uds::Level::Read {
        return Err(CoreError::Unsafe(format!(
            "Scana won't send {} ({}): {}. Only read requests are allowed here.",
            uds::hex(request),
            uds::service_name(request.first().copied().unwrap_or(0)),
            level.refusal()
        )));
    }
    Ok(client.request_on(ch, request)?.map(|r| r.reply))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_link::sim::SimLink;
    use scana_link::{Link, OpenMode};
    use scana_sim::VirtualCar;

    /// A client on a simulated car, with a short gap between requests so tests run quickly
    /// (the simulated modules answer within 3 ms).
    fn client(car: VirtualCar) -> Client {
        let mut link = SimLink::new(car);
        link.open(OpenMode::Normal).unwrap();
        let mut c = Client::new(Box::new(link));
        c.options.min_gap_ms = 1;
        c.options.shared_gap_ms = 1;
        c
    }

    fn fast() -> DiscoverOptions {
        DiscoverOptions { listen_ms: 200, window_ms: 8, gateways: vec![0x750], ..DiscoverOptions::default() }
    }

    #[test]
    fn candidates_leave_out_ids_in_use() {
        let mut seen = std::collections::HashSet::new();
        seen.insert(CanId::Standard(0x7B0));
        seen.insert(CanId::Standard(0x7E0));
        let c = candidates(&DiscoverOptions::default(), &seen);
        assert!(!c.contains(&(CanId::Standard(0x7B0), None)));
        assert!(c.contains(&(CanId::Standard(0x7E0), None)));
        assert!(!c.iter().any(|(id, _)| *id == CanId::Standard(0x7DF)));
        assert!(!c.iter().any(|(id, _)| matches!(id, CanId::Standard(x) if (0x7E8..=0x7EF).contains(x))));
        assert_eq!(c.len(), 0xF8 - 1 - 8 - 1);
        let with_gw = candidates(&DiscoverOptions { gateways: vec![0x750], ..DiscoverOptions::default() }, &seen);
        assert_eq!(with_gw.iter().filter(|(_, e)| e.is_some()).count(), 256);
        seen.insert(CanId::Standard(0x750));
        let busy_gw = candidates(&DiscoverOptions { gateways: vec![0x750], ..DiscoverOptions::default() }, &seen);
        assert_eq!(busy_gw.iter().filter(|(_, e)| e.is_some()).count(), 0);
        let ext = candidates(&DiscoverOptions { standard: false, extended: true, ..DiscoverOptions::default() }, &seen);
        assert_eq!(ext.len(), 255);
    }

    #[test]
    fn discovery_finds_the_demo_modules() {
        let mut c = client(VirtualCar::hijet());
        let mut probes = 0;
        let d = discover(&mut c, &fast(), |p| {
            if matches!(p, Progress::Probing { .. }) {
                probes += 1;
            }
            true
        })
        .unwrap();
        let labels: Vec<String> = d.found.iter().map(|f| f.channel.label()).collect();
        assert_eq!(labels, vec!["780", "7B0", "7C0", "7C4", "7E0", "750/40"], "{labels:?}");
        assert_eq!(probes, d.tried);
        let body = d.found.iter().find(|f| f.channel.ext == Some(0x40)).unwrap();
        assert_eq!(body.channel.rx, CanId::Standard(0x758));
        assert!(!body.looks_like_uds());
        assert!(d.found.iter().find(|f| f.channel.label() == "780").unwrap().looks_like_uds());
    }

    #[test]
    fn discovery_with_a_dash_on_the_bus_still_works_and_skips_nothing_it_needs() {
        let mut c = client(VirtualCar::hijet().with_dash());
        let opts = DiscoverOptions { gateways: vec![], ..fast() };
        let d = discover(&mut c, &opts, |_| true).unwrap();
        assert_eq!(d.found.len(), 5);
        // The dash's 7DF and the ECU's 7E8 were heard, and are listed as skipped (7E8 only:
        // 7DF is a request ID Scana never probes anyway).
        assert!(d.skipped.contains(&CanId::Standard(0x7E8)));
    }

    #[test]
    fn discovery_on_29_bit_ids() {
        let mut c = client(VirtualCar::hijet().extended());
        c.addressing = Addressing::Extended;
        let opts =
            DiscoverOptions { listen_ms: 100, window_ms: 8, ..DiscoverOptions::for_car(Addressing::Extended, None) };
        let d = discover(&mut c, &opts, |_| true).unwrap();
        // Five modules, and the engine computer, which refuses 3E (7F 3E 11) but is there.
        assert_eq!(d.found.len(), 6);
        assert!(d.found.iter().all(|f| f.channel.tx.is_extended()));
        assert!(d.found.iter().any(|f| f.channel.tx == CanId::Extended(0x18DA_10F1) && !f.looks_like_uds()));
    }

    #[test]
    fn a_reply_at_request_minus_0x280_is_found_only_for_the_chrysler_family() {
        let build = || {
            let mut car = VirtualCar::hijet().without_modules();
            let std = CanId::Standard;
            let mut abs = scana_sim::modules::Module::new(
                "ABS (simulated)",
                std(0x747),
                std(0x4C7),
                None,
                scana_sim::modules::Dialect::Uds,
            );
            abs.identity = vec![(0xF197, b"SIM ABS".to_vec())];
            car.modules.push(abs);
            car
        };
        let opts = |family| DiscoverOptions {
            listen_ms: 100,
            window_ms: 8,
            ..DiscoverOptions::for_car(Addressing::Standard, family)
        };
        assert_eq!(opts(Some("chrysler")).reply_offsets, vec![-0x280]);
        assert!(opts(None).reply_offsets.is_empty());
        let mut c = client(build());
        let d = discover(&mut c, &opts(Some("chrysler")), |_| true).unwrap();
        let abs = d.found.iter().find(|f| f.channel.label() == "747").expect("747 found");
        assert_eq!(abs.channel.rx, CanId::Standard(0x4C7));
        assert!(abs.looks_like_uds());
        // The channel it gets works for later reads.
        let info = read_module(&mut c, abs.channel, None, Some("chrysler")).unwrap();
        assert!(info.identity.iter().any(|(_, v)| v.contains("SIM ABS")), "{info:?}");
        let mut c = client(build());
        let d = discover(&mut c, &opts(None), |_| true).unwrap();
        assert!(d.found.iter().all(|f| f.channel.label() != "747"), "{:?}", d.found);
        let mut c = client(build());
        let d = discover(&mut c, &opts(Some("toyota")), |_| true).unwrap();
        assert!(d.found.iter().all(|f| f.channel.label() != "747"));
    }

    #[test]
    fn discovery_can_be_stopped() {
        let mut c = client(VirtualCar::hijet());
        let d = discover(&mut c, &fast(), |p| !matches!(p, Progress::Probing { done: 10, .. })).unwrap();
        assert!(d.stopped_early);
        assert_eq!(d.tried, 10);
    }

    #[test]
    fn reading_modules_gets_names_identity_and_codes() {
        let mut c = client(VirtualCar::hijet_with_faults());
        let srs = Channel::parse("780").unwrap();
        let m = read_module(&mut c, srs, None, Some("toyota")).unwrap();
        assert_eq!(m.dialect, Dialect::Uds);
        assert_eq!(m.name, "SRS AIRBAG SIM");
        assert_eq!(m.name_from, NameFrom::Module);
        assert!(m.identity.iter().any(|(k, v)| k == "Part number" && v == "89170-SIM01"));
        assert_eq!(m.codes, Ok(vec![]));

        let meter = read_module(&mut c, Channel::parse("7C0").unwrap(), None, Some("toyota")).unwrap();
        let codes = meter.codes.unwrap();
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].to_string(), "U0100-87");

        let abs_found = Found { channel: Channel::parse("7B0").unwrap(), answer: vec![0x7F, 0x3E, 0x12] };
        let abs = read_module(&mut c, abs_found.channel, Some(&abs_found), Some("toyota")).unwrap();
        assert_eq!(abs.dialect, Dialect::Kwp);
        assert_eq!(abs.name, "SIM ABS");
        assert!(abs.identity.iter().any(|(_, v)| v == "89541-SIM01"));
        assert_eq!(abs.codes.unwrap()[0].code.to_string(), "U0100");

        // The engine computer's enhanced side lists no codes, but its OBD-II side has P0128.
        let ecm = read_module(&mut c, Channel::parse("7E0").unwrap(), None, Some("toyota")).unwrap();
        assert_eq!(ecm.codes.unwrap().iter().map(|d| d.code.to_string()).collect::<Vec<_>>(), vec!["P0128", "P0171"]);

        let body_ch = Channel::parse("750/40").unwrap();
        let body = read_module(&mut c, body_ch, None, Some("toyota")).unwrap();
        assert_eq!(body.dialect, Dialect::Kwp);
        assert_eq!(body.name, "Body control (main body computer)");
        assert_eq!(body.name_from, NameFrom::Address);
        assert_eq!(body.display_name(), "Body control (main body computer) (probably)");
    }

    #[test]
    fn scanning_finds_data_and_stops_on_the_wrong_kind() {
        let mut c = client(VirtualCar::hijet());
        let body = Channel::parse("750/40").unwrap();
        let items = scan_data(&mut c, body, DataKind::Lid, 0x00..=0x0F, |_, _| true).unwrap();
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(items[0].bytes.len(), 3);
        let e = scan_data(&mut c, body, DataKind::Did, 0x0100..=0x0110, |_, _| true).unwrap_err();
        assert!(e.to_string().contains("doesn't support"), "{e}");
        let meter = Channel::parse("7C0").unwrap();
        let items = scan_data(&mut c, meter, DataKind::Did, 0x1000..=0x1008, |_, _| true).unwrap();
        assert_eq!(items.len(), 4);
        // Nobody at 7D0: it gives up after 5 silences.
        let e = scan_data(&mut c, Channel::parse("7D0").unwrap(), DataKind::Did, 0..=20, |_, _| true).unwrap_err();
        assert!(matches!(e, CoreError::NoReply(_)));
    }

    #[test]
    fn the_console_only_sends_reads() {
        let mut c = client(VirtualCar::hijet());
        let meter = Channel::parse("7C0").unwrap();
        let r = console_request(&mut c, meter, &[0x22, 0x10, 0x02]).unwrap();
        assert!(matches!(r, Some(Reply::Positive { service: 0x22, .. })));
        for bad in [&[0x2E, 0x10, 0x02, 0x00][..], &[0x27, 0x01], &[0x31, 0x01, 0x02, 0x03], &[0x14, 0xFF, 0xFF, 0xFF]]
        {
            let sent = c.requests_sent;
            let e = console_request(&mut c, meter, bad).unwrap_err();
            assert!(matches!(e, CoreError::Unsafe(_)), "{e}");
            assert_eq!(c.requests_sent, sent, "nothing sent for {bad:02X?}");
        }
    }

    #[test]
    fn discovery_stops_when_a_hybrid_creeps_off_on_electric_power() {
        // The hybrid stands for 20 s, then moves with its engine at 0 rpm.
        let mut c = client(VirtualCar::hybrid());
        let ecus = vec![(CanId::Standard(0x7E8), vec![0x0D]), (CanId::Standard(0x7EA), vec![0x0D])];
        c.idle_until(19_000).unwrap();
        assert_eq!(check_still(&mut c, &ecus).unwrap(), None);
        let opts = DiscoverOptions { still_ecus: ecus.clone(), ..fast() };
        let e = discover(&mut c, &opts, |_| true).unwrap_err();
        assert!(matches!(e, CoreError::Unsafe(ref m) if m.contains("moving")), "{e}");
        // Without the checks it carries on to the end.
        let mut c = client(VirtualCar::hybrid());
        c.idle_until(19_000).unwrap();
        assert!(discover(&mut c, &fast(), |_| true).is_ok());
    }

    #[test]
    fn a_pending_obd_code_is_shown_as_pending_in_the_module_view() {
        let mut c = client(VirtualCar::hatch_two_ecus());
        let ecm = read_module(&mut c, Channel::parse("7E0").unwrap(), None, None).unwrap();
        let codes = ecm.codes.unwrap();
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].state().words(), "pending");
    }

    #[test]
    fn check_still_reads_speed() {
        let mut c = client(VirtualCar::hijet());
        let ecus = vec![(CanId::Standard(0x7E8), vec![0x0C, 0x0D])];
        assert_eq!(check_still(&mut c, &ecus).unwrap(), None);
        let note = check_still(&mut c, &[(CanId::Standard(0x7E8), vec![0x0C])]).unwrap();
        assert!(note.is_some());
    }
}
