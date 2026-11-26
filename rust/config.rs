//! This machine's choices (game and store folders) and the private Nexus key.
use crate::{files, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::PathBuf,
};

/// The tool's folder; `SMS_CONFIG_DIR` picks another (portable use, isolated test runs).
pub fn directory() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SMS_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(p);
    }
    dirs::config_dir()
        .map(|d| d.join("skyrim-mod-sync"))
        .ok_or_else(|| Error::new("setup_required", "No user config folder on this system."))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// The Skyrim folder (with SkyrimSE.exe).
    pub game: Option<PathBuf>,
    /// Synced files and downloaded archives.
    pub store: Option<PathBuf>,
    /// The game's AppData/Local/Skyrim Special Edition.
    pub local: Option<PathBuf>,
    /// The game's Documents/My Games/Skyrim Special Edition.
    pub documents: Option<PathBuf>,
}
impl Settings {
    pub fn load() -> Result<Self> {
        Ok(files::read_json(&directory()?.join("settings.json")).unwrap_or_default())
    }
    pub fn save(&self) -> Result<()> {
        files::write_json(&directory()?.join("settings.json"), self)
    }
}
pub fn require(value: &Option<PathBuf>, what: &str) -> Result<PathBuf> {
    value.clone().ok_or_else(|| {
        Error::new(
            "setup_required",
            format!("No {what} chosen yet: run `sms setup`."),
        )
    })
}

/// Asks at a terminal (Enter takes `default`); refuses in --json mode. Empty when skipped.
pub fn ask(question: &str, default: Option<&str>, interactive: bool) -> Result<String> {
    if !interactive || !io::stdin().is_terminal() {
        return Err(Error::new(
            "setup_required",
            format!("{question} Pass it as an argument."),
        ));
    }
    match default {
        Some(d) => eprint!("{question} [Enter: {d}] "),
        None => eprint!("{question} "),
    }
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let line = line.trim().trim_matches('"').to_owned();
    Ok(if line.is_empty() {
        default.unwrap_or_default().to_owned()
    } else {
        line
    })
}

fn secret(name: &str) -> Result<Option<String>> {
    Ok(fs::read_to_string(directory()?.join(name))
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty()))
}
fn save_secret(name: &str, value: &str) -> Result<()> {
    let path = directory()?.join(name);
    files::write_atomic(&path, value.as_bytes())?;
    #[cfg(unix)]
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}
pub fn nexus_key() -> Result<Option<String>> {
    secret("nexus-api-key")
}
/// A key from one line of stdin, or asked at a terminal (hidden; Enter skips). None when skipped.
pub fn read_key(stdin: bool, interactive: bool) -> Result<Option<String>> {
    let key = if stdin {
        let mut key = String::new();
        io::stdin().read_line(&mut key)?;
        key
    } else if interactive && io::stdin().is_terminal() {
        rpassword::prompt_password(
            "Nexus API key (Premium; nexusmods.com > Site preferences > API keys; Enter to skip): ",
        )?
    } else {
        return Ok(None);
    };
    Ok(Some(key.trim().to_owned()).filter(|k| !k.is_empty()))
}
pub fn save_key(key: &str) -> Result<()> {
    save_secret("nexus-api-key", key)
}

/// A GitHub token for the pack's gist: GH_TOKEN/GITHUB_TOKEN, the GitHub CLI's login, the saved one,
/// or asked once (hidden) and saved.
pub fn github_token(interactive: bool) -> Result<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(t) = std::env::var(var).ok().filter(|t| !t.trim().is_empty()) {
            return Ok(t.trim().to_owned());
        }
    }
    if let Some(t) = gh_token() {
        return Ok(t);
    }
    if let Some(t) = secret("github-token")? {
        return Ok(t);
    }
    if !interactive || !io::stdin().is_terminal() {
        return Err(Error::new(
            "github_token",
            "Sharing the pack needs a GitHub token with the gist scope: set GH_TOKEN, or `gh auth login`.",
        ));
    }
    let t = rpassword::prompt_password(
        "GitHub token, to share the pack as a secret gist (github.com/settings/tokens/new?scopes=gist): ",
    )?;
    let t = t.trim().to_owned();
    if t.is_empty() {
        return Err(Error::new("github_token", "No GitHub token given."));
    }
    save_secret("github-token", &t)?;
    Ok(t)
}

/// The GitHub CLI's login: `gh auth token`, or older versions' `gh auth status --show-token`.
fn gh_token() -> Option<String> {
    let run = |args: &[&str]| std::process::Command::new("gh").args(args).output().ok();
    if let Some(o) = run(&["auth", "token"]).filter(|o| o.status.success()) {
        let t = String::from_utf8_lossy(&o.stdout).trim().to_owned();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let o = run(&["auth", "status", "--show-token"])?;
    let text = String::from_utf8_lossy(&o.stdout) + String::from_utf8_lossy(&o.stderr);
    text.lines()
        .find_map(|l| l.split_once("Token: ").map(|(_, t)| t.trim().to_owned()))
        .filter(|t| !t.is_empty() && !t.starts_with('*'))
}

/// Forgets a saved GitHub token GitHub refused.
pub fn forget_github_token() -> Result<()> {
    let _ = fs::remove_file(directory()?.join("github-token"));
    Ok(())
}
