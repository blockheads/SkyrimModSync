use crate::{files, Error, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};
const FIELDS: [&str; 9] = [
    "base",
    "coop",
    "downloads",
    "game",
    "documents",
    "local",
    "steam",
    "proton",
    "prefix",
];
pub fn home() -> Result<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| {
            Error::new(
                "configuration_required",
                "A valid absolute user home directory is required.",
            )
        })
}
pub fn env_path(name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or(fallback)
}
pub fn directory() -> Result<PathBuf> {
    let home = home()?;
    Ok(if cfg!(windows) {
        env_path("LOCALAPPDATA", home.join("AppData/Local")).join("SkyrimModSync")
    } else {
        env_path("XDG_CONFIG_HOME", home.join(".config")).join("skyrim-mod-sync")
    })
}
pub fn default_storage() -> Result<PathBuf> {
    let home = home()?;
    Ok(if cfg!(windows) {
        directory()?
    } else {
        env_path("XDG_DATA_HOME", home.join(".local/share")).join("skyrim-mod-sync")
    })
}
pub fn load() -> Result<BTreeMap<String, PathBuf>> {
    let path = directory()?.join("config.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let data: BTreeMap<String, PathBuf> = serde_json::from_slice(&files::bounded(&path, 65536)?)?;
    if data
        .iter()
        .any(|(k, v)| !FIELDS.contains(&k.as_str()) || !v.is_absolute())
    {
        return Err(Error::new(
            "invalid_configuration",
            format!(
                "Fix or remove {}. Only absolute local paths belong in settings.",
                path.display()
            ),
        ));
    }
    Ok(data)
}
pub fn save(values: BTreeMap<String, PathBuf>) -> Result<()> {
    let mut data = load()?;
    for (k, v) in values {
        if !FIELDS.contains(&k.as_str()) {
            return Err(Error::new("invalid_configuration", "Unknown path setting."));
        }
        data.insert(k, absolute(&v)?);
    }
    let path = directory()?.join("config.json");
    files::atomic_json(&path, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    let p = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for part in p.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::new(
                        "invalid_path",
                        "Path escapes its filesystem root.",
                    ));
                }
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    let mut ancestor = normalized.as_path();
    let mut suffix = Vec::new();
    while !ancestor.try_exists()? {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| Error::new("invalid_path", "Missing path ancestor."))?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::new("invalid_path", "Missing path ancestor."))?;
    }
    let mut resolved = ancestor.canonicalize()?;
    for name in suffix.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}
pub fn resolve(name: &str, supplied: Option<&Path>, interactive: bool) -> Result<PathBuf> {
    if let Some(p) = supplied {
        return absolute(p);
    }
    if let Some(p) = load()?.get(name) {
        return Ok(p.clone());
    }
    if name == "base" {
        let found = discover(&home()?, None)?;
        let candidates = found["candidates"].as_array().unwrap();
        if candidates.len() == 1 && candidates[0]["flatpak"] != true {
            let base = PathBuf::from(candidates[0]["base"].as_str().unwrap());
            if interactive {
                save([("base".into(), base.clone())].into_iter().collect())?;
            }
            return Ok(base);
        }
    }
    if interactive && io::stdin().is_terminal() {
        eprint!("{name} folder: ");
        io::stderr().flush()?;
        let mut text = String::new();
        io::stdin().read_line(&mut text)?;
        if !text.trim().is_empty() {
            let p = absolute(Path::new(text.trim()))?;
            save([(name.into(), p.clone())].into_iter().collect())?;
            return Ok(p);
        }
    }
    Err(Error::new(
        "configuration_required",
        format!("Supply --{name} PATH or run configure to save the {name} folder."),
    ))
}
pub fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}
pub fn load_key(required: bool) -> Result<Option<String>> {
    let path = directory()?.join("nexus-api-key");
    if !path.exists() {
        if required {
            return Err(Error::new("nexus_key_required",format!("Run nexus-login to save your personal Nexus API key privately in {}. It is read from this file, never argv or a share package.",path.display())));
        }
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
            return Err(Error::new(
                "key_permissions",
                "Nexus key must be private; chmod 600 the nexus-api-key file.",
            ));
        }
    }
    let bytes = files::bounded(&path, 16384)?;
    let key = String::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_key", "Key file must use UTF-8."))?
        .trim()
        .to_owned();
    if key.is_empty() || key.contains(['\n', '\r']) {
        return Err(Error::new(
            "invalid_key",
            "Invalid key file; paste one key on a single line.",
        ));
    }
    Ok(Some(key))
}
// Small strict VDF reader: recursion and input size are bounded; malformed secondary
// metadata never prevents discovery in the primary library.
fn vdf(text: &str) -> Result<Value> {
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
pub fn discover(home: &Path, steam: Option<&Path>) -> Result<Value> {
    let roots = if let Some(p) = steam {
        vec![p.to_owned()]
    } else {
        vec![
            home.join(".steam/steam"),
            home.join(".steam/debian-installation"),
            home.join(".local/share/Steam"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        ]
    };
    let mut seen = BTreeMap::new();
    let mut diagnostics = vec![];
    let mut candidates = vec![];
    for r in roots {
        if !r.join("steamapps").is_dir() {
            continue;
        }
        let root = r.canonicalize()?;
        if seen.insert(root.clone(), ()).is_some() {
            continue;
        }
        let flatpak = root.to_string_lossy().contains("com.valvesoftware.Steam");
        let mut libraries = BTreeSetHelper::new(root.clone());
        let metadata = root.join("steamapps/libraryfolders.vdf");
        if metadata.is_file() {
            match files::bounded(&metadata,4*1024*1024).and_then(|b|vdf(&String::from_utf8_lossy(&b))) {
            Ok(v)=>if let Some(entries)=v["libraryfolders"].as_object(){for e in entries.values(){if let Some(p)=e["path"].as_str().or(e.as_str()){if Path::new(p).is_absolute(){libraries.add(PathBuf::from(p));}}}},
            Err(_)=>diagnostics.push(json!({"reason":"malformed_library_metadata","path":metadata,"fix":"Primary library still scanned. Repair Steam metadata or supply explicit --base and --steam."}))
        }
        }
        for library in &libraries.0 {
            let apps = library.join("steamapps");
            let m = apps.join("appmanifest_489830.acf");
            if let Ok(bytes) = files::bounded(&m, 1024 * 1024) {
                if let Ok(v) = vdf(&String::from_utf8_lossy(&bytes)) {
                    if let Some(folder) = v["AppState"]["installdir"].as_str() {
                        if files::relative(folder).is_ok() && !folder.contains('/') {
                            let game = apps.join("common").join(folder);
                            if game.join("SkyrimSE.exe").is_file() {
                                let proton = libraries
                                    .0
                                    .iter()
                                    .map(|l| l.join("steamapps/common/Proton 10.0"))
                                    .find(|p| p.join("proton").is_file());
                                candidates.push(json!({"base":game.canonicalize()?,"steam":root,"proton":proton,"flatpak":flatpak,"integration_validated":false}));
                            }
                        }
                    }
                }
            }
        }
        if flatpak {
            diagnostics.push(json!({"reason":"flatpak_unvalidated","path":root,"fix":"Flatpak Steam was detected. Reconstruction/launch integration has not been validated; select a native Steam installation for the supported path."}));
        }
    }
    Ok(json!({"candidates":candidates,"diagnostics":diagnostics,"ambiguous":candidates.len()>1}))
}
struct BTreeSetHelper(std::collections::BTreeSet<PathBuf>);
impl BTreeSetHelper {
    fn new(p: PathBuf) -> Self {
        Self([p].into_iter().collect())
    }
    fn add(&mut self, p: PathBuf) {
        self.0.insert(p.canonicalize().unwrap_or(p));
    }
}

pub fn nexus_login(stdin: bool, interactive: bool) -> Result<()> {
    let key = if stdin {
        let mut key = String::new();
        std::io::BufRead::read_line(&mut io::stdin().lock(), &mut key)?;
        key.trim().to_owned()
    } else if interactive && io::stdin().is_terminal() {
        rpassword::prompt_password("Personal Nexus API key: ")?
            .trim()
            .to_owned()
    } else {
        return Err(Error::new(
            "configuration_required",
            "Use nexus-login at a terminal or --stdin with a private input pipe.",
        ));
    };
    if key.is_empty() || key.len() > 16384 || key.contains(['\n', '\r']) {
        return Err(Error::new(
            "invalid_key",
            "Expected one personal Nexus API key.",
        ));
    }
    let premium = crate::acquisition::validate_nexus(&key)?;
    let dir = directory()?;
    fs::create_dir_all(&dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
    temp.write_all(key.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join("nexus-api-key"))
        .map_err(|e| Error::from(e.error))?;
    crate::event("nexus_validated", json!({"premium":premium}));
    Ok(())
}
