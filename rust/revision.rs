//! Versioned editable packs. Public data never contains host paths or credentials.
use crate::{
    files,
    package::{Artifact, Instruction, Manifest, Source as LegacySource},
    snapshot::Snapshot,
    target::Storage,
    Error, Result,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Base { path: String },
    Recipe { path: String },
    Blob { hash: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub source: Source,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub schema: u32,
    pub name: String,
    pub snapshot: Snapshot,
    pub files: Vec<Entry>,
    pub recipe: Option<Artifact>,
    pub policy: String,
}
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
        if self.schema != 3 || !files::digest_valid(&self.pack) {
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
            "SMS3.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() > 16384 {
            return Err(Error::new("invalid_code", "Code exceeds limit."));
        }
        let raw = URL_SAFE_NO_PAD
            .decode(
                s.strip_prefix("SMS3.")
                    .ok_or_else(|| Error::new("invalid_code", "Expected SMS3 code."))?,
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
        &json!({"domain":"SkyrimModSync/channel-head/v3","schema":h.schema,"channel":h.channel,"sequence":h.sequence,"revision":h.revision,"parent":h.parent}),
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
            schema: 3,
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
        if self.schema != 3
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
        key.verify(&signed_bytes(self)?, &sig).map_err(|_| {
            Error::new(
                "invalid_signature",
                "Revision was not signed by its publisher.",
            )
        })
    }
}
impl Revision {
    pub fn validate(&self) -> Result<()> {
        self.snapshot.validate()?;
        if self.schema != 3
            || self.name.is_empty()
            || self.name.len() > 200
            || self.policy != "isolated-personal-v1"
            || !self.snapshot.warnings.is_empty()
        {
            return Err(Error::new(
                "invalid_revision",
                "Unsupported or incomplete revision.",
            ));
        }
        let ids = self
            .files
            .iter()
            .map(|e| (files::fold(&e.path), (e.size, e.sha256.clone())))
            .collect::<BTreeMap<_, _>>();
        let want = self
            .snapshot
            .files
            .iter()
            .map(|e| (files::fold(&e.path), (e.size, e.sha256.clone())))
            .collect::<BTreeMap<_, _>>();
        if ids.len() != self.files.len() || ids != want {
            return Err(Error::new(
                "invalid_revision",
                "Revision files disagree with its snapshot.",
            ));
        }
        for e in &self.files {
            match &e.source {
                Source::Base { path } | Source::Recipe { path } => files::relative(path)?,
                Source::Blob { hash } => {
                    hash.parse::<iroh_blobs::Hash>()
                        .map_err(|_| Error::new("invalid_revision", "Invalid blob hash."))?;
                    if protected_base(&e.path) {
                        return Err(Error::new(
                            "base_provenance_required",
                            format!(
                                "Licensed game content cannot be supplied as a peer blob: {}",
                                e.path
                            ),
                        ));
                    }
                }
            }
            if matches!(e.source, Source::Recipe { .. }) && self.recipe.is_none() {
                return Err(Error::new(
                    "invalid_revision",
                    "Recipe source has no pinned recipe.",
                ));
            }
        }
        if let Some(a) = &self.recipe {
            if !files::digest_valid(&a.sha256) || a.size > 8 * 1024u64.pow(3) || a.url.is_some() {
                return Err(Error::new(
                    "invalid_revision",
                    "Invalid embedded recipe identity.",
                ));
            }
        }
        self.legacy().validate()?;
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(files::hash_bytes(&serde_json::to_vec(
            &serde_json::to_value(self)?,
        )?))
    }
    /// Internal comparison adapter only; never used to acquire v3 source bytes.
    pub fn legacy(&self) -> Manifest {
        Manifest {
            schema: 2,
            name: self.name.clone(),
            artifact: self.recipe.clone().unwrap_or(Artifact {
                sha256: "0".repeat(64),
                size: 0,
                url: None,
            }),
            files: self
                .files
                .iter()
                .map(|e| Instruction {
                    path: e.path.clone(),
                    size: e.size,
                    sha256: e.sha256.clone(),
                    source: LegacySource {
                        kind: "base".into(),
                        path: e.path.clone(),
                    },
                })
                .collect(),
            sources: vec![],
            snapshot: Some(self.snapshot.clone()),
            profile: Some("Revision".into()),
            layout: Some("stock-mo2".into()),
        }
    }
    pub fn objects(&self) -> BTreeSet<String> {
        self.files
            .iter()
            .filter_map(|e| {
                if let Source::Blob { hash } = &e.source {
                    Some(hash.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}
pub fn protected_base(path: &str) -> bool {
    let p = files::fold(path);
    let name = p.strip_prefix("game/").unwrap_or(&p);
    [
        "skyrimse.exe",
        "skyrimseLauncher.exe",
        "steam_api64.dll",
        "bink2w64.dll",
    ]
    .iter()
    .any(|n| files::fold(n) == name)
        || ["skyrim", "update", "dawnguard", "hearthfires", "dragonborn"]
            .iter()
            .any(|n| {
                name == format!("data/{n}.esm")
                    || name == format!("data/{n}.bsa")
                    || name.starts_with(&format!("data/{n} - ")) && name.ends_with(".bsa")
            })
        || name.starts_with("data/cc")
            && (name.ends_with(".esm") || name.ends_with(".esl") || name.ends_with(".bsa"))
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
pub fn matches(path: &Path, e: &Entry) -> Result<bool> {
    Ok(fs::metadata(path)?.len() == e.size && files::sha(path)? == e.sha256)
}
pub fn save(storage: &Path, rev: &Revision) -> Result<PathBuf> {
    let p = storage
        .join("revisions")
        .join(format!("{}.json", rev.id()?));
    files::atomic_json(&p, rev)?;
    Ok(p)
}
pub fn load(path: &Path) -> Result<Revision> {
    let r: Revision = files::read_json(path)?;
    r.validate()?;
    Ok(r)
}
pub fn installed(storage: &Storage) -> Result<Revision> {
    let receipt: Value = files::read_json(&storage.instance().join(".skyrimsync-install.json"))?;
    let id = receipt["target_id"]
        .as_str()
        .ok_or_else(|| Error::new("unowned_destination", "Missing receipt identity."))?;
    if let Ok(r) = load(&storage.root.join("installed-revision.json")) {
        if r.snapshot.id()? == id {
            return Ok(r);
        }
    }
    for e in fs::read_dir(storage.root.join("revisions"))? {
        let p = e?.path();
        if p.extension().is_some_and(|e| e == "json") {
            let r = load(&p)?;
            if r.snapshot.id()? == id {
                files::atomic_json(&storage.root.join("installed-revision.json"), &r)?;
                return Ok(r);
            }
        }
    }
    Err(Error::new(
        "revision_missing",
        "No verified revision matches the current receipt. Retry sync or import.",
    ))
}
pub fn is_package(path: &Path) -> bool {
    File::open(path)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .is_some_and(|mut z| z.by_name("revision.json").is_ok())
}
pub fn export(path: &Path, rev: &Revision, head: &Head, storage: &Path) -> Result<()> {
    rev.validate()?;
    head.validate()?;
    if head.revision != rev.id()? {
        return Err(Error::new(
            "invalid_revision",
            "Head does not pin this export.",
        ));
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
            ("revision.json", serde_json::to_vec(rev)?),
            ("head.json", serde_json::to_vec(head)?),
        ] {
            z.start_file(name, zip::write::SimpleFileOptions::default())?;
            z.write_all(&bytes)?;
        }
        if let Some(a) = &rev.recipe {
            z.start_file("recipe.wabbajack", zip::write::SimpleFileOptions::default())?;
            let p = storage
                .join("recipes")
                .join(format!("{}.wabbajack", a.sha256));
            let mut src = File::open(&p)?;
            if files::sha(&p)? != a.sha256 {
                return Err(Error::new("artifact_mismatch", "Pinned recipe differs."));
            }
            std::io::copy(&mut src, &mut z)?;
        }
        z.finish()?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist_noclobber(path)
        .map_err(|e| Error::from(e.error))?;
    files::sync_dir(parent)
}
pub fn read_package(path: &Path, storage: &Path) -> Result<(Revision, Head)> {
    files::regular(path)?;
    let mut z = zip::ZipArchive::new(File::open(path)?)?;
    let names = z.file_names().map(str::to_owned).collect::<BTreeSet<_>>();
    if names.len() != z.len()
        || names
            .iter()
            .any(|n| !["revision.json", "head.json", "recipe.wabbajack"].contains(&n.as_str()))
    {
        return Err(Error::new(
            "invalid_package",
            "Unexpected revision package entries.",
        ));
    }
    let rev: Revision = serde_json::from_slice(&crate::package::zip_bytes(
        &mut z,
        "revision.json",
        files::MAX_JSON,
    )?)?;
    let head: Head =
        serde_json::from_slice(&crate::package::zip_bytes(&mut z, "head.json", 16384)?)?;
    rev.validate()?;
    head.validate()?;
    if rev.id()? != head.revision {
        return Err(Error::new(
            "invalid_revision",
            "Export differs from signed revision.",
        ));
    }
    if let Some(a) = &rev.recipe {
        let mut src = z.by_name("recipe.wabbajack")?;
        if src.size() != a.size {
            return Err(Error::new(
                "artifact_mismatch",
                "Wrong embedded recipe size.",
            ));
        }
        fs::create_dir_all(storage.join("recipes"))?;
        let mut tmp = tempfile::NamedTempFile::new_in(storage.join("recipes"))?;
        std::io::copy(&mut src.by_ref().take(a.size + 1), &mut tmp)?;
        tmp.as_file().sync_all()?;
        if tmp.as_file().metadata()?.len() != a.size || files::sha(tmp.path())? != a.sha256 {
            return Err(Error::new("artifact_mismatch", "Embedded recipe differs."));
        }
        tmp.persist(
            storage
                .join("recipes")
                .join(format!("{}.wabbajack", a.sha256)),
        )
        .map_err(|e| Error::from(e.error))?;
    } else if names.contains("recipe.wabbajack") {
        return Err(Error::new("invalid_package", "Undeclared recipe."));
    }
    save(storage, &rev)?;
    Ok((rev, head))
}

pub(crate) struct InstallPlan {
    _lock: files::Lock,
    root: PathBuf,
    revision: String,
    target: String,
    existed: bool,
    pub(crate) changed: BTreeSet<String>,
    removed: Vec<String>,
    unchanged: usize,
    bytes: u64,
}
pub(crate) fn prepare_install(storage: &Storage, rev: &Revision) -> Result<InstallPlan> {
    rev.validate()?;
    let lock = files::Lock::take(&storage.root.join("locks/target.lock"))?;
    crate::target::running(&storage.instance())?;
    storage.recover()?;
    storage.owned_instance()?;
    save(&storage.root, rev)?;
    let target = rev.snapshot.id()?;
    let legacy = rev.legacy();
    let existed = storage.instance().exists();
    let (changed, removed, unchanged, bytes) = if existed {
        let diff = crate::repair::diff(&legacy, &storage.instance())?;
        (
            diff.changed
                .iter()
                .map(|f| f.path.clone())
                .collect::<BTreeSet<_>>(),
            diff.removed,
            diff.unchanged,
            diff.bytes,
        )
    } else {
        (
            rev.files.iter().map(|f| f.path.clone()).collect(),
            vec![],
            0,
            rev.files.iter().try_fold(0_u64, |total, e| {
                total
                    .checked_add(e.size)
                    .ok_or_else(|| Error::new("invalid_revision", "Revision size overflows."))
            })?,
        )
    };
    Ok(InstallPlan {
        _lock: lock,
        root: storage.root.clone(),
        revision: rev.id()?,
        target,
        existed,
        changed,
        removed,
        unchanged,
        bytes,
    })
}
pub fn install(
    storage: &Storage,
    rev: &Revision,
    resolve: impl Fn(&Entry) -> Result<PathBuf>,
) -> Result<Value> {
    let plan = prepare_install(storage, rev)?;
    install_prepared(storage, rev, &plan, resolve)
}
pub(crate) fn install_prepared(
    storage: &Storage,
    rev: &Revision,
    plan: &InstallPlan,
    resolve: impl Fn(&Entry) -> Result<PathBuf>,
) -> Result<Value> {
    if plan.root != storage.root || plan.revision != rev.id()? {
        return Err(Error::new(
            "invalid_install_plan",
            "Install plan does not match the locked revision/storage.",
        ));
    }
    // Keep the lock through acquisition, staging, commit and final verification.
    let target = &plan.target;
    let existed = plan.existed;
    let changed = &plan.changed;
    let removed = &plan.removed;
    let unchanged = plan.unchanged;
    let bytes = plan.bytes;
    let legacy = rev.legacy();
    fs::create_dir_all(storage.root.join("stages"))?;
    let stage = tempfile::tempdir_in(storage.root.join("stages"))?;
    files::space(&storage.root, bytes)?;
    for e in rev.files.iter().filter(|e| changed.contains(&e.path)) {
        let src = resolve(e)?;
        if !matches(&src, e)? {
            return Err(Error::new(
                "source_mismatch",
                format!(
                    "Source differs for {}. Previous installation is retained.",
                    e.path
                ),
            ));
        }
        files::copy(&src, &stage.path().join(&e.path))?;
        if !matches(&stage.path().join(&e.path), e)? {
            return Err(Error::new(
                "staging_mismatch",
                format!("Copied source changed: {}", e.path),
            ));
        }
    }
    if existed && !changed.is_empty() || !removed.is_empty() {
        let mut actions = changed
            .iter()
            .map(|p| (p.clone(), Some(stage.path().join(p))))
            .collect::<Vec<_>>();
        actions.extend(removed.iter().map(|p| (p.clone(), None)));
        crate::repair::transact(storage, actions, target, || {
            if crate::target::verify(&legacy, &storage.instance())?["files_match"] != true {
                return Err(Error::new("repair_mismatch", "Revision did not verify."));
            }
            Ok(())
        })?;
    } else if !existed {
        if crate::target::verify(&legacy, stage.path())?["files_match"] != true {
            return Err(Error::new(
                "staging_mismatch",
                "Imported revision is incomplete.",
            ));
        }
        files::atomic_json(
            &stage.path().join(".skyrimsync-install.json"),
            &json!({"schema":1,"target_id":target}),
        )?;
        storage.publish_stage(stage.path(), target)?;
    } else if crate::target::verify(&legacy, &storage.instance())?["files_match"] != true {
        return Err(Error::new(
            "instance_mismatch",
            "Current revision does not verify.",
        ));
    }
    files::atomic_json(&storage.root.join("installed-revision.json"), rev)?;
    Ok(
        json!({"state":"files_verified","revision":rev.id()?,"target_id":target,"files_replaced":changed.len(),"files_removed":removed.len(),"files_unchanged":unchanged,"staged_bytes":bytes,"incremental":existed,"runtime_verified":false}),
    )
}
