//! Installation et lancement de Minecraft + NeoForge.
//! L'installateur officiel de NeoForge prepare les versions et bibliotheques ; ensuite on lit les deux fichiers de
//! version (vanilla + NeoForge), on complete les telechargements et on construit la ligne de commande.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::auth::Session;
use crate::config::{instance_dir, root_dir, Progress};
use crate::java::Java;
use crate::net::{download_all, download_one, Download};

fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).with_context(|| format!("lecture de {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} illisible", path.display()))
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

/// Installe NeoForge (et la version vanilla) avec l'installateur officiel, une seule fois.
pub fn ensure_neoforge(java: &Java, mc: &str, neo: &str, progress: &Progress) -> Result<()> {
    let root = root_dir();
    let id = format!("neoforge-{neo}");
    let neo_json = root.join("versions").join(&id).join(format!("{id}.json"));
    let mc_json = root.join("versions").join(mc).join(format!("{mc}.json"));
    if neo_json.exists() && mc_json.exists() {
        return Ok(());
    }
    fs::create_dir_all(root.join("logs"))?;
    let profiles = root.join("launcher_profiles.json");
    if !profiles.exists() {
        fs::write(&profiles, "{\"profiles\":{}}")?;
    }
    let cache = root.join("cache");
    let installer = cache.join(format!("neoforge-{neo}-installer.jar"));
    progress.stage("Telechargement de NeoForge", 1);
    download_one(&Download {
        url: format!("https://maven.neoforged.net/releases/net/neoforged/neoforge/{neo}/neoforge-{neo}-installer.jar"),
        path: installer.clone(),
        sha1: None,
        size: None,
        verify_existing: false,
    })?;

    progress.stage("Installation de NeoForge (1 a 3 minutes)", 1);
    let log_path = root.join("logs").join("neoforge-installer.log");
    let log = File::create(&log_path)?;
    let mut cmd = Command::new(&java.java);
    cmd.arg("-jar")
        .arg(&installer)
        .arg("--install-client")
        .arg(&root)
        .current_dir(&cache)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    hide_console(&mut cmd);
    let status = cmd.status().context("impossible de lancer l'installateur NeoForge")?;
    if !status.success() || !neo_json.exists() {
        bail!("Installation de NeoForge echouee (journal : {})", log_path.display());
    }
    progress.advance(1);
    Ok(())
}

// ---- regles des fichiers de version -----------------------------------------------------------------------------

fn os_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn rule_matches(rule: &Value, features: &HashMap<&str, bool>) -> bool {
    if let Some(os) = rule.get("os") {
        if let Some(name) = os.get("name").and_then(Value::as_str) {
            if name != os_name() {
                return false;
            }
        }
        if let Some(arch) = os.get("arch").and_then(Value::as_str) {
            let ours = if cfg!(target_arch = "x86") { "x86" } else if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
            if arch != ours {
                return false;
            }
        }
    }
    if let Some(wanted) = rule.get("features").and_then(Value::as_object) {
        for (k, v) in wanted {
            if features.get(k.as_str()).copied().unwrap_or(false) != v.as_bool().unwrap_or(false) {
                return false;
            }
        }
    }
    true
}

fn allowed(rules: Option<&Value>, features: &HashMap<&str, bool>) -> bool {
    let Some(rules) = rules.and_then(Value::as_array) else { return true };
    let mut ok = false;
    for rule in rules {
        if rule_matches(rule, features) {
            ok = rule.get("action").and_then(Value::as_str) == Some("allow");
        }
    }
    ok
}

/// "groupe:artefact:version[:classifier][@ext]" -> chemin dans le depot maven.
fn maven_path(name: &str) -> Option<String> {
    let (coords, ext) = match name.split_once('@') {
        Some((c, e)) => (c, e),
        None => (name, "jar"),
    };
    let parts: Vec<&str> = coords.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let (group, artifact, version) = (parts[0].replace('.', "/"), parts[1], parts[2]);
    let file = match parts.get(3) {
        Some(classifier) => format!("{artifact}-{version}-{classifier}.{ext}"),
        None => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!("{group}/{artifact}/{version}/{file}"))
}

fn library_key(name: &str) -> String {
    let parts: Vec<&str> = name.split('@').next().unwrap_or(name).split(':').collect();
    match parts.len() {
        0..=2 => name.to_string(),
        3 => format!("{}:{}", parts[0], parts[1]),
        _ => format!("{}:{}:{}", parts[0], parts[1], parts[3]),
    }
}

fn collect_args(list: Option<&Value>, features: &HashMap<&str, bool>, out: &mut Vec<String>) {
    let Some(items) = list.and_then(Value::as_array) else { return };
    for item in items {
        match item {
            Value::String(s) => out.push(s.clone()),
            Value::Object(_) => {
                if !allowed(item.get("rules"), features) {
                    continue;
                }
                match item.get("value") {
                    Some(Value::String(s)) => out.push(s.clone()),
                    Some(Value::Array(values)) => out.extend(values.iter().filter_map(Value::as_str).map(str::to_string)),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn substitute(arg: &str, vars: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match vars.get(key) {
                    Some(v) => out.push_str(v),
                    None => out.push_str(&rest[start..start + 2 + end + 1]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

pub struct LaunchOptions {
    pub ram_mb: u32,
    pub server: Option<String>,
}

/// Complete les fichiers manquants puis lance le jeu. Rend la main une fois le jeu demarre.
pub fn launch(java: &Java, mc: &str, neo: &str, session: &Session, opts: &LaunchOptions, progress: &Progress) -> Result<()> {
    let root = root_dir();
    let inst = instance_dir();
    let id = format!("neoforge-{neo}");
    let libdir = root.join("libraries");
    let vanilla = read_json(&root.join("versions").join(mc).join(format!("{mc}.json")))?;
    let neoforge = read_json(&root.join("versions").join(&id).join(format!("{id}.json")))?;

    let mut features: HashMap<&str, bool> = HashMap::new();
    features.insert("is_quick_play_multiplayer", opts.server.is_some());

    // bibliotheques : NeoForge d'abord (prioritaire), puis vanilla
    let mut seen = HashSet::new();
    let mut classpath: Vec<PathBuf> = Vec::new();
    let mut files: Vec<Download> = Vec::new();
    for version in [&neoforge, &vanilla] {
        for lib in version.get("libraries").and_then(Value::as_array).into_iter().flatten() {
            if !allowed(lib.get("rules"), &features) {
                continue;
            }
            let Some(name) = lib.get("name").and_then(Value::as_str) else { continue };
            if !seen.insert(library_key(name)) {
                continue;
            }
            let artifact = lib.pointer("/downloads/artifact");
            let rel = artifact
                .and_then(|a| a.get("path"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| maven_path(name));
            let Some(rel) = rel else { continue };
            let path = libdir.join(&rel);
            let url = artifact.and_then(|a| a.get("url")).and_then(Value::as_str).unwrap_or("");
            if !url.is_empty() {
                files.push(Download {
                    url: url.to_string(),
                    path: path.clone(),
                    sha1: artifact.and_then(|a| a.get("sha1")).and_then(Value::as_str).map(str::to_string),
                    size: artifact.and_then(|a| a.get("size")).and_then(Value::as_u64),
                    verify_existing: false,
                });
            }
            classpath.push(path);
        }
    }

    // jar du jeu : celui de la version vanilla, copie sous le nom de la version NeoForge (comme le launcher officiel)
    let client = vanilla.pointer("/downloads/client").context("jar du jeu absent du fichier de version")?;
    let mc_jar = root.join("versions").join(mc).join(format!("{mc}.jar"));
    files.push(Download {
        url: client.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
        path: mc_jar.clone(),
        sha1: client.get("sha1").and_then(Value::as_str).map(str::to_string),
        size: client.get("size").and_then(Value::as_u64),
        verify_existing: false,
    });
    download_all("Bibliotheques", files, progress)?;
    let version_jar = root.join("versions").join(&id).join(format!("{id}.jar"));
    let need_copy = match (fs::metadata(&version_jar), fs::metadata(&mc_jar)) {
        (Ok(a), Ok(b)) => a.len() != b.len(),
        _ => true,
    };
    if need_copy {
        fs::copy(&mc_jar, &version_jar).context("copie du jar du jeu")?;
    }
    classpath.push(version_jar);
    for path in &classpath {
        if !path.exists() {
            bail!("Fichier manquant : {} (reinstalle NeoForge en supprimant le dossier versions)", path.display());
        }
    }

    // ressources (sons, langues, textures vanilla)
    let asset_index = vanilla.get("assetIndex").context("assetIndex absent")?;
    let index_id = asset_index.get("id").and_then(Value::as_str).unwrap_or(mc).to_string();
    let index_path = root.join("assets").join("indexes").join(format!("{index_id}.json"));
    let index_dl = Download {
        url: asset_index.get("url").and_then(Value::as_str).unwrap_or_default().to_string(),
        path: index_path.clone(),
        sha1: asset_index.get("sha1").and_then(Value::as_str).map(str::to_string),
        size: asset_index.get("size").and_then(Value::as_u64),
        verify_existing: true,
    };
    if !crate::net::is_present(&index_dl) {
        download_one(&index_dl)?;
    }
    let index = read_json(&index_path)?;
    let mut assets = Vec::new();
    if let Some(objects) = index.get("objects").and_then(Value::as_object) {
        for obj in objects.values() {
            let Some(hash) = obj.get("hash").and_then(Value::as_str) else { continue };
            if hash.len() < 2 {
                continue;
            }
            assets.push(Download {
                url: format!("https://resources.download.minecraft.net/{}/{hash}", &hash[..2]),
                path: root.join("assets").join("objects").join(&hash[..2]).join(hash),
                sha1: Some(hash.to_string()),
                size: obj.get("size").and_then(Value::as_u64),
                verify_existing: false,
            });
        }
    }
    download_all("Ressources", assets, progress)?;

    // ligne de commande
    progress.stage("Lancement du jeu", 1);
    let natives = root.join("natives").join(&id);
    fs::create_dir_all(&natives)?;
    fs::create_dir_all(&inst)?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = classpath.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(sep);
    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", session.name.clone());
    vars.insert("version_name", id.clone());
    vars.insert("game_directory", inst.to_string_lossy().into_owned());
    vars.insert("assets_root", root.join("assets").to_string_lossy().into_owned());
    vars.insert("assets_index_name", index_id);
    vars.insert("auth_uuid", session.uuid.clone());
    vars.insert("auth_access_token", session.access_token.clone());
    vars.insert("clientid", String::new());
    vars.insert("auth_xuid", "0".to_string());
    vars.insert("user_type", "msa".to_string());
    vars.insert("version_type", "release".to_string());
    vars.insert("natives_directory", natives.to_string_lossy().into_owned());
    vars.insert("launcher_name", "JustBetter".to_string());
    vars.insert("launcher_version", env!("CARGO_PKG_VERSION").to_string());
    vars.insert("classpath", cp);
    vars.insert("classpath_separator", sep.to_string());
    vars.insert("library_directory", libdir.to_string_lossy().into_owned());
    vars.insert("quickPlayMultiplayer", opts.server.clone().unwrap_or_default());

    let mut jvm = vec![
        format!("-Xmx{}M", opts.ram_mb),
        "-Xms1024M".to_string(),
        "-XX:+UseG1GC".to_string(),
        "-XX:+ParallelRefProcEnabled".to_string(),
        "-XX:MaxGCPauseMillis=200".to_string(),
    ];
    collect_args(vanilla.pointer("/arguments/jvm"), &features, &mut jvm);
    collect_args(neoforge.pointer("/arguments/jvm"), &features, &mut jvm);
    let mut game = Vec::new();
    collect_args(vanilla.pointer("/arguments/game"), &features, &mut game);
    collect_args(neoforge.pointer("/arguments/game"), &features, &mut game);
    let main_class = neoforge
        .get("mainClass")
        .or_else(|| vanilla.get("mainClass"))
        .and_then(Value::as_str)
        .context("classe principale absente")?;

    let mut args: Vec<String> = jvm.iter().map(|a| substitute(a, &vars)).collect();
    args.push(main_class.to_string());
    args.extend(game.iter().map(|a| substitute(a, &vars)));

    fs::create_dir_all(inst.join("logs"))?;
    let log_path = inst.join("logs").join("launcher-game.log");
    let log = File::create(&log_path)?;
    let mut cmd = Command::new(&java.javaw);
    cmd.args(&args)
        .current_dir(&inst)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    hide_console(&mut cmd);
    let mut child = cmd.spawn().context("impossible de lancer Java")?;

    // si le jeu s'arrete tout de suite, c'est une erreur de lancement : on la signale
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("Le jeu s'est arrete au demarrage (journal : {})", log_path.display());
            }
            break;
        }
    }
    progress.advance(1);
    Ok(())
}
