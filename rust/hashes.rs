//! Remembered file hashes, keyed by path, size and modification time, so a 15 GB game folder is
//! hashed once, not on every publish or sync.
use crate::{files, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Seen {
    size: u64,
    modified: u128,
    sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blake3: Option<String>,
}

pub struct Cache {
    path: PathBuf,
    seen: BTreeMap<String, Seen>,
    dirty: bool,
}

fn stamp(path: &Path) -> Result<(u64, u128)> {
    let m = std::fs::metadata(path)?;
    let modified = m
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok((m.len(), modified))
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self> {
        let seen = if path.is_file() {
            files::read_json(path).unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path: path.into(),
            seen,
            dirty: false,
        })
    }
    fn fresh(&self, file: &Path) -> Result<Option<&Seen>> {
        let (size, modified) = stamp(file)?;
        Ok(self
            .seen
            .get(&file.to_string_lossy().into_owned())
            .filter(|s| s.size == size && s.modified == modified))
    }
    /// The file's SHA-256 (and size).
    pub fn sha256(&mut self, file: &Path) -> Result<(u64, String)> {
        if let Some(s) = self.fresh(file)? {
            return Ok((s.size, s.sha256.clone()));
        }
        let (size, modified) = stamp(file)?;
        let sha256 = files::sha(file)?;
        if stamp(file)? != (size, modified) {
            return Err(crate::Error::new(
                "source_changed",
                format!("{} changed while being read.", file.display()),
            ));
        }
        self.seen.insert(
            file.to_string_lossy().into_owned(),
            Seen {
                size,
                modified,
                sha256: sha256.clone(),
                blake3: None,
            },
        );
        self.dirty = true;
        Ok((size, sha256))
    }
    /// Whether the file holds exactly `sha256`; a link (Vortex's symlink deployment) never does.
    pub fn holds(&mut self, file: &Path, sha256: &str) -> Result<bool> {
        if std::fs::symlink_metadata(file)?.file_type().is_symlink() {
            return Ok(false);
        }
        Ok(self.sha256(file)?.1 == sha256)
    }
    pub fn blake3(&self, file: &Path) -> Result<Option<String>> {
        Ok(self.fresh(file)?.and_then(|s| s.blake3.clone()))
    }
    pub fn set_blake3(&mut self, file: &Path, hash: &str) -> Result<()> {
        self.sha256(file)?;
        if let Some(s) = self.seen.get_mut(&file.to_string_lossy().into_owned()) {
            s.blake3 = Some(hash.into());
            self.dirty = true;
        }
        Ok(())
    }
    pub fn save(&mut self) -> Result<()> {
        if self.dirty {
            files::atomic_json(&self.path, &self.seen)?;
            self.dirty = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_file_is_hashed_once_and_a_link_never_holds_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.esp");
        std::fs::write(&f, b"plugin").unwrap();
        let mut c = Cache::open(&dir.path().join("hashes.json")).unwrap();
        assert!(c.holds(&f, &files::hash_bytes(b"plugin")).unwrap());
        c.save().unwrap();
        assert!(Cache::open(&dir.path().join("hashes.json"))
            .unwrap()
            .fresh(&f)
            .unwrap()
            .is_some());
        #[cfg(unix)]
        {
            let link = dir.path().join("link.esp");
            std::os::unix::fs::symlink(&f, &link).unwrap();
            assert!(!c.holds(&link, &files::hash_bytes(b"plugin")).unwrap());
        }
    }
}
