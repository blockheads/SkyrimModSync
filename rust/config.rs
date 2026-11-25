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

/// Asks for a folder at a terminal; refuses in --json mode.
pub fn ask_folder(question: &str, interactive: bool) -> Result<PathBuf> {
    if !interactive || !io::stdin().is_terminal() {
        return Err(Error::new(
            "setup_required",
            format!("{question} Pass it as a flag."),
        ));
    }
    eprint!("{question} ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(PathBuf::from(line.trim()))
}

fn key_path() -> Result<PathBuf> {
    Ok(directory()?.join("nexus-api-key"))
}
pub fn nexus_key() -> Result<Option<String>> {
    Ok(fs::read_to_string(key_path()?)
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty()))
}
/// A key from the terminal (hidden) or one line of stdin.
pub fn read_key(stdin: bool, interactive: bool) -> Result<String> {
    let key = if stdin {
        let mut key = String::new();
        io::stdin().read_line(&mut key)?;
        key
    } else if interactive && io::stdin().is_terminal() {
        rpassword::prompt_password("Nexus API key (nexusmods.com > Site preferences > API keys): ")?
    } else {
        return Err(Error::new(
            "setup_required",
            "Run nexus-login at a terminal, or pass --stdin.",
        ));
    };
    Ok(key.trim().to_owned())
}
pub fn save_key(key: &str) -> Result<()> {
    let path = key_path()?;
    files::write_atomic(&path, key.as_bytes())?;
    #[cfg(unix)]
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}
