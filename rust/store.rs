//! A friend's store (a folder the user picks): every pack file by SHA-256, the downloaded archives,
//! and the pack last synced. Load hardlinks from here into the game folder.
use crate::{channel::Head, files, pack::Pack, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct Store {
    pub root: PathBuf,
}

/// The pack a store holds completely.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Current {
    pub pack: Pack,
    pub head: Head,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        Ok(Self {
            root: files::root(root)?,
        })
    }
    pub fn object(&self, sha256: &str) -> PathBuf {
        self.root.join("objects").join(&sha256[..2]).join(sha256)
    }
    pub fn has(&self, sha256: &str, size: u64) -> bool {
        fs::metadata(self.object(sha256)).is_ok_and(|m| m.is_file() && m.len() == size)
    }
    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }
    /// Copies `reader` in as an object; refused unless it hashes to `sha256`.
    pub fn put(&self, reader: &mut dyn Read, sha256: &str, size: u64) -> Result<()> {
        if self.has(sha256, size) {
            std::io::copy(reader, &mut std::io::sink())?;
            return Ok(());
        }
        let dest = self.object(sha256);
        fs::create_dir_all(dest.parent().unwrap())?;
        let mut tmp = tempfile::NamedTempFile::new_in(dest.parent().unwrap())?;
        let mut h = Sha256::new();
        let mut buf = vec![0; 1024 * 1024];
        let mut count = 0u64;
        loop {
            crate::check_cancel()?;
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            count += n as u64;
            h.update(&buf[..n]);
            tmp.write_all(&buf[..n])?;
        }
        if count != size || format!("{:x}", h.finalize()) != sha256 {
            return Err(Error::new(
                "object_mismatch",
                format!("A file meant to be {sha256} has other bytes."),
            ));
        }
        tmp.as_file().sync_all()?;
        tmp.persist(&dest).map_err(|e| Error::from(e.error))?;
        Ok(())
    }
    /// Takes a file already on this machine in as an object: hardlinked when it can be, checked either way.
    pub fn take_local(&self, path: &Path, sha256: &str, size: u64) -> Result<()> {
        if self.has(sha256, size) {
            return Ok(());
        }
        let dest = self.object(sha256);
        fs::create_dir_all(dest.parent().unwrap())?;
        if fs::hard_link(path, &dest).is_ok() {
            if fs::metadata(&dest)?.len() == size && files::sha(&dest)? == sha256 {
                return Ok(());
            }
            fs::remove_file(&dest)?;
            return Err(Error::new(
                "source_changed",
                format!("{} changed since it was published.", path.display()),
            ));
        }
        self.put(&mut fs::File::open(path)?, sha256, size)
    }
    pub fn current(&self) -> Result<Option<Current>> {
        let p = self.root.join("current.json");
        if !p.is_file() {
            return Ok(None);
        }
        let c: Current = files::read_json(&p)?;
        c.head.validate()?;
        if c.pack.id()? != c.head.revision {
            return Err(Error::new(
                "invalid_pack",
                "The store's pack differs from its signed head.",
            ));
        }
        Ok(Some(c))
    }
    pub fn set_current(&self, c: &Current) -> Result<()> {
        files::atomic_json(&self.root.join("current.json"), c)
    }
    /// The highest head seen for a channel: a host can't silently roll a friend back.
    pub fn accepted(&self, pack: &str) -> Result<Option<Head>> {
        let p = self.root.join("accepted").join(format!("{pack}.json"));
        if p.is_file() {
            Ok(Some(files::read_json(&p)?))
        } else {
            Ok(None)
        }
    }
    pub fn accept(&self, head: &Head) -> Result<()> {
        if let Some(seen) = self.accepted(&head.channel.pack)? {
            if seen.channel.publisher != head.channel.publisher {
                return Err(Error::new(
                    "channel_mismatch",
                    "This pack was first published by someone else.",
                ));
            }
            if head.sequence < seen.sequence
                || head.sequence == seen.sequence && head.revision != seen.revision
            {
                return Err(Error::new(
                    "revision_replay",
                    "The host offers an older pack than one already synced.",
                ));
            }
        }
        files::atomic_json(
            &self
                .root
                .join("accepted")
                .join(format!("{}.json", head.channel.pack)),
            head,
        )
    }
}
