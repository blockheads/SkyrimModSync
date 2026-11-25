//! A host's channel: its identity (the code friends get) and signed heads naming each published pack.
//! Host objects are BLAKE3 blobs, the hash iroh-blobs serves them by.
use crate::{files, pack::Pack, Error, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub schema: u32,
    pub endpoint: String,
    pub publisher: String,
    pub pack: String,
    pub relay: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub schema: u32,
    pub channel: Channel,
    pub sequence: u64,
    pub revision: String,
    pub parent: Option<String>,
    pub signature: String,
}
impl Channel {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 4 || !files::digest_valid(&self.pack) {
            return Err(Error::new("invalid_code", "Unsupported channel identity."));
        }
        decode32(&self.publisher)?;
        self.endpoint
            .parse::<iroh::EndpointId>()
            .map_err(|_| Error::new("invalid_code", "Invalid peer endpoint."))?;
        if let Some(r) = &self.relay {
            r.parse::<iroh::RelayUrl>()
                .map_err(|_| Error::new("invalid_code", "Invalid relay address."))?;
        }
        Ok(())
    }
    pub fn code(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "SMS4.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() > 16384 {
            return Err(Error::new("invalid_code", "Code exceeds limit."));
        }
        let raw = URL_SAFE_NO_PAD
            .decode(
                s.strip_prefix("SMS4.")
                    .ok_or_else(|| Error::new("invalid_code", "Expected an SMS4 code."))?,
            )
            .map_err(|_| Error::new("invalid_code", "Malformed channel code."))?;
        let c: Self = serde_json::from_slice(&raw)?;
        c.validate()?;
        Ok(c)
    }
}
pub fn decode32(s: &str) -> Result<[u8; 32]> {
    let b = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::new("invalid_identity", "Invalid key encoding."))?;
    b.try_into()
        .map_err(|_| Error::new("invalid_identity", "Expected 32-byte key."))
}
fn signed_bytes(h: &Head) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &json!({"domain":"SkyrimModSync/channel-head/v4","schema":h.schema,"channel":h.channel,"sequence":h.sequence,"revision":h.revision,"parent":h.parent}),
    )?)
}
impl Head {
    pub fn sign(
        channel: Channel,
        sequence: u64,
        revision: String,
        parent: Option<String>,
        key: &SigningKey,
    ) -> Result<Self> {
        let mut h = Self {
            schema: 4,
            channel,
            sequence,
            revision,
            parent,
            signature: String::new(),
        };
        h.signature = URL_SAFE_NO_PAD.encode(key.sign(&signed_bytes(&h)?).to_bytes());
        h.validate()?;
        Ok(h)
    }
    pub fn validate(&self) -> Result<()> {
        self.channel.validate()?;
        if self.schema != 4
            || self.sequence == 0
            || !files::digest_valid(&self.revision)
            || self
                .parent
                .as_ref()
                .is_some_and(|p| !files::digest_valid(p))
        {
            return Err(Error::new("invalid_revision", "Invalid revision head."));
        }
        let key = VerifyingKey::from_bytes(&decode32(&self.channel.publisher)?)
            .map_err(|_| Error::new("invalid_identity", "Invalid publisher key."))?;
        let sig = URL_SAFE_NO_PAD
            .decode(&self.signature)
            .map_err(|_| Error::new("invalid_signature", "Invalid head signature."))?;
        let sig = Signature::from_slice(&sig)
            .map_err(|_| Error::new("invalid_signature", "Invalid head signature."))?;
        key.verify(&signed_bytes(self)?, &sig)
            .map_err(|_| Error::new("invalid_signature", "Pack was not signed by its publisher."))
    }
}
pub fn object_path(storage: &Path, hash: &str) -> Result<PathBuf> {
    hash.parse::<iroh_blobs::Hash>()
        .map_err(|_| Error::new("invalid_revision", "Invalid object hash."))?;
    Ok(storage.join("objects").join(hash))
}
pub fn freeze(storage: &Path, path: &Path) -> Result<String> {
    files::regular(path)?;
    fs::create_dir_all(storage.join("objects"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(storage.join("objects"))?;
    let mut from = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        crate::check_cancel()?;
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        tmp.write_all(&buf[..n])?;
    }
    tmp.as_file().sync_all()?;
    let hash = iroh_blobs::Hash::from_bytes(*h.finalize().as_bytes()).to_string();
    let dest = object_path(storage, &hash)?;
    if dest.exists() {
        if blob_hash(&dest)? != hash {
            return Err(Error::new(
                "object_corrupt",
                "Cached object differs; preserve it and retry after removing the corrupt object.",
            ));
        }
    } else {
        tmp.persist_noclobber(&dest)
            .map_err(|e| Error::from(e.error))?;
        files::sync_dir(dest.parent().unwrap())?;
    }
    Ok(hash)
}
pub fn blob_hash(path: &Path) -> Result<String> {
    files::regular(path)?;
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        crate::check_cancel()?;
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(iroh_blobs::Hash::from_bytes(*h.finalize().as_bytes()).to_string())
}

/// A pack file (`.smspack`): the pack and its signed head, for packs friends can sync without the host
/// online (every file from the base game or a download).
pub fn export(path: &Path, pack: &Pack, head: &Head) -> Result<()> {
    head.validate()?;
    if head.revision != pack.id()? {
        return Err(Error::new("invalid_pack", "Head does not pin this pack."));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut z = zip::ZipWriter::new(tmp.as_file_mut());
        for (name, bytes) in [
            ("pack.json", serde_json::to_vec(pack)?),
            ("head.json", serde_json::to_vec(head)?),
        ] {
            z.start_file(name, zip::write::SimpleFileOptions::default())?;
            z.write_all(&bytes)?;
        }
        z.finish()?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| Error::from(e.error))?;
    Ok(())
}
pub fn read_file(path: &Path) -> Result<(Pack, Head)> {
    files::regular(path)?;
    let mut z = zip::ZipArchive::new(File::open(path)?)?;
    let names = z.file_names().map(str::to_owned).collect::<BTreeSet<_>>();
    if names
        != ["head.json", "pack.json"]
            .map(String::from)
            .into_iter()
            .collect()
    {
        return Err(Error::new("invalid_pack", "Unexpected pack file entries."));
    }
    let mut read = |name: &str, limit: u64| -> Result<Vec<u8>> {
        let mut bytes = vec![];
        z.by_name(name)?.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(Error::new(
                "invalid_pack",
                "Pack file entry exceeds its limit.",
            ));
        }
        Ok(bytes)
    };
    let pack: Pack = serde_json::from_slice(&read("pack.json", files::MAX_JSON)?)?;
    let head: Head = serde_json::from_slice(&read("head.json", 16384)?)?;
    head.validate()?;
    if pack.id()? != head.revision {
        return Err(Error::new(
            "invalid_pack",
            "Pack differs from its signed head.",
        ));
    }
    Ok((pack, head))
}

/// The host's private half of its channel: never leaves the host's machine.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateChannel {
    pub schema: u32,
    pub endpoint_key: String,
    pub signing_key: String,
    pub pack: String,
    pub relay: Option<String>,
}
impl PrivateChannel {
    /// Loads the host's channel, making one on first use.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return files::read_json(path);
        }
        use rand::RngCore;
        let mut keys = [[0u8; 32]; 3];
        for k in &mut keys {
            rand::rngs::OsRng.fill_bytes(k);
        }
        let c = Self {
            schema: 1,
            endpoint_key: URL_SAFE_NO_PAD.encode(keys[0]),
            signing_key: URL_SAFE_NO_PAD.encode(keys[1]),
            pack: files::hash_bytes(&keys[2]),
            relay: None,
        };
        files::atomic_json(path, &c)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(c)
    }
    pub fn signing_key(&self) -> Result<SigningKey> {
        Ok(SigningKey::from_bytes(&decode32(&self.signing_key)?))
    }
    pub fn channel(&self) -> Result<Channel> {
        let key = iroh::SecretKey::from_bytes(&decode32(&self.endpoint_key)?);
        Ok(Channel {
            schema: 4,
            endpoint: key.public().to_string(),
            publisher: URL_SAFE_NO_PAD.encode(self.signing_key()?.verifying_key().to_bytes()),
            pack: self.pack.clone(),
            relay: self.relay.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_signed_head_round_trips_through_a_pack_file_and_a_code() {
        let dir = tempfile::tempdir().unwrap();
        let private = PrivateChannel::load_or_create(&dir.path().join("c.json")).unwrap();
        let channel = private.channel().unwrap();
        assert_eq!(
            Channel::parse(&channel.code().unwrap()).unwrap().pack,
            channel.pack
        );
        let pack = crate::pack::sample();
        let head = Head::sign(
            channel,
            1,
            pack.id().unwrap(),
            None,
            &private.signing_key().unwrap(),
        )
        .unwrap();
        export(&dir.path().join("p.smspack"), &pack, &head).unwrap();
        let (read, h) = read_file(&dir.path().join("p.smspack")).unwrap();
        assert_eq!(read, pack);
        assert_eq!(h.sequence, 1);
    }
    #[test]
    fn a_tampered_pack_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let private = PrivateChannel::load_or_create(&dir.path().join("c.json")).unwrap();
        let pack = crate::pack::sample();
        let head = Head::sign(
            private.channel().unwrap(),
            1,
            pack.id().unwrap(),
            None,
            &private.signing_key().unwrap(),
        )
        .unwrap();
        let mut other = pack.clone();
        other.name = "Other".into();
        assert!(export(&dir.path().join("p.smspack"), &other, &head).is_err());
    }
}
