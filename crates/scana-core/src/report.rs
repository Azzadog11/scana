//! The health check: everything that matters in one pass, a plain-English summary, and a
//! report page (HTML) to keep or print.

use crate::client::{Client, CoreError};
use crate::modules::ModuleInfo;
use crate::readers::{self, FreezeFrame, PidReading, VehicleInfo};
use scana_db::{dtc as dtcdb, profile::Profile};
use scana_proto::mode06::{self, TestResult};
use scana_proto::obd;
use scana_proto::pids::MonitorStatus;
use scana_proto::{CanId, Dtc};

/// One ECU's part of the report.
#[derive(Clone, Debug, PartialEq)]
pub struct EcuReport {
    pub ecu: CanId,
    pub name: String,
    pub monitors: Option<MonitorStatus>,
    pub stored: Vec<Dtc>,
    pub pending: Vec<Dtc>,
    pub permanent: Vec<Dtc>,
    pub freeze: Option<FreezeFrame>,
    pub values: Vec<PidReading>,
    pub info: VehicleInfo,
    pub tests: Vec<TestResult>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HealthReport {
    pub when: String,
    pub vehicle: String,
    pub link: String,
    pub protocol: String,
    pub ecus: Vec<EcuReport>,
    /// Every module found by module discovery, with its codes read during the check.
    pub modules: Vec<ModuleInfo>,
}

/// How a summary line should look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Note,
    Bad,
}

/// Values worth a look in a health check, when the car supports them.
const KEY_VALUES: [u8; 10] = [0x05, 0x0C, 0x42, 0x04, 0x06, 0x07, 0x0F, 0x10, 0x03, 0x1F];

/// Runs the health check on every ECU. `ecus` lists each ECU with its supported PIDs.
pub fn run_health_check(
    client: &mut Client,
    ecus: &[(CanId, Vec<u8>)],
    profile: Option<&Profile>,
    vehicle: &str,
    protocol: &str,
) -> Result<HealthReport, CoreError> {
    let stored = readers::read_codes(client, obd::STORED_CODES)?;
    let pending = readers::read_codes(client, obd::PENDING_CODES)?;
    let permanent = readers::read_codes(client, obd::PERMANENT_CODES).unwrap_or_default();
    let codes_for = |list: &[(CanId, Vec<Dtc>)], ecu: CanId| -> Vec<Dtc> {
        list.iter().find(|(e, _)| *e == ecu).map(|(_, c)| c.clone()).unwrap_or_default()
    };
    let mut out = Vec::new();
    for (ecu, supported) in ecus {
        let monitors = if supported.contains(&0x01) { Some(readers::readiness(client, *ecu)?) } else { None };
        let st = codes_for(&stored, *ecu);
        let freeze = if st.is_empty() { None } else { readers::freeze_frame(client, *ecu)? };
        let want: Vec<u8> = KEY_VALUES.into_iter().filter(|p| supported.contains(p)).collect();
        let values = if want.is_empty() { vec![] } else { readers::read_pids(client, *ecu, &want)? };
        let info = readers::vehicle_info(client, *ecu)?;
        let tests = readers::test_results(client, *ecu)?;
        let id = u16::try_from(ecu.raw()).unwrap_or(0);
        out.push(EcuReport {
            ecu: *ecu,
            name: profile.and_then(|p| p.ecu_name(id)).map_or_else(|| ecu_default_name(*ecu), str::to_string),
            monitors,
            stored: st,
            pending: codes_for(&pending, *ecu),
            permanent: codes_for(&permanent, *ecu),
            freeze,
            values,
            info,
            tests,
        });
    }
    Ok(HealthReport {
        when: now_text(),
        vehicle: vehicle.into(),
        link: client.link().describe(),
        protocol: protocol.into(),
        ecus: out,
        modules: vec![],
    })
}

/// A module code as one line: "U0100-87 (stored): Lost communication with the engine computer;
/// a network message is missing".
pub fn module_code_line(d: &scana_proto::uds::ModuleDtc) -> String {
    let mut text = format!("{d} ({}): {}", d.state().words(), dtcdb::lookup(d.code).summary());
    if let Some(ft) = d.failure_type
        && ft != 0
    {
        text.push_str(&format!("; {}", scana_proto::uds::failure_type_text(ft)));
    }
    text
}

/// The local time as "2026-10-16 10:30", for reports.
pub fn now_text() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// "Engine (7E8)" for the usual OBD IDs.
pub fn ecu_default_name(ecu: CanId) -> String {
    match ecu {
        CanId::Standard(0x7E8) | CanId::Extended(0x18DA_F110) => format!("Engine ({ecu})"),
        CanId::Standard(0x7E9) | CanId::Extended(0x18DA_F118) => format!("Gearbox ({ecu})"),
        _ => format!("ECU {ecu}"),
    }
}

impl HealthReport {
    /// The summary, a line at a time, worst first.
    pub fn summary(&self) -> Vec<(Tone, String)> {
        let mut lines = Vec::new();
        for e in &self.ecus {
            let who = &e.name;
            if let Some(m) = &e.monitors {
                if m.mil {
                    lines.push((Tone::Bad, format!("{who}: the check engine light is ON.")));
                } else {
                    lines.push((Tone::Good, format!("{who}: the check engine light is off.")));
                }
                let avail: Vec<_> = m.monitors.iter().filter(|x| x.available).collect();
                let done = avail.iter().filter(|x| x.complete).count();
                if done == avail.len() {
                    lines.push((Tone::Good, format!("{who}: all {} readiness monitors complete.", avail.len())));
                } else {
                    lines.push((
                        Tone::Note,
                        format!(
                            "{who}: {} of {} readiness monitors not complete (normal soon after codes are cleared).",
                            avail.len() - done,
                            avail.len()
                        ),
                    ));
                }
            }
            for (list, label, tone) in [
                (&e.stored, "stored", Tone::Bad),
                (&e.pending, "pending", Tone::Note),
                (&e.permanent, "permanent", Tone::Bad),
            ] {
                for c in list {
                    lines.push((tone, format!("{who}: {label} code {c}: {}.", dtcdb::lookup(*c).summary())));
                }
            }
            if e.stored.is_empty() && e.pending.is_empty() && e.permanent.is_empty() {
                lines.push((Tone::Good, format!("{who}: no fault codes.")));
            }
            let failed: Vec<&TestResult> = e.tests.iter().filter(|t| !t.passed()).collect();
            if !failed.is_empty() {
                for t in failed {
                    lines.push((
                        Tone::Bad,
                        format!(
                            "{who}: test failed: {}, {}.",
                            mode06::monitor_name(t.mid),
                            mode06::test_name(t.mid, t.tid)
                        ),
                    ));
                }
            } else if !e.tests.is_empty() {
                lines.push((Tone::Good, format!("{who}: all {} on-board test results within limits.", e.tests.len())));
            }
        }
        let mut quiet = 0;
        for m in &self.modules {
            match &m.codes {
                Ok(c) if c.is_empty() => quiet += 1,
                Ok(c) => {
                    for d in c {
                        let tone = if d.state().active { Tone::Bad } else { Tone::Note };
                        lines.push((tone, format!("{}: code {}.", m.display_name(), module_code_line(d))));
                    }
                }
                Err(e) => lines.push((Tone::Note, format!("{}: codes not read ({e}).", m.display_name()))),
            }
        }
        if quiet > 0 {
            lines.push((Tone::Good, format!("{quiet} other module(s): no fault codes.")));
        }
        lines.sort_by_key(|(t, _)| match t {
            Tone::Bad => 0,
            Tone::Note => 1,
            Tone::Good => 2,
        });
        lines
    }

    /// Every code in the report, for the session summary.
    pub fn all_codes(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .ecus
            .iter()
            .flat_map(|e| {
                e.stored
                    .iter()
                    .map(|c| format!("{c} stored"))
                    .chain(e.pending.iter().map(|c| format!("{c} pending")))
                    .chain(e.permanent.iter().map(|c| format!("{c} permanent")))
                    .collect::<Vec<_>>()
            })
            .collect();
        for m in &self.modules {
            if let Ok(codes) = &m.codes {
                out.extend(codes.iter().map(|d| format!("{d} {} ({})", d.state().words(), m.channel.label())));
            }
        }
        out
    }

    /// The report as one self-contained HTML page.
    pub fn to_html(&self) -> String {
        let mut h = String::new();
        h.push_str("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">");
        h.push_str(&format!("<title>Scana health report: {}</title>", esc(&self.vehicle)));
        h.push_str(
            "<style>body{font-family:sans-serif;max-width:900px;margin:2em auto;padding:0 1em;color:#222}\
             h1{font-size:1.6em}h2{margin-top:1.6em;border-bottom:1px solid #ccc}table{border-collapse:collapse;width:100%}\
             td,th{border:1px solid #ccc;padding:4px 8px;text-align:left;vertical-align:top}th{background:#f2f2f2}\
             .bad{color:#b00020;font-weight:bold}.note{color:#8a5a00}.good{color:#1b6e2d}.small{color:#666;font-size:.9em}</style>",
        );
        h.push_str("</head><body>");
        h.push_str(&format!("<h1>Health report: {}</h1>", esc(&self.vehicle)));
        h.push_str(&format!(
            "<p class=\"small\">{} &middot; {} &middot; {}</p>",
            esc(&self.when),
            esc(&self.protocol),
            esc(&self.link)
        ));
        h.push_str("<h2>Summary</h2><ul>");
        for (tone, line) in self.summary() {
            let class = match tone {
                Tone::Good => "good",
                Tone::Note => "note",
                Tone::Bad => "bad",
            };
            h.push_str(&format!("<li class=\"{class}\">{}</li>", esc(&line)));
        }
        h.push_str("</ul>");
        for e in &self.ecus {
            h.push_str(&format!("<h2>{}</h2>", esc(&e.name)));
            let codes: Vec<(&str, &Dtc)> = e
                .stored
                .iter()
                .map(|c| ("Stored", c))
                .chain(e.pending.iter().map(|c| ("Pending", c)))
                .chain(e.permanent.iter().map(|c| ("Permanent", c)))
                .collect();
            if codes.is_empty() {
                h.push_str("<p>No fault codes.</p>");
            } else {
                h.push_str("<table><colgroup><col style=\"width:11%\"><col style=\"width:9%\"><col style=\"width:30%\"><col style=\"width:50%\"></colgroup><tr><th>Kind</th><th>Code</th><th>Meaning</th><th>What to check</th></tr>");
                for (kind, c) in codes {
                    let info = dtcdb::lookup(*c);
                    let check = info.advice.map(|a| a.check.join(" ")).unwrap_or_default();
                    h.push_str(&format!(
                        "<tr><td>{kind}</td><td>{c}</td><td>{}</td><td>{}</td></tr>",
                        esc(&info.summary()),
                        esc(&check)
                    ));
                }
                h.push_str("</table>");
            }
            if let Some(ff) = &e.freeze {
                h.push_str(&format!("<h3>Freeze frame (stored by {})</h3><table>", ff.dtc));
                for r in &ff.readings {
                    h.push_str(&format!("<tr><td>{}</td><td>{}</td></tr>", esc(r.name), esc(&r.display())));
                }
                h.push_str("</table>");
            }
            if let Some(m) = &e.monitors {
                h.push_str("<h3>Readiness</h3><table><tr><th>Monitor</th><th>State</th></tr>");
                for x in m.monitors.iter().filter(|x| x.available) {
                    let (class, state) = if x.complete { ("good", "complete") } else { ("note", "not complete") };
                    h.push_str(&format!("<tr><td>{}</td><td class=\"{class}\">{state}</td></tr>", esc(x.name)));
                }
                h.push_str("</table>");
            }
            if !e.values.is_empty() {
                h.push_str("<h3>Key values</h3><table>");
                for r in &e.values {
                    h.push_str(&format!("<tr><td>{}</td><td>{}</td></tr>", esc(r.name), esc(&r.display())));
                }
                h.push_str("</table>");
            }
            if !e.tests.is_empty() {
                h.push_str("<h3>On-board test results</h3><table><tr><th>Monitor</th><th>Test</th><th>Value</th><th>Limits</th><th>Result</th></tr>");
                for t in &e.tests {
                    let (class, res) = if t.passed() { ("good", "pass") } else { ("bad", "FAIL") };
                    h.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"{class}\">{res}</td></tr>",
                        esc(&mode06::monitor_name(t.mid)),
                        esc(&mode06::test_name(t.mid, t.tid)),
                        esc(&t.value_text()),
                        esc(&t.limits_text())
                    ));
                }
                h.push_str("</table>");
            }
            let i = &e.info;
            if i.vin.is_some() || i.calibration_id.is_some() || i.ecu_name.is_some() {
                h.push_str("<h3>Vehicle information</h3><table>");
                for (k, v) in [("VIN", &i.vin), ("Calibration ID", &i.calibration_id), ("ECU name", &i.ecu_name)] {
                    if let Some(v) = v {
                        h.push_str(&format!("<tr><td>{k}</td><td>{}</td></tr>", esc(v)));
                    }
                }
                h.push_str("</table>");
            }
        }
        if !self.modules.is_empty() {
            h.push_str("<h2>All modules</h2><p class=\"small\">Every module that answered Scana's module search, with the fault codes it holds.</p>");
            h.push_str("<table><tr><th>Module</th><th>Address</th><th>Fault codes</th></tr>");
            for m in &self.modules {
                let codes = match &m.codes {
                    Ok(c) if c.is_empty() => "<span class=\"good\">none</span>".to_string(),
                    Ok(c) => c.iter().map(|d| esc(&module_code_line(d))).collect::<Vec<_>>().join("<br>"),
                    Err(e) => format!("<span class=\"note\">not read: {}</span>", esc(e)),
                };
                h.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{codes}</td></tr>",
                    esc(&m.display_name()),
                    esc(&m.channel.label())
                ));
            }
            h.push_str("</table>");
        }
        h.push_str(&format!(
            "<p class=\"small\">Made by Scana {}. Code descriptions are general guidance: check the maker's workshop manual before replacing parts.</p>",
            env!("CARGO_PKG_VERSION")
        ));
        h.push_str("</body></html>\n");
        h
    }
}

/// Escapes text for HTML.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{DetectOptions, detect};
    use scana_link::sim::SimLink;
    use scana_sim::VirtualCar;

    fn report(car: VirtualCar) -> HealthReport {
        let mut c = Client::new(Box::new(SimLink::new(car)));
        let d = detect(&mut c, &DetectOptions { listen_ms: 100, ..DetectOptions::default() }).unwrap();
        let ecus: Vec<(CanId, Vec<u8>)> =
            d.ecus.iter().map(|e| (*e, readers::supported_pids(&mut c, *e).unwrap())).collect();
        let profile = scana_db::profile::find_match(&ecus[0].1);
        run_health_check(&mut c, &ecus, profile, "Demo", "500 kbit/s, 11-bit IDs").unwrap()
    }

    #[test]
    fn healthy_car() {
        let r = report(VirtualCar::hijet());
        assert_eq!(r.ecus[0].name, "Engine (ECM)"); // from the Hijet profile
        let s = r.summary();
        assert!(s.iter().all(|(t, _)| *t == Tone::Good), "{s:?}");
        assert!(s.iter().any(|(_, l)| l.contains("check engine light is off")));
        assert!(r.to_html().contains("No fault codes."));
    }

    #[test]
    fn faulty_car() {
        let r = report(VirtualCar::hijet_with_faults());
        let s = r.summary();
        assert_eq!(s[0].0, Tone::Bad);
        assert!(s.iter().any(|(_, l)| l.contains("ON")));
        assert!(s.iter().any(|(_, l)| l.contains("P0128") && l.contains("thermostat")));
        assert!(s.iter().any(|(_, l)| l.contains("pending code P0171")));
        assert!(s.iter().any(|(_, l)| l.contains("Catalyst, bank 1")));
        assert!(r.ecus[0].freeze.is_some());
        assert_eq!(r.all_codes(), vec!["P0128 stored", "P0171 pending"]);
        let html = r.to_html();
        assert!(
            html.contains("Freeze frame (stored by P0128)") && html.contains("FAIL") && html.contains("What to check")
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(esc("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }
}
