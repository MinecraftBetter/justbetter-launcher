//! Le pack : manifeste (pack.json) et synchronisation du dossier de jeu avec le .mrpack (format Modrinth).
//! Les mods sont telecharges et verifies (SHA-1) ; ceux retires du pack sont supprimes ; les configs du pack
//! ne sont reecrites qu'au changement de version (les reglages du joueur sont gardes entre deux lancements).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{instance_dir, manifest_url, root_dir, Progress};
use crate::net::{download_all, download_one, get_json, Download};

fn java21() -> u32 {
    21
}

#[derive(Deserialize, Serialize, Clone)]
pub struct PackManifest {
    pub name: String,
    pub version: String,
    pub minecraft: String,
    pub neoforge: String,
    #[serde(default = "java21")]
    pub java: u32,
    /// Lien du .mrpack de cette version.
    pub mrpack: String,
    /// Adresse du serveur (connexion directe au lancement).
    #[serde(default)]
    pub server: Option<String>,
    /// Quelques lignes d'actualite affichees dans le launcher.
    #[serde(default)]
    pub news: Vec<String>,
}

/// Lit le manifeste en ligne ; sans connexion, reprend la derniere copie connue.
pub fn fetch_manifest() -> Result<PackManifest> {
    let cache = root_dir().join("pack.json");
    match get_json::<PackManifest>(&manifest_url()) {
        Ok(m) => {
            let _ = fs::create_dir_all(root_dir());
            let _ = fs::write(&cache, serde_json::to_string_pretty(&m)?);
            Ok(m)
        }
        Err(e) => {
            let text = fs::read_to_string(&cache).map_err(|_| e)?;
            Ok(serde_json::from_str(&text)?)
        }
    }
}

#[derive(Deserialize)]
struct Index {
    files: Vec<IndexFile>,
}

#[derive(Deserialize)]
struct IndexFile {
    path: String,
    hashes: Hashes,
    #[serde(default)]
    env: Option<Env>,
    downloads: Vec<String>,
    #[serde(rename = "fileSize", default)]
    file_size: Option<u64>,
}

#[derive(Deserialize)]
struct Hashes {
    sha1: String,
}

#[derive(Deserialize)]
struct Env {
    #[serde(default)]
    client: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct PackState {
    version: String,
    managed: Vec<String>,
}

/// Chemin relatif sur (pas de "..", pas de chemin absolu ni de lecteur).
fn safe_rel(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

pub fn sync(manifest: &PackManifest, progress: &Progress) -> Result<()> {
    let inst = instance_dir();
    fs::create_dir_all(&inst)?;
    let state_path = inst.join(".justbetter-pack.json");
    let state: PackState = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // 1. le .mrpack de cette version
    let archive = root_dir()
        .join("cache")
        .join("packs")
        .join(format!("{}.mrpack", manifest.version.replace(['/', '\\', ':'], "_")));
    if !archive.exists() {
        progress.stage(format!("Telechargement du pack {}", manifest.version), 1);
        download_one(&Download {
            url: manifest.mrpack.clone(),
            path: archive.clone(),
            sha1: None,
            size: None,
            verify_existing: false,
        })
        .context("telechargement du pack impossible")?;
    }
    let mut zip = zip::ZipArchive::new(File::open(&archive)?).context("pack illisible")?;
    let index: Index = {
        let entry = zip.by_name("modrinth.index.json").context("modrinth.index.json absent du pack")?;
        serde_json::from_reader(entry).context("modrinth.index.json illisible")?
    };

    // 2. les mods et fichiers references
    let mut managed: Vec<String> = Vec::new();
    let mut downloads = Vec::new();
    for f in &index.files {
        if f.env.as_ref().and_then(|e| e.client.as_deref()) == Some("unsupported") {
            continue;
        }
        let Some(rel) = safe_rel(&f.path) else { bail!("chemin refuse dans le pack : {}", f.path) };
        let Some(url) = f.downloads.first() else { continue };
        downloads.push(Download {
            url: url.clone(),
            path: inst.join(rel),
            sha1: Some(f.hashes.sha1.clone()),
            size: f.file_size,
            verify_existing: true,
        });
        managed.push(f.path.clone());
    }
    download_all("Mods", downloads, progress)?;

    // 3. fichiers inclus : les mods a chaque fois, le reste seulement a la mise a jour du pack
    let fresh = state.version != manifest.version;
    progress.stage("Configuration du pack", zip.len() as u64);
    for i in 0..zip.len() {
        progress.advance(1);
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let Some(rel) = name.strip_prefix("overrides/").or_else(|| name.strip_prefix("client-overrides/")) else {
            continue;
        };
        let Some(rel_path) = safe_rel(rel) else { continue };
        let is_mod = rel.starts_with("mods/");
        if !is_mod && !fresh {
            continue;
        }
        let target = inst.join(&rel_path);
        if rel == "options.txt" && target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)?;
        io::copy(&mut entry, &mut out)?;
        if is_mod {
            managed.push(rel.to_string());
        }
    }

    // 4. mods retires du pack
    let keep: HashSet<&String> = managed.iter().collect();
    for old in &state.managed {
        if !keep.contains(old) {
            if let Some(rel) = safe_rel(old) {
                let _ = fs::remove_file(inst.join(rel));
            }
        }
    }

    let new_state = PackState { version: manifest.version.clone(), managed };
    fs::write(&state_path, serde_json::to_string_pretty(&new_state)?)?;
    Ok(())
}
