//! Java 21 (Eclipse Temurin) telecharge une fois dans le dossier du launcher : rien a installer pour le joueur.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::{root_dir, Progress};
use crate::net::{download_one, Download};

pub struct Java {
    /// java(.exe) : outils en ligne de commande (installateur NeoForge).
    pub java: PathBuf,
    /// javaw.exe sous Windows (pas de console), java ailleurs : lancement du jeu.
    pub javaw: PathBuf,
}

fn binaries(home: &Path) -> Java {
    let bin = home.join("bin");
    if cfg!(windows) {
        Java { java: bin.join("java.exe"), javaw: bin.join("javaw.exe") }
    } else {
        Java { java: bin.join("java"), javaw: bin.join("java") }
    }
}

/// Dossier contenant bin/java : la racine ou le premier sous-dossier (les archives ont un dossier jdk-21...).
fn find_home(dir: &Path) -> Option<PathBuf> {
    if binaries(dir).java.exists() {
        return Some(dir.to_path_buf());
    }
    fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| binaries(p).java.exists())
}

pub fn ensure_java(major: u32, progress: &Progress) -> Result<Java> {
    let dir = root_dir().join("runtime").join(format!("java-{major}"));
    if let Some(home) = find_home(&dir) {
        return Ok(binaries(&home));
    }

    let os = if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "mac" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x64" };
    let url = format!("https://api.adoptium.net/v3/binary/latest/{major}/ga/{os}/{arch}/jre/hotspot/normal/eclipse");
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let archive = root_dir().join("runtime").join(format!("java-{major}.{ext}"));

    progress.stage(format!("Telechargement de Java {major}"), 1);
    download_one(&Download { url, path: archive.clone(), sha1: None, size: None, verify_existing: false })
        .context("telechargement de Java impossible")?;

    progress.stage(format!("Installation de Java {major}"), 1);
    let tmp = dir.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    if cfg!(windows) {
        let mut zip = zip::ZipArchive::new(File::open(&archive)?)?;
        zip.extract(&tmp)?;
    } else {
        let gz = flate2::read::GzDecoder::new(File::open(&archive)?);
        tar::Archive::new(gz).unpack(&tmp)?;
    }
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&tmp, &dir)?;
    let _ = fs::remove_file(&archive);
    progress.advance(1);

    match find_home(&dir) {
        Some(home) => Ok(binaries(&home)),
        None => bail!("Java {major} installe mais introuvable dans {}", dir.display()),
    }
}
