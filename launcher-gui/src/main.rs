#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod config;
mod factorio;
mod rcon;
mod session;

use config::{Config, BACKENDS};
use factorio::SaveEntry;
use session::{Event, LaunchOpts, Session};
use std::path::PathBuf;
use std::time::Duration;

const MAX_LOG_LINES: usize = 2000;

enum Action {
    None,
    Launch(Option<PathBuf>),
    Stop,
    Export,
    Refresh,
    BrowseBinary,
    BrowseData,
}

fn project_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        for base in exe.ancestors().skip(1).take(5) {
            if base.join("config.toml").is_file() && base.join("mod").is_dir() {
                return base.to_path_buf();
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for base in cwd.ancestors().take(3) {
        if base.join("config.toml").is_file() && base.join("mod").is_dir() {
            return base.to_path_buf();
        }
    }
    cwd
}

struct App {
    root: PathBuf,
    cfg: Option<Config>,
    cfg_error: Option<String>,
    data_dir: Option<PathBuf>,
    binary: Option<PathBuf>,
    saves: Vec<SaveEntry>,
    selected: Option<usize>,
    backend: String,
    launch_client: bool,
    show_autosaves: bool,
    session: Option<Session>,
    log: Vec<String>,
    status: String,
    server_up: bool,
    toast: Option<String>,
}

impl App {
    fn new() -> Self {
        let root = project_root();
        let (cfg, cfg_error) = match Config::load(&root.join("config.toml")) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e)),
        };
        let backend = cfg
            .as_ref()
            .map(|c| c.bridge.backend.clone())
            .unwrap_or_else(|| "claude-cli".into());
        let data_dir = factorio::detect_data_dir();
        let saves = data_dir.as_deref().map(factorio::list_saves).unwrap_or_default();
        App {
            root,
            cfg,
            cfg_error,
            data_dir,
            binary: factorio::detect_binary(),
            saves,
            selected: None,
            backend,
            launch_client: true,
            show_autosaves: true,
            session: None,
            log: Vec::new(),
            status: "idle".into(),
            server_up: false,
            toast: None,
        }
    }

    fn refresh_saves(&mut self) {
        self.saves = self.data_dir.as_deref().map(factorio::list_saves).unwrap_or_default();
        self.selected = None;
    }

    fn blocker(&self) -> Option<String> {
        if self.cfg.is_none() {
            return Some(self.cfg_error.clone().unwrap_or_else(|| "config.toml missing".into()));
        }
        if self.binary.is_none() {
            return Some("Factorio executable not found — set it above".into());
        }
        if self.data_dir.is_none() {
            return Some("Factorio data folder not found — set it above".into());
        }
        None
    }

    fn session_save(&self) -> PathBuf {
        self.root.join("serverdata").join("saves").join("session.zip")
    }

    fn launch(&mut self, save: Option<PathBuf>) {
        let (Some(cfg), Some(binary), Some(data_dir)) =
            (self.cfg.clone(), self.binary.clone(), self.data_dir.clone())
        else {
            return;
        };
        let _ = Config::save_backend(&self.root.join("config.toml"), &self.backend);
        self.log.clear();
        self.server_up = false;
        self.status = "starting".into();
        self.toast = None;
        self.session = Some(Session::start(LaunchOpts {
            root: self.root.clone(),
            binary,
            data_dir,
            save,
            backend: self.backend.clone(),
            launch_client: self.launch_client,
            cfg,
        }));
    }

    fn drain_events(&mut self) {
        let mut finished = false;
        if let Some(s) = &self.session {
            while let Ok(ev) = s.events.try_recv() {
                match ev {
                    Event::Log(line) => {
                        self.log.push(line);
                        if self.log.len() > MAX_LOG_LINES {
                            let excess = self.log.len() - MAX_LOG_LINES;
                            self.log.drain(..excess);
                        }
                    }
                    Event::Status(s) => self.status = s,
                    Event::ServerUp => self.server_up = true,
                    Event::Failed(e) => {
                        self.status = format!("failed: {e}");
                        self.log.push(format!("[error] {e}"));
                    }
                    Event::Finished => finished = true,
                }
            }
        }
        if finished {
            if let Some(s) = &mut self.session {
                s.finish();
            }
            self.session = None;
            self.server_up = false;
            if !self.status.starts_with("failed") {
                self.status = "idle".into();
            }
            self.refresh_saves();
        }
    }

    fn export_session(&mut self) {
        let src = self.session_save();
        if !src.exists() {
            self.toast = Some("No session save to export yet".into());
            return;
        }
        let start = self.data_dir.clone().map(|d| d.join("saves")).unwrap_or_default();
        if let Some(dest) = rfd::FileDialog::new()
            .set_directory(start)
            .set_file_name("llm-scout-export.zip")
            .add_filter("Factorio save", &["zip"])
            .save_file()
        {
            self.toast = match std::fs::copy(&src, &dest) {
                Ok(_) => Some(format!("Exported to {}", dest.display())),
                Err(e) => Some(format!("Export failed: {e}")),
            };
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.session.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }

        let mut action = Action::None;

        // Locals so the closures never borrow self mutably.
        let mut backend = self.backend.clone();
        let mut launch_client = self.launch_client;
        let mut show_autosaves = self.show_autosaves;
        let mut selected = self.selected;
        let mut dismiss_toast = false;

        let running = self.session.is_some();
        let server_up = self.server_up;
        let blocker = self.blocker();
        let has_session = self.session_save().exists();
        let status = self.status.clone();
        let toast = self.toast.clone();
        let bin_label = path_label(&self.binary);
        let data_label = path_label(&self.data_dir);
        let saves = &self.saves;
        let log = &self.log;

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("LLM Scout");
                ui.add_space(10.0);
                let (dot, tint) = if server_up {
                    ("running", egui::Color32::from_rgb(120, 200, 120))
                } else if running {
                    ("starting", egui::Color32::from_rgb(220, 190, 110))
                } else {
                    ("idle", egui::Color32::GRAY)
                };
                ui.colored_label(tint, format!("\u{25CF} {dot}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&status).weak());
                });
            });
            ui.add_space(6.0);
            egui::Grid::new("paths").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label("Factorio");
                ui.add(egui::Label::new(egui::RichText::new(&bin_label).monospace().small()).truncate());
                if ui.small_button("Browse\u{2026}").clicked() {
                    action = Action::BrowseBinary;
                }
                ui.end_row();

                ui.label("Saves");
                ui.add(egui::Label::new(egui::RichText::new(&data_label).monospace().small()).truncate());
                if ui.small_button("Browse\u{2026}").clicked() {
                    action = Action::BrowseData;
                }
                ui.end_row();
            });
            ui.add_space(6.0);
        });

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            if let Some(problem) = &blocker {
                ui.colored_label(egui::Color32::from_rgb(230, 130, 130), format!("\u{26A0} {problem}"));
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.label("Model:");
                egui::ComboBox::from_id_salt("backend")
                    .selected_text(backend.clone())
                    .show_ui(ui, |ui| {
                        for b in BACKENDS {
                            ui.selectable_value(&mut backend, b.to_string(), b);
                        }
                    });
                ui.add_enabled(!running, egui::Checkbox::new(&mut launch_client, "Launch game"));
                ui.separator();

                if running {
                    if ui.button("Stop").clicked() {
                        action = Action::Stop;
                    }
                } else {
                    let can_launch = blocker.is_none() && selected.is_some();
                    if ui
                        .add_enabled(can_launch, egui::Button::new("Launch"))
                        .on_disabled_hover_text("Pick a save first")
                        .clicked()
                    {
                        action = Action::Launch(selected.and_then(|i| saves.get(i)).map(|s| s.path.clone()));
                    }
                    if ui
                        .add_enabled(blocker.is_none() && has_session, egui::Button::new("Resume session"))
                        .on_disabled_hover_text("No previous session save")
                        .clicked()
                    {
                        action = Action::Launch(None);
                    }
                    if ui.add_enabled(has_session, egui::Button::new("Export session\u{2026}")).clicked() {
                        action = Action::Export;
                    }
                }
            });
            if let Some(t) = &toast {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t).small());
                    if ui.small_button("dismiss").clicked() {
                        dismiss_toast = true;
                    }
                });
            }
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if running {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Log").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("{} lines", log.len())).weak().small());
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    for line in log {
                        let color = if line.contains("[error]")
                            || line.contains("WARNING")
                            || line.contains("ERROR")
                        {
                            egui::Color32::from_rgb(230, 140, 140)
                        } else if line.starts_with("[setup]") {
                            egui::Color32::from_rgb(140, 190, 240)
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        ui.label(egui::RichText::new(line).monospace().small().color(color));
                    }
                });
                return;
            }

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Choose a save").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{21BB} refresh").clicked() {
                        action = Action::Refresh;
                    }
                    ui.checkbox(&mut show_autosaves, "autosaves");
                });
            });
            ui.separator();

            if saves.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label("No saves found.");
                    ui.label(
                        egui::RichText::new("Point \"Saves\" at your Factorio data folder.")
                            .weak()
                            .small(),
                    );
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                for (idx, entry) in saves.iter().enumerate() {
                    if !show_autosaves && entry.is_autosave() {
                        continue;
                    }
                    let text = format!(
                        "{:<32}  {:>13}  {:>9}{}",
                        truncate(&entry.name, 32),
                        entry.age_label(),
                        entry.size_label(),
                        if entry.is_autosave() { "  autosave" } else { "" }
                    );
                    let row = ui.selectable_label(selected == Some(idx), egui::RichText::new(text).monospace());
                    if row.clicked() {
                        selected = Some(idx);
                    }
                    if row.double_clicked() && blocker.is_none() {
                        selected = Some(idx);
                        action = Action::Launch(Some(entry.path.clone()));
                    }
                }
            });
        });

        self.backend = backend;
        self.launch_client = launch_client;
        self.show_autosaves = show_autosaves;
        self.selected = selected;
        if dismiss_toast {
            self.toast = None;
        }

        match action {
            Action::None => {}
            Action::Launch(save) => self.launch(save),
            Action::Stop => {
                if let Some(s) = &self.session {
                    s.request_stop();
                }
                self.status = "stopping".into();
            }
            Action::Export => self.export_session(),
            Action::Refresh => self.refresh_saves(),
            Action::BrowseBinary => {
                if let Some(p) = rfd::FileDialog::new().pick_file() {
                    self.binary = Some(p);
                }
            }
            Action::BrowseData => {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.data_dir = Some(p);
                    self.refresh_saves();
                }
            }
        }
    }
}

fn path_label(p: &Option<PathBuf>) -> String {
    p.as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "not found".into())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Prints what the launcher detected and exits. Useful when a path guess is wrong.
fn run_check() {
    let root = project_root();
    println!("project root : {}", root.display());
    match Config::load(&root.join("config.toml")) {
        Ok(c) => println!(
            "config.toml  : ok (backend={}, rcon={}:{}, game port={})",
            c.bridge.backend, c.rcon.host, c.rcon.port, c.server.port
        ),
        Err(e) => println!("config.toml  : ERROR {e}"),
    }
    match factorio::detect_binary() {
        Some(p) => println!("factorio     : {}", p.display()),
        None => println!("factorio     : NOT FOUND"),
    }
    match factorio::detect_data_dir() {
        Some(p) => {
            println!("data dir     : {}", p.display());
            let saves = factorio::list_saves(&p);
            println!("saves        : {} found", saves.len());
            for s in saves.iter().take(5) {
                println!(
                    "   {:<32} {:>13} {:>9}{}",
                    truncate(&s.name, 32),
                    s.age_label(),
                    s.size_label(),
                    if s.is_autosave() { "  autosave" } else { "" }
                );
            }
        }
        None => println!("data dir     : NOT FOUND"),
    }
    let sess = root.join("serverdata/saves/session.zip");
    println!("session save : {}", if sess.exists() { "present" } else { "none yet" });
}

/// A one-shot question from the terminal, the dev tool that used to be bridge/ask.py.
fn run_ask(args: &[String]) -> Result<(), String> {
    let mut question = None;
    let mut backend = None;
    let mut tier = "full".to_string();
    let mut show_snapshot = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--ask" => question = rest.next().cloned(),
            "--backend" => backend = rest.next().cloned(),
            "--tier" => {
                if let Some(v) = rest.next() {
                    tier = v.clone();
                }
            }
            "--show-snapshot" => show_snapshot = true,
            _ => {}
        }
    }
    let question = question.ok_or("--ask needs a question")?;
    let root = project_root();
    let cfg = Config::load(&root.join("config.toml"))?;
    bridge::ask_once(
        &root,
        &cfg,
        &question,
        backend.as_deref(),
        &tier,
        show_snapshot,
    )
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check") {
        run_check();
        return Ok(());
    }
    if args.iter().any(|a| a == "--ask") {
        if let Err(e) = run_ask(&args) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([780.0, 640.0])
            .with_min_inner_size([640.0, 440.0])
            .with_title("LLM Scout"),
        ..Default::default()
    };
    eframe::run_native("LLM Scout", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}
