//! Telechargements : client HTTP commun, verification SHA-1, telechargement en parallele avec reprise sur erreur.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sha1::{Digest, Sha1};

use crate::config::{Progress, USER_AGENT};

pub fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("client HTTP")
    })
}

pub fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let resp = client().get(url).send().with_context(|| format!("connexion a {url}"))?;
    if !resp.status().is_success() {
        bail!("{url} : HTTP {}", resp.status());
    }
    resp.json().with_context(|| format!("reponse illisible de {url}"))
}

/// Un fichier a avoir sur le disque.
#[derive(Clone)]
pub struct Download {
    pub url: String,
    pub path: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    /// Recalculer le SHA-1 d'un fichier deja present (sinon seule la taille est verifiee).
    pub verify_existing: bool,
}

pub fn sha1_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Le fichier est-il deja present et correct ?
pub fn is_present(d: &Download) -> bool {
    let Ok(meta) = fs::metadata(&d.path) else { return false };
    if let Some(size) = d.size {
        if meta.len() != size {
            return false;
        }
    }
    match (&d.sha1, d.verify_existing) {
        (Some(expected), true) => sha1_file(&d.path).map(|h| h.eq_ignore_ascii_case(expected)).unwrap_or(false),
        _ => true,
    }
}

/// Telecharge un fichier dans un .part, verifie son SHA-1, puis le met en place.
pub fn download_one(d: &Download) -> Result<()> {
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(800 * attempt));
        }
        match try_download(d) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap())
}

fn try_download(d: &Download) -> Result<()> {
    if let Some(parent) = d.path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut resp = client().get(&d.url).send().with_context(|| format!("connexion a {}", d.url))?;
    if !resp.status().is_success() {
        bail!("{} : HTTP {}", d.url, resp.status());
    }
    let part = d.path.with_extension("part");
    let mut out = File::create(&part)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    drop(out);
    if let Some(expected) = &d.sha1 {
        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&part);
            bail!("fichier corrompu : {} (SHA-1 inattendu)", d.url);
        }
    }
    fs::rename(&part, &d.path)?;
    Ok(())
}

/// Verifie puis telecharge ce qui manque, avec 8 telechargements en parallele.
pub fn download_all(label: &str, list: Vec<Download>, progress: &Progress) -> Result<()> {
    progress.stage(format!("{label} : verification"), list.len() as u64);
    let missing: Vec<Download> = {
        let todo = Mutex::new(list.into_iter());
        let found = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| loop {
                    let next = todo.lock().unwrap().next();
                    let Some(d) = next else { break };
                    if !is_present(&d) {
                        found.lock().unwrap().push(d);
                    }
                    progress.advance(1);
                });
            }
        });
        found.into_inner().unwrap()
    };
    if missing.is_empty() {
        return Ok(());
    }

    progress.stage(format!("{label} : telechargement"), missing.len() as u64);
    let todo = Mutex::new(missing.into_iter());
    let error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| loop {
                if error.lock().unwrap().is_some() {
                    break;
                }
                let next = todo.lock().unwrap().next();
                let Some(d) = next else { break };
                if let Err(e) = download_one(&d) {
                    *error.lock().unwrap() = Some(e);
                    break;
                }
                progress.advance(1);
            });
        }
    });
    match error.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
