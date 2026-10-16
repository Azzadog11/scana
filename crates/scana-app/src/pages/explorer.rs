//! The data explorer (stage 3): reads every data ID a module answers, then watches them so
//! you can see which bytes change when you do something (lights on, a door open, the brake
//! pressed), and name them. The console (advanced mode) sends one read request by hand.

use crate::app::{Page, ScanaApp};
use crate::names::ValueName;
use egui::{RichText, Ui};
use scana_core::modules::DataKind;
use scana_core::session::{Command, WatchSpec};
use scana_proto::obd::{self, Reply};
use scana_proto::uds;

/// A byte counts as "just changed" for this long, in ms.
const RECENT_MS: u64 = 2000;

impl ScanaApp {
    pub fn page_explorer(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Data explorer",
            "Find a module's own values: read every data ID it answers, then watch which bytes change when you do something.",
        );
        if !self.need_car(ui) {
            return;
        }
        if self.model.modules.is_empty() {
            ui.label("Find the car's modules first.");
            if ui.button("Go to Modules").clicked() {
                self.go(Page::Modules);
            }
            return;
        }
        let modules = self.model.modules.clone();
        if self.explore_channel.is_none_or(|c| !modules.iter().any(|m| m.channel == c))
            && let Some(m) = modules.first()
        {
            self.open_explorer(m.channel, DataKind::for_dialect(m.dialect));
        }
        let Some(channel) = self.explore_channel else { return };
        let busy = self.model.busy.is_some();

        ui.horizontal_wrapped(|ui| {
            ui.label("Module:");
            let current = modules
                .iter()
                .find(|m| m.channel == channel)
                .map_or_else(|| channel.label(), |m| format!("{} ({})", m.display_name(), m.channel.label()));
            let mut pick = None;
            egui::ComboBox::from_id_salt("explore-module").selected_text(current).width(320.0).show_ui(ui, |ui| {
                for m in &modules {
                    let label = format!("{} ({})", m.display_name(), m.channel.label());
                    if ui.selectable_label(m.channel == channel, label).clicked() {
                        pick = Some((m.channel, DataKind::for_dialect(m.dialect)));
                    }
                }
            });
            if let Some((c, k)) = pick {
                self.stop_watch();
                self.open_explorer(c, k);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Read with:");
            if ui
                .selectable_label(self.explore_kind == DataKind::Did, "UDS data IDs (22)")
                .on_hover_text("Most newer modules: 16-bit IDs such as F190")
                .clicked()
            {
                self.explore_kind = DataKind::Did;
            }
            if ui
                .selectable_label(self.explore_kind == DataKind::Lid, "KWP local IDs (21)")
                .on_hover_text("Older modules: 8-bit IDs 00 to FF")
                .clicked()
            {
                self.explore_kind = DataKind::Lid;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut self.explore_from).desired_width(60.0).font(egui::TextStyle::Monospace),
            );
            ui.label("to");
            ui.add(
                egui::TextEdit::singleline(&mut self.explore_to).desired_width(60.0).font(egui::TextStyle::Monospace),
            );
            let presets: &[(&str, &str, &str)] = if self.explore_kind == DataKind::Lid {
                &[("All (00-FF)", "00", "FF")]
            } else {
                &[("Identity", "F180", "F19F"), ("0100-01FF", "0100", "01FF"), ("1000-10FF", "1000", "10FF")]
            };
            for (label, a, b) in presets {
                if ui.small_button(*label).clicked() {
                    self.explore_from = (*a).into();
                    self.explore_to = (*b).into();
                }
            }
        });
        let range = parse_id(&self.explore_from).zip(parse_id(&self.explore_to));
        ui.horizontal(|ui| {
            let n = range.map_or(0, |(a, b)| u32::from(a.max(b) - a.min(b)) + 1);
            let ok = range.is_some()
                && n <= 4096
                && (self.explore_kind == DataKind::Did || range.is_some_and(|(a, b)| a.max(b) <= 0xFF));
            if ui
                .add_enabled(ok && !busy, crate::theme::accent_button(&pal, "Scan"))
                .on_hover_text("Ask for each ID in the range once")
                .clicked()
                && let Some((from, to)) = range
            {
                self.stop_watch();
                self.session.send(Command::ScanData { channel, kind: self.explore_kind, from, to });
            }
            if busy && self.model.progress.is_some() && ui.button("Stop").clicked() {
                self.session.send(Command::Stop);
            }
            if range.is_none() {
                ui.label(RichText::new("Type the range in hex, for example F180 and F19F.").color(pal.note));
            } else if n > 4096 {
                ui.label(RichText::new("At most 4096 IDs at a time.").color(pal.note));
            } else {
                // About 20 requests a second when Scana is alone.
                ui.label(RichText::new(format!("{n} IDs, about {} s.", (n / 18).max(1))).color(pal.dim));
            }
        });
        if let Some(p) = self.model.progress {
            ui.add(egui::ProgressBar::new(p).desired_width(ui.available_width().min(520.0)).show_percentage());
        }
        ui.add_space(6.0);

        let ex = self.model.explore.clone();
        let showing = ex.channel == Some(channel) && ex.kind == Some(self.explore_kind);
        if !showing {
            ui.label(RichText::new("Press Scan to see which IDs this module answers.").color(pal.dim));
        } else if ex.items.is_empty() {
            ui.label("The module answered none of these IDs. Try another range, or the other kind of ID.");
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{} ID(s) answered.", ex.items.len())).strong());
                if !ex.complete {
                    ui.label(RichText::new("(the scan was stopped early)").color(pal.note));
                }
                let mut watch = self.explore_watch;
                if ui
                    .checkbox(&mut watch, "Watch")
                    .on_hover_text("Keep reading these IDs and light up the bytes that change")
                    .changed()
                {
                    if watch {
                        self.session.send(Command::Watch(Some(WatchSpec {
                            channel,
                            kind: self.explore_kind,
                            ids: ex.items.keys().copied().collect(),
                        })));
                    } else {
                        self.session.send(Command::Watch(None));
                    }
                    self.explore_watch = watch;
                }
                if ui.button("Reset changes").on_hover_text("Start counting changes from now").clicked() {
                    self.model.explore.items.values_mut().for_each(|i| i.reset());
                }
            });
            if self.explore_watch {
                ui.label(
                    RichText::new("Now do one thing at a time in the car (headlamps on, a door open, the brake pressed) and see which byte lights up.")
                        .color(pal.note),
                );
            }
            self.explore_table(ui, channel);
        }
        if let Some(sel) = self.explore_byte {
            self.byte_panel(ui, channel, sel);
        }
        ui.add_space(10.0);
        self.console_box(ui, channel);
    }

    /// Stops the watch, if on (leaving the page or starting a scan).
    pub fn stop_watch(&mut self) {
        if self.explore_watch {
            self.session.send(Command::Watch(None));
            self.explore_watch = false;
        }
    }

    fn vehicle_name(&self) -> String {
        self.model.conn.as_ref().map_or_else(|| "Unknown car".into(), |c| c.vehicle.clone())
    }

    fn explore_table(&mut self, ui: &mut Ui, channel: scana_core::Channel) {
        let pal = self.pal();
        let ex = self.model.explore.clone();
        let kind = self.explore_kind;
        let vehicle = self.vehicle_name();
        egui::Grid::new("explore-table").striped(true).num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("ID").strong());
            ui.label(RichText::new("Bytes (click one)").strong());
            ui.label(RichText::new("Changes").strong());
            ui.label(RichText::new("Meaning").strong());
            ui.end_row();
            for (&id, item) in &ex.items {
                ui.label(RichText::new(kind.label(id)).monospace());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    for (i, b) in item.bytes.iter().enumerate() {
                        let recent = item
                            .changed_at
                            .get(i)
                            .copied()
                            .flatten()
                            .is_some_and(|t| ex.now_ms.saturating_sub(t) < RECENT_MS);
                        let ever = item.changes.get(i).is_some_and(|c| *c > 0);
                        let mut text = RichText::new(format!("{b:02X}")).monospace();
                        if recent {
                            text = text.color(egui::Color32::BLACK).background_color(pal.accent).strong();
                        } else if ever {
                            text = text.color(pal.note).strong();
                        }
                        let selected = self.explore_byte == Some((id, i));
                        if ui.selectable_label(selected, text).on_hover_text(format!("Byte {i}: {b} decimal")).clicked()
                        {
                            self.explore_byte = if selected { None } else { Some((id, i)) };
                            self.name_text = self
                                .names
                                .get(&vehicle, &channel.label(), &kind.label(id), i)
                                .unwrap_or_default()
                                .to_string();
                        }
                    }
                });
                let total: u32 = item.changes.iter().sum();
                ui.label(if total == 0 {
                    RichText::new("-").color(pal.dim)
                } else {
                    RichText::new(total.to_string()).color(pal.note)
                });
                let mut meaning: Vec<String> = Vec::new();
                for (byte, name) in self.names.for_id(&vehicle, &channel.label(), &kind.label(id)) {
                    meaning.push(format!("byte {byte}: {name}"));
                }
                if kind == DataKind::Did
                    && let Some(n) = uds::did_name(id)
                {
                    meaning.push(n);
                }
                if let Some(t) = uds::text_of(&item.bytes)
                    && item.bytes.len() >= 3
                {
                    meaning.push(format!("\"{t}\""));
                }
                ui.label(meaning.join("; "));
                ui.end_row();
            }
        });
    }

    fn byte_panel(&mut self, ui: &mut Ui, channel: scana_core::Channel, (id, i): (u16, usize)) {
        let pal = self.pal();
        let kind = self.explore_kind;
        let Some(item) = self.model.explore.items.get(&id).cloned() else { return };
        let Some(&b) = item.bytes.get(i) else { return };
        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("Byte {i} of {} on {}", kind.label(id), channel.label())).strong());
            ui.label(format!("Now: {b:02X} hex, {b} decimal. Changed {} time(s) since the scan or the last reset.", item.changes.get(i).copied().unwrap_or(0)));
            let bits: Vec<String> = (0..8).rev().filter(|n| b & (1 << n) != 0).map(|n| n.to_string()).collect();
            let mut lines = vec![
                format!("As a switch or flags: bits on: {}", if bits.is_empty() { "none".into() } else { bits.join(", ") }),
                format!("As a signed number: {}", b as i8),
                format!("As a temperature (value minus 40): {} °C", i32::from(b) - 40),
                format!("As a percentage (value x 100 / 255): {:.1} %", f64::from(b) * 100.0 / 255.0),
                format!("As a voltage (value / 10): {:.1} V", f64::from(b) / 10.0),
            ];
            if let Some(&n) = item.bytes.get(i + 1) {
                let w = u16::from_be_bytes([b, n]);
                lines.push(format!("With the next byte, as one number (high byte first): {w}"));
            }
            for l in lines {
                ui.label(RichText::new(format!("  • {l}")).color(pal.dim));
            }
            ui.label(RichText::new("These are guesses at common scalings. Watch the byte while you change one thing to find which is right.").small().color(pal.dim));
            ui.horizontal(|ui| {
                ui.label("Name it:");
                ui.add(egui::TextEdit::singleline(&mut self.name_text).hint_text("for example Headlamp switch").desired_width(260.0));
                if ui.button("Save name").clicked() {
                    let key = ValueName {
                        vehicle: self.vehicle_name(),
                        module: channel.label(),
                        id: kind.label(id),
                        byte: i,
                        name: self.name_text.clone(),
                    };
                    if let Err(e) = self.names.set(key) {
                        self.model.banner(format!("The name couldn't be saved: {e}"), true);
                    }
                }
            });
            ui.label(RichText::new("Names stay on this computer, for this vehicle and module.").small().color(pal.dim));
        });
    }

    fn console_box(&mut self, ui: &mut Ui, channel: scana_core::Channel) {
        let pal = self.pal();
        if !self.settings.advanced {
            ui.label(
                RichText::new("Turn on advanced mode in Settings to send your own read requests to a module.")
                    .small()
                    .color(pal.dim),
            );
            return;
        }
        ui.label(RichText::new(format!("Send a read request to {}", channel.label())).size(16.0).strong());
        ui.label(RichText::new("Hex bytes, for example 22 F1 90 (UDS) or 21 01 (KWP). Anything that changes a module is refused before it is sent.").color(pal.dim));
        let mut send = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.console_text)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(260.0),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                send = true;
            }
            if ui.add_enabled(self.model.busy.is_none(), egui::Button::new("Send")).clicked() {
                send = true;
            }
        });
        if send {
            match uds::parse_hex(&self.console_text) {
                Some(req) => self.session.send(Command::Console { channel, request: req }),
                None => self.model.banner("Type the request as hex bytes, for example 22 F1 90.", true),
            }
        }
        for e in self.model.console.iter().rev().take(8) {
            let answer = match &e.reply {
                Ok(Some(Reply::Positive { service, data })) => {
                    let text = uds::text_of(data).map(|t| format!("  \"{t}\"")).unwrap_or_default();
                    format!("{:02X} {}{text}", service + 0x40, uds::hex(data))
                }
                Ok(Some(Reply::Negative { service, code })) => {
                    format!("7F {service:02X} {code:02X}: refused, {}", obd::negative_code_text(*code))
                }
                Ok(None) => "no answer".into(),
                Err(msg) => msg.clone(),
            };
            ui.label(
                RichText::new(format!("{} ← {}   → {answer}", e.channel.label(), uds::hex(&e.request)))
                    .monospace()
                    .small(),
            );
        }
    }
}

/// A data ID typed in hex: "F190", "0xF190" or "01".
fn parse_id(s: &str) -> Option<u16> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    if t.is_empty() || t.len() > 4 {
        return None;
    }
    u16::from_str_radix(t, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::parse_id;

    #[test]
    fn ids_parse() {
        assert_eq!(parse_id("F190"), Some(0xF190));
        assert_eq!(parse_id("0x01"), Some(1));
        assert_eq!(parse_id(" 7 "), Some(7));
        assert_eq!(parse_id("12345"), None);
        assert_eq!(parse_id("zz"), None);
        assert_eq!(parse_id(""), None);
    }
}
