//! Finding Steam, Skyrim and the folders the game reads its settings from, on each OS.
use crate::{files, Error, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub const APP_ID: &str = "489830";

// Small strict VDF reader: recursion and input size are bounded; malformed secondary
// metadata never prevents discovery in the primary library.
pub fn vdf(text: &str) -> Result<Value> {
    let mut tokens = vec![];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            while chars.next().is_some_and(|c| c != '\n') {}
            continue;
        }
        if c == '{' || c == '}' {
            tokens.push(c.to_string());
        } else if c == '"' {
            let mut s = String::new();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == '"' {
                    closed = true;
                    break;
                }
                if c == '\\' {
                    s.push(
                        chars
                            .next()
                            .ok_or_else(|| Error::new("invalid_vdf", "Truncated escape"))?,
                    );
                } else {
                    s.push(c);
                }
            }
            if !closed {
                return Err(Error::new("invalid_vdf", "Unclosed string"));
            }
            tokens.push(s);
        } else {
            let mut s = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| !c.is_whitespace() && *c != '{' && *c != '}')
            {
                s.push(chars.next().unwrap());
            }
            tokens.push(s);
        }
    }
    fn parse(t: &[String], i: &mut usize, nested: bool, depth: usize) -> Result<Value> {
        if depth > 32 {
            return Err(Error::new("invalid_vdf", "VDF too deep"));
        }
        let mut map = serde_json::Map::new();
        while *i < t.len() {
            let key = &t[*i];
            *i += 1;
            if key == "}" {
                if nested {
                    return Ok(Value::Object(map));
                }
                return Err(Error::new("invalid_vdf", "Unexpected brace"));
            }
            if key == "{" || *i == t.len() {
                return Err(Error::new("invalid_vdf", "Missing value"));
            }
            let value = &t[*i];
            *i += 1;
            let v = if value == "{" {
                parse(t, i, true, depth + 1)?
            } else {
                Value::String(value.clone())
            };
            if map.insert(key.clone(), v).is_some() {
                return Err(Error::new("invalid_vdf", "Duplicate key"));
            }
        }
        if nested {
            return Err(Error::new("invalid_vdf", "Unclosed object"));
        }
        Ok(Value::Object(map))
    }
    parse(&tokens, &mut 0, false, 0)
}

/// Steam's own folders on this machine, most likely first.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    #[cfg(windows)]
    {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam") {
            if let Ok(p) = k.get_value::<String, _>("SteamPath") {
                roots.push(PathBuf::from(p));
            }
        }
        roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
    }
    if let Some(home) = dirs::home_dir() {
        if cfg!(target_os = "macos") {
            roots.push(home.join("Library/Application Support/Steam"));
        } else if cfg!(unix) {
            roots.push(home.join(".steam/steam"));
            roots.push(home.join(".steam/debian-installation"));
            roots.push(home.join(".local/share/Steam"));
            roots.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        }
    }
    roots
}

/// A Skyrim install Steam knows about.
#[derive(Clone, Debug, PartialEq)]
pub struct Install {
    /// The game folder (with SkyrimSE.exe).
    pub game: PathBuf,
    /// The Steam library holding it.
    pub library: PathBuf,
}

/// Every Skyrim SE install in the given Steam roots' libraries.
pub fn installs(roots: &[PathBuf]) -> Vec<Install> {
    let mut libraries = BTreeSet::new();
    for r in roots {
        let Ok(root) = r.canonicalize() else { continue };
        if !root.join("steamapps").is_dir() {
            continue;
        }
        libraries.insert(root.clone());
        let metadata = root.join("steamapps/libraryfolders.vdf");
        if let Ok(v) = files::bounded(&metadata, 4 * 1024 * 1024)
            .and_then(|b| vdf(&String::from_utf8_lossy(&b)))
        {
            if let Some(entries) = v["libraryfolders"].as_object() {
                for e in entries.values() {
                    if let Some(p) = e["path"].as_str().or(e.as_str()) {
                        let p = PathBuf::from(p);
                        if p.is_absolute() {
                            libraries.insert(p.canonicalize().unwrap_or(p));
                        }
                    }
                }
            }
        }
    }
    let mut found = vec![];
    for library in libraries {
        let apps = library.join("steamapps");
        let Ok(bytes) =
            files::bounded(&apps.join(format!("appmanifest_{APP_ID}.acf")), 1024 * 1024)
        else {
            continue;
        };
        let Ok(v) = vdf(&String::from_utf8_lossy(&bytes)) else {
            continue;
        };
        let Some(folder) = v["AppState"]["installdir"].as_str() else {
            continue;
        };
        if files::relative(folder).is_err() || folder.contains('/') {
            continue;
        }
        let game = apps.join("common").join(folder);
        if game.join("SkyrimSE.exe").is_file() {
            let game = game.canonicalize().unwrap_or(game);
            if !found.iter().any(|i: &Install| i.game == game) {
                found.push(Install {
                    game,
                    library: library.clone(),
                });
            }
        }
    }
    found
}

/// The library a game folder sits in (`<library>/steamapps/common/<game>`).
pub fn library_of(game: &Path) -> Option<PathBuf> {
    let common = game.parent()?;
    let apps = common.parent()?;
    (files::fold(&common.file_name()?.to_string_lossy()) == "common"
        && files::fold(&apps.file_name()?.to_string_lossy()) == "steamapps")
        .then(|| apps.parent().map(Path::to_path_buf))
        .flatten()
}

/// The folders the game reads plugins.txt and its INIs from: (AppData/Local/Skyrim Special Edition,
/// Documents/My Games/Skyrim Special Edition). Linux: inside Steam's Proton prefix for the game.
pub fn user_folders(game: &Path) -> Result<(PathBuf, PathBuf)> {
    if cfg!(windows) {
        let local = dirs::data_local_dir()
            .ok_or_else(|| Error::new("setup_required", "No AppData/Local folder."))?;
        let docs = dirs::document_dir()
            .ok_or_else(|| Error::new("setup_required", "No Documents folder."))?;
        return Ok((
            local.join("Skyrim Special Edition"),
            docs.join("My Games/Skyrim Special Edition"),
        ));
    }
    let library = library_of(game).ok_or_else(|| {
        Error::new(
            "setup_required",
            "The game is not in a Steam library; pass --local and --documents.",
        )
    })?;
    let user = library.join(format!(
        "steamapps/compatdata/{APP_ID}/pfx/drive_c/users/steamuser"
    ));
    if !user.is_dir() {
        return Err(Error::new(
            "setup_required",
            "Steam has not made Skyrim's Proton prefix yet: start Skyrim once from Steam, quit, then retry.",
        ));
    }
    Ok((
        user.join("AppData/Local/Skyrim Special Edition"),
        user.join("Documents/My Games/Skyrim Special Edition"),
    ))
}

pub fn describe(found: &[Install]) -> Value {
    json!(found
        .iter()
        .map(|i| json!({"game": i.game, "library": i.library}))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_skyrim_in_a_secondary_library() {
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("steam");
        let lib = dir.path().join("lib2");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::create_dir_all(lib.join("steamapps/common/Skyrim Special Edition")).unwrap();
        std::fs::write(
            lib.join("steamapps/common/Skyrim Special Edition/SkyrimSE.exe"),
            b"",
        )
        .unwrap();
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} }}",
                lib.display()
            ),
        )
        .unwrap();
        std::fs::write(
            lib.join("steamapps/appmanifest_489830.acf"),
            "\"AppState\" { \"installdir\" \"Skyrim Special Edition\" }",
        )
        .unwrap();
        let found = installs(&[steam]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            library_of(&found[0].game).unwrap(),
            lib.canonicalize().unwrap()
        );
    }
}
