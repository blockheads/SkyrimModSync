//! A friend's store (a folder the user picks): pack files by BLAKE3, downloaded archives, the pack.
use crate::{files, hash, pack::Pack, Error, Result};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        Ok(Self {
            root: files::root(root)?,
        })
    }
    pub fn object(&self, hash: &str) -> PathBuf {
        self.root.join("objects").join(&hash[..2]).join(hash)
    }
    pub fn has(&self, hash: &str, size: u64) -> bool {
        fs::metadata(self.object(hash)).is_ok_and(|m| m.is_file() && m.len() == size)
    }
    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }
    /// `reader` in as an object, refused unless it hashes to `hash`.
    pub fn put(&self, reader: &mut dyn Read, hash: &str) -> Result<()> {
        let dest = self.object(hash);
        fs::create_dir_all(dest.parent().unwrap())?;
        let temp = tempfile::NamedTempFile::new_in(dest.parent().unwrap())?;
        let mut out = hash::Writer {
            inner: temp.as_file(),
            hasher: blake3::Hasher::new(),
        };
        std::io::copy(reader, &mut out)?;
        if out.hasher.finalize().to_hex().as_str() != hash {
            return Err(Error::new(
                "object_mismatch",
                format!("A file meant to be {hash} has other bytes."),
            ));
        }
        temp.persist(&dest).map_err(|e| Error::from(e.error))?;
        Ok(())
    }
    pub fn pack(&self) -> Result<Option<Pack>> {
        let p = self.root.join("pack.json");
        if !p.is_file() {
            return Ok(None);
        }
        let pack: Pack = files::read_json(&p)?;
        pack.validate()?;
        Ok(Some(pack))
    }
    pub fn set_pack(&self, pack: &Pack) -> Result<()> {
        files::write_json(&self.root.join("pack.json"), pack)
    }
    pub fn hashes(&self) -> hash::Cache {
        hash::Cache::open(&self.root.join("hashes.json"))
    }
    /// Removes objects and downloads the current pack doesn't use; returns the bytes freed.
    pub fn clean(&self) -> Result<u64> {
        let pack = self.pack()?;
        let keep: BTreeSet<String> = pack
            .iter()
            .flat_map(|p| {
                p.files
                    .iter()
                    .map(|e| e.hash.clone())
                    .chain(p.archives.iter().map(|a| a.hash.clone()))
            })
            .collect();
        let mut freed = 0;
        for (_, path) in files::walk(&self.root.join("objects"))?
            .into_iter()
            .chain(files::walk(&self.downloads())?)
        {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if !keep.contains(&name) {
                freed += fs::metadata(&path)?.len();
                fs::remove_file(&path)?;
            }
        }
        Ok(freed)
    }
}
