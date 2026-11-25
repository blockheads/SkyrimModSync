//! Changed-file repair. Unchanged files stay in place; a durable undo log protects
//! each rename. This is recoverable in-place publication, not an atomic tree swap.
use crate::{
    files,
    package::{Instruction, Manifest, Package},
    recipe::{self, Recipe},
    snapshot,
    target::{self, Storage},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

pub struct Diff<'a> {
    pub changed: Vec<&'a Instruction>,
    pub removed: Vec<String>,
    pub unchanged: usize,
    pub bytes: u64,
}
pub fn diff<'a>(manifest: &'a Manifest, root: &Path) -> Result<Diff<'a>> {
    let inventory = files::inventory(root)?;
    let indexed = inventory
        .iter()
        .map(|(n, p)| (files::fold(n), p))
        .collect::<BTreeMap<_, _>>();
    let want = manifest
        .files
        .iter()
        .map(|f| files::fold(&f.path))
        .collect::<BTreeSet<_>>();
    let logs = snapshot::known_logs(
        manifest.files.iter().map(|f| f.path.clone()).chain(
            inventory
                .keys()
                .filter(|n| {
                    ![
                        "game/data/skse/plugins/skyrimcooploader.dll",
                        "game/data/skse/plugins/skyrimcoop/skyrimtogetherclient.dll",
                    ]
                    .contains(&files::fold(n).as_str())
                        || want.contains(&files::fold(n))
                })
                .cloned(),
        ),
    );
    let mut changed = Vec::new();
    let mut bytes = 0u64;
    crate::event("repair_scanning", json!({"total":manifest.files.len()}));
    for (i, f) in manifest.files.iter().enumerate() {
        crate::check_cancel()?;
        if i % 100 == 0 {
            crate::event(
                "repair_scanning",
                json!({"completed":i,"total":manifest.files.len()}),
            );
        }
        if indexed
            .get(&files::fold(&f.path))
            .is_some_and(|p| fs::metadata(p).is_ok_and(|m| m.len() == f.size))
        {
            let p = indexed[&files::fold(&f.path)];
            if files::sha(p)? == f.sha256 {
                continue;
            }
        }
        bytes = bytes
            .checked_add(f.size)
            .ok_or_else(|| Error::new("invalid_package", "Repair size overflow."))?;
        changed.push(f);
    }
    let removed = inventory
        .keys()
        .filter(|n| {
            let shared = manifest.schema == 1 || files::fold(n).starts_with("game/");
            shared
                && *n != ".skyrimsync-install.json"
                && !want.contains(&files::fold(n))
                && !snapshot::ignored(n, &logs)
        })
        .cloned()
        .collect();
    Ok(Diff {
        unchanged: manifest.files.len() - changed.len(),
        changed,
        removed,
        bytes,
    })
}
fn matches(path: &Path, instruction: &Instruction) -> Result<bool> {
    Ok(fs::metadata(path)?.len() == instruction.size && files::sha(path)? == instruction.sha256)
}
fn cache_root(package: &Package, storage: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    match explicit {
        Some(p) => files::root(p),
        None => Ok(storage
            .join("jobs")
            .join(&package.manifest.artifact.sha256)
            .join("reconstructed")),
    }
}
fn cached_output(
    cache: &Path,
    storage: &Path,
    f: &Instruction,
    explicit: bool,
) -> Result<Option<PathBuf>> {
    if let Some(p) = files::locate(cache, &f.source.path)? {
        if matches(&p, f)? {
            return Ok(Some(p));
        }
        if explicit && !f.source.path.starts_with(".skyrimsync-generated/") {
            return Err(Error::new("reconstruction_mismatch", format!("Requested cached source differs: {}. Reconstruct it or omit --reconstructed to recover from the pinned recipe.", f.source.path)));
        }
    }
    if !explicit && storage.join("jobs").is_dir() {
        for e in fs::read_dir(storage.join("jobs"))? {
            let e = e?;
            if !files::digest_valid(&e.file_name().to_string_lossy()) {
                continue;
            }
            files::root(&e.path())?;
            let output = e.path().join("reconstructed");
            if output.exists() {
                files::root(&output)?;
            }
            if let Some(p) = files::locate(&output, &f.source.path)? {
                if matches(&p, f)? {
                    return Ok(Some(p));
                }
            }
        }
    }
    Ok(None)
}
fn load_recipe(package: &Package, storage: &Path) -> Result<Recipe> {
    let artifact = storage
        .join("recipes")
        .join(format!("{}.wabbajack", package.manifest.artifact.sha256));
    package.extract(&artifact)?;
    Recipe::load(&artifact)
}
fn pending_dependencies(
    recipe: &Recipe,
    needed: &[&Instruction],
    cache: &Path,
) -> Result<BTreeSet<String>> {
    let mut dependencies = BTreeSet::new();
    for f in needed {
        dependencies.extend(recipe::generated_dependencies(&f.source.path, recipe)?);
    }
    // Inline payloads are recovered directly, without an engine or downloads.
    let selected = recipe.select(&dependencies)?;
    let mut pending = BTreeSet::new();
    for d in selected["Directives"].as_array().unwrap() {
        if d["$type"] == "InlineFile" {
            continue;
        }
        let rel = d["To"].as_str().unwrap().replace('\\', "/");
        let p = files::locate(cache, &rel)?;
        if p.as_ref()
            .is_some_and(|p| fs::metadata(p).is_ok_and(|m| Some(m.len()) == d["Size"].as_u64()))
            && p.as_ref()
                .map(|p| crate::acquisition::wj_hash(p))
                .transpose()?
                .is_some_and(|h| Some(h.as_str()) == d["Hash"].as_str())
        {
            continue;
        }
        pending.insert(files::fold(&rel));
    }
    Ok(pending)
}
/// Plan only dependencies of changed outputs, never unrelated recipe archives.
pub fn plan(
    package: &Package,
    base: &Path,
    downloads: &Path,
    storage: &Path,
    reconstructed: Option<&Path>,
) -> Result<Value> {
    let delta = diff(&package.manifest, &storage.join("instance"))?;
    let cache = cache_root(package, storage, reconstructed)?;
    let mut needed = Vec::new();
    for f in &delta.changed {
        if f.source.kind != "wabbajack" {
            continue;
        }
        if cached_output(&cache, storage, f, reconstructed.is_some())?.is_some() {
            continue;
        }
        needed.push(*f);
    }
    let recipe = load_recipe(package, storage)?;
    let mut dependencies = BTreeSet::new();
    for f in &needed {
        dependencies.extend(recipe::generated_dependencies(&f.source.path, &recipe)?);
    }
    let source_bytes = recipe.selected_bytes(&dependencies)?;
    let pending = pending_dependencies(&recipe, &needed, &cache)?;
    let selected = Recipe {
        data: recipe.select(&pending)?,
        artifact: recipe.artifact,
        paths: BTreeMap::new(),
    };
    let acquisition = selected.archives(base, downloads)?;
    let required = delta
        .bytes
        .checked_add(source_bytes)
        .and_then(|n| {
            n.checked_add(
                acquisition["reconstruction_bytes"]
                    .as_u64()
                    .unwrap()
                    .saturating_mul(2),
            )
        })
        .and_then(|n| n.checked_add(acquisition["missing_bytes"].as_u64().unwrap()))
        .ok_or_else(|| Error::new("invalid_recipe", "Working size overflow."))?;
    let mut blockers = Vec::new();
    for f in delta.changed.iter().filter(|f| f.source.kind == "base") {
        if files::locate(base, &f.source.path)?
            .as_ref()
            .map(|p| matches(p, f))
            .transpose()?
            .unwrap_or(false)
        {
            continue;
        }
        blockers.push(json!({"reason":"base_mismatch","path":f.source.path,"fix":"Select the required licensed base game with --base."}));
    }
    if !acquisition["base_problems"].as_array().unwrap().is_empty() {
        blockers.push(json!({"reason":"base_mismatch","fix":"Select the required licensed base game with --base."}));
    }
    if !acquisition["unsupported_missing_sources"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        blockers.push(json!({"reason":"manual_downloads_required","fix":"Cache the listed archives in --downloads."}));
    }
    if acquisition["missing"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["kind"].as_str().unwrap_or("").contains("Nexus"))
        && crate::config::load_key(false)?.is_none()
    {
        blockers.push(json!({"reason":"nexus_key_required","fix":"Save a Nexus key or manually cache the listed archives."}));
    }
    if fs2::available_space(storage)? < required {
        blockers.push(
            json!({"reason":"disk_full","fix":"Free space or choose another --coop folder."}),
        );
    }
    Ok(
        json!({"state":"planned","incremental":true,"target_id":package.manifest.id()?,"files_replaced":delta.changed.len(),"files_removed":delta.removed.len(),"files_unchanged":delta.unchanged,"changed_files":delta.changed.iter().map(|f| json!({"path":f.path,"size":f.size})).collect::<Vec<_>>(),"removed_files":delta.removed,"staged_bytes":delta.bytes,"source_working_bytes":source_bytes,"acquisition":acquisition,"working_space_bytes":required,"free_space_bytes":fs2::available_space(storage)?,"blockers":blockers,"base_read_only":true,"runtime_verified":false}),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn sync(
    package: &Package,
    storage: &Storage,
    base: &Path,
    downloads: &Path,
    steam: Option<&Path>,
    proton: Option<&Path>,
    reconstructed: Option<&Path>,
) -> Result<Value> {
    let delta = diff(&package.manifest, &storage.instance())?;
    let id = package.manifest.id()?;
    if delta.changed.is_empty() && delta.removed.is_empty() {
        let verified = target::verify(&package.manifest, &storage.instance())?;
        if verified["files_match"] != true {
            return Err(Error::new(
                "instance_mismatch",
                "Instance metadata does not match the target; inspect verify diagnostics.",
            ));
        }
        return Ok(
            json!({"state":"files_verified","target_id":id,"instance":storage.instance(),"package":package.path,"reused":true,"incremental":true,"files_replaced":0,"files_removed":0,"files_unchanged":delta.unchanged,"staged_bytes":0,"runtime_verified":false}),
        );
    }
    files::space(&storage.root, delta.bytes)?;
    fs::create_dir_all(storage.root.join("stages"))?;
    let stage = tempfile::tempdir_in(storage.root.join("stages"))?;
    let cache = cache_root(package, &storage.root, reconstructed)?;
    let mut needed = Vec::new();
    for f in &delta.changed {
        crate::check_cancel()?;
        let source = if f.source.kind == "base" {
            let p = files::require(base, &f.source.path).map_err(|_| {
                Error::new(
                    "base_mismatch",
                    format!("Required base file is missing: {}.", f.source.path),
                )
            })?;
            if !matches(&p, f)? {
                return Err(Error::new(
                    "base_mismatch",
                    format!("Required base file differs: {}.", f.source.path),
                ));
            }
            Some(p)
        } else {
            cached_output(&cache, &storage.root, f, reconstructed.is_some())?
        };
        if let Some(p) = source {
            files::copy(&p, &stage.path().join(&f.path))?;
            continue;
        }
        needed.push(*f);
    }
    if !needed.is_empty() {
        let recipe = load_recipe(package, &storage.root)?;
        let sources = tempfile::tempdir_in(storage.root.join("stages"))?;
        let mut dependencies = BTreeSet::new();
        for f in &needed {
            dependencies.extend(recipe::generated_dependencies(&f.source.path, &recipe)?);
        }
        let source_bytes = recipe.selected_bytes(&dependencies)?;
        files::space(
            &storage.root,
            delta
                .bytes
                .checked_add(source_bytes)
                .ok_or_else(|| Error::new("invalid_recipe", "Working size overflow."))?,
        )?;
        let noninline = recipe.extract_inline(&dependencies, sources.path())?;
        let pending = pending_dependencies(&recipe, &needed, &cache)?;
        for rel in noninline.difference(&pending) {
            files::copy(
                &files::require(&cache, rel)?,
                &files::destination(sources.path(), rel)?,
            )?;
        }
        if !pending.is_empty() {
            let subset = recipe.subset(&pending, &sources.path().join("repair.wabbajack"))?;
            crate::event(
                "repair_acquisition",
                json!({"outputs":pending.len(),"archives":subset.data["Archives"].as_array().unwrap().len()}),
            );
            let output =
                crate::engine::reconstruct(&subset, &storage.root, base, downloads, steam, proton)?;
            for rel in &pending {
                files::copy(
                    &files::require(&output, rel)?,
                    &files::destination(sources.path(), rel)?,
                )?;
            }
        }
        for f in needed {
            recipe::generate_selected(sources.path(), &f.source.path)?;
            let p = files::require(sources.path(), &f.source.path)?;
            if !matches(&p, f)? {
                return Err(Error::new("source_mismatch", format!("Pinned recipe output differs for {}. Previous installation remains intact.", f.path)));
            }
            files::copy(&p, &stage.path().join(&f.path))?;
        }
    }
    // Verify the bytes after staging, including copies from validated cached files.
    for f in &delta.changed {
        if !matches(&stage.path().join(&f.path), f)? {
            return Err(Error::new(
                "staging_mismatch",
                format!("Staged file differs: {}.", f.path),
            ));
        }
    }
    let mut actions = delta
        .changed
        .iter()
        .map(|f| (f.path.clone(), Some(stage.path().join(&f.path))))
        .collect::<Vec<_>>();
    actions.extend(delta.removed.iter().map(|p| (p.clone(), None)));
    crate::event(
        "repair_staged",
        json!({"files_replaced":delta.changed.len(),"files_removed":delta.removed.len(),"files_unchanged":delta.unchanged,"staged_bytes":delta.bytes}),
    );
    let backup = transact(storage, actions, &id, || {
        let result = target::verify(&package.manifest, &storage.instance())?;
        if result["files_match"] != true {
            crate::event("repair_differences", result);
            return Err(Error::new(
                "repair_mismatch",
                "Repaired instance does not match; changed files will be restored.",
            ));
        }
        Ok(())
    })?;
    Ok(
        json!({"state":"files_verified","target_id":id,"instance":storage.instance(),"package":package.path,"incremental":true,"files_replaced":delta.changed.len(),"files_removed":delta.removed.len(),"files_unchanged":delta.unchanged,"staged_bytes":delta.bytes,"backup":backup,"backup_retained":true,"runtime_verified":false}),
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub path: String,
    pub old: bool,
    pub new: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    pub schema: u32,
    pub from_id: String,
    pub to_id: String,
    pub operations: Vec<Operation>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    stage: String,
    backup: String,
    started: usize,
    committed: bool,
    delta: Delta,
}

/// Preserve existing directory casing and refuse file/directory conflicts or links.
fn destination(root: &Path, rel: &str) -> Result<PathBuf> {
    files::relative(rel)?;
    let parts = rel.split('/').collect::<Vec<_>>();
    let mut p = root.to_owned();
    for (i, part) in parts.iter().enumerate() {
        if p.is_dir() {
            let matches = fs::read_dir(&p)?
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .filter(|e| files::fold(&e.file_name().to_string_lossy()) == files::fold(part))
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(Error::new(
                    "case_collision",
                    format!("Ambiguous repair path: {rel}"),
                ));
            }
            if let Some(e) = matches.first() {
                let ty = e.file_type()?;
                if ty.is_symlink() {
                    return Err(Error::new(
                        "path_escape",
                        format!("Symbolic link in repair path: {rel}"),
                    ));
                }
                if (i + 1 < parts.len() && !ty.is_dir()) || (i + 1 == parts.len() && !ty.is_file())
                {
                    return Err(Error::new("path_conflict", format!("File/directory conflict at {}. Move the conflicting entry aside and retry.", e.path().display())));
                }
                p = e.path();
                continue;
            }
        }
        p.push(part);
    }
    Ok(p)
}
fn mkdir(parent: &Path, root: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut p = parent;
    while !p.exists() {
        missing.push(p.to_path_buf());
        p = p
            .parent()
            .ok_or_else(|| Error::new("path_escape", "Missing parent."))?;
    }
    if !p.starts_with(root) {
        return Err(Error::new(
            "path_escape",
            "Repair parent escaped managed storage.",
        ));
    }
    for dir in missing.iter().rev() {
        fs::create_dir(dir)?;
        files::sync_dir(dir.parent().unwrap())?;
    }
    Ok(())
}
fn move_file(from: &Path, to: &Path, root: &Path) -> Result<()> {
    files::regular(from)?;
    mkdir(to.parent().unwrap(), root)?;
    fs::rename(from, to)?;
    files::sync_dir(from.parent().unwrap())?;
    files::sync_dir(to.parent().unwrap())?;
    Ok(())
}
fn receipt_id(root: &Path) -> Result<String> {
    let v: Value = files::read_json(&root.join(".skyrimsync-install.json"))?;
    let id = v["target_id"].as_str().unwrap_or("");
    if v["schema"] != 1 || !files::digest_valid(id) {
        return Err(Error::new(
            "unowned_destination",
            "Invalid installation receipt.",
        ));
    }
    Ok(id.to_owned())
}
fn validate(delta: &Delta) -> Result<()> {
    if delta.schema != 2
        || !files::digest_valid(&delta.from_id)
        || !files::digest_valid(&delta.to_id)
        || delta.operations.len() > files::MAX_FILES + 1
    {
        return Err(Error::new(
            "invalid_transaction",
            "Invalid incremental repair metadata.",
        ));
    }
    let mut paths = BTreeSet::new();
    for op in &delta.operations {
        files::relative(&op.path)?;
        if !paths.insert(files::fold(&op.path)) || (!op.old && !op.new) {
            return Err(Error::new(
                "invalid_transaction",
                "Invalid repair operations.",
            ));
        }
    }
    if !delta
        .operations
        .last()
        .is_some_and(|op| op.path == ".skyrimsync-install.json" && op.old && op.new)
    {
        return Err(Error::new(
            "invalid_transaction",
            "Repair receipt operation is missing.",
        ));
    }
    Ok(())
}
pub(crate) fn transact(
    storage: &Storage,
    actions: Vec<(String, Option<PathBuf>)>,
    id: &str,
    verify: impl FnOnce() -> Result<()>,
) -> Result<PathBuf> {
    let from_id = receipt_id(&storage.instance())?;
    fs::create_dir_all(storage.root.join("backups"))?;
    fs::create_dir_all(storage.root.join("stages"))?;
    let stage = tempfile::tempdir_in(storage.root.join("stages"))?;
    let backup_guard = tempfile::Builder::new()
        .prefix(&format!("{}-", target::unique()))
        .tempdir_in(storage.root.join("backups"))?;
    let backup = backup_guard.path().to_owned();
    files::sync_dir(backup.parent().unwrap())?;
    let mut operations = Vec::new();
    for (rel, source) in actions {
        let dest = destination(&storage.instance(), &rel)?;
        let path = dest
            .strip_prefix(storage.instance())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(p) = &source {
            if p.starts_with(storage.root.join("stages")) {
                move_file(p, &stage.path().join(&path), &storage.root)?;
            } else {
                files::copy(p, &stage.path().join(&path))?;
            }
        }
        operations.push(Operation {
            path,
            old: dest.exists(),
            new: source.is_some(),
        });
    }
    files::atomic_json(
        &stage.path().join(".skyrimsync-install.json"),
        &json!({"schema":1,"target_id":id}),
    )?;
    operations.push(Operation {
        path: ".skyrimsync-install.json".into(),
        old: true,
        new: true,
    });
    let delta = Delta {
        schema: 2,
        from_id,
        to_id: id.to_owned(),
        operations,
    };
    validate(&delta)?;
    let mut j = Journal {
        schema: 2,
        stage: stage
            .path()
            .strip_prefix(&storage.root)
            .unwrap()
            .to_string_lossy()
            .into(),
        backup: backup
            .strip_prefix(&storage.root)
            .unwrap()
            .to_string_lossy()
            .into(),
        started: 0,
        committed: false,
        delta,
    };
    let journal = storage.root.join("transaction.json");
    // Recovery needs these directory entries even if power is lost before the
    // first rename. Individual file moves fsync their source/destination parents.
    files::sync_dir(stage.path())?;
    files::sync_dir(stage.path().parent().unwrap())?;
    files::sync_dir(&storage.root)?;
    files::atomic_json(&journal, &j)?;
    let stage = stage.keep(); // Recovery owns it from this point onward.
    let backup = backup_guard.keep();
    let result: Result<()> = (|| {
        target::running(&storage.instance())?;
        for (i, op) in j.delta.operations.iter().enumerate() {
            crate::check_cancel()?;
            j.started = i + 1;
            files::atomic_json(&journal, &j)?; // intent is durable before either rename
            let dest = destination(&storage.instance(), &op.path)?;
            if op.old {
                move_file(&dest, &backup.join(&op.path), &storage.root)?;
            }
            if op.new {
                move_file(&stage.join(&op.path), &dest, &storage.root)?;
            }
        }
        verify()?;
        crate::check_cancel()?;
        j.committed = true;
        files::atomic_json(&journal, &j)?;
        Ok(())
    })();
    // Also undo a cancelled transaction; recovery itself must not check cancellation.
    recover(storage).map_err(|e| Error::new("recovery_required", format!("Repair recovery is incomplete: {e}. Preserve transaction.json and backups, fix the reported path/storage problem and retry sync.")))?;
    result?;
    Ok(backup)
}
pub fn recover(storage: &Storage) -> Result<()> {
    let path = storage.root.join("transaction.json");
    let j: Journal = files::read_json(&path)?;
    validate(&j.delta)?;
    for (rel, prefix) in [(&j.stage, "stages/"), (&j.backup, "backups/")] {
        files::relative(rel)?;
        if !rel.starts_with(prefix) || rel[prefix.len()..].contains('/') {
            return Err(Error::new(
                "invalid_transaction",
                "Invalid repair storage path.",
            ));
        }
        files::root(&storage.root.join(rel))?;
    }
    if j.schema != 2 || j.started > j.delta.operations.len() {
        return Err(Error::new(
            "invalid_transaction",
            "Invalid repair recovery journal.",
        ));
    }
    let backup = storage.root.join(&j.backup);
    if !j.committed {
        for op in j.delta.operations[..j.started].iter().rev() {
            let dest = destination(&storage.instance(), &op.path)?;
            let saved = destination(&backup, &op.path)?;
            if saved.exists() {
                if dest.exists() {
                    files::regular(&dest)?;
                    fs::remove_file(&dest)?;
                    files::sync_dir(dest.parent().unwrap())?;
                }
                move_file(&saved, &dest, &storage.root)?;
            } else if !op.old && dest.exists() {
                files::regular(&dest)?;
                fs::remove_file(&dest)?;
                files::sync_dir(dest.parent().unwrap())?;
            }
        }
        // Remove incomplete backups so rollback cannot select an aborted repair.
        if backup.join(".skyrimsync-delta.json").exists() {
            fs::remove_file(backup.join(".skyrimsync-delta.json"))?;
        }
    }
    if j.committed {
        files::atomic_json(&backup.join(".skyrimsync-delta.json"), &j.delta)?;
    }
    fs::remove_file(path)?;
    files::sync_dir(&storage.root)?;
    fs::remove_dir_all(storage.root.join(j.stage))?;
    if !j.committed {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}
pub fn rollback(storage: &Storage, backup: &Path) -> Result<Value> {
    let delta: Delta = files::read_json(&backup.join(".skyrimsync-delta.json"))?;
    validate(&delta)?;
    if receipt_id(&storage.instance())? != delta.to_id {
        return Err(Error::new(
            "backup_mismatch",
            "Latest repair backup belongs to a different installed target.",
        ));
    }
    let actions = delta
        .operations
        .iter()
        .filter(|op| op.path != ".skyrimsync-install.json")
        .map(|op| (op.path.clone(), op.old.then(|| backup.join(&op.path))))
        .collect();
    transact(storage, actions, &delta.from_id, || Ok(()))?;
    Ok(
        json!({"state":"rolled_back","incremental":true,"target_id":delta.from_id,"instance":storage.instance(),"runtime_verified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owned() -> (tempfile::TempDir, Storage) {
        let d = tempfile::tempdir().unwrap();
        let storage = Storage::open(&d.path().join("owned")).unwrap();
        fs::create_dir(storage.instance()).unwrap();
        files::atomic_json(
            &storage.instance().join(".skyrimsync-install.json"),
            &json!({"schema":1,"target_id":"a".repeat(64)}),
        )
        .unwrap();
        fs::write(storage.instance().join("unchanged"), b"same").unwrap();
        fs::write(storage.instance().join("changed"), b"old").unwrap();
        (d, storage)
    }
    #[test]
    fn interrupted_repair_recovers_before_and_after_each_rename() {
        for started in 1..=3 {
            for phase in 0..=2 {
                let (_d, storage) = owned();
                let stage = storage.root.join("stages/test");
                let backup = storage.root.join("backups/test");
                fs::create_dir_all(&stage).unwrap();
                fs::create_dir_all(&backup).unwrap();
                fs::write(stage.join("changed"), b"new").unwrap();
                fs::write(stage.join("added"), b"added").unwrap();
                files::atomic_json(
                    &stage.join(".skyrimsync-install.json"),
                    &json!({"schema":1,"target_id":"b".repeat(64)}),
                )
                .unwrap();
                let operations = vec![
                    Operation {
                        path: "changed".into(),
                        old: true,
                        new: true,
                    },
                    Operation {
                        path: "added".into(),
                        old: false,
                        new: true,
                    },
                    Operation {
                        path: ".skyrimsync-install.json".into(),
                        old: true,
                        new: true,
                    },
                ];
                for (i, op) in operations.iter().take(started).enumerate() {
                    if (i + 1 < started || phase >= 1) && op.old {
                        fs::rename(storage.instance().join(&op.path), backup.join(&op.path))
                            .unwrap();
                    }
                    if i + 1 < started || phase == 2 {
                        fs::rename(stage.join(&op.path), storage.instance().join(&op.path))
                            .unwrap();
                    }
                }
                let j = Journal {
                    schema: 2,
                    stage: "stages/test".into(),
                    backup: "backups/test".into(),
                    started,
                    committed: false,
                    delta: Delta {
                        schema: 2,
                        from_id: "a".repeat(64),
                        to_id: "b".repeat(64),
                        operations,
                    },
                };
                files::atomic_json(&storage.root.join("transaction.json"), &j).unwrap();
                storage.recover().unwrap();
                assert_eq!(
                    fs::read(storage.instance().join("changed")).unwrap(),
                    b"old"
                );
                assert_eq!(
                    fs::read(storage.instance().join("unchanged")).unwrap(),
                    b"same"
                );
                assert!(!storage.instance().join("added").exists());
                assert_eq!(receipt_id(&storage.instance()).unwrap(), "a".repeat(64));
                assert!(!storage.root.join("transaction.json").exists());
                storage.recover().unwrap();
            }
        }
    }
    #[test]
    fn committed_recovery_retains_delta_and_recovery_can_resume_an_undo() {
        for committed in [false, true] {
            let (_d, storage) = owned();
            let stage = storage.root.join("stages/test");
            let backup = storage.root.join("backups/test");
            fs::create_dir_all(&stage).unwrap();
            fs::create_dir_all(&backup).unwrap();
            fs::rename(storage.instance().join("changed"), backup.join("changed")).unwrap();
            fs::write(storage.instance().join("changed"), b"new").unwrap();
            fs::rename(
                storage.instance().join(".skyrimsync-install.json"),
                backup.join(".skyrimsync-install.json"),
            )
            .unwrap();
            files::atomic_json(
                &storage.instance().join(".skyrimsync-install.json"),
                &json!({"schema":1,"target_id":"b".repeat(64)}),
            )
            .unwrap();
            let delta = Delta {
                schema: 2,
                from_id: "a".repeat(64),
                to_id: "b".repeat(64),
                operations: vec![
                    Operation {
                        path: "changed".into(),
                        old: true,
                        new: true,
                    },
                    Operation {
                        path: ".skyrimsync-install.json".into(),
                        old: true,
                        new: true,
                    },
                ],
            };
            let j = Journal {
                schema: 2,
                stage: "stages/test".into(),
                backup: "backups/test".into(),
                started: 2,
                committed,
                delta,
            };
            files::atomic_json(&storage.root.join("transaction.json"), &j).unwrap();
            if !committed {
                // A second interruption occurred after the receipt was restored.
                fs::remove_file(storage.instance().join(".skyrimsync-install.json")).unwrap();
                fs::rename(
                    backup.join(".skyrimsync-install.json"),
                    storage.instance().join(".skyrimsync-install.json"),
                )
                .unwrap();
            }
            storage.recover().unwrap();
            if committed {
                assert_eq!(
                    fs::read(storage.instance().join("changed")).unwrap(),
                    b"new"
                );
                assert!(backup.join(".skyrimsync-delta.json").is_file());
                assert_eq!(receipt_id(&storage.instance()).unwrap(), "b".repeat(64));
            } else {
                assert_eq!(
                    fs::read(storage.instance().join("changed")).unwrap(),
                    b"old"
                );
                assert_eq!(receipt_id(&storage.instance()).unwrap(), "a".repeat(64));
            }
            assert!(!storage.root.join("transaction.json").exists());
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn final_validation_failure_restores_changed_files_and_ownership() {
        let (_d, storage) = owned();
        let source = storage.root.join("replacement");
        fs::write(&source, b"new").unwrap();
        let result = transact(
            &storage,
            vec![("changed".into(), Some(source))],
            &"b".repeat(64),
            || Err(Error::new("validation_failed", "injected")),
        );
        assert_eq!(result.unwrap_err().code, "validation_failed");
        assert_eq!(
            fs::read(storage.instance().join("changed")).unwrap(),
            b"old"
        );
        assert_eq!(receipt_id(&storage.instance()).unwrap(), "a".repeat(64));
        assert_eq!(
            fs::read_dir(storage.root.join("backups")).unwrap().count(),
            0
        );
    }
}
