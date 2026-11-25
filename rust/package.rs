//! Data-only packages. Executable backend identity is selected by this binary.
use crate::{
    files::{self, Identity, MAX_JSON},
    snapshot::Snapshot,
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub sha256: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: String,
    pub path: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Instruction {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub source: Source,
}
impl Instruction {
    pub fn identity(&self) -> Identity {
        Identity {
            path: self.path.clone(),
            size: self.size,
            sha256: self.sha256.clone(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceHint {
    pub name: String,
    pub kind: String,
    pub url: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub name: String,
    pub artifact: Artifact,
    pub files: Vec<Instruction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}
#[derive(Debug)]
pub struct Package {
    pub path: PathBuf,
    pub manifest: Manifest,
}
pub fn public_url(s: &str, cdn: bool) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(s)
        .map_err(|_| Error::new("invalid_url", "Expected a public HTTPS URL."))?;
    if s.len() > 8192
        || u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.host_str().is_none()
        || cdn && u.host_str() != Some("authored-files.wabbajack.org")
    {
        return Err(Error::new(
            "invalid_url",
            "Use a public HTTPS URL without credentials, query parameters or fragments.",
        ));
    }
    Ok(u)
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        if ![1, 2].contains(&self.schema)
            || self.name.is_empty()
            || self.name.len() > 200
            || self.files.is_empty()
            || self.files.len() > files::MAX_FILES
            || self.artifact.size > 8 * 1024u64.pow(3)
            || !files::digest_valid(&self.artifact.sha256)
        {
            return Err(Error::new(
                "invalid_package",
                "Invalid package schema, name, recipe identity or file count.",
            ));
        }
        if let Some(u) = &self.artifact.url {
            public_url(u, true)?;
        }
        let mut paths = BTreeSet::new();
        let mut sources = std::collections::BTreeMap::new();
        for f in &self.files {
            files::relative(&f.path)?;
            files::relative(&f.source.path)?;
            if !f.path.starts_with("game/")
                && !f.path.starts_with("config/documents/")
                && !f.path.starts_with("config/local/")
                || !files::digest_valid(&f.sha256)
                || f.size > 1024u64.pow(4)
                || !["base", "wabbajack"].contains(&f.source.kind.as_str())
            {
                return Err(Error::new("invalid_package", "Invalid file instruction."));
            }
            if !paths.insert(files::fold(&f.path)) {
                return Err(Error::new("case_collision", "Duplicate target path."));
            }
            let k = (f.source.kind.clone(), files::fold(&f.source.path));
            if let Some(old) = sources.insert(k, (f.size, f.sha256.clone())) {
                if old != (f.size, f.sha256.clone()) {
                    return Err(Error::new(
                        "invalid_package",
                        "Conflicting source identities.",
                    ));
                }
            }
        }
        for p in &paths {
            let mut p = p.as_str();
            while let Some((parent, _)) = p.rsplit_once('/') {
                if paths.contains(parent) {
                    return Err(Error::new(
                        "invalid_package",
                        "A target path is both file and directory.",
                    ));
                }
                p = parent;
            }
        }
        if !paths.contains("game/skyrimse.exe") || !paths.contains("game/skse64_loader.exe") {
            return Err(Error::new(
                "invalid_package",
                "Expected SkyrimSE.exe and skse64_loader.exe.",
            ));
        }
        if self.sources.len() > 10000 {
            return Err(Error::new("invalid_package", "Too many source hints."));
        }
        for h in &self.sources {
            if h.name.len() > 4096 || h.kind.len() > 4096 || h.url.len() > 4096 {
                return Err(Error::new("invalid_package", "Source hint too long."));
            }
            if !h.url.is_empty() {
                public_url(&h.url, false)?;
            }
        }
        if self.schema == 1 {
            if self.snapshot.is_some() || self.profile.is_some() || self.layout.is_some() {
                return Err(Error::new(
                    "invalid_package",
                    "Legacy recipe-v1 cannot carry sync-v2 fields.",
                ));
            }
        } else {
            let snapshot = self
                .snapshot
                .as_ref()
                .ok_or_else(|| Error::new("invalid_package", "Missing host snapshot."))?;
            snapshot.validate()?;
            let want = snapshot
                .files
                .iter()
                .map(|f| (files::fold(&f.path), (f.size, f.sha256.clone())))
                .collect::<std::collections::BTreeMap<_, _>>();
            let have = self
                .files
                .iter()
                .map(|f| (files::fold(&f.path), (f.size, f.sha256.clone())))
                .collect::<std::collections::BTreeMap<_, _>>();
            if want != have || !snapshot.warnings.is_empty() {
                return Err(Error::new("invalid_package","Snapshot disagrees with instructions or has unresolved configuration/plugin diagnostics."));
            }
            let p = self
                .profile
                .as_deref()
                .ok_or_else(|| Error::new("invalid_package", "Missing selected profile."))?;
            files::relative(p)?;
            if p.contains('/') || self.layout.as_deref() != Some("stock-mo2") {
                return Err(Error::new(
                    "unsupported_layout",
                    "Sync-v2 currently supports the stock-game profile adapter.",
                ));
            }
        }
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        if let Some(snapshot) = &self.snapshot {
            return snapshot.id();
        }
        let mut identities = self
            .files
            .iter()
            .map(Instruction::identity)
            .collect::<Vec<_>>();
        identities.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files::hash_bytes(&serde_json::to_vec(
            &serde_json::to_value(identities)?,
        )?))
    }
}
pub fn zip_bytes<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name)?;
    if entry.size() > limit || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
        return Err(Error::new(
            "invalid_package",
            "Linked or oversized package entry.",
        ));
    }
    let mut data = Vec::new();
    entry.by_ref().take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(Error::new(
            "invalid_package",
            "Expanded entry exceeds limit.",
        ));
    }
    Ok(data)
}
pub fn load(path: &Path) -> Result<Package> {
    files::regular(path)?;
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let names = zip.file_names().map(str::to_owned).collect::<BTreeSet<_>>();
    if zip.len() != names.len()
        || !names.contains("manifest.json")
        || names
            .iter()
            .any(|n| n != "manifest.json" && n != "recipe.wabbajack")
    {
        return Err(Error::new(
            "invalid_package",
            "Expected manifest.json and optional recipe.wabbajack only.",
        ));
    }
    let manifest: Manifest =
        serde_json::from_slice(&zip_bytes(&mut zip, "manifest.json", MAX_JSON)?)?;
    manifest.validate()?;
    if manifest.artifact.url.is_some() == names.contains("recipe.wabbajack") {
        return Err(Error::new(
            "invalid_package",
            "Recipe must be embedded or URL-addressed, exclusively.",
        ));
    }
    if names.contains("recipe.wabbajack")
        && zip.by_name("recipe.wabbajack")?.size() != manifest.artifact.size
    {
        return Err(Error::new(
            "artifact_mismatch",
            "Embedded recipe size disagrees with manifest.",
        ));
    }
    Ok(Package {
        path: files::root(path)?,
        manifest,
    })
}
pub fn write(path: &Path, manifest: &Manifest, artifact: &Path) -> Result<()> {
    manifest.validate()?;
    if fs::metadata(artifact)?.len() != manifest.artifact.size
        || files::sha(artifact)? != manifest.artifact.sha256
    {
        return Err(Error::new(
            "artifact_mismatch",
            "Recipe changed before publication.",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut zip = ZipWriter::new(temp.as_file_mut());
        zip.start_file(
            "manifest.json",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )?;
        zip.write_all(&serde_json::to_vec(manifest)?)?;
        if manifest.artifact.url.is_none() {
            zip.start_file("recipe.wabbajack", SimpleFileOptions::default())?;
            let mut src = File::open(artifact)?;
            let mut buf = vec![0; 1024 * 1024];
            loop {
                crate::check_cancel()?;
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buf[..n])?;
            }
        }
        zip.finish()?;
    }
    temp.as_file().sync_all()?;
    load(temp.path())?;
    if files::sha(artifact)? != manifest.artifact.sha256 {
        return Err(Error::new(
            "source_changed",
            "Recipe changed during packaging.",
        ));
    }
    temp.persist_noclobber(path)
        .map_err(|e| Error::from(e.error))?;
    files::sync_dir(parent)?;
    Ok(())
}
impl Package {
    pub fn extract(&self, to: &Path) -> Result<()> {
        let a = &self.manifest.artifact;
        if to.is_file() && fs::metadata(to)?.len() == a.size && files::sha(to)? == a.sha256 {
            return Ok(());
        }
        fs::create_dir_all(to.parent().unwrap())?;
        if let Some(url) = &a.url {
            crate::acquisition::download_recipe(url, to, a)?;
        } else {
            let mut zip = ZipArchive::new(File::open(&self.path)?)?;
            let mut src = zip.by_name("recipe.wabbajack")?;
            let mut temp = tempfile::NamedTempFile::new_in(to.parent().unwrap())?;
            let mut count = 0;
            let mut buf = vec![0; 1024 * 1024];
            loop {
                crate::check_cancel()?;
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                count += n as u64;
                if count > a.size {
                    return Err(Error::new(
                        "artifact_mismatch",
                        "Recipe exceeds declared size.",
                    ));
                }
                temp.write_all(&buf[..n])?;
            }
            if count != a.size || files::sha(temp.path())? != a.sha256 {
                return Err(Error::new(
                    "artifact_mismatch",
                    "Embedded recipe hash mismatch.",
                ));
            }
            temp.as_file().sync_all()?;
            temp.persist(to).map_err(|e| Error::from(e.error))?;
        }
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Code {
    pub schema: u32,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}
impl Code {
    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        use base64::Engine;
        Ok(format!(
            "SMS2.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub fn decode(code: &str) -> Result<Self> {
        use base64::Engine;
        if code.len() > 16384 {
            return Err(Error::new("invalid_code", "Code too long."));
        }
        let body = code.strip_prefix("SMS2.").ok_or_else(|| {
            Error::new(
                "invalid_code",
                "Expected SMS2 package code; legacy SMS1 recipe codes are not sync targets.",
            )
        })?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| Error::new("invalid_code", "Invalid code encoding."))?;
        let data: Self = serde_json::from_slice(&bytes)?;
        data.validate()?;
        Ok(data)
    }
    fn validate(&self) -> Result<()> {
        public_url(&self.url, false)?;
        if self.schema != 2
            || !files::digest_valid(&self.sha256)
            || self.size == 0
            || self.size > 9 * 1024u64.pow(3)
        {
            return Err(Error::new(
                "invalid_code",
                "Invalid pinned package identity.",
            ));
        }
        Ok(())
    }
}
