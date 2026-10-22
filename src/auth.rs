//! Connexion Microsoft par code (microsoft.com/link), puis Xbox Live, XSTS et compte Minecraft.
//! Aucun mot de passe ne passe par le launcher : seul le jeton de renouvellement est garde (settings.json).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::net::client;

/// Application Microsoft publique utilisee par les clients Minecraft pour la connexion par code.
const CLIENT_ID: &str = "00000000441cc96b";
const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
const DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";

fn five() -> u64 {
    5
}

fn fifteen_minutes() -> u64 {
    900
}

#[derive(Deserialize, Clone)]
pub struct DeviceCode {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    #[serde(default = "fifteen_minutes")]
    pub expires_in: u64,
    #[serde(default = "five")]
    pub interval: u64,
}

pub struct MsaTokens {
    pub access_token: String,
    pub refresh_token: String,
}

/// Session de jeu prete a l'emploi.
#[derive(Clone)]
pub struct Session {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub refresh_token: String,
}

fn body_of(resp: reqwest::blocking::Response) -> (reqwest::StatusCode, Value) {
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/// Demande un code a saisir sur microsoft.com/link.
pub fn start_device_code() -> Result<DeviceCode> {
    let resp = client()
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE), ("response_type", "device_code")])
        .send()
        .context("impossible de joindre Microsoft")?;
    let (status, body) = body_of(resp);
    if !status.is_success() {
        bail!("Microsoft a refuse la demande de connexion : {body}");
    }
    serde_json::from_value(body).context("reponse de Microsoft illisible")
}

/// Attend que le joueur ait saisi le code (ou que ce soit annule / expire).
pub fn wait_for_login(dc: &DeviceCode, cancel: &AtomicBool) -> Result<MsaTokens> {
    let deadline = Instant::now() + Duration::from_secs(dc.expires_in);
    let mut interval = dc.interval.max(1);
    loop {
        for _ in 0..interval {
            if cancel.load(Ordering::Relaxed) {
                bail!("Connexion annulee.");
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        if Instant::now() > deadline {
            bail!("Le code a expire : recommence la connexion.");
        }
        let resp = client()
            .post(TOKEN_URL)
            .form(&[
                ("client_id", CLIENT_ID),
                ("device_code", dc.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .context("impossible de joindre Microsoft")?;
        let (_, body) = body_of(resp);
        if let Some(token) = body.get("access_token").and_then(Value::as_str) {
            return Ok(MsaTokens {
                access_token: token.to_string(),
                refresh_token: body.get("refresh_token").and_then(Value::as_str).unwrap_or_default().to_string(),
            });
        }
        match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("expired_token") => bail!("Le code a expire : recommence la connexion."),
            Some("authorization_declined") | Some("access_denied") => bail!("Connexion refusee sur la page Microsoft."),
            _ => bail!("Erreur Microsoft : {body}"),
        }
    }
}

/// Renouvelle la connexion Microsoft a partir du jeton garde.
pub fn refresh(refresh_token: &str) -> Result<MsaTokens> {
    let resp = client()
        .post(TOKEN_URL)
        .form(&[
            ("client_id", CLIENT_ID),
            ("scope", SCOPE),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .context("impossible de joindre Microsoft")?;
    let (_, body) = body_of(resp);
    match body.get("access_token").and_then(Value::as_str) {
        Some(token) => Ok(MsaTokens {
            access_token: token.to_string(),
            refresh_token: body
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or(refresh_token)
                .to_string(),
        }),
        None => bail!("La connexion Microsoft a expire : deconnecte-toi puis reconnecte-toi."),
    }
}

/// Xbox Live -> XSTS -> Minecraft -> profil.
pub fn minecraft_login(msa: MsaTokens) -> Result<Session> {
    let (xbl_token, uhs) = xbox_live(&msa.access_token)?;
    let xsts_token = xsts(&xbl_token)?;

    let resp = client()
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .json(&json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts_token}") }))
        .send()
        .context("impossible de joindre les serveurs Minecraft")?;
    let (status, body) = body_of(resp);
    let Some(mc_token) = body.get("access_token").and_then(Value::as_str).map(str::to_string) else {
        bail!("Connexion Minecraft refusee (HTTP {status}) : {body}");
    };

    let resp = client()
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&mc_token)
        .send()
        .context("impossible de lire le profil Minecraft")?;
    let (status, body) = body_of(resp);
    if status.as_u16() == 404 {
        bail!("Ce compte Microsoft ne possede pas Minecraft Java Edition.");
    }
    let (Some(name), Some(id)) = (body.get("name").and_then(Value::as_str), body.get("id").and_then(Value::as_str)) else {
        bail!("Profil Minecraft illisible (HTTP {status}) : {body}");
    };
    Ok(Session {
        name: name.to_string(),
        uuid: id.to_string(),
        access_token: mc_token,
        refresh_token: msa.refresh_token,
    })
}

fn xbox_live(access_token: &str) -> Result<(String, String)> {
    let mut last = String::new();
    // Le prefixe attendu depend du type de jeton : on essaie les trois formes connues.
    for prefix in ["t=", "d=", ""] {
        let resp = client()
            .post("https://user.auth.xboxlive.com/user/authenticate")
            .header("Accept", "application/json")
            .json(&json!({
                "Properties": {
                    "AuthMethod": "RPS",
                    "SiteName": "user.auth.xboxlive.com",
                    "RpsTicket": format!("{prefix}{access_token}")
                },
                "RelyingParty": "http://auth.xboxlive.com",
                "TokenType": "JWT"
            }))
            .send()
            .context("impossible de joindre Xbox Live")?;
        let (status, body) = body_of(resp);
        if status.is_success() {
            let token = body.get("Token").and_then(Value::as_str);
            let uhs = body.pointer("/DisplayClaims/xui/0/uhs").and_then(Value::as_str);
            if let (Some(token), Some(uhs)) = (token, uhs) {
                return Ok((token.to_string(), uhs.to_string()));
            }
        }
        last = format!("HTTP {status}");
    }
    bail!("Connexion Xbox Live refusee ({last}).")
}

fn xsts(xbl_token: &str) -> Result<String> {
    let resp = client()
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .header("Accept", "application/json")
        .json(&json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }))
        .send()
        .context("impossible de joindre Xbox Live")?;
    let (status, body) = body_of(resp);
    if let Some(token) = body.get("Token").and_then(Value::as_str) {
        return Ok(token.to_string());
    }
    let reason = match body.get("XErr").and_then(Value::as_u64) {
        Some(2148916233) => "ce compte Microsoft n'a pas de profil Xbox : cree-en un sur xbox.com puis reessaie".to_string(),
        Some(2148916235) => "Xbox Live n'est pas disponible dans ton pays".to_string(),
        Some(2148916236) | Some(2148916237) => "une verification d'age est demandee sur xbox.com".to_string(),
        Some(2148916238) => "compte enfant : un adulte doit l'ajouter a une famille Microsoft".to_string(),
        _ => format!("HTTP {status} {body}"),
    };
    bail!("Connexion Xbox refusee : {reason}.")
}
