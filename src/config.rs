//! Chemins, reglages du joueur et etat partage entre l'interface et les taches de fond.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Fichier qui decrit le pack a jouer (versions, lien du .mrpack, serveur). Modifiable sans recompiler le launcher.
pub const MANIFEST_URL: &str = "https://raw.githubusercontent.com/MinecraftBetter/justbetter-launcher/main/pack.json";
pub const USER_AGENT: &str = concat!("JustBetterLauncher/", env!("CARGO_PKG_VERSION"));

/// Dossier racine : %APPDATA%\.justbetter sous Windows, ~/.local/share/justbetter sous Linux.
pub fn root_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        base.join(".justbetter")
    } else {
        base.join("justbetter")
    }
}

/// Dossier du jeu (mods, config, sauvegardes, captures).
pub fn instance_dir() -> PathBuf {
    root_dir().join("instance")
}

/// Adresse du manifeste : variable d'environnement JUSTBETTER_MANIFEST (tests) ou adresse par defaut.
pub fn manifest_url() -> String {
    std::env::var("JUSTBETTER_MANIFEST").unwrap_or_else(|_| MANIFEST_URL.to_string())
}

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredAccount {
    pub name: String,
    pub uuid: String,
    pub refresh_token: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub ram_mb: u32,
    #[serde(default)]
    pub account: Option<StoredAccount>,
    #[serde(default = "yes")]
    pub auto_connect: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings { ram_mb: 8192, account: None, auto_connect: true }
    }
}

impl Settings {
    fn path() -> PathBuf {
        root_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(root_dir());
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), json);
        }
    }
}

/// Avancement de la tache en cours, lu par l'interface a chaque image.
#[derive(Default, Clone)]
pub struct Status {
    pub busy: bool,
    pub label: String,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    pub info: Option<String>,
}

#[derive(Clone, Default)]
pub struct Progress(pub Arc<Mutex<Status>>);

impl Progress {
    pub fn stage(&self, label: impl Into<String>, total: u64) {
        let mut s = self.0.lock().unwrap();
        s.label = label.into();
        s.done = 0;
        s.total = total;
    }

    pub fn advance(&self, n: u64) {
        let mut s = self.0.lock().unwrap();
        s.done += n;
    }

    pub fn snapshot(&self) -> Status {
        self.0.lock().unwrap().clone()
    }

    pub fn start(&self) {
        let mut s = self.0.lock().unwrap();
        *s = Status { busy: true, ..Default::default() };
    }

    pub fn finish(&self, result: Result<String, String>) {
        let mut s = self.0.lock().unwrap();
        s.busy = false;
        s.label.clear();
        s.done = 0;
        s.total = 0;
        match result {
            Ok(msg) => s.info = Some(msg),
            Err(e) => s.error = Some(e),
        }
    }
}
