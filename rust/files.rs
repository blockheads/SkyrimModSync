//! Small file helpers: case-folded paths (Skyrim's are case-insensitive), JSON, locks, tree walks.
use crate::{check_cancel, Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

pub fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// A safe relative path for Windows and Linux: '/'-separated, no `..`, no reserved names.
pub fn relative(s: &str) -> Result<()> {
    let bad = s.is_empty()
        || s.len() > 2048
        || s.split('/').any(|p| {
            let stem = fold(p.split('.').next().unwrap_or_default());
            p.is_empty()
                || p == "."
                || p == ".."
                || p.ends_with(['.', ' '])
                || p.chars()
                    .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
                || ["con", "prn", "aux", "nul"].contains(&stem.as_str())
                || (stem.len() == 4
                    && (stem.starts_with("com") || stem.starts_with("lpt"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        });
    if bad {
        return Err(Error::new("invalid_path", format!("Unsafe path: {s:?}")));
    }
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// Written whole or not at all: a temp file, flushed, renamed over.
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    write_atomic(path, &serde_json::to_vec(value)?)
}
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::new("invalid_path", "No parent folder."))?;
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| Error::from(e.error))?;
    Ok(())
}

/// A name in `dir`, matched without case.
pub fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| fold(&e.file_name().to_string_lossy()) == fold(name))
        .map(|e| e.path())
}
pub fn require(dir: &Path, name: &str) -> Result<PathBuf> {
    find(dir, name)
        .ok_or_else(|| Error::new("missing_file", format!("No {name} in {}.", dir.display())))
}

/// Every file under `root` (links followed): relative '/'-path -> path.
pub fn walk(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    fn go(dir: &Path, rel: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
        check_cancel()?;
        for e in fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let meta = fs::metadata(e.path())?;
            if meta.is_dir() {
                go(&e.path(), &path, out)?;
            } else if meta.is_file() {
                out.insert(path, e.path());
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    if root.is_dir() {
        go(root, "", &mut out)?;
    }
    Ok(out)
}

/// One writer at a time for a folder's state (released when dropped).
pub struct Lock(#[allow(dead_code)] File);
impl Lock {
    pub fn take(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        f.try_lock()
            .map_err(|_| Error::new("busy", "Another sms command is using this folder."))?;
        Ok(Self(f))
    }
}

/// Folders must not nest (the store inside the game folder would deploy into itself).
pub fn separate(a: &Path, b: &Path) -> Result<()> {
    if a.starts_with(b) || b.starts_with(a) {
        return Err(Error::new(
            "overlapping_paths",
            "The game and store folders must not be inside one another.",
        ));
    }
    Ok(())
}

/// A folder the user names, made absolute with links resolved.
pub fn root(path: &Path) -> Result<PathBuf> {
    path.canonicalize().map_err(|e| {
        Error::new(
            "missing_path",
            format!("Cannot open {}: {e}.", path.display()),
        )
    })
}
