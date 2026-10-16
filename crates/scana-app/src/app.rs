//! The app's frame: top bar, page list, status bar, the Learn panel, messages and dialogs.
//! Each page lives in `pages/`.

use crate::screenshot::ShotPlan;
use crate::settings::{Settings, ThemeChoice};
use crate::state::Model;
use crate::theme::{self, Palette};
use egui::{Align, Color32, Layout, RichText, Ui};
use scana_core::safety::{CLEAR_WARNING, ClearConsent};
use scana_core::session::{Command, LinkState, SessionHandle, Source};
use scana_core::store::{LoadedSession, SessionInfo};
use scana_link::serial::PortCandidate;
use scana_proto::{CanId, Dtc};
use std::collections::BTreeSet;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Health,
    Codes,
    Diagnose,
    Live,
    Graphs,
    Readiness,
    Tests,
    Info,
    Modules,
    Explorer,
    Monitor,
    Sessions,
    Learn,
    Glossary,
    Settings,
}

impl Page {
    pub const ALL: [Page; 16] = [
        Page::Home,
        Page::Health,
        Page::Codes,
        Page::Diagnose,
        Page::Live,
        Page::Graphs,
        Page::Readiness,
        Page::Tests,
        Page::Info,
        Page::Modules,
        Page::Explorer,
        Page::Monitor,
        Page::Sessions,
        Page::Learn,
        Page::Glossary,
        Page::Settings,
    ];

    /// The menu: section headings and their pages.
    pub const MENU: [(&'static str, &'static [Page]); 6] = [
        ("", &[Page::Home]),
        ("CAR", &[Page::Health, Page::Codes, Page::Live, Page::Graphs, Page::Readiness, Page::Tests, Page::Info]),
        ("DIAGNOSE", &[Page::Diagnose]),
        ("MODULES", &[Page::Modules, Page::Explorer]),
        ("LEARN", &[Page::Learn, Page::Glossary]),
        ("TOOLS", &[Page::Monitor, Page::Sessions, Page::Settings]),
    ];

    /// The name in the menu, where it differs from the page's title.
    pub fn menu_label(self) -> &'static str {
        match self {
            Page::Diagnose => "Guides",
            Page::Modules => "All modules",
            Page::Sessions => "Sessions",
            p => p.title(),
        }
    }

    /// The menu icon (assets/icons/icon-NAME.png).
    pub fn icon(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Health => "health",
            Page::Codes => "codes",
            Page::Diagnose => "diagnose",
            Page::Live => "live",
            Page::Graphs => "graphs",
            Page::Readiness => "readiness",
            Page::Tests => "tests",
            Page::Info => "info",
            Page::Modules => "modules",
            Page::Explorer => "explorer",
            Page::Monitor => "monitor",
            Page::Sessions => "sessions",
            Page::Learn => "library",
            Page::Glossary => "glossary",
            Page::Settings => "settings",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Health => "health",
            Page::Codes => "codes",
            Page::Diagnose => "diagnose",
            Page::Live => "live",
            Page::Graphs => "graphs",
            Page::Readiness => "readiness",
            Page::Tests => "tests",
            Page::Info => "info",
            Page::Modules => "modules",
            Page::Explorer => "explorer",
            Page::Monitor => "monitor",
            Page::Sessions => "sessions",
            Page::Learn => "learn",
            Page::Glossary => "glossary",
            Page::Settings => "settings",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Home => "Home",
            Page::Health => "Health check",
            Page::Codes => "Fault codes",
            Page::Diagnose => "Diagnose",
            Page::Live => "Live data",
            Page::Graphs => "Graphs",
            Page::Readiness => "Readiness",
            Page::Tests => "Test results",
            Page::Info => "Vehicle info",
            Page::Modules => "Modules",
            Page::Explorer => "Data explorer",
            Page::Monitor => "Bus monitor",
            Page::Sessions => "Sessions, reports",
            Page::Learn => "Library",
            Page::Glossary => "Glossary",
            Page::Settings => "Settings",
        }
    }

    pub fn from_id(id: &str) -> Option<Page> {
        Page::ALL.iter().copied().find(|p| p.id() == id)
    }
}

/// What the Learn panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Topic {
    Pid(CanId, u8),
    Dtc(Dtc),
    Term(String),
    /// A Library topic, by id.
    Library(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceChoice {
    Bridge,
    Demo,
    DemoFaults,
    DemoCorolla,
}

pub struct ScanaApp {
    pub session: SessionHandle,
    pub model: Model,
    pub settings: Settings,
    /// False in screenshot mode: never touch the user's settings file.
    pub persist: bool,
    pub page: Page,
    pub topic: Option<Topic>,
    pub ports: Vec<PortCandidate>,
    pub source_choice: SourceChoice,
    pub port_choice: Option<String>,
    /// The clear-codes dialog is open; the bool is the "I understand" tick.
    pub clear_dialog: Option<bool>,
    pub graph_window_s: f64,
    pub graph_paused: bool,
    /// How far the graphs are panned back from the newest data, in seconds.
    pub graph_offset_s: f64,
    pub graph_hover: Option<f64>,
    pub loaded: Option<(String, LoadedSession)>,
    pub loaded_selected: BTreeSet<(String, u8)>,
    pub sessions: Option<Vec<SessionInfo>>,
    pub glossary_filter: String,
    pub monitor_obd_only: bool,
    pub open_job: Option<String>,
    pub sessions_dir: std::path::PathBuf,
    pub shot: Option<ShotPlan>,
    /// Stage 3: the module open on the Modules page.
    pub module_selected: Option<scana_core::Channel>,
    /// The data explorer's module, kind of ID, range, watch and chosen byte.
    pub explore_channel: Option<scana_core::Channel>,
    pub explore_kind: scana_core::modules::DataKind,
    pub explore_from: String,
    pub explore_to: String,
    pub explore_watch: bool,
    pub explore_byte: Option<(u16, usize)>,
    pub name_text: String,
    pub console_text: String,
    /// Names the user gave to data explorer values.
    pub names: crate::names::Names,
    /// The diagnosis open on the Diagnose page, the code typed, the open test card,
    /// a live measurement in progress, and a read waiting to answer a test.
    pub diag: Option<scana_core::diagnose::Diagnosis>,
    pub diag_code_text: String,
    /// Filters the list of guides on the Diagnose page.
    pub diag_filter: String,
    pub diag_open: Option<String>,
    pub diag_measure: Option<crate::pages::diagnose::DiagMeasure>,
    /// One-off reads waiting to answer tests (several can wait at once).
    pub diag_wait: Vec<crate::pages::diagnose::DiagWait>,
    /// The Learn panel's history: Back and Forward.
    pub learn_back: Vec<Option<Topic>>,
    pub learn_forward: Vec<Option<Topic>>,
    /// The sidebar's search box.
    pub search_text: String,
    /// The logo and menu icons, loaded on the first frame.
    pub assets: Option<crate::assets::Assets>,
    applied: Option<(ThemeChoice, f32)>,
}

impl ScanaApp {
    pub fn new(
        session: SessionHandle,
        settings: Settings,
        persist: bool,
        sessions_dir: std::path::PathBuf,
        shot: Option<ShotPlan>,
    ) -> Self {
        let mut app = ScanaApp {
            session,
            model: Model::default(),
            settings,
            persist,
            page: Page::Home,
            topic: None,
            ports: scana_link::serial::list_ports(),
            source_choice: SourceChoice::Bridge,
            port_choice: None,
            clear_dialog: None,
            graph_window_s: 60.0,
            graph_paused: false,
            graph_offset_s: 0.0,
            graph_hover: None,
            loaded: None,
            loaded_selected: BTreeSet::new(),
            sessions: None,
            glossary_filter: String::new(),
            monitor_obd_only: false,
            open_job: None,
            names: crate::names::Names::load(
                persist.then(|| sessions_dir.parent().map(|d| d.join("names.toml"))).flatten(),
            ),
            sessions_dir,
            shot,
            module_selected: None,
            explore_channel: None,
            explore_kind: scana_core::modules::DataKind::Did,
            explore_from: "F180".into(),
            explore_to: "F19F".into(),
            explore_watch: false,
            explore_byte: None,
            name_text: String::new(),
            console_text: String::new(),
            diag: None,
            diag_code_text: String::new(),
            diag_filter: String::new(),
            diag_open: None,
            diag_measure: None,
            diag_wait: Vec::new(),
            learn_back: Vec::new(),
            learn_forward: Vec::new(),
            search_text: String::new(),
            assets: None,
            applied: None,
        };
        app.push_gaps();
        app
    }

    pub fn pal(&self) -> Palette {
        theme::palette(self.settings.theme)
    }

    pub fn connect(&mut self, source: Source) {
        self.model.banners.clear();
        self.session.send(Command::Connect(source));
    }

    pub fn connect_choice(&mut self) {
        let src = match self.source_choice {
            SourceChoice::Bridge => Source::Bridge { port: self.port_choice.clone() },
            SourceChoice::Demo => Source::Demo,
            SourceChoice::DemoFaults => Source::DemoFaults,
            SourceChoice::DemoCorolla => Source::DemoCorolla,
        };
        if self.settings.first_run {
            self.settings.first_run = false;
            self.save_settings();
        }
        self.connect(src);
    }

    pub fn send_live(&mut self) {
        self.session.send(Command::SetLive(self.model.selected.clone()));
    }

    pub fn push_gaps(&mut self) {
        self.session
            .send(Command::SetGaps { alone_ms: self.settings.gap_alone_ms, shared_ms: self.settings.gap_shared_ms });
    }

    pub fn save_settings(&mut self) {
        self.settings.clamp();
        if self.persist
            && let Err(e) = self.settings.save()
        {
            self.model.banner(e, true);
        }
    }

    pub fn go(&mut self, page: Page) {
        if self.page == Page::Explorer && page != Page::Explorer {
            // Watching takes bus time from the live values: stop it when leaving.
            self.stop_watch();
        }
        self.page = page;
        if page == Page::Sessions {
            self.sessions = None; // rescan on entry
        }
    }

    /// Shows something in the Learn panel (`None`: the page's help), keeping Back and Forward.
    pub fn set_topic(&mut self, t: Option<Topic>) {
        if self.topic != t {
            self.learn_back.push(self.topic.take());
            if self.learn_back.len() > 50 {
                self.learn_back.remove(0);
            }
            self.learn_forward.clear();
            self.topic = t;
        }
    }

    /// Opens something in the Learn panel, showing the panel if it was hidden.
    pub fn open_topic(&mut self, t: Topic) {
        self.set_topic(Some(t));
        if !self.settings.show_learn {
            self.settings.show_learn = true;
            self.save_settings();
        }
    }

    pub fn learn_go_back(&mut self) {
        if let Some(t) = self.learn_back.pop() {
            self.learn_forward.push(std::mem::replace(&mut self.topic, t));
        }
    }

    pub fn learn_go_forward(&mut self) {
        if let Some(t) = self.learn_forward.pop() {
            self.learn_back.push(std::mem::replace(&mut self.topic, t));
        }
    }

    /// Follows a link from a word, a related entry or a search result.
    pub fn follow(&mut self, link: scana_db::library::Link) {
        use scana_db::library::Link;
        match link {
            Link::Topic(id) => self.open_topic(Topic::Library(id)),
            Link::Term(name) => self.open_topic(Topic::Term(name)),
            Link::Pid(p) => {
                let ecu =
                    self.model.conn.as_ref().and_then(|c| c.ecus.first()).map_or(CanId::Standard(0x7E8), |e| e.id);
                self.open_topic(Topic::Pid(ecu, p));
            }
            Link::Code(c) => {
                if let Some(d) = Dtc::parse(&c) {
                    self.open_topic(Topic::Dtc(d));
                }
            }
            Link::Guide(g) => {
                if let Some(g) = scana_db::diag::guide(&g) {
                    self.start_guide(g);
                }
            }
            Link::Page(p) => {
                if let Some(p) = Page::from_id(&p) {
                    self.go(p);
                }
            }
        }
    }

    pub fn set_record(&mut self, on: bool) {
        self.session.send(Command::Record(on));
    }

    pub fn open_path(&mut self, path: &std::path::Path) {
        if let Err(e) = std::process::Command::new("xdg-open").arg(path).spawn() {
            self.model.banner(format!("Couldn't open {}: {e}", path.display()), true);
        }
    }

    fn apply_look(&mut self, ctx: &egui::Context) {
        let want = (self.settings.theme, self.settings.zoom);
        if self.applied != Some(want) {
            theme::apply(ctx, want.0);
            ctx.set_zoom_factor(want.1);
            self.applied = Some(want);
        }
        // Ctrl + / Ctrl - change the zoom directly: keep the setting in step.
        let z = ctx.zoom_factor();
        if (z - self.settings.zoom).abs() > 0.001 {
            self.settings.zoom = z;
            self.applied = Some((self.settings.theme, z));
            self.save_settings();
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        // Asked before `input`, never inside it: `input` holds egui's lock, so asking there froze
        // the app on the first M typed.
        let typing = ctx.egui_wants_keyboard_input();
        let (f1, rec, health, mark) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F1),
                i.modifiers.ctrl && i.key_pressed(egui::Key::R),
                i.modifiers.ctrl && i.key_pressed(egui::Key::H),
                !typing && !i.modifiers.any() && i.key_pressed(egui::Key::M),
            )
        });
        if f1 {
            self.settings.show_learn = !self.settings.show_learn;
            self.save_settings();
        }
        if rec && self.model.connected() {
            self.set_record(self.model.recording.is_none());
        }
        if health && self.model.connected() {
            self.session.send(Command::HealthCheck);
            self.go(Page::Health);
        }
        if mark && self.model.connected() && matches!(self.page, Page::Live | Page::Graphs) {
            self.mark();
        }
    }

    pub fn mark(&mut self) {
        let t = self.model.live.values().map(|l| l.t_ms).max().unwrap_or(0) as f64 / 1000.0;
        self.model.marks.push(t);
        self.session.send(Command::Mark(format!("mark {}", self.model.marks.len())));
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if let Some(logo) = self.assets.as_ref().and_then(|a| a.logo.as_ref()) {
                ui.add(egui::Image::new((logo.id(), egui::vec2(30.0, 30.0))));
            }
            ui.label(RichText::new("Scana").size(24.0).strong());
            ui.add_space(8.0);
            ui.vertical(|ui| {
                let (title, sub) = match &self.model.conn {
                    Some(c) => (c.vehicle.clone(), c.protocol()),
                    None => ("No car connected".to_string(), "Connect on the Home page".to_string()),
                };
                ui.label(RichText::new(title).strong());
                ui.label(RichText::new(sub).small().color(pal.dim));
            });
            ui.add_space(12.0);
            let (text, color) = self.pill();
            egui::Frame::new().fill(color).corner_radius(12).inner_margin(egui::Margin::symmetric(12, 4)).show(
                ui,
                |ui| {
                    ui.label(RichText::new(text).color(Color32::WHITE).strong());
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let connected = self.model.connected();
                if self.model.conn.is_some()
                    || matches!(self.model.state, Some(LinkState::Lost(_) | LinkState::Connecting(_)))
                {
                    if ui.button("Disconnect").clicked() {
                        self.session.send(Command::Disconnect);
                    }
                } else if ui.button("Connect").clicked() {
                    self.go(Page::Home);
                }
                let learn_on = self.settings.show_learn;
                if ui
                    .add(egui::Button::new("Learn").selected(learn_on))
                    .on_hover_text("Show or hide the Learn panel on the right (F1)")
                    .clicked()
                {
                    self.settings.show_learn = !learn_on;
                    self.save_settings();
                }
                let rec_on = self.model.recording.is_some();
                let rec_label =
                    if rec_on { RichText::new("● Recording").color(pal.bad) } else { RichText::new("○ Record") };
                if ui
                    .add_enabled(connected, egui::Button::new(rec_label).selected(rec_on))
                    .on_hover_text("Record every frame and value to a session folder (Ctrl+R)")
                    .clicked()
                {
                    self.set_record(!rec_on);
                }
                if ui
                    .add_enabled(connected, crate::theme::accent_button(&pal, "Health check"))
                    .on_hover_text("Codes, readiness, freeze frame and key values in one go (Ctrl+H)")
                    .clicked()
                {
                    self.session.send(Command::HealthCheck);
                    self.go(Page::Health);
                }
            });
        });
        ui.add_space(4.0);
    }

    /// The connection pill: text and colour.
    fn pill(&self) -> (String, Color32) {
        let pal = self.pal();
        match &self.model.state {
            Some(LinkState::Connected) => {
                let mil = match self.model.mil() {
                    Some(true) => " · check engine light ON",
                    Some(false) => " · check engine light off",
                    None => "",
                };
                let color = if self.model.mil() == Some(true) {
                    Color32::from_rgb(0x9B, 0x22, 0x26)
                } else {
                    Color32::from_rgb(0x1F, 0x5F, 0x3A)
                };
                let _ = pal;
                (format!("Connected{mil}"), color)
            }
            Some(LinkState::Connecting(s)) => (format!("{s}..."), Color32::from_rgb(0x2F, 0x5F, 0x90)),
            Some(LinkState::Lost(_)) => ("Lost: trying again".into(), Color32::from_rgb(0x8A, 0x5A, 0x00)),
            Some(LinkState::Failed(_)) => ("Not connected (see message)".into(), Color32::from_rgb(0x7A, 0x2A, 0x2A)),
            Some(LinkState::Disconnected) | None => ("Not connected".into(), Color32::from_rgb(0x44, 0x4A, 0x52)),
        }
    }

    /// The sidebar: a search box over everything Scana knows, then the menu in sections with
    /// icons. While something is typed, the results take the menu's place.
    fn nav(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        ui.add_space(8.0);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.search_text).hint_text("Search anything").desired_width(f32::INFINITY),
        );
        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.search_text.clear();
        }
        ui.add_space(6.0);
        if self.search_text.trim().len() >= 2 {
            self.search_results(ui);
            return;
        }
        egui::ScrollArea::vertical().id_salt("menu").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (heading, pages) in Page::MENU {
                if !heading.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(heading).small().strong().color(pal.dim));
                    ui.add_space(1.0);
                }
                for &p in pages {
                    if self.menu_item(ui, p) {
                        self.go(p);
                        self.set_topic(None);
                    }
                }
            }
            if self.settings.advanced {
                ui.add_space(6.0);
                ui.label(RichText::new("Advanced mode on").small().color(pal.note));
            }
        });
    }

    /// One menu row: icon and label, with an orange bar and background when selected.
    fn menu_item(&mut self, ui: &mut Ui, p: Page) -> bool {
        let pal = self.pal();
        let selected = self.page == p;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 5.0, pal.sel_bg);
            painter.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, pal.accent);
        } else if resp.hovered() {
            painter.rect_filled(rect, 5.0, pal.tile);
        }
        let colour = if selected { pal.accent } else { pal.dim };
        let icon_rect =
            egui::Rect::from_center_size(egui::pos2(rect.left() + 20.0, rect.center().y), egui::vec2(18.0, 18.0));
        if let Some(tex) = self.assets.as_ref().and_then(|a| a.icon(p.icon())) {
            egui::Image::new((tex.id(), icon_rect.size())).tint(colour).paint_at(ui, icon_rect);
        }
        let text_colour = if selected { pal.accent } else { ui.visuals().text_color() };
        let font = egui::FontId::proportional(15.0);
        ui.painter().text(
            egui::pos2(rect.left() + 38.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            p.menu_label(),
            font,
            text_colour,
        );
        resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    }

    /// Search results in the sidebar: click one to open it.
    fn search_results(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        let hits = scana_db::library::search(&self.search_text, 40);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} found", hits.len())).small().color(pal.dim));
            if ui.small_button("Clear").clicked() {
                self.search_text.clear();
            }
        });
        let mut chosen = None;
        egui::ScrollArea::vertical().id_salt("search").auto_shrink([false, false]).show(ui, |ui| {
            if hits.is_empty() {
                ui.label(RichText::new("Nothing matches. Try fewer or other words.").color(pal.dim));
            }
            for h in &hits {
                ui.label(RichText::new(h.kind.label().to_uppercase()).small().color(pal.dim));
                let resp = ui
                    .add(egui::Label::new(RichText::new(&h.title).color(pal.link)).sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(&h.detail);
                if resp.clicked() {
                    chosen = Some(h.link.clone());
                }
                ui.add_space(2.0);
            }
        });
        if let Some(link) = chosen {
            self.follow(link);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let state = match &self.model.state {
                Some(LinkState::Connected) => {
                    self.model.conn.as_ref().map_or("Connected".to_string(), |c| c.link.clone())
                }
                Some(LinkState::Connecting(s)) => s.clone(),
                Some(LinkState::Lost(s)) | Some(LinkState::Failed(s)) => s.clone(),
                Some(LinkState::Disconnected) | None => "Not connected".into(),
            };
            ui.label(RichText::new(state).small());
            if let Some(s) = &self.model.stats {
                ui.label(RichText::new(format!("{:.0} requests/s", s.requests_per_s)).small());
                ui.label(RichText::new(format!("{:.0} frames/s", s.frames_per_s)).small());
                if s.other_tester {
                    ui.label(RichText::new("another tester on the bus: Scana slows down").small().color(pal.note));
                }
            }
            if let Some(b) = &self.model.busy {
                ui.spinner();
                ui.label(RichText::new(b).small().color(pal.note));
            }
            let level = if self.clear_dialog.is_some() {
                ("Safety: CLEAR CODES (level 1)", pal.note)
            } else {
                ("Safety: READ ONLY", pal.good)
            };
            ui.label(RichText::new(level.0).small().strong().color(level.1))
                .on_hover_text("Scana only reads, except clearing codes after a confirmation.");
            if let Some(dir) = &self.model.recording {
                ui.label(
                    RichText::new(format!(
                        "Recording to {}",
                        dir.file_name().map(|f| f.to_string_lossy()).unwrap_or_default()
                    ))
                    .small()
                    .color(pal.bad),
                );
            }
            ui.label(RichText::new(format!("Scana {}", env!("CARGO_PKG_VERSION"))).small().color(pal.dim));
        });
    }

    fn banners(&mut self, ui: &mut Ui) {
        let pal = self.pal();
        self.model.banners.retain(|b| b.error || b.since.elapsed() < Duration::from_secs(8));
        let mut dismiss = None;
        for (i, b) in self.model.banners.iter().enumerate() {
            let fill = if b.error { pal.bad.gamma_multiply(0.25) } else { pal.accent.gamma_multiply(0.25) };
            egui::Frame::new().fill(fill).corner_radius(6).inner_margin(egui::Margin::symmetric(10, 6)).show(
                ui,
                |ui| {
                    ui.set_width(ui.available_width());
                    // A title row, then the text below it, so long messages wrap instead of widening the page.
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if b.error { "Problem" } else { "Note" }).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("✕").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                    ui.label(&b.text);
                },
            );
        }
        if let Some(i) = dismiss {
            self.model.banners.remove(i);
        }
    }

    fn clear_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut understood) = self.clear_dialog else { return };
        let pal = self.pal();
        let mut close = false;
        let mut go = false;
        let resp = egui::Modal::new(egui::Id::unique("clear-codes")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("Clear fault codes?");
            ui.label("Clearing codes also erases:");
            for line in CLEAR_WARNING {
                ui.label(format!("  • {line}"));
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Before it sends anything, Scana saves a health report and checks the engine is off and the car is still.").color(pal.dim));
            ui.label(RichText::new("Clear codes only after fixing the cause: the light comes back otherwise.").color(pal.note));
            ui.add_space(6.0);
            ui.checkbox(&mut understood, "I understand, and I want to clear the codes");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(understood, egui::Button::new(RichText::new("Clear codes").strong()).fill(pal.bad.gamma_multiply(0.6))).clicked() {
                    go = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if resp.should_close() {
            close = true;
        }
        if go {
            self.session.send(Command::ClearCodes(ClearConsent::confirmed_by_user()));
            self.clear_dialog = None;
        } else if close {
            self.clear_dialog = None;
        } else {
            self.clear_dialog = Some(understood);
        }
    }

    fn page(&mut self, ui: &mut Ui) {
        self.banners(ui);
        egui::ScrollArea::vertical().id_salt(("page", self.page.id())).auto_shrink([false, false]).show(ui, |ui| {
            match self.page {
                Page::Home => self.page_home(ui),
                Page::Health => self.page_health(ui),
                Page::Codes => self.page_codes(ui),
                Page::Diagnose => self.page_diagnose(ui),
                Page::Live => self.page_live(ui),
                Page::Graphs => self.page_graphs(ui),
                Page::Readiness => self.page_readiness(ui),
                Page::Tests => self.page_tests(ui),
                Page::Info => self.page_info(ui),
                Page::Modules => self.page_modules(ui),
                Page::Explorer => self.page_explorer(ui),
                Page::Monitor => self.page_monitor(ui),
                Page::Sessions => self.page_sessions(ui),
                Page::Learn => self.page_learn(ui),
                Page::Glossary => self.page_glossary(ui),
                Page::Settings => self.page_settings(ui),
            }
        });
    }

    /// A page heading with a one-line explanation.
    pub fn heading(&self, ui: &mut Ui, title: &str, sub: &str) {
        ui.heading(title);
        if !sub.is_empty() {
            ui.label(RichText::new(sub).color(self.pal().dim));
        }
        ui.add_space(6.0);
    }

    /// "Connect first" for pages that need a car.
    pub fn need_car(&mut self, ui: &mut Ui) -> bool {
        if self.model.connected() {
            return true;
        }
        ui.label("Connect to a car (or the demo car) on the Home page first.");
        if ui.button("Go to Home").clicked() {
            self.go(Page::Home);
        }
        false
    }
}

impl eframe::App for ScanaApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for u in self.session.poll() {
            self.diag_hook(&u);
            self.model.apply(u);
        }
        self.diag_after_updates();
        self.apply_look(&ctx);
        self.keys(&ctx);
        if self.assets.is_none() {
            self.assets = Some(crate::assets::Assets::load(&ctx));
        }

        egui::Panel::top("top-bar").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status-bar").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("nav").resizable(false).exact_size(210.0).show(ui, |ui| self.nav(ui));
        // On a narrow window the Learn panel gives way to the page.
        let wide = ctx.content_rect().width() >= 1100.0;
        if self.settings.show_learn && wide {
            egui::Panel::right("learn").resizable(true).default_size(300.0).show(ui, |ui| self.learn_panel(ui));
        }
        let page_frame = egui::Frame::central_panel(ui.style()).fill(self.pal().page_bg);
        egui::CentralPanel::default().frame(page_frame).show(ui, |ui| self.page(ui));
        self.clear_dialog(&ctx);

        if let Some(plan) = self.shot.take() {
            self.shot = crate::screenshot::step(self, plan, &ctx);
        }
        if self.model.connected() || self.model.busy.is_some() || self.shot.is_some() || self.diag_measure.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scana_core::session::WorkerSettings;
    use std::sync::mpsc;

    /// Typing "m" in the search box froze the app: the M (mark) shortcut asked egui a question
    /// while holding its lock. Runs on a thread, so a freeze fails the test instead of hanging it.
    #[test]
    fn typing_m_in_a_text_box_does_not_freeze() {
        let (done, wait) = mpsc::channel();
        std::thread::spawn(move || {
            let dir = std::env::temp_dir().join(format!("scana-keys-test-{}", std::process::id()));
            let worker = WorkerSettings { sessions_dir: dir.join("sessions"), auto_record: false };
            let session = SessionHandle::spawn(worker, || {}).expect("session thread");
            let mut app = ScanaApp::new(session, Settings::default(), false, dir.join("sessions"), None);
            app.search_text = "intake ".into();
            let ctx = egui::Context::default();
            let key = |pressed| egui::Event::Key {
                key: egui::Key::M,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let frames = [vec![], vec![key(true), egui::Event::Text("m".into()), key(false)], vec![]];
            for (n, events) in frames.into_iter().enumerate() {
                let input = egui::RawInput { events, ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    app.keys(ui.ctx());
                    let r = ui.add(egui::TextEdit::singleline(&mut app.search_text));
                    if n == 0 {
                        r.request_focus();
                    }
                });
                out.textures_delta.clear();
            }
            // And with nothing focused: the key is a shortcut, still no freeze.
            ctx.memory_mut(|m| m.stop_text_input());
            let input = egui::RawInput { events: vec![key(true), key(false)], ..Default::default() };
            ctx.run_ui(input, |ui| app.keys(ui.ctx())).textures_delta.clear();
            let _ = done.send(app.search_text.clone());
        });
        let typed = wait.recv_timeout(Duration::from_secs(20)).expect("the app froze on the M key");
        assert_eq!(typed, "intake m");
    }
}
