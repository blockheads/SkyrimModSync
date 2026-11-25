//! This machine's choices (game folder, store folder) and the private Nexus key.
use crate::{files, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

/// The tool's config folder (`SMS_CONFIG_DIR` overrides it, for tests and portable use).
pub fn directory() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SMS_CONFIG_DIR").map(PathBuf::from) {
        if p.is_absolute() {
            return Ok(p);
        }
    }
    dirs::config_dir()
        .map(|d| d.join("skyrim-mod-sync"))
        .ok_or_else(|| {
            Error::new(
                "configuration_required",
                "No user config folder on this system.",
            )
        })
}

/// The host's state: channel keys, published packs, frozen host files, hash caches.
pub fn host_directory() -> Result<PathBuf> {
    Ok(directory()?.join("host"))
}

/// A friend's choices, saved by `sms setup`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The Skyrim folder (with SkyrimSE.exe).
    pub game: Option<PathBuf>,
    /// The user's store folder: synced files and downloaded archives.
    pub store: Option<PathBuf>,
    /// The game's AppData/Local/Skyrim Special Edition (plugins.txt).
    pub local: Option<PathBuf>,
    /// The game's Documents/My Games/Skyrim Special Edition (INIs).
    pub documents: Option<PathBuf>,
}
impl Settings {
    pub fn load() -> Result<Self> {
        let p = directory()?.join("settings.json");
        if p.exists() {
            files::read_json(&p)
        } else {
            Ok(Self::default())
        }
    }
    pub fn save(&self) -> Result<()> {
        files::atomic_json(&directory()?.join("settings.json"), self)
    }
    pub fn require(value: &Option<PathBuf>, what: &str) -> Result<PathBuf> {
        value.clone().ok_or_else(|| {
            Error::new(
                "setup_required",
                format!("No {what} chosen yet. Run `sms setup` first."),
            )
        })
    }
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
    let p = PathBuf::from(line.trim());
    if !p.is_absolute() {
        return Err(Error::new("invalid_path", "Give an absolute folder path."));
    }
    Ok(p)
}

fn key_path() -> Result<PathBuf> {
    Ok(directory()?.join("nexus-api-key"))
}

/// The personal Nexus API key, if one was saved.
pub fn nexus_key() -> Result<Option<String>> {
    let path = key_path()?;
    if !path.exists() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(
                "key_permissions",
                "The Nexus key file must be private (chmod 600).",
            ));
        }
    }
    let key = String::from_utf8(files::bounded(&path, 16384)?)
        .map_err(|_| Error::new("invalid_key", "The key file must be UTF-8."))?
        .trim()
        .to_owned();
    if key.is_empty() || key.contains(['\n', '\r']) {
        return Err(Error::new(
            "invalid_key",
            "Invalid key file; run `sms nexus-login` again.",
        ));
    }
    Ok(Some(key))
}

/// Reads a key from the terminal (or one line of stdin) and saves it privately.
pub fn read_key(stdin: bool, interactive: bool) -> Result<String> {
    let key = if stdin {
        let mut key = String::new();
        io::stdin().read_line(&mut key)?;
        key.trim().to_owned()
    } else if interactive && io::stdin().is_terminal() {
        rpassword::prompt_password("Nexus API key (nexusmods.com > Site preferences > API keys): ")?
            .trim()
            .to_owned()
    } else {
        return Err(Error::new(
            "configuration_required",
            "Run nexus-login at a terminal, or pass --stdin.",
        ));
    };
    if key.is_empty() || key.len() > 16384 || key.contains(['\n', '\r']) {
        return Err(Error::new("invalid_key", "Expected one Nexus API key."));
    }
    Ok(key)
}

pub fn save_key(key: &str) -> Result<()> {
    let path = key_path()?;
    let dir = path.parent().unwrap();
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(key.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|e| Error::from(e.error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Folders must not nest: the store inside the game folder would deploy into itself.
pub fn separate(a: &Path, b: &Path) -> Result<()> {
    files::separate(a, b)
}
