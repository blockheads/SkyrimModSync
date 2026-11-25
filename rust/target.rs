use crate::{
    files::{self, Lock},
    package::{Manifest, Package},
    recipe::Recipe,
    snapshot, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Debug)]
pub struct Storage {
    pub root: PathBuf,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    schema: u32,
    kind: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    target_id: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    stage: String,
    backup: String,
    target_id: String,
}
impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            fs::create_dir_all(path)?;
        }
        let root = files::root(path)?;
        let marker = root.join(".skyrimsync-owner.json");
        if marker.exists() {
            let owner: Owner = files::read_json(&marker)?;
            if owner.schema != 1 || owner.kind != "skyrim-mod-sync-storage" {
                return Err(Error::new(
                    "unowned_destination",
                    "Folder is not owned by SkyrimModSync.",
                ));
            }
        } else {
            if fs::read_dir(&root)?.next().is_some() {
                return Err(Error::new("unowned_destination","Co-op storage is nonempty and has no ownership marker. Select a new empty --coop folder; existing Skyrim installations can be compared without being modified."));
            }
            let _lock = Lock::take(&root.join("ownership.lock"))?;
            files::atomic_json(
                &marker,
                &Owner {
                    schema: 1,
                    kind: "skyrim-mod-sync-storage".into(),
                },
            )?;
        }
        for p in [
            "locks",
            "packages",
            "recipes",
            "jobs",
            "tools",
            "engine-config",
            "engine-data",
            "backups",
            "stages",
            "instance",
            "revisions",
            "objects",
            "runtime",
            "personal",
            "peer",
            "published",
        ] {
            if root
                .join(p)
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(Error::new(
                    "path_escape",
                    format!("Managed {p} must not be a symbolic link."),
                ));
            }
        }
        Ok(Self { root })
    }
    pub fn instance(&self) -> PathBuf {
        self.root.join("instance")
    }
    pub(crate) fn owned_instance(&self) -> Result<()> {
        if self.instance().exists() {
            validate_receipt(&self.instance())?;
        }
        Ok(())
    }
    pub fn recover(&self) -> Result<()> {
        let path = self.root.join("transaction.json");
        if !path.exists() {
            return Ok(());
        }
        let raw: Value = files::read_json(&path)?;
        if raw["schema"] == 2 {
            return crate::repair::recover(self);
        }
        let j: Journal = serde_json::from_value(raw)?;
        if j.schema != 1
            || !j.backup.starts_with("backups/")
            || !(j.stage.starts_with("stages/") || j.stage.starts_with("backups/"))
            || !files::digest_valid(&j.target_id)
        {
            return Err(Error::new(
                "invalid_transaction",
                "Invalid recovery journal. Leave backups intact and inspect the managed folder.",
            ));
        }
        files::relative(&j.stage)?;
        files::relative(&j.backup)?;
        let instance = self.instance();
        let backup = self.root.join(&j.backup);
        let stage = self.root.join(&j.stage);
        if instance.exists() {
            let receipt = validate_receipt(&instance)?;
            // Rename publication already completed, or the old instance never moved.
            if receipt.target_id != j.target_id && backup.exists() {
                return Err(Error::new("recovery_required","Both an old instance and backup exist unexpectedly; preserve them and inspect transaction.json."));
            }
        } else if backup.exists() {
            validate_receipt(&backup)?;
            fs::rename(&backup, &instance)?;
            files::sync_dir(backup.parent().unwrap())?;
            files::sync_dir(&self.root)?;
        } else if stage.exists() {
            validate_receipt(&stage)?; /* Nothing was published. A new run can stage again. */
        }
        fs::remove_file(path)?;
        files::sync_dir(&self.root)?;
        Ok(())
    }
    pub fn rollback(&self) -> Result<Value> {
        let _lock = Lock::take(&self.root.join("locks/target.lock"))?;
        running(&self.instance())?;
        self.recover()?;
        self.owned_instance()?;
        let backupdir = self.root.join("backups");
        let mut entries = if backupdir.is_dir() {
            fs::read_dir(&backupdir)?.collect::<std::io::Result<Vec<_>>>()?
        } else {
            vec![]
        };
        entries.retain(|e| {
            e.path().join(".skyrimsync-install.json").is_file()
                || e.path().join(".skyrimsync-delta.json").is_file()
        });
        entries.sort_by_key(|e| e.file_name());
        let last = entries.last().ok_or_else(|| {
            Error::new("backup_missing", "No previous installation is available.")
        })?;
        let previous = last.path();
        if previous.join(".skyrimsync-delta.json").exists() {
            return crate::repair::rollback(self, &previous);
        }
        let receipt = validate_receipt(&previous)?;
        self.publish_stage(&previous, &receipt.target_id)?;
        Ok(
            json!({"state":"rolled_back","target_id":receipt.target_id,"instance":self.instance(),"runtime_verified":false}),
        )
    }
    pub(crate) fn publish_stage(&self, stage: &Path, id: &str) -> Result<()> {
        self.owned_instance()?;
        running(&self.instance())?;
        crate::check_cancel()?;
        fs::create_dir_all(self.root.join("backups"))?;
        let backup = self.root.join("backups").join(unique());
        let journal = Journal {
            schema: 1,
            stage: stage
                .strip_prefix(&self.root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
            backup: backup
                .strip_prefix(&self.root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
            target_id: id.to_owned(),
        };
        files::atomic_json(&self.root.join("transaction.json"), &journal)?;
        if self.instance().exists() {
            fs::rename(self.instance(), &backup)?;
            files::sync_dir(backup.parent().unwrap())?;
            files::sync_dir(&self.root)?;
        }
        if let Err(e) = fs::rename(stage, self.instance()) {
            if backup.exists() {
                fs::rename(backup, self.instance())?;
            }
            return Err(e.into());
        }
        files::sync_dir(stage.parent().unwrap())?;
        files::sync_dir(&self.root)?;
        fs::remove_file(self.root.join("transaction.json"))?;
        files::sync_dir(&self.root)?;
        Ok(())
    }
}
pub(crate) fn unique() -> String {
    format!(
        "{:020}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    )
}
fn validate_receipt(root: &Path) -> Result<Receipt> {
    files::root(root)?;
    let path = root.join(".skyrimsync-install.json");
    if !path.is_file() {
        return Err(Error::new(
            "unowned_destination",
            "Managed instance has no ownership receipt; refusing replacement.",
        ));
    }
    let r: Receipt = files::read_json(&path)?;
    if r.schema != 1 || !files::digest_valid(&r.target_id) {
        return Err(Error::new(
            "unowned_destination",
            "Invalid installation ownership receipt.",
        ));
    }
    Ok(r)
}
pub fn running(root: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if !root.exists() {
            return Ok(());
        }
        let canonical = root.canonicalize()?;
        let literal = canonical.to_string_lossy();
        for p in fs::read_dir("/proc")?.filter_map(|e| e.ok()).filter(|p| {
            p.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        }) {
            if p.file_name() == std::process::id().to_string().as_str() {
                continue;
            }
            let maps = fs::read_to_string(p.path().join("maps")).unwrap_or_default();
            let cwd = fs::read_link(p.path().join("cwd")).ok();
            let fd = fs::read_dir(p.path().join("fd"))
                .ok()
                .is_some_and(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| fs::read_link(e.path()).is_ok_and(|v| v.starts_with(&canonical)))
                });
            let command = fs::read(p.path().join("cmdline")).unwrap_or_default();
            let command = String::from_utf8_lossy(&command);
            if maps.lines().any(|l| l.contains(literal.as_ref()))
                || cwd.is_some_and(|p| p.starts_with(&canonical))
                || fd
                || command.contains(&format!("{literal}/game/SkyrimSE.exe"))
            {
                return Err(Error::new("target_running",format!("Process {} is using the managed instance. Close Skyrim and related tools before repair or rollback.",p.file_name().to_string_lossy())));
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Err(Error::new("repair_unsupported","Running-game detection and transactional repair are currently validated on Linux only."))
    }
}
pub fn verify(manifest: &Manifest, root: &Path) -> Result<Value> {
    manifest.validate()?;
    if let Some(want) = &manifest.snapshot {
        if !root.join("game").is_dir() {
            return Ok(
                json!({"state":"different","files_match":false,"runtime_verified":false,
                "expected_id":want.id()?,"actual_id":null,"warnings":[],
                "differences":[{"kind":"missing_directory","path":"game"}]}),
            );
        }
        return Ok(serde_json::to_value(snapshot::compare(
            want,
            &snapshot::capture_install(root)?,
        )?)?);
    }
    let logs = snapshot::known_logs(manifest.files.iter().map(|f| f.path.clone()));
    let mut differences = Vec::new();
    let want = manifest
        .files
        .iter()
        .map(|f| files::fold(&f.path))
        .collect::<std::collections::BTreeSet<_>>();
    for f in &manifest.files {
        let p = files::locate(root, &f.path)?;
        if let Some(p) = p {
            if fs::metadata(&p)?.len() != f.size || files::sha(&p)? != f.sha256 {
                differences.push(json!({"kind":"changed","path":f.path}));
            }
        } else {
            differences.push(json!({"kind":"missing","path":f.path}));
        }
    }
    for (n, _) in files::inventory(root)? {
        if n == ".skyrimsync-install.json" {
            continue;
        }
        if !want.contains(&files::fold(&n)) && !snapshot::ignored(&n, &logs) {
            differences.push(json!({"kind":"extra","path":n}));
        }
    }
    Ok(
        json!({"state":if differences.is_empty(){"files_match"}else{"different"},"files_match":differences.is_empty(),"runtime_verified":false,"differences":differences}),
    )
}
pub fn plan(package: &Package, base: &Path, downloads: &Path, storage: &Path) -> Result<Value> {
    plan_with_cache(package, base, downloads, storage, None)
}
pub fn plan_with_cache(
    package: &Package,
    base: &Path,
    downloads: &Path,
    storage: &Path,
    reconstructed: Option<&Path>,
) -> Result<Value> {
    if storage.join("instance/.skyrimsync-install.json").is_file() {
        return crate::repair::plan(package, base, downloads, storage, reconstructed);
    }
    let artifact = storage
        .join("recipes")
        .join(format!("{}.wabbajack", package.manifest.artifact.sha256));
    package.extract(&artifact)?;
    let recipe = Recipe::load(&artifact)?;
    let acquisition = recipe.archives(base, downloads)?;
    let current = storage.join("instance");
    let comparison = if current.join("game").is_dir() {
        verify(&package.manifest, &current)?
    } else {
        json!({"state":"not_installed","files_match":false,"runtime_verified":false})
    };
    let target_bytes = package.manifest.files.iter().try_fold(0u64, |n, f| {
        n.checked_add(f.size)
            .ok_or_else(|| Error::new("invalid_package", "Target size overflow."))
    })?;
    let reconstruction_bytes = acquisition["reconstruction_bytes"].as_u64().unwrap();
    let required = target_bytes
        .checked_add(
            reconstruction_bytes
                .checked_mul(2)
                .ok_or_else(|| Error::new("invalid_recipe", "Working size overflow."))?,
        )
        .ok_or_else(|| Error::new("invalid_recipe", "Working size overflow."))?;
    let mut blockers = Vec::new();
    if !acquisition["base_problems"].as_array().unwrap().is_empty() {
        blockers.push(json!({"reason":"base_mismatch","fix":"Select required base Skyrim edition/version with --base."}));
    }
    if !acquisition["unsupported_missing_sources"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        blockers.push(json!({"reason":"manual_downloads_required","fix":"Put listed source archives in --downloads, then retry."}));
    }
    if fs2::available_space(storage)? < required {
        blockers.push(
            json!({"reason":"disk_full","fix":"Free space or choose another --coop folder."}),
        );
    }
    let nexus = acquisition["missing"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["kind"].as_str().unwrap_or("").contains("Nexus"));
    if nexus && crate::config::load_key(false)?.is_none() {
        blockers.push(json!({"reason":"nexus_key_required","fix":"Save a private Nexus key or manually cache the listed archives."}));
    }
    Ok(
        json!({"state":"planned","target_id":package.manifest.id()?,"comparison":comparison,"acquisition":acquisition,"target_bytes":target_bytes,"working_space_bytes":required,"free_space_bytes":fs2::available_space(storage)?,"base_read_only":true,"blockers":blockers,"runtime_verified":false}),
    )
}
pub fn sync(
    package: &Package,
    storage: &Storage,
    base: &Path,
    downloads: &Path,
    steam: Option<&Path>,
    proton: Option<&Path>,
    reconstructed: Option<&Path>,
) -> Result<Value> {
    let _lock = Lock::take(&storage.root.join("locks/target.lock"))?;
    running(&storage.instance())?;
    storage.recover()?;
    storage.owned_instance()?;
    let id = package.manifest.id()?;
    files::separate(base, &storage.root)?;
    files::separate(downloads, base)?;
    files::separate(downloads, &storage.root)?;
    if storage.instance().exists() {
        return crate::repair::sync(
            package,
            storage,
            base,
            downloads,
            steam,
            proton,
            reconstructed,
        );
    }
    let bytes = package.manifest.files.iter().try_fold(0u64, |n, f| {
        n.checked_add(f.size)
            .ok_or_else(|| Error::new("invalid_package", "Target size overflow."))
    })?;
    files::space(&storage.root, bytes)?;
    fs::create_dir_all(storage.root.join("stages"))?;
    let stage = tempfile::tempdir_in(storage.root.join("stages"))?;
    let mut needs_recipe = false;
    let mut to_copy = Vec::new();
    for f in &package.manifest.files {
        crate::check_cancel()?;
        let source = if f.source.kind == "base" {
            files::locate(base, &f.source.path)?
        } else {
            None
        };
        if let Some(p) = source {
            if fs::metadata(&p)?.len() == f.size && files::sha(&p)? == f.sha256 {
                files::copy(&p, &stage.path().join(&f.path))?;
                continue;
            }
        }
        needs_recipe |= f.source.kind == "wabbajack";
        to_copy.push(f);
    }
    let mut source_tree = None;
    if needs_recipe {
        let artifact = storage
            .root
            .join("recipes")
            .join(format!("{}.wabbajack", package.manifest.artifact.sha256));
        package.extract(&artifact)?;
        let recipe = Recipe::load(&artifact)?;
        let reconstruction = if let Some(p) = reconstructed {
            files::root(p)?
        } else {
            crate::engine::reconstruct(&recipe, &storage.root, base, downloads, steam, proton)?
        };
        let problems = recipe.verify(&reconstruction)?;
        if !problems.is_empty() {
            crate::event(
                "reconstruction_differences",
                json!(problems.iter().take(50).collect::<Vec<_>>()),
            );
            return Err(Error::new("reconstruction_mismatch","Reconstructed files differ from the pinned recipe. Reconstruct again; host changes need a new recipe."));
        }
        if package.manifest.schema == 2 {
            let candidate = tempfile::tempdir_in(storage.root.join("stages"))?;
            let manifest = crate::recipe::materialize(
                &recipe,
                &reconstruction,
                package.manifest.profile.as_deref().unwrap(),
                candidate.path(),
            )?;
            if manifest.id()? != id {
                return Err(Error::new("host_mismatch","Recipe/profile no longer reproduces the pinned host snapshot. Publication refused."));
            }
            source_tree = Some(candidate);
        } else {
            crate::recipe::regenerate_legacy(&reconstruction, &package.manifest)?;
        }
        for f in to_copy.iter().filter(|f| f.source.kind == "wabbajack") {
            let path = if let Some(c) = &source_tree {
                files::require(c.path(), &f.path)?
            } else {
                files::require(&reconstruction, &f.source.path)?
            };
            if fs::metadata(&path)?.len() != f.size || files::sha(&path)? != f.sha256 {
                return Err(Error::new(
                    "source_mismatch",
                    format!(
                        "Recipe source differs for {}. Use the pinned recipe and profile.",
                        f.path
                    ),
                ));
            }
            files::copy(&path, &stage.path().join(&f.path))?;
        }
    }
    for f in to_copy.iter().filter(|f| f.source.kind == "base") {
        let src = files::require(base, &f.source.path)?;
        if fs::metadata(&src)?.len() != f.size || files::sha(&src)? != f.sha256 {
            return Err(Error::new(
                "base_mismatch",
                format!(
                    "Base file differs: {}. Select required licensed base game.",
                    f.source.path
                ),
            ));
        }
        files::copy(&src, &stage.path().join(&f.path))?;
    }
    let result = verify(&package.manifest, stage.path())?;
    if result["files_match"] != true {
        crate::event("staging_differences", result);
        return Err(Error::new(
            "staging_mismatch",
            "Staged installation does not match; previous installation remains intact.",
        ));
    }
    files::atomic_json(
        &stage.path().join(".skyrimsync-install.json"),
        &Receipt {
            schema: 1,
            target_id: id.clone(),
        },
    )?;
    files::sync_dir(stage.path())?;
    storage.publish_stage(stage.path(), &id)?;
    Ok(
        json!({"state":"files_verified","target_id":id,"instance":storage.instance(),"package":package.path,"backup_retained":false,"runtime_verified":false}),
    )
}
pub fn publish(
    host: &snapshot::Snapshot,
    recipe: &Recipe,
    reconstructed: &Path,
    profile: &str,
    output: &Path,
    storage: &Storage,
) -> Result<Value> {
    host.validate()?;
    if !host.warnings.is_empty() {
        return Err(Error::new("incomplete_target","Host capture has missing configuration or invalid/missing plugins. Fix its diagnostics and capture again."));
    }
    let _lock = Lock::take(&storage.root.join("locks/publish.lock"))?;
    let problems = recipe.verify(reconstructed)?;
    if !problems.is_empty() {
        crate::event(
            "reconstruction_differences",
            json!(problems.iter().take(50).collect::<Vec<_>>()),
        );
        return Err(Error::new(
            "reconstruction_mismatch",
            "Recipe output has been modified or is incomplete; publication refused.",
        ));
    }
    fs::create_dir_all(storage.root.join("stages"))?;
    let temp = tempfile::tempdir_in(storage.root.join("stages"))?;
    let manifest = crate::recipe::materialize(recipe, reconstructed, profile, temp.path())?;
    let comparison = snapshot::compare(host, manifest.snapshot.as_ref().unwrap())?;
    if !comparison.files_match {
        crate::event("unreproducible_target", serde_json::to_value(&comparison)?);
        return Err(Error::new("unreproducible_target","Recipe/profile does not reproduce the host. Put added/changed mods, generated assets and settings into the recipe before sharing."));
    }
    crate::package::write(output, &manifest, &recipe.artifact)?;
    Ok(
        json!({"state":"published","package":output,"target_id":manifest.id()?,"sha256":files::sha(output)?,"size":fs::metadata(output)?.len(),"runtime_verified":false}),
    )
}
