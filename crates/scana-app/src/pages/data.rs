//! Live data, Graphs, Readiness and Test results.

use crate::app::{ScanaApp, Topic};
use crate::chart::{self, Chart, ChartStyle};
use egui::{Align, Layout, RichText, Sense, Ui};
use scana_core::session::Command;
use scana_db::profile::Level;
use scana_proto::{CanId, mode06, pids};

impl ScanaApp {
    pub fn page_live(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Live data", "Tick the values to watch. Click a tile to learn what it means.");
        if !self.need_car(ui) {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} chosen", self.model.selected.len()));
            if ui.button("Mark (M)").on_hover_text("Mark this moment on the graphs and in the recording").clicked() {
                self.mark();
            }
            if ui
                .button("Choose all")
                .on_hover_text("Every value the car supports: good for recording a drive")
                .clicked()
            {
                if let Some(conn) = &self.model.conn {
                    self.model.selected = conn
                        .ecus
                        .iter()
                        .flat_map(|e| e.supported.iter().map(move |p| (e.id, *p)))
                        .filter(|(_, p)| !pids::is_support_pid(*p))
                        .collect();
                }
                self.send_live();
            }
            if ui.button("Choose none").clicked() {
                self.model.selected.clear();
                self.send_live();
            }
            let rec = self.model.recording.is_some();
            if ui.button(if rec { "Stop recording" } else { "Record" }).clicked() {
                self.set_record(!rec);
            }
            if let Some(s) = &self.model.stats {
                ui.label(RichText::new(format!("{:.0} requests a second", s.requests_per_s)).color(pal.dim));
            }
        });
        ui.add_space(6.0);
        let wide = ui.available_width() > 820.0;
        if wide {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(270.0);
                    self.live_chooser(ui);
                });
                ui.separator();
                ui.vertical(|ui| self.live_tiles(ui));
            });
        } else {
            egui::CollapsingHeader::new("Choose values").default_open(false).show(ui, |ui| self.live_chooser(ui));
            self.live_tiles(ui);
        }
    }

    fn live_chooser(&mut self, ui: &mut Ui) {
        let Some(conn) = self.model.conn.clone() else { return };
        egui::ScrollArea::vertical().id_salt("chooser").max_height(520.0).show(ui, |ui| {
            for ecu in &conn.ecus {
                if conn.ecus.len() > 1 {
                    ui.label(RichText::new(&ecu.name).strong());
                }
                for &pid in ecu.supported.iter().filter(|p| !pids::is_support_pid(**p)) {
                    let key = (ecu.id, pid);
                    let mut on = self.model.selected.contains(&key);
                    let name = pids::def(pid).map_or("Unknown PID", |d| d.name);
                    if ui.checkbox(&mut on, format!("{name} ({pid:02X})")).changed() {
                        if on {
                            self.model.selected.push(key);
                        } else {
                            self.model.selected.retain(|k| *k != key);
                        }
                        self.send_live();
                    }
                }
            }
        });
    }

    fn live_tiles(&mut self, ui: &mut Ui) {
        let profile = self.model.profile();
        let running = self.model.engine_running();
        if self.model.selected.is_empty() {
            ui.label("Nothing chosen yet: tick some values.");
            return;
        }
        const TILE_W: f32 = 232.0;
        const TILE_H: f32 = 118.0;
        let gap = ui.spacing().item_spacing.x;
        let cols = (((ui.available_width() + gap) / (TILE_W + gap)).floor() as usize).max(1);
        let keys = self.model.selected.clone();
        for row in keys.chunks(cols) {
            ui.horizontal(|ui| {
                for &key in row {
                    ui.allocate_ui_with_layout(egui::vec2(TILE_W, TILE_H), Layout::top_down(Align::Min), |ui| {
                        self.tile(ui, key, profile, running)
                    });
                }
            });
        }
    }

    fn tile(&mut self, ui: &mut Ui, key: (CanId, u8), profile: Option<&scana_db::profile::Profile>, running: bool) {
        let pal = self.pal();
        let (ecu, pid) = key;
        let name = pids::def(pid).map_or("Unknown PID", |d| d.name);
        let live = self.model.live.get(&key).cloned();
        let selected = self.topic == Some(Topic::Pid(ecu, pid));
        let level = match (&live, profile.and_then(|p| p.limit(pid))) {
            (Some(l), Some(lim)) => l.value().map_or(Level::Ok, |v| lim.check(v, running)),
            _ => Level::Ok,
        };
        let value_color = match level {
            Level::Ok => pal.tile_text,
            Level::Caution => pal.note,
            Level::Alarm => pal.bad,
        };
        let stroke =
            if selected { egui::Stroke::new(2.0, pal.accent) } else { egui::Stroke::new(1.0, pal.tile_border) };
        let resp = egui::Frame::new().fill(pal.tile).stroke(stroke).corner_radius(8).inner_margin(10).show(ui, |ui| {
            ui.set_width(212.0);
            ui.set_height(96.0);
            ui.label(RichText::new(name).color(pal.dim).small());
            match live.as_ref().and_then(|l| l.reading.decoded.clone()) {
                Some(d) if d.text.is_none() && !d.values.is_empty() => {
                    let q = &d.values[0];
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(pids::format_value(q.value, q.unit)).size(32.0).strong().color(value_color),
                        );
                        ui.label(RichText::new(q.unit).size(16.0).color(pal.dim));
                    });
                    if d.values.len() > 1 {
                        let extra: Vec<String> = d.values[1..]
                            .iter()
                            .map(|x| format!("{} {} {}", x.label, pids::format_value(x.value, x.unit), x.unit))
                            .collect();
                        ui.label(RichText::new(extra.join(", ")).small().color(pal.dim));
                    }
                }
                Some(d) => {
                    ui.label(RichText::new(d.display()).size(16.0).color(pal.tile_text));
                }
                None => {
                    ui.label(RichText::new("waiting...").size(18.0).color(pal.dim));
                }
            }
            let rate = live.as_ref().map_or(0.0, |l| l.rate_hz());
            let mut foot = format!("PID {pid:02X}");
            if rate > 0.0 {
                foot = format!("{rate:.1} Hz · {foot}");
            }
            if self.settings.advanced
                && let Some(l) = &live
            {
                foot.push_str(&format!(
                    " · {}",
                    l.reading.raw.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
                ));
            }
            ui.with_layout(Layout::bottom_up(Align::Min), |ui| ui.label(RichText::new(foot).small().color(pal.dim)));
        });
        let click = ui.interact(resp.response.rect, ui.scope_id().with(("tile", ecu.raw(), pid)), Sense::click());
        if click.clicked() {
            self.topic = Some(Topic::Pid(ecu, pid));
        }
    }

    pub fn page_graphs(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        let loaded = self.loaded.clone();
        let series: Vec<(String, String, Vec<[f64; 2]>)>;
        let marks: Vec<f64>;
        if let Some((name, l)) = &loaded {
            self.heading(
                ui,
                "Graphs",
                &format!("Session {name}. Drag a graph sideways to move through time; point at it to read values."),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.button("Back to live").clicked() {
                    self.loaded = None;
                    self.graph_offset_s = 0.0;
                }
                for (key, s) in &l.series {
                    let mut on = self.loaded_selected.contains(key);
                    if ui.checkbox(&mut on, format!("{} ({:02X})", s.name, key.1)).changed() {
                        if on {
                            self.loaded_selected.insert(key.clone());
                        } else {
                            self.loaded_selected.remove(key);
                        }
                    }
                }
            });
            series = l
                .series
                .iter()
                .filter(|(k, _)| self.loaded_selected.contains(*k))
                .map(|(_, s)| (s.name.clone(), s.unit.clone(), s.points.iter().map(|&(t, v)| [t, v]).collect()))
                .collect();
            marks = l.markers.iter().map(|m| m.0).collect();
        } else {
            self.heading(ui, "Graphs", "The live values, one graph each, on one time axis. Point at a graph to read values; pause, then drag to look back.");
            if !self.need_car(ui) {
                return;
            }
            let keys: Vec<(CanId, u8)> =
                self.model.selected.iter().copied().filter(|k| self.model.history.contains_key(k)).collect();
            series = keys
                .iter()
                .map(|k| {
                    let name = pids::def(k.1).map_or("Unknown PID", |d| d.name).to_string();
                    let unit = self
                        .model
                        .live
                        .get(k)
                        .and_then(|l| l.reading.decoded.as_ref())
                        .and_then(|d| d.main())
                        .map_or(String::new(), |q| q.unit.to_string());
                    let pts =
                        self.model.history.get(k).map(|h| h.iter().map(|&(t, v)| [t, v]).collect()).unwrap_or_default();
                    (name, unit, pts)
                })
                .collect();
            marks = self.model.marks.clone();
        }
        ui.horizontal(|ui| {
            for (label, secs) in [("30 s", 30.0), ("2 min", 120.0), ("10 min", 600.0), ("All", f64::INFINITY)] {
                if ui.selectable_label(self.graph_window_s == secs, label).clicked() {
                    self.graph_window_s = secs;
                    self.graph_offset_s = 0.0;
                }
            }
            if loaded.is_none() {
                if ui.checkbox(&mut self.graph_paused, "Pause").changed() && !self.graph_paused {
                    self.graph_offset_s = 0.0;
                }
                if ui.button("Mark (M)").clicked() {
                    self.mark();
                }
                ui.label(RichText::new("Values come from Live data: tick more there.").color(pal.dim));
            } else if self.graph_offset_s > 0.0 && ui.button("Go to the end").clicked() {
                self.graph_offset_s = 0.0;
            }
        });
        if series.is_empty() {
            ui.label(if loaded.is_some() { "Tick a value above." } else { "No numbers to draw yet." });
            return;
        }
        let first = series.iter().filter_map(|s| s.2.first().map(|p| p[0])).fold(f64::INFINITY, f64::min);
        let last = series.iter().filter_map(|s| s.2.last().map(|p| p[0])).fold(0.0, f64::max);
        let span = if self.graph_window_s.is_finite() { self.graph_window_s } else { (last - first).max(1.0) };
        let x1 = last - self.graph_offset_s;
        let x0 = x1 - span;
        let style = ChartStyle { bg: pal.tile, grid: pal.dim.gamma_multiply(0.35), text: pal.dim, mark: pal.note };
        let n = series.len() as f32;
        let h = ((ui.available_height() - 24.0 * n) / n).clamp(100.0, 220.0);
        let mut hover = self.graph_hover;
        let mut over_any = false;
        let mut drag_px = 0.0;
        let mut width = 1.0;
        for (i, (name, unit, pts)) in series.iter().enumerate() {
            let color = pal.series[i % pal.series.len()];
            let title = if unit.is_empty() { name.clone() } else { format!("{name} ({unit})") };
            ui.label(RichText::new(title).color(color).strong());
            let c = Chart { unit, color, points: pts };
            let r = chart::chart(ui, &c, x0, x1, h, &marks, &style, &mut hover, i + 1 == series.len());
            over_any |= r.hovered();
            if r.dragged() {
                drag_px += r.drag_delta().x;
                width = r.rect.width() - 66.0;
            }
        }
        self.graph_hover = if over_any { hover } else { None };
        if drag_px != 0.0 && (loaded.is_some() || self.graph_paused) {
            let secs = f64::from(drag_px / width.max(1.0)) * span;
            self.graph_offset_s = (self.graph_offset_s + secs).clamp(0.0, (last - first - span).max(0.0));
        } else if drag_px != 0.0 {
            self.graph_paused = true;
        }
    }

    pub fn page_readiness(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(
            ui,
            "Readiness",
            "The engine computer's self-tests. Not complete means the test hasn't run since the codes were last cleared.",
        );
        if !self.need_car(ui) {
            return;
        }
        if ui.add(egui::Button::new(RichText::new("Read readiness").strong()).fill(pal.accent)).clicked() {
            self.session.send(Command::ReadReadiness);
        }
        ui.add_space(8.0);
        let Some(list) = self.model.readiness.clone() else {
            ui.label("Press Read readiness.");
            return;
        };
        for (ecu, m) in list {
            ui.label(RichText::new(self.model.ecu_name(ecu)).size(17.0).strong());
            let mil = if m.mil {
                RichText::new("Check engine light: ON").color(pal.bad)
            } else {
                RichText::new("Check engine light: off").color(pal.good)
            };
            ui.label(mil.strong());
            ui.label(format!(
                "{} code(s) stored. {} engine monitors.",
                m.dtc_count,
                if m.compression { "Diesel" } else { "Petrol" }
            ));
            egui::Grid::new(("ready", ecu.raw())).striped(true).num_columns(2).spacing([30.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new("Monitor").strong());
                ui.label(RichText::new("State").strong());
                ui.end_row();
                for x in m.monitors.iter().filter(|x| x.available) {
                    ui.label(x.name);
                    if x.complete {
                        ui.label(RichText::new("✔ complete").color(pal.good));
                    } else {
                        ui.label(RichText::new("⚠ not complete").color(pal.note));
                    }
                    ui.end_row();
                }
            });
            let missing: Vec<&str> = m.monitors.iter().filter(|x| !x.available).map(|x| x.name).collect();
            if !missing.is_empty() {
                ui.label(
                    RichText::new(format!("Not fitted on this car: {}.", missing.join(", "))).small().color(pal.dim),
                );
            }
            ui.add_space(8.0);
        }
        if ui.link("What is a readiness monitor?").clicked() {
            self.topic = Some(Topic::Term("Readiness monitor".into()));
        }
    }

    pub fn page_tests(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.heading(ui, "Test results", "Mode 06: the numbers behind the readiness monitors, each with its limits. Tests 80 and up are the maker's own.");
        if !self.need_car(ui) {
            return;
        }
        if ui.add(egui::Button::new(RichText::new("Read test results").strong()).fill(pal.accent)).clicked() {
            self.session.send(Command::ReadTests);
        }
        ui.add_space(8.0);
        let Some(list) = self.model.tests.clone() else {
            ui.label("Press Read test results.");
            return;
        };
        for (ecu, results) in list {
            ui.label(RichText::new(self.model.ecu_name(ecu)).size(17.0).strong());
            if results.is_empty() {
                ui.label(RichText::new("This computer doesn't report test results.").color(pal.dim));
                continue;
            }
            egui::Grid::new(("tests", ecu.raw())).striped(true).num_columns(4).spacing([18.0, 4.0]).show(ui, |ui| {
                for h in ["Monitor and test", "Value", "Limits", "Result"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                let mut last_mid = None;
                for t in &results {
                    if last_mid != Some(t.mid) {
                        ui.label(RichText::new(mode06::monitor_name(t.mid)).strong());
                        ui.label("");
                        ui.label("");
                        ui.label("");
                        ui.end_row();
                        last_mid = Some(t.mid);
                    }
                    ui.label(format!("    {}", mode06::test_name(t.mid, t.tid)));
                    let f = |v: f64| pids::format_value(v, if t.unit.is_empty() { "x" } else { t.unit });
                    let unit = if t.unit == "counts" && t.value == 1.0 { "count" } else { t.unit };
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(format!("{} {unit}", f(t.value)))
                    });
                    ui.label(format!("{} to {}", f(t.min), f(t.max)));
                    if t.passed() {
                        ui.label(RichText::new("✔ pass").color(pal.good));
                    } else {
                        ui.label(RichText::new("✖ FAIL").color(pal.bad).strong());
                    }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
        }
    }
}
