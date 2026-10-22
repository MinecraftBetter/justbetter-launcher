#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! JustBetter Launcher : connexion Microsoft, installation de Java / NeoForge / du pack, et lancement du jeu.

mod auth;
mod config;
mod game;
mod java;
mod net;
mod pack;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use eframe::egui::{self, Color32, RichText};

use config::{instance_dir, Progress, Settings, StoredAccount};
use pack::PackManifest;

const BG: Color32 = Color32::from_rgb(0x1C, 0x1A, 0x18);
const PANEL: Color32 = Color32::from_rgb(0x23, 0x20, 0x1D);
const BORDER: Color32 = Color32::from_rgb(0x4E, 0x44, 0x3A);
const ACCENT: Color32 = Color32::from_rgb(0xE9, 0xB5, 0x3D);
const ACCENT_DARK: Color32 = Color32::from_rgb(0x2B, 0x1A, 0x06);
const TEXT: Color32 = Color32::from_rgb(0xED, 0xE6, 0xDA);
const MUTED: Color32 = Color32::from_rgb(0xA3, 0x9A, 0x8D);
const BAD: Color32 = Color32::from_rgb(0xE8, 0x72, 0x7A);
const GOOD: Color32 = Color32::from_rgb(0x7B, 0xD8, 0x8F);

const ICON: &[u8] = include_bytes!("../assets/icon.png");

enum Login {
    Idle,
    Working,
    Waiting(auth::DeviceCode),
    Error(String),
}

struct Shared {
    settings: Settings,
    login: Login,
    manifest: Option<Result<PackManifest, String>>,
}

struct App {
    shared: Arc<Mutex<Shared>>,
    progress: Progress,
    cancel_login: Arc<AtomicBool>,
    logo: Option<egui::TextureHandle>,
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(ACCENT).strong().size(15.0));
    ui.add_space(4.0);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.override_text_color = Some(TEXT);
        visuals.selection.bg_fill = ACCENT;
        visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x65, 0x45, 0x29);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x7D, 0x56, 0x34);
        visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x8A, 0x64, 0x40);
        cc.egui_ctx.set_visuals(visuals);

        let logo = image::load_from_memory(ICON).ok().map(|img| {
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                Default::default(),
            )
        });

        let app = App {
            shared: Arc::new(Mutex::new(Shared { settings: Settings::load(), login: Login::Idle, manifest: None })),
            progress: Progress::default(),
            cancel_login: Arc::new(AtomicBool::new(false)),
            logo,
        };
        let shared = app.shared.clone();
        let ctx = cc.egui_ctx.clone();
        thread::spawn(move || {
            let m = pack::fetch_manifest().map_err(|e| format!("{e:#}"));
            shared.lock().unwrap().manifest = Some(m);
            ctx.request_repaint();
        });
        app
    }

    fn start_login(&self, ctx: &egui::Context) {
        self.cancel_login.store(false, Ordering::Relaxed);
        self.shared.lock().unwrap().login = Login::Working;
        let shared = self.shared.clone();
        let cancel = self.cancel_login.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<auth::Session> {
                let dc = auth::start_device_code()?;
                shared.lock().unwrap().login = Login::Waiting(dc.clone());
                ctx.request_repaint();
                let tokens = auth::wait_for_login(&dc, &cancel)?;
                shared.lock().unwrap().login = Login::Working;
                ctx.request_repaint();
                auth::minecraft_login(tokens)
            })();
            let mut s = shared.lock().unwrap();
            match result {
                Ok(session) => {
                    s.settings.account = Some(StoredAccount {
                        name: session.name,
                        uuid: session.uuid,
                        refresh_token: session.refresh_token,
                    });
                    s.settings.save();
                    s.login = Login::Idle;
                }
                Err(e) => s.login = Login::Error(format!("{e:#}")),
            }
            ctx.request_repaint();
        });
    }

    fn play(&self, ctx: &egui::Context) {
        self.progress.start();
        let shared = self.shared.clone();
        let progress = self.progress.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = (|| -> anyhow::Result<String> {
                let (account, ram_mb, auto_connect) = {
                    let s = shared.lock().unwrap();
                    (s.settings.account.clone(), s.settings.ram_mb, s.settings.auto_connect)
                };
                let account = account.context("connecte-toi d'abord")?;

                progress.stage("Connexion au compte", 1);
                let session = auth::minecraft_login(auth::refresh(&account.refresh_token)?)?;
                {
                    let mut s = shared.lock().unwrap();
                    s.settings.account = Some(StoredAccount {
                        name: session.name.clone(),
                        uuid: session.uuid.clone(),
                        refresh_token: session.refresh_token.clone(),
                    });
                    s.settings.save();
                }

                progress.stage("Lecture du pack", 1);
                let manifest = pack::fetch_manifest()?;
                shared.lock().unwrap().manifest = Some(Ok(manifest.clone()));

                let java = java::ensure_java(manifest.java, &progress)?;
                game::ensure_neoforge(&java, &manifest.minecraft, &manifest.neoforge, &progress)?;
                pack::sync(&manifest, &progress)?;
                let opts = game::LaunchOptions {
                    ram_mb,
                    server: if auto_connect { manifest.server.clone() } else { None },
                };
                game::launch(&java, &manifest.minecraft, &manifest.neoforge, &session, &opts, &progress)?;
                Ok("Jeu lance, bon jeu !".to_string())
            })();
            progress.finish(result.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
    }

    fn account_card(&mut self, ui: &mut egui::Ui, busy: bool) {
        card(ui, |ui| {
            title(ui, "Compte Minecraft");
            let mut s = self.shared.lock().unwrap();
            if let Some(account) = s.settings.account.clone() {
                ui.label(RichText::new(format!("Connecte : {}", account.name)).color(GOOD).size(15.0));
                ui.add_space(4.0);
                if ui.add_enabled(!busy, egui::Button::new("Se deconnecter")).clicked() {
                    s.settings.account = None;
                    s.settings.save();
                }
                return;
            }
            match &s.login {
                Login::Idle => {
                    ui.label(RichText::new("Connecte-toi avec ton compte Microsoft.").color(MUTED));
                    ui.add_space(4.0);
                    if ui.button("Se connecter avec Microsoft").clicked() {
                        drop(s);
                        self.start_login(ui.ctx());
                    }
                }
                Login::Working => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connexion en cours...");
                    });
                }
                Login::Waiting(dc) => {
                    let dc = dc.clone();
                    ui.label(format!("1. Ouvre {}", dc.verification_uri));
                    ui.label("2. Entre ce code :");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&dc.user_code).color(ACCENT).strong().size(24.0).monospace());
                        if ui.button("Copier").clicked() {
                            ui.ctx().copy_text(dc.user_code.clone());
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Ouvrir la page").clicked() {
                            let _ = open::that(&dc.verification_uri);
                        }
                        if ui.button("Annuler").clicked() {
                            self.cancel_login.store(true, Ordering::Relaxed);
                        }
                    });
                    ui.label(RichText::new("Le launcher continue tout seul une fois le code valide.").color(MUTED).small());
                }
                Login::Error(e) => {
                    ui.label(RichText::new(e.as_str()).color(BAD));
                    ui.add_space(4.0);
                    if ui.button("Reessayer").clicked() {
                        drop(s);
                        self.start_login(ui.ctx());
                    }
                }
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let status = self.progress.snapshot();
        let waiting_login = matches!(self.shared.lock().unwrap().login, Login::Waiting(_) | Login::Working);
        if status.busy || waiting_login {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(BG).inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                // en-tete
                let manifest = self.shared.lock().unwrap().manifest.clone();
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(egui::Image::new((logo.id(), egui::vec2(64.0, 64.0))));
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new("JustBetter").color(ACCENT).strong().size(30.0));
                        let sub = match &manifest {
                            Some(Ok(m)) => format!("{}  -  version {}", m.name, m.version),
                            Some(Err(_)) => "Pack indisponible".to_string(),
                            None => "Chargement...".to_string(),
                        };
                        ui.label(RichText::new(sub).color(MUTED));
                    });
                });
                ui.add_space(12.0);

                ui.columns(2, |cols| {
                    // gauche : actualites et serveur
                    card(&mut cols[0], |ui| {
                        title(ui, "Actualites");
                        match &manifest {
                            Some(Ok(m)) => {
                                if m.news.is_empty() {
                                    ui.label(RichText::new("Rien de neuf pour le moment.").color(MUTED));
                                }
                                for line in &m.news {
                                    ui.label(format!("- {line}"));
                                }
                                if let Some(server) = &m.server {
                                    ui.add_space(8.0);
                                    ui.label(RichText::new(format!("Serveur : {server}")).color(MUTED));
                                }
                                ui.label(RichText::new(format!("Minecraft {}  -  NeoForge {}", m.minecraft, m.neoforge)).color(MUTED));
                            }
                            Some(Err(e)) => {
                                ui.label(RichText::new(format!("Impossible de lire le pack : {e}")).color(BAD));
                            }
                            None => {
                                ui.spinner();
                            }
                        }
                    });

                    // droite : compte, memoire, jouer
                    let busy = status.busy;
                    self.account_card(&mut cols[1], busy);
                    cols[1].add_space(10.0);
                    card(&mut cols[1], |ui| {
                        title(ui, "Reglages");
                        let mut s = self.shared.lock().unwrap();
                        let mut gb = (s.settings.ram_mb / 1024).clamp(4, 16);
                        let changed_ram = ui
                            .add_enabled(!busy, egui::Slider::new(&mut gb, 4..=16).text("Go de memoire"))
                            .changed();
                        let changed_auto = ui
                            .add_enabled(!busy, egui::Checkbox::new(&mut s.settings.auto_connect, "Rejoindre le serveur directement"))
                            .changed();
                        if changed_ram {
                            s.settings.ram_mb = gb * 1024;
                        }
                        if changed_ram || changed_auto {
                            s.settings.save();
                        }
                    });
                    cols[1].add_space(10.0);
                    let can_play = !busy
                        && self.shared.lock().unwrap().settings.account.is_some()
                        && matches!(manifest, Some(Ok(_)));
                    let button = egui::Button::new(RichText::new("JOUER").size(24.0).strong().color(if can_play { ACCENT_DARK } else { MUTED }))
                        .fill(if can_play { ACCENT } else { PANEL })
                        .min_size(egui::vec2(cols[1].available_width(), 56.0));
                    if cols[1].add_enabled(can_play, button).clicked() {
                        self.play(ctx);
                    }
                });

                // bas : progression, message, dossier
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Ouvrir le dossier du jeu").clicked() {
                            let dir = instance_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = open::that(dir);
                        }
                        ui.label(RichText::new(format!("Launcher {}", env!("CARGO_PKG_VERSION"))).color(MUTED).small());
                    });
                    ui.add_space(6.0);
                    if status.busy {
                        let fraction = if status.total > 0 { status.done as f32 / status.total as f32 } else { 0.0 };
                        let text = if status.total > 1 {
                            format!("{} ({}/{})", status.label, status.done, status.total)
                        } else {
                            status.label.clone()
                        };
                        ui.add(egui::ProgressBar::new(fraction).text(text).fill(ACCENT).desired_height(22.0));
                    } else if let Some(err) = &status.error {
                        ui.label(RichText::new(err).color(BAD));
                    } else if let Some(info) = &status.info {
                        ui.label(RichText::new(info).color(GOOD));
                    }
                });
            });
    }
}

/// Mode test (developpement) : tout le parcours sans connexion Microsoft, compte hors ligne, journal en console.
fn test_offline(name: &str) -> anyhow::Result<()> {
    let progress = Progress::default();
    let watcher = progress.clone();
    thread::spawn(move || {
        let mut last = String::new();
        loop {
            let s = watcher.snapshot();
            let line = format!("{} {}/{}", s.label, s.done, s.total);
            if line != last {
                println!("{line}");
                last = line;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
    let manifest = pack::fetch_manifest()?;
    println!("pack {} {}", manifest.name, manifest.version);
    let java = java::ensure_java(manifest.java, &progress)?;
    game::ensure_neoforge(&java, &manifest.minecraft, &manifest.neoforge, &progress)?;
    pack::sync(&manifest, &progress)?;
    let session = auth::Session {
        name: name.to_string(),
        uuid: "00000000000000000000000000000000".to_string(),
        access_token: "0".to_string(),
        refresh_token: String::new(),
    };
    let opts = game::LaunchOptions { ram_mb: Settings::load().ram_mb, server: None };
    game::launch(&java, &manifest.minecraft, &manifest.neoforge, &session, &opts, &progress)?;
    println!("jeu lance");
    Ok(())
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--test-offline") {
        if let Err(e) = test_offline(args.get(2).map(String::as_str).unwrap_or("Testeur")) {
            eprintln!("ERREUR : {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("JustBetter Launcher")
        .with_inner_size([900.0, 560.0])
        .with_min_inner_size([760.0, 500.0]);
    if let Ok(img) = image::load_from_memory(ICON) {
        let rgba = img.to_rgba8();
        let (width, height) = (rgba.width(), rgba.height());
        viewport = viewport.with_icon(egui::IconData { rgba: rgba.into_raw(), width, height });
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("JustBetter Launcher", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
