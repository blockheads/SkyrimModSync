//! The players' window: the same steps as the double-click text flow (setup, pack code, what is out of
//! sync, enable, disable), each run on a worker thread. Hosts use the command line.
use crate::{check_pack, describe, enable, game, setup, size};
use eframe::egui;
use skyrim_mod_sync::{
    config::{self, Settings},
    deploy, launch, steam, Result,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// What the worker threads report back.
#[derive(Default)]
struct Shared {
    busy: Option<&'static str>,
    /// What is out of sync, as the CLI lists it.
    list: String,
    /// The enable button's label once a check found something (or nothing) to get.
    enable: Option<String>,
    log: Vec<String>,
    error: Option<String>,
    status: String,
    /// A pack is in the game (Disable has something to do).
    enabled: bool,
}

struct App {
    game: String,
    store: String,
    key: String,
    key_saved: bool,
    code: String,
    shared: Arc<Mutex<Shared>>,
}

pub fn run() -> Result<serde_json::Value> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SkyrimModSync")
            .with_inner_size([640.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "SkyrimModSync",
        options,
        Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))),
    )
    .map_err(|e| skyrim_mod_sync::Error::new("gui", format!("The window could not open ({e}).")))?;
    Ok(serde_json::json!({"state": "closed"}))
}

fn is_enabled(s: &Settings) -> bool {
    game(s).is_ok_and(|g| g.state() != deploy::State::Clean)
}

/// Enabled or not, in a sentence.
fn status_text(s: &Settings) -> String {
    match game(s).map(|g| g.state()) {
        Ok(deploy::State::Loaded(name)) => {
            format!("Enabled: {name}. Start Skyrim from Steam; Disable puts your game back.")
        }
        Ok(deploy::State::Interrupted) => {
            "An enable was cut short: the next step undoes it.".into()
        }
        Ok(deploy::State::Clean) => "Not enabled: your game is as it was.".into(),
        Err(_) => "Not set up yet.".into(),
    }
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let s = Settings::load().unwrap_or_default();
        let game = s.game.clone().or_else(crate::beside_game).or_else(|| {
            let found = steam::installs(&steam::steam_roots());
            (found.len() == 1).then(|| found[0].game.clone())
        });
        let store = s.store.clone().or_else(|| {
            let g = game.as_ref()?;
            Some(
                steam::library_of(g)
                    .or_else(|| g.parent().map(PathBuf::from))?
                    .join("SkyrimModSync"),
            )
        });
        let code = s
            .store
            .as_ref()
            .and_then(|st| std::fs::read_to_string(st.join("source.txt")).ok())
            .map(|c| c.trim_start_matches("gist:").to_owned())
            .unwrap_or_default();
        let shared = Arc::new(Mutex::new(Shared {
            status: status_text(&s),
            enabled: is_enabled(&s),
            ..Default::default()
        }));
        let (sink, repaint) = (shared.clone(), ctx.clone());
        skyrim_mod_sync::set_event_sink(move |line| {
            let mut sh = sink.lock().unwrap();
            sh.log.push(line.to_owned());
            let excess = sh.log.len().saturating_sub(200);
            sh.log.drain(..excess);
            repaint.request_repaint();
        });
        let show = |p: Option<PathBuf>| p.map(|p| p.display().to_string()).unwrap_or_default();
        Self {
            game: show(game),
            store: show(store),
            key: String::new(),
            key_saved: config::nexus_key().ok().flatten().is_some(),
            code,
            shared,
        }
    }

    /// Saves the folders and key (setup), then runs `work` on a thread.
    fn start(
        &self,
        ctx: &egui::Context,
        busy: &'static str,
        work: impl FnOnce(&Settings, &Arc<Mutex<Shared>>) -> Result<()> + Send + 'static,
    ) {
        {
            let mut sh = self.shared.lock().unwrap();
            sh.busy = Some(busy);
            sh.error = None;
            sh.log.clear();
        }
        let path = |s: &str| Some(s.trim()).filter(|s| !s.is_empty()).map(PathBuf::from);
        let (game, store, key) = (
            path(&self.game),
            path(&self.store),
            self.key.trim().to_owned(),
        );
        let (shared, ctx) = (self.shared.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                if !key.is_empty() {
                    config::save_key(&key)?;
                }
                // Keep the settings folders already found (setup finds them only for a new game).
                let saved = Settings::load()?;
                let same = game.is_none() || game == saved.game;
                let (local, documents) = if same {
                    (saved.local, saved.documents)
                } else {
                    (None, None)
                };
                setup(game, store, local, documents, false, false)?;
                let s = Settings::load()?;
                work(&s, &shared)?;
                let mut sh = shared.lock().unwrap();
                sh.status = status_text(&s);
                sh.enabled = is_enabled(&s);
                Ok(())
            })();
            let mut sh = shared.lock().unwrap();
            sh.busy = None;
            if let Err(e) = result {
                sh.error = Some(e.message);
            }
            ctx.request_repaint();
        });
    }

    fn check(&self, ctx: &egui::Context) {
        let code = self.code.trim().to_owned();
        self.start(ctx, "Checking…", move |s, shared| {
            let c = check_pack(Some(code), None, s)?;
            let mut sh = shared.lock().unwrap();
            sh.list = describe(&c);
            sh.enable = Some(if c.needs_download() {
                format!("Download about {} and enable", size(c.download_bytes))
            } else {
                "Enable".into()
            });
            Ok(())
        });
    }

    fn enable(&self, ctx: &egui::Context) {
        let code = self.code.trim().to_owned();
        self.start(ctx, "Downloading and enabling…", move |s, shared| {
            // The person saw the list and pressed the button: that is the yes.
            enable(Some(code), None, s, &|_| Ok(true))?;
            let mut sh = shared.lock().unwrap();
            sh.list.clear();
            sh.enable = None;
            Ok(())
        });
    }

    fn disable(&self, ctx: &egui::Context) {
        self.start(ctx, "Disabling…", |s, shared| {
            let g = game(s)?;
            launch::refuse_if_running()?;
            deploy::unload(&g)?;
            shared.lock().unwrap().enable = None;
            Ok(())
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (busy, list, enable_label, log, error, status, enabled) = {
            let sh = self.shared.lock().unwrap();
            (
                sh.busy,
                sh.list.clone(),
                sh.enable.clone(),
                sh.log.clone(),
                sh.error.clone(),
                sh.status.clone(),
                sh.enabled,
            )
        };
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SkyrimModSync");
            ui.add_space(6.0);
            let before = self.code.clone();
            egui::Grid::new("fields")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    fn wide(text: &mut String) -> egui::TextEdit<'_> {
                        egui::TextEdit::singleline(text).desired_width(f32::INFINITY)
                    }
                    ui.label("Skyrim folder");
                    ui.add(wide(&mut self.game).hint_text("the folder with SkyrimSE.exe"));
                    ui.end_row();
                    ui.label("Mods folder");
                    ui.add(wide(&mut self.store).hint_text("on the same drive as Skyrim"));
                    ui.end_row();
                    ui.label("Nexus API key");
                    let hint = if self.key_saved {
                        "saved (type to replace)"
                    } else {
                        "optional: Premium downloads from Nexus"
                    };
                    ui.add(wide(&mut self.key).password(true).hint_text(hint));
                    ui.end_row();
                    ui.label("Pack code");
                    ui.add(wide(&mut self.code).hint_text("paste the code your host gave you"));
                    ui.end_row();
                });
            if self.code != before {
                // Another pack: check it again before enabling.
                let mut sh = self.shared.lock().unwrap();
                sh.enable = None;
                sh.list.clear();
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let idle = busy.is_none();
                let has_code = !self.code.trim().is_empty();
                if ui
                    .add_enabled(idle && has_code, egui::Button::new("Check"))
                    .clicked()
                {
                    self.check(ctx);
                }
                if let Some(label) = &enable_label {
                    if ui.add_enabled(idle, egui::Button::new(label)).clicked() {
                        self.enable(ctx);
                    }
                }
                if ui
                    .add_enabled(idle && enabled, egui::Button::new("Disable"))
                    .clicked()
                {
                    self.disable(ctx);
                }
                if let Some(text) = busy {
                    ui.spinner();
                    ui.label(text);
                }
            });
            ui.add_space(4.0);
            ui.label(&status);
            if let Some(e) = &error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if list.is_empty() && log.is_empty() {
                        ui.weak("Check lists what your game lacks for the pack before anything downloads.");
                    }
                    if !list.is_empty() {
                        ui.monospace(&list);
                    }
                    for line in &log {
                        ui.monospace(line);
                    }
                });
        });
    }
}
