//! BLAKE3 for every file, on all cores, remembered by path, size and modification time.
use crate::{files, Error, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// A file's size and BLAKE3 (hex). Plain sequential reads: faster than memory-mapping when the data
/// is bigger than the page cache (a game folder is); `Cache::many` runs files in parallel.
pub fn file(path: &Path) -> Result<(u64, String)> {
    crate::check_cancel()?;
    let mut h = blake3::Hasher::new();
    let mut f = std::fs::File::open(path)
        .map_err(|e| Error::new("io_error", format!("Cannot read {}: {e}", path.display())))?;
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok((h.count(), h.finalize().to_hex().to_string()))
}

/// What a writer hashed while writing.
pub struct Writer<W> {
    pub inner: W,
    pub hasher: blake3::Hasher,
}
impl<W: std::io::Write> std::io::Write for Writer<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        self.hasher.update(&b[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Seen {
    size: u64,
    modified: u128,
    hash: String,
}

/// Hashes remembered across runs, so a 15 GB game folder is read once.
pub struct Cache {
    path: PathBuf,
    seen: HashMap<String, Seen>,
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
    pub fn open(path: &Path) -> Self {
        let seen = files::read_json(path).unwrap_or_default();
        Self {
            path: path.into(),
            seen,
        }
    }
    pub fn save(&self) -> Result<()> {
        files::write_json(&self.path, &self.seen)
    }
    /// Size and hash of each path, in order; the ones not remembered are hashed in parallel.
    pub fn many(&mut self, paths: &[PathBuf]) -> Result<Vec<(u64, String)>> {
        let stamps = paths.iter().map(|p| stamp(p)).collect::<Result<Vec<_>>>()?;
        let fresh = |i: usize| {
            let s = self.seen.get(&paths[i].to_string_lossy().into_owned())?;
            (s.size == stamps[i].0 && s.modified == stamps[i].1).then(|| (s.size, s.hash.clone()))
        };
        let known: Vec<Option<(u64, String)>> = (0..paths.len()).map(fresh).collect();
        let missing: Vec<usize> = (0..paths.len()).filter(|i| known[*i].is_none()).collect();
        if missing.len() > 50 {
            crate::event("hashing", serde_json::json!({"files": missing.len()}));
        }
        let hashed = missing
            .par_iter()
            .map(|&i| file(&paths[i]).map(|r| (i, r)))
            .collect::<Result<Vec<_>>>()?;
        let mut out = known;
        for (i, (size, hash)) in hashed {
            let key = paths[i].to_string_lossy().into_owned();
            self.seen.insert(
                key,
                Seen {
                    size,
                    modified: stamps[i].1,
                    hash: hash.clone(),
                },
            );
            out[i] = Some((size, hash));
        }
        if !missing.is_empty() {
            self.save()?;
        }
        Ok(out.into_iter().map(Option::unwrap).collect())
    }
    pub fn one(&mut self, path: &Path) -> Result<(u64, String)> {
        Ok(self.many(&[path.to_path_buf()])?.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn files_are_hashed_once_and_match_blake3() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.esp");
        std::fs::write(&f, b"plugin").unwrap();
        let mut c = Cache::open(&dir.path().join("hashes.json"));
        let (size, hash) = c.one(&f).unwrap();
        assert_eq!(
            (size, hash.as_str()),
            (6, blake3::hash(b"plugin").to_hex().as_str())
        );
        let again = Cache::open(&dir.path().join("hashes.json"));
        assert!(again.seen.contains_key(&f.to_string_lossy().into_owned()));
    }
}
