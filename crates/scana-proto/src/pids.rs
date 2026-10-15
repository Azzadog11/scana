//! Service 01 and 02 PIDs: names, data lengths and formulas (SAE J1979).
//! Formulas from SAE J1979 as listed at <https://en.wikipedia.org/wiki/OBD-II_PIDs>.
//! A and B are the first and second data bytes after the PID.

/// One decoded number.
#[derive(Clone, Debug, PartialEq)]
pub struct Quantity {
    /// Empty for the PID's main value; otherwise what this number is ("trim", "ratio").
    pub label: &'static str,
    pub value: f64,
    pub unit: &'static str,
}

/// A decoded PID: numbers, and text for values that are words (fuel system, standard).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Decoded {
    pub values: Vec<Quantity>,
    pub text: Option<String>,
}

impl Decoded {
    fn one(value: f64, unit: &'static str) -> Self {
        Decoded { values: vec![Quantity { label: "", value, unit }], text: None }
    }
    fn text(t: impl Into<String>) -> Self {
        Decoded { values: vec![], text: Some(t.into()) }
    }

    /// The main number, if there is one.
    pub fn main(&self) -> Option<&Quantity> {
        self.values.first()
    }

    /// "1874 rpm", "closed loop", "0.25 V, trim -10.2 %". Text, when there is any, says it all.
    pub fn display(&self) -> String {
        if let Some(t) = &self.text {
            return t.clone();
        }
        self.values
            .iter()
            .map(|q| {
                let v = format_value(q.value, q.unit);
                let unit = if q.unit.is_empty() { String::new() } else { format!(" {}", q.unit) };
                if q.label.is_empty() { format!("{v}{unit}") } else { format!("{} {v}{unit}", q.label) }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Rounds for reading: whole rpm, 1 decimal for percent and temperatures, more for volts.
pub fn format_value(v: f64, unit: &str) -> String {
    let places = match unit {
        "rpm" | "km/h" | "s" | "min" | "km" | "Nm" | "Pa" | "" => 0,
        "%" | "°C" | "° before TDC" | "°" | "kPa" | "L/h" | "mA" => 1,
        "g/s" => 2,
        _ => 3,
    };
    let s = format!("{v:.places$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".into() } else { s }
}

/// A PID's definition.
pub struct PidDef {
    pub pid: u8,
    /// Data bytes after the PID in a reply.
    pub len: u8,
    pub name: &'static str,
    pub decode: fn(&[u8]) -> Decoded,
}

fn a(d: &[u8]) -> f64 {
    f64::from(d[0])
}
fn ab(d: &[u8]) -> f64 {
    f64::from(u16::from_be_bytes([d[0], d[1]]))
}
fn percent(d: &[u8]) -> Decoded {
    Decoded::one(a(d) * 100.0 / 255.0, "%")
}
fn temp(d: &[u8]) -> Decoded {
    Decoded::one(a(d) - 40.0, "°C")
}
fn trim(d: &[u8]) -> Decoded {
    Decoded::one((a(d) - 128.0) * 100.0 / 128.0, "%")
}
fn kpa(d: &[u8]) -> Decoded {
    Decoded::one(a(d), "kPa")
}
fn bitmask(d: &[u8]) -> Decoded {
    Decoded::text(format!("{:02X}{:02X}{:02X}{:02X}", d[0], d[1], d[2], d[3]))
}
fn o2_narrow(d: &[u8]) -> Decoded {
    let mut out = Decoded::one(a(d) / 200.0, "V");
    if d[1] != 0xFF {
        out.values.push(Quantity { label: "trim", value: (f64::from(d[1]) - 128.0) * 100.0 / 128.0, unit: "%" });
    }
    out
}
fn o2_wide_voltage(d: &[u8]) -> Decoded {
    Decoded {
        values: vec![
            Quantity { label: "ratio", value: ab(d) * 2.0 / 65536.0, unit: "λ" },
            Quantity { label: "voltage", value: ab(&d[2..]) * 8.0 / 65536.0, unit: "V" },
        ],
        text: None,
    }
}
fn o2_wide_current(d: &[u8]) -> Decoded {
    Decoded {
        values: vec![
            Quantity { label: "ratio", value: ab(d) * 2.0 / 65536.0, unit: "λ" },
            Quantity { label: "current", value: ab(&d[2..]) / 256.0 - 128.0, unit: "mA" },
        ],
        text: None,
    }
}
fn catalyst(d: &[u8]) -> Decoded {
    Decoded::one(ab(d) / 10.0 - 40.0, "°C")
}
fn secondary_trim(d: &[u8]) -> Decoded {
    Decoded {
        values: vec![
            Quantity { label: "bank 1/3", value: (a(d) - 128.0) * 100.0 / 128.0, unit: "%" },
            Quantity { label: "bank 2/4", value: (f64::from(d[1]) - 128.0) * 100.0 / 128.0, unit: "%" },
        ],
        text: None,
    }
}

fn monitor(d: &[u8]) -> Decoded {
    let m = MonitorStatus::parse(d);
    let complete = m.monitors.iter().filter(|x| x.available && x.complete).count();
    let not_complete = m.monitors.iter().filter(|x| x.available && !x.complete).count();
    Decoded {
        values: vec![
            Quantity { label: "MIL", value: f64::from(u8::from(m.mil)), unit: "" },
            Quantity { label: "codes", value: f64::from(m.dtc_count), unit: "" },
        ],
        text: Some(format!(
            "MIL {}, {} code{}, {complete} monitors complete, {not_complete} not complete",
            if m.mil { "on" } else { "off" },
            m.dtc_count,
            if m.dtc_count == 1 { "" } else { "s" }
        )),
    }
}

fn fuel_system(d: &[u8]) -> Decoded {
    let name = |b: u8| match b {
        0 => None,
        1 => Some("open loop, engine not warm yet"),
        2 => Some("closed loop"),
        4 => Some("open loop, load or deceleration"),
        8 => Some("open loop, system fault"),
        16 => Some("closed loop, fault in one sensor"),
        _ => Some("unknown"),
    };
    let mut parts = vec![];
    if let Some(n) = name(d[0]) {
        parts.push(format!("system 1: {n}"));
    }
    if let Some(n) = name(d[1]) {
        parts.push(format!("system 2: {n}"));
    }
    if parts.len() == 1 {
        parts[0] = parts[0].trim_start_matches("system 1: ").to_string();
    }
    Decoded::text(if parts.is_empty() { "not reported".into() } else { parts.join("; ") })
}

fn secondary_air(d: &[u8]) -> Decoded {
    Decoded::text(match d[0] {
        1 => "upstream",
        2 => "downstream of catalyst",
        4 => "from outside or off",
        8 => "pump on for diagnostics",
        _ => "not reported",
    })
}

fn o2_present_2banks(d: &[u8]) -> Decoded {
    let names = ["B1S1", "B1S2", "B1S3", "B1S4", "B2S1", "B2S2", "B2S3", "B2S4"];
    let present: Vec<&str> = (0..8).filter(|i| d[0] & (1 << i) != 0).map(|i| names[i]).collect();
    Decoded::text(if present.is_empty() { "none".into() } else { present.join(" ") })
}

fn o2_present_4banks(d: &[u8]) -> Decoded {
    let names = ["B1S1", "B1S2", "B2S1", "B2S2", "B3S1", "B3S2", "B4S1", "B4S2"];
    let present: Vec<&str> = (0..8).filter(|i| d[0] & (1 << i) != 0).map(|i| names[i]).collect();
    Decoded::text(if present.is_empty() { "none".into() } else { present.join(" ") })
}

/// The OBD standard a vehicle meets (PID 1C), SAE J1979 table.
pub fn obd_standard_name(v: u8) -> &'static str {
    match v {
        1 => "OBD-II (California ARB)",
        2 => "OBD (US EPA)",
        3 => "OBD and OBD-II",
        4 => "OBD-I",
        5 => "Not OBD compliant",
        6 => "EOBD (Europe)",
        7 => "EOBD and OBD-II",
        8 => "EOBD and OBD",
        9 => "EOBD, OBD and OBD-II",
        10 => "JOBD (Japan)",
        11 => "JOBD and OBD-II",
        12 => "JOBD and EOBD",
        13 => "JOBD, EOBD and OBD-II",
        17 => "Engine Manufacturer Diagnostics (EMD)",
        18 => "EMD+",
        19 => "Heavy duty OBD (child/partial)",
        20 => "Heavy duty OBD",
        21 => "World Wide Harmonized OBD",
        23 => "Heavy duty EOBD-I",
        24 => "Heavy duty EOBD-I N",
        25 => "Heavy duty EOBD-II",
        26 => "Heavy duty EOBD-II N",
        28 => "Brazil OBD phase 1",
        29 => "Brazil OBD phase 2",
        30 => "Korean OBD",
        31 => "India OBD I",
        32 => "India OBD II",
        33 => "Heavy duty EOBD stage VI",
        _ => "unknown",
    }
}

fn obd_standard(d: &[u8]) -> Decoded {
    Decoded::text(obd_standard_name(d[0]))
}

fn fuel_type(d: &[u8]) -> Decoded {
    Decoded::text(match d[0] {
        1 => "petrol",
        2 => "methanol",
        3 => "ethanol",
        4 => "diesel",
        5 => "LPG",
        6 => "CNG",
        7 => "propane",
        8 => "electric",
        9 => "bifuel, petrol",
        12 => "bifuel, LPG",
        17 => "hybrid, petrol",
        19 => "hybrid, diesel",
        20 => "hybrid, electric",
        _ => "other",
    })
}

fn signed16(d: &[u8]) -> f64 {
    f64::from(i16::from_be_bytes([d[0], d[1]]))
}

macro_rules! pid {
    ($pid:expr, $len:expr, $name:expr, $decode:expr) => {
        PidDef { pid: $pid, len: $len, name: $name, decode: $decode }
    };
}

/// Every PID Scana knows, in order.
pub static PIDS: &[PidDef] = &[
    pid!(0x00, 4, "PIDs supported 01-20", bitmask),
    pid!(0x01, 4, "Monitor status since codes cleared", monitor),
    pid!(0x02, 2, "Code that stored this freeze frame", |d| Decoded::text(
        crate::dtc::Dtc::from_bytes(d[0], d[1]).to_string()
    )),
    pid!(0x03, 2, "Fuel system status", fuel_system),
    pid!(0x04, 1, "Calculated engine load", percent),
    pid!(0x05, 1, "Coolant temperature", temp),
    pid!(0x06, 1, "Short term fuel trim, bank 1", trim),
    pid!(0x07, 1, "Long term fuel trim, bank 1", trim),
    pid!(0x08, 1, "Short term fuel trim, bank 2", trim),
    pid!(0x09, 1, "Long term fuel trim, bank 2", trim),
    pid!(0x0A, 1, "Fuel pressure", |d| Decoded::one(a(d) * 3.0, "kPa")),
    pid!(0x0B, 1, "Intake manifold pressure", kpa),
    pid!(0x0C, 2, "Engine speed", |d| Decoded::one(ab(d) / 4.0, "rpm")),
    pid!(0x0D, 1, "Vehicle speed", |d| Decoded::one(a(d), "km/h")),
    pid!(0x0E, 1, "Ignition timing advance", |d| Decoded::one(a(d) / 2.0 - 64.0, "° before TDC")),
    pid!(0x0F, 1, "Intake air temperature", temp),
    pid!(0x10, 2, "Mass air flow", |d| Decoded::one(ab(d) / 100.0, "g/s")),
    pid!(0x11, 1, "Throttle position", percent),
    pid!(0x12, 1, "Commanded secondary air status", secondary_air),
    pid!(0x13, 1, "Oxygen sensors present", o2_present_2banks),
    pid!(0x14, 2, "Oxygen sensor bank 1 sensor 1", o2_narrow),
    pid!(0x15, 2, "Oxygen sensor bank 1 sensor 2", o2_narrow),
    pid!(0x16, 2, "Oxygen sensor bank 1 sensor 3", o2_narrow),
    pid!(0x17, 2, "Oxygen sensor bank 1 sensor 4", o2_narrow),
    pid!(0x18, 2, "Oxygen sensor bank 2 sensor 1", o2_narrow),
    pid!(0x19, 2, "Oxygen sensor bank 2 sensor 2", o2_narrow),
    pid!(0x1A, 2, "Oxygen sensor bank 2 sensor 3", o2_narrow),
    pid!(0x1B, 2, "Oxygen sensor bank 2 sensor 4", o2_narrow),
    pid!(0x1C, 1, "OBD standard", obd_standard),
    pid!(0x1D, 1, "Oxygen sensors present (4 banks)", o2_present_4banks),
    pid!(0x1E, 1, "Auxiliary input status", |d| Decoded::text(if d[0] & 1 != 0 {
        "power take-off active"
    } else {
        "power take-off off"
    })),
    pid!(0x1F, 2, "Time since engine start", |d| Decoded::one(ab(d), "s")),
    pid!(0x20, 4, "PIDs supported 21-40", bitmask),
    pid!(0x21, 2, "Distance with the MIL on", |d| Decoded::one(ab(d), "km")),
    pid!(0x22, 2, "Fuel rail pressure (relative to manifold)", |d| Decoded::one(ab(d) * 0.079, "kPa")),
    pid!(0x23, 2, "Fuel rail pressure", |d| Decoded::one(ab(d) * 10.0, "kPa")),
    pid!(0x24, 4, "Oxygen sensor 1 (wide range)", o2_wide_voltage),
    pid!(0x25, 4, "Oxygen sensor 2 (wide range)", o2_wide_voltage),
    pid!(0x26, 4, "Oxygen sensor 3 (wide range)", o2_wide_voltage),
    pid!(0x27, 4, "Oxygen sensor 4 (wide range)", o2_wide_voltage),
    pid!(0x28, 4, "Oxygen sensor 5 (wide range)", o2_wide_voltage),
    pid!(0x29, 4, "Oxygen sensor 6 (wide range)", o2_wide_voltage),
    pid!(0x2A, 4, "Oxygen sensor 7 (wide range)", o2_wide_voltage),
    pid!(0x2B, 4, "Oxygen sensor 8 (wide range)", o2_wide_voltage),
    pid!(0x2C, 1, "Commanded EGR", percent),
    pid!(0x2D, 1, "EGR error", trim),
    pid!(0x2E, 1, "Commanded evaporative purge", percent),
    pid!(0x2F, 1, "Fuel tank level", percent),
    pid!(0x30, 1, "Warm-ups since codes cleared", |d| Decoded::one(a(d), "")),
    pid!(0x31, 2, "Distance since codes cleared", |d| Decoded::one(ab(d), "km")),
    pid!(0x32, 2, "Evaporative system vapour pressure", |d| Decoded::one(signed16(d) / 4.0, "Pa")),
    pid!(0x33, 1, "Barometric pressure", kpa),
    pid!(0x34, 4, "Oxygen sensor 1 (wide range, current)", o2_wide_current),
    pid!(0x35, 4, "Oxygen sensor 2 (wide range, current)", o2_wide_current),
    pid!(0x36, 4, "Oxygen sensor 3 (wide range, current)", o2_wide_current),
    pid!(0x37, 4, "Oxygen sensor 4 (wide range, current)", o2_wide_current),
    pid!(0x38, 4, "Oxygen sensor 5 (wide range, current)", o2_wide_current),
    pid!(0x39, 4, "Oxygen sensor 6 (wide range, current)", o2_wide_current),
    pid!(0x3A, 4, "Oxygen sensor 7 (wide range, current)", o2_wide_current),
    pid!(0x3B, 4, "Oxygen sensor 8 (wide range, current)", o2_wide_current),
    pid!(0x3C, 2, "Catalyst temperature, bank 1 sensor 1", catalyst),
    pid!(0x3D, 2, "Catalyst temperature, bank 2 sensor 1", catalyst),
    pid!(0x3E, 2, "Catalyst temperature, bank 1 sensor 2", catalyst),
    pid!(0x3F, 2, "Catalyst temperature, bank 2 sensor 2", catalyst),
    pid!(0x40, 4, "PIDs supported 41-60", bitmask),
    pid!(0x41, 4, "Monitor status this drive cycle", monitor),
    pid!(0x42, 2, "Control module voltage", |d| Decoded::one(ab(d) / 1000.0, "V")),
    pid!(0x43, 2, "Absolute load", |d| Decoded::one(ab(d) * 100.0 / 255.0, "%")),
    pid!(0x44, 2, "Commanded air-fuel ratio", |d| Decoded::one(ab(d) * 2.0 / 65536.0, "λ")),
    pid!(0x45, 1, "Relative throttle position", percent),
    pid!(0x46, 1, "Outside air temperature", temp),
    pid!(0x47, 1, "Throttle position B", percent),
    pid!(0x48, 1, "Throttle position C", percent),
    pid!(0x49, 1, "Accelerator pedal position D", percent),
    pid!(0x4A, 1, "Accelerator pedal position E", percent),
    pid!(0x4B, 1, "Accelerator pedal position F", percent),
    pid!(0x4C, 1, "Commanded throttle actuator", percent),
    pid!(0x4D, 2, "Time run with the MIL on", |d| Decoded::one(ab(d), "min")),
    pid!(0x4E, 2, "Time since codes cleared", |d| Decoded::one(ab(d), "min")),
    pid!(0x4F, 4, "Maximum values for ratio, O2 voltage, O2 current, manifold pressure", |d| {
        Decoded::text(format!("λ {}, {} V, {} mA, {} kPa", d[0], d[1], d[2], u16::from(d[3]) * 10))
    }),
    pid!(0x50, 4, "Maximum mass air flow", |d| Decoded::one(a(d) * 10.0, "g/s")),
    pid!(0x51, 1, "Fuel type", fuel_type),
    pid!(0x52, 1, "Ethanol fuel", percent),
    pid!(0x53, 2, "Absolute evaporative vapour pressure", |d| Decoded::one(ab(d) / 200.0, "kPa")),
    pid!(0x54, 2, "Evaporative vapour pressure", |d| Decoded::one(signed16(d), "Pa")),
    pid!(0x55, 2, "Short term secondary oxygen sensor trim", secondary_trim),
    pid!(0x56, 2, "Long term secondary oxygen sensor trim", secondary_trim),
    pid!(0x57, 2, "Short term secondary oxygen sensor trim, banks 2 and 4", secondary_trim),
    pid!(0x58, 2, "Long term secondary oxygen sensor trim, banks 2 and 4", secondary_trim),
    pid!(0x59, 2, "Fuel rail absolute pressure", |d| Decoded::one(ab(d) * 10.0, "kPa")),
    pid!(0x5A, 1, "Relative accelerator pedal position", percent),
    pid!(0x5B, 1, "Hybrid battery remaining life", percent),
    pid!(0x5C, 1, "Engine oil temperature", temp),
    pid!(0x5D, 2, "Fuel injection timing", |d| Decoded::one(ab(d) / 128.0 - 210.0, "°")),
    pid!(0x5E, 2, "Engine fuel rate", |d| Decoded::one(ab(d) / 20.0, "L/h")),
    pid!(0x5F, 1, "Emission requirements", |d| Decoded::text(format!("code {}", d[0]))),
    pid!(0x60, 4, "PIDs supported 61-80", bitmask),
    pid!(0x61, 1, "Driver's demanded engine torque", |d| Decoded::one(a(d) - 125.0, "%")),
    pid!(0x62, 1, "Actual engine torque", |d| Decoded::one(a(d) - 125.0, "%")),
    pid!(0x63, 2, "Engine reference torque", |d| Decoded::one(ab(d), "Nm")),
    pid!(0x80, 4, "PIDs supported 81-A0", bitmask),
    pid!(0xA0, 4, "PIDs supported A1-C0", bitmask),
    pid!(0xC0, 4, "PIDs supported C1-E0", bitmask),
    pid!(0xE0, 4, "PIDs supported E1-FF", bitmask),
];

/// The definition of a PID, if Scana knows it.
pub fn def(pid: u8) -> Option<&'static PidDef> {
    PIDS.binary_search_by_key(&pid, |p| p.pid).ok().map(|i| &PIDS[i])
}

/// True for the "PIDs supported" bit-mask PIDs: 00, 20, 40 ... E0.
pub fn is_support_pid(pid: u8) -> bool {
    pid.is_multiple_of(0x20)
}

/// Decodes a PID's data bytes. `None` if the PID is unknown or the data is too short.
pub fn decode(pid: u8, data: &[u8]) -> Option<Decoded> {
    let d = def(pid)?;
    if data.len() < usize::from(d.len) {
        return None;
    }
    Some((d.decode)(&data[..usize::from(d.len)]))
}

/// One readiness monitor from PID 01 or 41.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: &'static str,
    /// The vehicle has this monitor.
    pub available: bool,
    /// Its test has run and finished.
    pub complete: bool,
}

/// PID 01 (or 41): the check engine light, the code count and the readiness monitors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorStatus {
    pub mil: bool,
    pub dtc_count: u8,
    /// Diesel (compression ignition) monitors instead of petrol ones.
    pub compression: bool,
    pub monitors: Vec<Monitor>,
}

impl MonitorStatus {
    pub fn parse(d: &[u8]) -> Self {
        let (a, b, c, dd) = (d[0], d[1], d[2], d[3]);
        let compression = b & 0x08 != 0;
        let mut monitors = Vec::new();
        for (i, name) in ["Misfire", "Fuel system", "Components"].iter().enumerate() {
            monitors.push(Monitor { name, available: b & (1 << i) != 0, complete: b & (1 << (i + 4)) == 0 });
        }
        let names: [&'static str; 8] = if compression {
            [
                "NMHC catalyst",
                "NOx/SCR",
                "Reserved",
                "Boost pressure",
                "Reserved",
                "Exhaust gas sensor",
                "PM filter",
                "EGR and VVT",
            ]
        } else {
            [
                "Catalyst",
                "Heated catalyst",
                "Evaporative system",
                "Secondary air",
                "A/C refrigerant",
                "Oxygen sensor",
                "Oxygen sensor heater",
                "EGR and VVT",
            ]
        };
        for (i, name) in names.iter().enumerate() {
            if *name == "Reserved" {
                continue;
            }
            monitors.push(Monitor { name, available: c & (1 << i) != 0, complete: dd & (1 << i) == 0 });
        }
        MonitorStatus { mil: a & 0x80 != 0, dtc_count: a & 0x7F, compression, monitors }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main(pid: u8, data: &[u8]) -> f64 {
        decode(pid, data).unwrap().main().unwrap().value
    }

    #[test]
    fn table_is_sorted_and_lengths_are_sane() {
        for w in PIDS.windows(2) {
            assert!(w[0].pid < w[1].pid, "{:02X} before {:02X}", w[0].pid, w[1].pid);
        }
        for p in PIDS {
            assert!((1..=5).contains(&p.len), "{:02X}", p.pid);
            // Every decoder copes with all-zero and all-FF data.
            (p.decode)(&vec![0u8; usize::from(p.len)]);
            (p.decode)(&vec![0xFFu8; usize::from(p.len)]);
        }
    }

    #[test]
    fn hijet_scan_values_decode() {
        // Readings from the Hijet's scan of 2026-10-10, encoded the way the ECU sends them.
        assert_eq!(main(0x05, &[70]), 30.0); // coolant 30 °C
        assert_eq!(main(0x0C, &[0x1D, 0x48]), 1874.0); // 7496 / 4
        assert_eq!(main(0x0D, &[0]), 0.0);
        assert_eq!(main(0x0B, &[39]), 39.0);
        assert_eq!(main(0x0E, &[136]), 4.0);
        assert_eq!(main(0x0F, &[62]), 22.0);
        assert_eq!(main(0x10, &[0x01, 0x21]), 2.89);
        assert_eq!(main(0x33, &[94]), 94.0);
        assert_eq!(main(0x42, &[0x37, 0xC8]), 14.28);
        assert_eq!(main(0x1F, &[0, 52]), 52.0);
        assert!((main(0x06, &[115]) - -10.156).abs() < 0.01); // -10.2 %
        assert!((main(0x07, &[130]) - 1.5625).abs() < 0.01); // 1.6 %
        assert!((main(0x04, &[69]) - 27.06).abs() < 0.01); // 27.1 %
        assert_eq!(decode(0x1C, &[10]).unwrap().display(), "JOBD (Japan)");
        assert_eq!(decode(0x03, &[2, 0]).unwrap().display(), "closed loop");
        let o2 = decode(0x14, &[50, 115]).unwrap();
        assert_eq!(o2.values[0].value, 0.25);
        assert_eq!(o2.display(), "0.25 V, trim -10.2 %");
        assert_eq!(decode(0x15, &[7, 0xFF]).unwrap().values.len(), 1); // no trim
    }

    #[test]
    fn monitor_status_decodes_like_the_hijet() {
        // MIL off, 0 codes; misfire, fuel, components; catalyst, evap, O2, O2 heater; all complete.
        let m = MonitorStatus::parse(&[0x00, 0x07, 0x65, 0x00]);
        assert!(!m.mil && m.dtc_count == 0 && !m.compression);
        let avail: Vec<&str> = m.monitors.iter().filter(|x| x.available).map(|x| x.name).collect();
        assert_eq!(
            avail,
            [
                "Misfire",
                "Fuel system",
                "Components",
                "Catalyst",
                "Evaporative system",
                "Oxygen sensor",
                "Oxygen sensor heater"
            ]
        );
        assert!(m.monitors.iter().filter(|x| x.available).all(|x| x.complete));
        assert_eq!(
            decode(0x01, &[0x00, 0x07, 0x65, 0x00]).unwrap().text.unwrap(),
            "MIL off, 0 codes, 7 monitors complete, 0 not complete"
        );
        let m = MonitorStatus::parse(&[0x82, 0x07, 0x65, 0x04]);
        assert!(m.mil && m.dtc_count == 2);
        assert!(!m.monitors.iter().find(|x| x.name == "Evaporative system").unwrap().complete);
    }

    #[test]
    fn short_data_and_unknown_pids_give_none() {
        assert!(decode(0x0C, &[0x1D]).is_none());
        assert!(decode(0x64, &[0; 8]).is_none());
        assert!(def(0x0C).is_some() && def(0x64).is_none());
        assert!(is_support_pid(0x40) && !is_support_pid(0x41));
    }

    #[test]
    fn numbers_display_cleanly() {
        assert_eq!(decode(0x0C, &[0x1D, 0x48]).unwrap().display(), "1874 rpm");
        assert_eq!(decode(0x42, &[0x37, 0xC8]).unwrap().display(), "14.28 V");
        assert_eq!(decode(0x05, &[70]).unwrap().display(), "30 °C");
        assert_eq!(decode(0x04, &[69]).unwrap().display(), "27.1 %");
        assert_eq!(decode(0x0C, &[0x0D, 0x53]).unwrap().display(), "853 rpm");
        assert_eq!(
            decode(0x01, &[0, 7, 0x65, 0]).unwrap().display(),
            "MIL off, 0 codes, 7 monitors complete, 0 not complete"
        );
        assert_eq!(format_value(-0.04, "%"), "0");
    }
}
