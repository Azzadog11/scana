//! Service 06: on-board monitor test results (SAE J1979, ISO 15031-5), in the CAN format.
//!
//! `06 MID` asks for the results of one monitor. The reply is `46` followed by 9-byte
//! records: MID, test ID, unit-and-scaling ID, then value, minimum and maximum (2 bytes each).
//! `06 00`, `06 20` ... list the supported monitors as bit masks, like service 01 PIDs.

use crate::obd::ObdError;

/// How to turn a raw 2-byte test value into a number with a unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scaling {
    pub signed: bool,
    pub scale: f64,
    pub offset: f64,
    pub unit: &'static str,
}

const fn u(scale: f64, unit: &'static str) -> Scaling {
    Scaling { signed: false, scale, offset: 0.0, unit }
}
const fn s(scale: f64, unit: &'static str) -> Scaling {
    Scaling { signed: true, scale, offset: 0.0, unit }
}

/// The unit-and-scaling table (ISO 15031-5 annex). IDs not listed show as raw numbers.
pub fn scaling(uasid: u8) -> Option<Scaling> {
    Some(match uasid {
        0x01 => u(1.0, ""),
        0x02 => u(0.1, ""),
        0x03 => u(0.01, ""),
        0x04 => u(0.001, ""),
        0x05 => u(0.000_030_5, ""),
        0x06 => u(0.000_305, ""),
        0x07 => u(0.25, "rpm"),
        0x08 => u(0.01, "km/h"),
        0x09 => u(1.0, "km/h"),
        0x0A => u(0.122, "mV"),
        0x0B => u(0.001, "V"),
        0x0C => u(0.01, "V"),
        0x0D => u(0.003_906_25, "mA"),
        0x0E => u(0.001, "A"),
        0x0F => u(0.01, "A"),
        0x10 => u(1.0, "ms"),
        0x11 => u(100.0, "ms"),
        0x12 => u(1.0, "s"),
        0x13 => u(1.0, "mΩ"),
        0x14 => u(1.0, "Ω"),
        0x15 => u(1.0, "kΩ"),
        0x16 => Scaling { signed: false, scale: 0.1, offset: -40.0, unit: "°C" },
        0x17 => u(0.01, "kPa"),
        0x18 => u(0.0117, "kPa"),
        0x19 => u(0.079, "kPa"),
        0x1A => u(1.0, "kPa"),
        0x1B => u(10.0, "kPa"),
        0x1C => u(0.01, "°"),
        0x1D => u(0.5, "°"),
        0x1E => u(0.000_030_5, "λ"),
        0x1F => u(0.05, "ratio"),
        0x20 => u(0.003_906_25, "ratio"),
        0x21 => u(1.0, "mHz"),
        0x22 => u(1.0, "Hz"),
        0x23 => u(1.0, "kHz"),
        0x24 => u(1.0, "counts"),
        0x25 => u(1.0, "km"),
        0x26 => u(0.1, "mV/ms"),
        0x27 => u(0.01, "g/s"),
        0x28 => u(1.0, "g/s"),
        0x29 => u(0.25, "Pa/s"),
        0x2A => u(0.001, "kg/h"),
        0x2B => u(1.0, "switches"),
        0x2C => u(0.01, "g/cyl"),
        0x2D => u(0.01, "mg/stroke"),
        0x2E => u(1.0, "true/false"),
        0x2F => u(0.01, "%"),
        0x30 => u(0.001_526, "%"),
        0x31 => u(0.001, "L"),
        0x32 => u(0.000_030_5, "in"),
        0x33 => u(0.000_244_14, "λ"),
        0x34 => u(1.0, "min"),
        0x35 => u(10.0, "ms"),
        0x36 => u(0.01, "g"),
        0x37 => u(0.1, "g"),
        0x38 => u(1.0, "g"),
        0x39 => Scaling { signed: false, scale: 0.01, offset: -327.68, unit: "%" },
        0x3A => u(0.001, "g"),
        0x3B => u(0.0001, "g"),
        0x3C => u(0.1, "µs"),
        0x3D => u(0.01, "mA"),
        0x3E => u(0.000_061_035_16, "mm²"),
        0x3F => u(0.01, "L"),
        0x40 => u(1.0, "ppm"),
        0x41 => u(0.01, "µA"),
        0x81 => s(1.0, ""),
        0x82 => s(0.1, ""),
        0x83 => s(0.01, ""),
        0x84 => s(0.001, ""),
        0x85 => s(0.000_030_5, ""),
        0x86 => s(0.000_305, ""),
        0x87 => s(1.0, "ppm"),
        0x8A => s(0.122, "mV"),
        0x8B => s(0.001, "V"),
        0x8C => s(0.01, "V"),
        0x8D => s(0.003_906_25, "mA"),
        0x8E => s(0.001, "A"),
        0x90 => s(1.0, "ms"),
        0x96 => s(0.1, "°C"),
        0x99 => s(0.1, "kPa"),
        0x9C => s(0.01, "°"),
        0x9D => s(0.5, "°"),
        0xA8 => s(1.0, "g/s"),
        0xA9 => s(0.25, "Pa/s"),
        0xAD => s(0.01, "mg/stroke"),
        0xAE => s(0.1, "mg/stroke"),
        0xAF => s(0.01, "%"),
        0xB0 => s(0.003_052, "%"),
        0xB1 => s(2.0, "mV/s"),
        0xFC => s(0.01, "kPa"),
        0xFD => s(0.001, "kPa"),
        0xFE => s(0.25, "Pa"),
        _ => return None,
    })
}

/// What a monitor ID tests, in plain words.
pub fn monitor_name(mid: u8) -> String {
    let bank_sensor = |base: u8| {
        let i = mid - base;
        format!("bank {} sensor {}", i / 4 + 1, i % 4 + 1)
    };
    match mid {
        0x01..=0x10 => format!("Oxygen sensor, {}", bank_sensor(0x01)),
        0x21..=0x24 => format!("Catalyst, bank {}", mid - 0x20),
        0x31..=0x34 => format!("EGR, bank {}", mid - 0x30),
        0x35..=0x38 => format!("Variable valve timing, bank {}", mid - 0x34),
        0x39 => "Evaporative system, cap off or 0.150 inch leak".into(),
        0x3A => "Evaporative system, 0.090 inch leak".into(),
        0x3B => "Evaporative system, 0.040 inch leak".into(),
        0x3C => "Evaporative system, 0.020 inch leak".into(),
        0x3D => "Purge flow".into(),
        0x41..=0x50 => format!("Oxygen sensor heater, {}", bank_sensor(0x41)),
        0x61..=0x64 => format!("Heated catalyst, bank {}", mid - 0x60),
        0x71..=0x74 => format!("Secondary air {}", mid - 0x70),
        0x81..=0x84 => format!("Fuel system, bank {}", mid - 0x80),
        0x85..=0x86 => format!("Boost pressure control, bank {}", mid - 0x84),
        0x90..=0x91 => format!("NOx absorber, bank {}", mid - 0x8F),
        0x98..=0x99 => format!("NOx catalyst, bank {}", mid - 0x97),
        0xA1 => "Misfire, general".into(),
        0xA2..=0xAD => format!("Misfire, cylinder {}", mid - 0xA1),
        0xB0..=0xB1 => format!("Particulate filter, bank {}", mid - 0xAF),
        _ if mid.is_multiple_of(0x20) => format!("Monitors supported {:02X}-{:02X}", mid + 1, mid.saturating_add(0x20)),
        _ => format!("Monitor {mid:02X}"),
    }
}

/// What a test ID measures. 01-0A are the standard oxygen sensor tests, 0B and 0C the misfire
/// counts; 80 and up are the maker's own.
pub fn test_name(mid: u8, tid: u8) -> String {
    if (0xA1..=0xAD).contains(&mid) {
        match tid {
            0x0B => return "Misfires, average of the last 10 drives".into(),
            0x0C => return "Misfires, this or the last drive".into(),
            _ => {}
        }
    }
    match tid {
        0x01 => "Rich to lean threshold voltage".into(),
        0x02 => "Lean to rich threshold voltage".into(),
        0x03 => "Low voltage for switch time".into(),
        0x04 => "High voltage for switch time".into(),
        0x05 => "Rich to lean switch time".into(),
        0x06 => "Lean to rich switch time".into(),
        0x07 => "Minimum voltage in the test".into(),
        0x08 => "Maximum voltage in the test".into(),
        0x09 => "Time between transitions".into(),
        0x0A => "Sensor period".into(),
        0x80..=0xFF => format!("Maker's test {tid:02X}"),
        _ => format!("Test {tid:02X}"),
    }
}

/// One test result.
#[derive(Clone, Debug, PartialEq)]
pub struct TestResult {
    pub mid: u8,
    pub tid: u8,
    pub uasid: u8,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub unit: &'static str,
    /// False if the unit-and-scaling ID is unknown: the numbers are raw.
    pub scaled: bool,
    /// The maximum is the largest number the record can hold: in effect no upper limit (misfire
    /// counts, for example).
    pub max_open: bool,
}

impl TestResult {
    /// Within its limits (minimum and maximum included).
    pub fn passed(&self) -> bool {
        self.value >= self.min && self.value <= self.max
    }

    fn number(&self, v: f64) -> String {
        crate::pids::format_value(v, if self.unit.is_empty() { "x" } else { self.unit })
    }

    /// The value with its unit: "0.707 ratio", "0 counts", "68 ms".
    pub fn value_text(&self) -> String {
        let unit = if self.unit == "counts" && self.value == 1.0 { "count" } else { self.unit };
        format!("{} {unit}", self.number(self.value)).trim_end().to_string()
    }

    /// The limits in words: "0 to 0.602", "at least 2", or "no limits".
    pub fn limits_text(&self) -> String {
        match (self.min == 0.0, self.max_open) {
            (true, true) => "no limits".into(),
            (false, true) => format!("at least {}", self.number(self.min)),
            _ => format!("{} to {}", self.number(self.min), self.number(self.max)),
        }
    }
}

fn scale(raw: [u8; 2], sc: Option<Scaling>) -> f64 {
    match sc {
        Some(sc) if sc.signed => f64::from(i16::from_be_bytes(raw)) * sc.scale + sc.offset,
        Some(sc) => f64::from(u16::from_be_bytes(raw)) * sc.scale + sc.offset,
        None => f64::from(u16::from_be_bytes(raw)),
    }
}

/// Parses a service 06 reply (after 0x46) into test results. Records are 9 bytes each;
/// trailing padding shorter than a record is ignored.
pub fn parse_results(data: &[u8]) -> Result<Vec<TestResult>, ObdError> {
    if data.is_empty() {
        return Err(ObdError::Empty);
    }
    if data.len() < 9 {
        return Err(ObdError::Malformed("test result shorter than one 9-byte record"));
    }
    let (records, _) = data.as_chunks::<9>();
    Ok(records
        .iter()
        .map(|r| {
            let sc = scaling(r[2]);
            TestResult {
                mid: r[0],
                tid: r[1],
                uasid: r[2],
                value: scale([r[3], r[4]], sc),
                min: scale([r[5], r[6]], sc),
                max: scale([r[7], r[8]], sc),
                unit: sc.map_or("", |s| s.unit),
                scaled: sc.is_some(),
                max_open: [r[7], r[8]] == [0xFF, 0xFF] && !sc.is_some_and(|s| s.signed),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn o2_sensor_result_scales() {
        // MID 01, TID 01, UASID 0B (0.001 V): value 0x01C2 = 0.450 V, limits 0.4-0.5 V.
        let r = parse_results(&[0x01, 0x01, 0x0B, 0x01, 0xC2, 0x01, 0x90, 0x01, 0xF4]).unwrap();
        assert_eq!(r.len(), 1);
        assert!((r[0].value - 0.45).abs() < 1e-9);
        assert!((r[0].min - 0.4).abs() < 1e-9 && (r[0].max - 0.5).abs() < 1e-9);
        assert_eq!(r[0].unit, "V");
        assert!(r[0].passed());
    }

    #[test]
    fn signed_and_offset_values() {
        // UASID 0x8C: signed 0.01 V. 0xFF9C = -100 -> -1.00 V.
        let r = parse_results(&[0x21, 0x80, 0x8C, 0xFF, 0x9C, 0x80, 0x00, 0x7F, 0xFF]).unwrap();
        assert!((r[0].value + 1.0).abs() < 1e-9);
        assert!(r[0].passed());
        // UASID 0x16: 0.1 °C, offset -40. 0x04B0 = 1200 -> 80 °C.
        let r = parse_results(&[0x21, 0x80, 0x16, 0x04, 0xB0, 0, 0, 0x04, 0x00]).unwrap();
        assert!((r[0].value - 80.0).abs() < 1e-9);
        assert!(!r[0].passed()); // max is 1024 * 0.1 - 40 = 62.4 °C
    }

    #[test]
    fn several_records_and_padding() {
        let mut d = vec![0xA2, 0x0B, 0x24, 0, 3, 0, 0, 0, 20];
        d.extend_from_slice(&[0xA2, 0x0C, 0x24, 0, 1, 0, 0, 0, 20]);
        d.extend_from_slice(&[0xCC, 0xCC]);
        let r = parse_results(&d).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].tid, 0x0C);
        assert_eq!(r[0].unit, "counts");
    }

    #[test]
    fn unknown_unit_shows_raw() {
        let r = parse_results(&[0x21, 0x81, 0x77, 0x00, 0x10, 0, 0, 0, 0x20]).unwrap();
        assert!(!r[0].scaled);
        assert_eq!(r[0].value, 16.0);
        assert!(parse_results(&[0x21, 0x81]).is_err());
        assert!(parse_results(&[]).is_err());
    }

    #[test]
    fn names() {
        assert_eq!(monitor_name(0x01), "Oxygen sensor, bank 1 sensor 1");
        assert_eq!(monitor_name(0x06), "Oxygen sensor, bank 2 sensor 2");
        assert_eq!(monitor_name(0x41), "Oxygen sensor heater, bank 1 sensor 1");
        assert_eq!(monitor_name(0x21), "Catalyst, bank 1");
        assert_eq!(monitor_name(0xA4), "Misfire, cylinder 3");
        assert_eq!(monitor_name(0x00), "Monitors supported 01-20");
        assert_eq!(test_name(0x01, 0x05), "Rich to lean switch time");
        assert_eq!(test_name(0x21, 0x91), "Maker's test 91");
        assert_eq!(test_name(0xA2, 0x0C), "Misfires, this or the last drive");
        assert_eq!(test_name(0x01, 0x0C), "Test 0C");
    }

    #[test]
    fn values_and_limits_in_words() {
        // Misfire counts: 0, limits 0 to 65535 (no limit in effect).
        let m = &parse_results(&[0xA2, 0x0B, 0x24, 0, 0, 0, 0, 0xFF, 0xFF]).unwrap()[0];
        assert_eq!((m.value_text().as_str(), m.limits_text().as_str()), ("0 counts", "no limits"));
        // O2 sensor switch time 68 ms, limits 0 to 150 ms.
        let t = &parse_results(&[0x01, 0x05, 0x10, 0, 68, 0, 0, 0, 150]).unwrap()[0];
        assert_eq!((t.value_text().as_str(), t.limits_text().as_str()), ("68 ms", "0 to 150"));
    }
}
