//! Nexus Mods through its documented API and the user's own API key. Downloads need a Premium
//! account's key; the tool never automates the website for free accounts (Nexus's terms).
use crate::{net, Error, Result};
use serde::Deserialize;
use serde_json::Value;

fn base() -> String {
    std::env::var("SMS_NEXUS_API").unwrap_or_else(|_| "https://api.nexusmods.com".into())
}

fn get(key: &str, path: &str) -> Result<reqwest::blocking::Response> {
    Ok(net::client()?
        .get(format!("{}{path}", base()))
        .header("apikey", key)
        .header("Application-Name", "SkyrimModSync")
        .header("Application-Version", env!("CARGO_PKG_VERSION"))
        .send()?)
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub name: String,
    pub is_premium: bool,
}

/// Checks a key; says whether its account is Premium.
pub fn validate(key: &str) -> Result<User> {
    let r = get(key, "/v1/users/validate.json")?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::new(
            "nexus_auth_failed",
            "Nexus refused the key: copy it again from nexusmods.com.",
        ));
    }
    Ok(r.error_for_status()?.json()?)
}

/// Whether a mod file can still be downloaded (not removed, hidden or archived).
pub fn available(key: &str, game: &str, r#mod: u64, file: u64) -> Result<bool> {
    let r = get(
        key,
        &format!("/v1/games/{game}/mods/{mod}/files/{file}.json"),
    )?;
    if r.status().is_client_error() {
        return Ok(false);
    }
    let v: Value = r.error_for_status()?.json()?;
    let category = v["category_name"]
        .as_str()
        .unwrap_or_default()
        .to_uppercase();
    Ok(!["ARCHIVED", "REMOVED", "DELETED"].contains(&category.as_str()))
}

/// A download link for a mod file (Premium accounts only).
pub fn download_link(key: &str, game: &str, r#mod: u64, file: u64) -> Result<String> {
    let r = get(
        key,
        &format!("/v1/games/{game}/mods/{mod}/files/{file}/download_link.json"),
    )?;
    match r.status().as_u16() {
        403 => return Err(Error::new(
            "nexus_premium_required",
            "Downloading through the Nexus API needs a Premium account's key. Ask the host to send these files instead.",
        )),
        404 => return Err(Error::new("nexus_file_gone", format!("Nexus no longer has mod {mod} file {file}."))),
        _ => {}
    }
    let links: Vec<Value> = r.error_for_status()?.json()?;
    links
        .iter()
        .find_map(|l| l["URI"].as_str().map(str::to_owned))
        .ok_or_else(|| {
            Error::new(
                "nexus_no_link",
                format!("Nexus gave no link for mod {mod} file {file}."),
            )
        })
}
