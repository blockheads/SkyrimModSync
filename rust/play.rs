//! Linux launch isolation. Installed files are read-only; runtime writes and user
//! state live in owned storage. No global Wine prefix or personal game is altered.
use crate::{
    files,
    revision::{self, Entry, Revision, Source},
    snapshot,
    target::Storage,
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub path: String,
    pub kind: String,
    pub personal: bool,
}
fn ini(path: &Path) -> Result<BTreeMap<(String, String), String>> {
    let raw = files::bounded(path, 4 * 1024 * 1024)?;
    let text = std::str::from_utf8(&raw)
        .map_err(|_| Error::new("invalid_configuration", "INI requires UTF-8."))?
        .trim_start_matches('\u{feff}');
    let mut section = String::new();
    let mut values = BTreeMap::new();
    for l in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', ';']))
    {
        if l.starts_with('[') && l.ends_with(']') {
            section = files::fold(&l[1..l.len() - 1]);
        } else if let Some((k, v)) = l.split_once('=') {
            let key = (section.clone(), files::fold(k.trim()));
            if values.insert(key, v.trim().to_owned()).is_some() {
                return Err(Error::new(
                    "invalid_configuration",
                    "Duplicate INI key; resolve ambiguity before sharing.",
                ));
            }
        } else {
            return Err(Error::new(
                "invalid_configuration",
                "Unsupported multiline INI.",
            ));
        }
    }
    Ok(values)
}
fn personal_key(file: &str, section: &str) -> bool {
    file.eq_ignore_ascii_case("SkyrimPrefs.ini") && ["display", "controls"].contains(&section)
}
fn shared_ini(path: &Path, file: &str) -> Result<BTreeMap<(String, String), String>> {
    let mut values = ini(path)?;
    values.retain(|(s, _), _| !personal_key(file, s));
    Ok(values)
}
fn write_ini(path: &Path, values: &BTreeMap<(String, String), String>) -> Result<()> {
    let mut out = String::new();
    let mut previous = None;
    for ((section, key), value) in values {
        if previous != Some(section) {
            if !section.is_empty() {
                out.push_str(&format!("\n[{section}]\n"));
            }
            previous = Some(section);
        }
        out.push_str(&format!("{key}={value}\n"));
    }
    fs::write(path, out)?;
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
fn private_directory(path: &Path, owned: &Path, replace_alias: bool) -> Result<()> {
    if !path.starts_with(owned) {
        return Err(Error::new(
            "unsafe_prefix",
            "Prefix path escaped owned storage.",
        ));
    }
    if let Some(parent) = path.parent() {
        if parent != owned {
            private_directory(parent, owned, false)?;
        }
    }
    if let Ok(m) = path.symlink_metadata() {
        if m.file_type().is_symlink() {
            if !replace_alias {
                return Err(Error::new(
                    "unsafe_prefix",
                    format!("Unexpected prefix link: {}", path.display()),
                ));
            }
            fs::remove_file(path)?;
        } else if !m.is_dir() {
            return Err(Error::new(
                "unsafe_prefix",
                "Expected a private prefix directory.",
            ));
        }
    }
    fs::create_dir_all(path)?;
    Ok(())
}
fn rt_root(storage: &Storage, rev: &Revision) -> Result<PathBuf> {
    Ok(storage.root.join("runtime").join(rev.id()?))
}
fn last_runtime(storage: &Storage) -> Result<Option<(Revision, PathBuf)>> {
    let p = storage.root.join("last-launch.json");
    if !p.exists() {
        return Ok(None);
    }
    let v: Value = files::read_json(&p)?;
    let id = v["revision"]
        .as_str()
        .ok_or_else(|| Error::new("invalid_runtime", "Missing runtime revision."))?;
    if !files::digest_valid(id) {
        return Err(Error::new("invalid_runtime", "Invalid runtime revision."));
    }
    let rev = revision::load(&storage.root.join("revisions").join(format!("{id}.json")))?;
    Ok(Some((rev, storage.root.join("runtime").join(id))))
}
#[cfg(target_os = "linux")]
fn overlay_attribute(path: &Path, name: &std::ffi::CStr) -> Result<Option<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::new("invalid_path", "Invalid runtime path."))?;
    let mut buf = [0u8; 16];
    let n = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n >= 0 {
        return Ok(Some(buf[..n as usize].to_vec()));
    }
    let err = io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
        Ok(None)
    } else {
        Err(err.into())
    }
}
type UpperInventory = (
    BTreeMap<String, PathBuf>,
    BTreeSet<String>,
    BTreeSet<String>,
);
fn upper_inventory(root: &Path) -> Result<UpperInventory> {
    fn walk(
        root: &Path,
        rel: &str,
        files_out: &mut BTreeMap<String, PathBuf>,
        deleted: &mut BTreeSet<String>,
        opaque: &mut BTreeSet<String>,
    ) -> Result<()> {
        let mut seen = BTreeSet::new();
        for e in fs::read_dir(root.join(rel))? {
            let e = e?;
            let n = e
                .file_name()
                .into_string()
                .map_err(|_| Error::new("invalid_path", "Runtime filenames must be Unicode."))?;
            let rel = if rel.is_empty() {
                n
            } else {
                format!("{rel}/{n}")
            };
            files::relative(&rel)?;
            if !seen.insert(files::fold(&rel)) {
                return Err(Error::new("case_collision", "Ambiguous runtime output."));
            }
            let ty = e.file_type()?;
            if ty.is_dir() {
                #[cfg(target_os = "linux")]
                if overlay_attribute(&e.path(), c"user.overlay.opaque")?.as_deref() == Some(b"y") {
                    opaque.insert(rel.clone());
                }
                walk(root, &rel, files_out, deleted, opaque)?;
            } else if ty.is_symlink() {
                return Err(Error::new("path_escape", "Unexpected runtime link."));
            } else if ty.is_file() {
                #[cfg(target_os = "linux")]
                if overlay_attribute(&e.path(), c"user.overlay.whiteout")?.is_some() {
                    deleted.insert(rel);
                    continue;
                }
                files_out.insert(rel, e.path());
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{FileTypeExt, MetadataExt};
                    if ty.is_char_device() && e.metadata()?.rdev() == 0 {
                        deleted.insert(rel);
                        continue;
                    }
                }
                return Err(Error::new(
                    "unsafe_file",
                    "Unsupported runtime special file.",
                ));
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    let mut deleted = BTreeSet::new();
    let mut opaque = BTreeSet::new();
    if root.is_dir() {
        walk(root, "", &mut entries, &mut deleted, &mut opaque)?;
    }
    Ok((entries, deleted, opaque))
}
pub fn changes(storage: &Storage) -> Result<Vec<Change>> {
    let Some((rev, root)) = last_runtime(storage)? else {
        return Ok(vec![]);
    };
    let ids = rev
        .files
        .iter()
        .map(|e| (files::fold(&e.path), e))
        .collect::<BTreeMap<_, _>>();
    let (upper, mut deleted, opaque) = upper_inventory(&root.join("upper"))?;
    for entry in &rev.files {
        if let Some(rel) = entry.path.strip_prefix("game/") {
            if opaque
                .iter()
                .chain(deleted.clone().iter())
                .any(|dir| files::fold(rel).starts_with(&format!("{}/", files::fold(dir))))
                && !upper.keys().any(|p| files::fold(p) == files::fold(rel))
            {
                deleted.insert(rel.to_owned());
            }
        }
    }
    let logs = snapshot::known_logs(
        rev.files
            .iter()
            .map(|e| e.path.clone())
            .chain(upper.keys().map(|n| format!("game/{n}"))),
    );
    let mut changes = Vec::new();
    for (rel, p) in upper {
        let target = format!("game/{rel}");
        let private = snapshot::ignored(&target, &logs)
            || [".log", ".dmp"]
                .iter()
                .any(|s| rel.to_lowercase().ends_with(s));
        if let Some(e) = ids.get(&files::fold(&target)) {
            if revision::matches(&p, e)? {
                continue;
            }
        }
        changes.push(Change {
            path: target,
            kind: if ids.contains_key(&files::fold(&format!("game/{rel}"))) {
                "changed"
            } else {
                "extra"
            }
            .into(),
            personal: private,
        });
    }
    for rel in deleted
        .into_iter()
        .filter(|rel| ids.contains_key(&files::fold(&format!("game/{rel}"))))
    {
        changes.push(Change {
            path: format!("game/{rel}"),
            kind: "deleted".into(),
            personal: false,
        });
    }
    for n in snapshot::DOCUMENTS {
        let rel = format!("config/documents/{n}");
        let p = root.join("documents").join(n);
        if p.is_file() {
            let seed = root.join("seed/documents").join(n);
            if seed.exists() && files::sha(&p)? != files::sha(&seed)? {
                let personal = shared_ini(&p, n)? == shared_ini(&seed, n)?;
                changes.push(Change {
                    path: rel,
                    kind: "changed".into(),
                    personal,
                });
            }
        }
    }
    for n in snapshot::LOCAL {
        let p = root.join("local").join(n);
        let seed = root.join("seed/local").join(n);
        if p.is_file() && seed.is_file() && files::sha(&p)? != files::sha(&seed)? {
            changes.push(Change {
                path: format!("config/local/{n}"),
                kind: "changed".into(),
                personal: false,
            });
        }
    }
    Ok(changes)
}
pub fn status(storage: &Storage) -> Result<Value> {
    let rev = revision::installed(storage)?;
    let runtime_revision = last_runtime(storage)?
        .map(|(revision, _)| revision.id())
        .transpose()?;
    Ok(
        json!({"state":"status","revision":rev.id()?,"target_id":rev.snapshot.id()?,"runtime_revision":runtime_revision,"runtime_changes":changes(storage)?,"runtime_verified":false}),
    )
}
pub fn promote(
    storage: &Storage,
    rev: &mut Revision,
    paths: &mut BTreeMap<String, PathBuf>,
    include: &[String],
    use_source: &BTreeSet<String>,
    keep_runtime: bool,
    interactive: bool,
) -> Result<()> {
    let candidate = changes(storage)?
        .into_iter()
        .filter(|c| !c.personal && !use_source.contains(&c.path))
        .collect::<Vec<_>>();
    let candidate = if let Some((_, root)) = last_runtime(storage)? {
        candidate
            .into_iter()
            .filter(|change| {
                if change.kind == "deleted" {
                    return rev.files.iter().any(|e| e.path == change.path);
                }
                let p = if let Some(rel) = change.path.strip_prefix("game/") {
                    root.join("upper").join(rel)
                } else if let Some(rel) = change.path.strip_prefix("config/documents/") {
                    root.join("documents").join(rel)
                } else {
                    root.join("local").join(
                        change
                            .path
                            .strip_prefix("config/local/")
                            .unwrap_or("invalid"),
                    )
                };
                !rev.files
                    .iter()
                    .find(|e| e.path == change.path)
                    .is_some_and(|e| revision::matches(&p, e).unwrap_or(false))
            })
            .collect::<Vec<_>>()
    } else {
        candidate
    };
    if candidate.is_empty() {
        if !include.is_empty() {
            return Err(Error::new(
                "runtime_change_missing",
                "No matching runtime changes exist.",
            ));
        }
        return Ok(());
    }
    crate::event("runtime_review", json!({"changes":candidate}));
    let mut selected = include.iter().cloned().collect::<BTreeSet<_>>();
    if selected.is_empty() && interactive {
        eprintln!("Runtime changes are preserved. Enter comma-separated paths to include, or press Enter to publish only the authoring source:");
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        selected.extend(
            line.trim()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        );
    }
    if selected.is_empty() && !interactive && !keep_runtime {
        return Err(Error::new("runtime_review_required","Runtime content changed. Run status; supply --include-change PATH for changes to share, or publish interactively to retain them as local state."));
    }
    let (old, root) = last_runtime(storage)?.unwrap();
    for path in selected {
        files::relative(&path)?;
        let change = candidate.iter().find(|c| c.path == path).ok_or_else(|| {
            Error::new(
                "runtime_change_missing",
                format!("Not a shareable runtime change: {path}"),
            )
        })?;
        let before = old
            .files
            .iter()
            .find(|e| files::fold(&e.path) == files::fold(&path));
        let now = rev
            .files
            .iter()
            .find(|e| files::fold(&e.path) == files::fold(&path));
        if before.map(|e| &e.sha256) != now.map(|e| &e.sha256) {
            return Err(Error::new("runtime_source_conflict",format!("Both authoring source and runtime changed {path}. Move the chosen result into your mod-manager profile, then reimport.")));
        }
        if change.kind == "deleted" {
            rev.files
                .retain(|e| files::fold(&e.path) != files::fold(&path));
            paths.remove(&path);
            continue;
        }
        let source = if let Some(rel) = path.strip_prefix("game/") {
            files::require(&root.join("upper"), rel)?
        } else if let Some(rel) = path.strip_prefix("config/documents/") {
            files::require(&root.join("documents"), rel)?
        } else if let Some(rel) = path.strip_prefix("config/local/") {
            files::require(&root.join("local"), rel)?
        } else {
            return Err(Error::new("invalid_path", "Unsupported runtime promotion."));
        };
        if revision::protected_base(&path) {
            return Err(Error::new(
                "base_provenance_required",
                "Runtime base-game edits cannot be peer-shared.",
            ));
        }
        let id = files::identity(path.clone(), &source)?;
        let hash = revision::freeze(&storage.root, &source)?;
        let frozen = revision::object_path(&storage.root, &hash)?;
        rev.files
            .retain(|e| files::fold(&e.path) != files::fold(&path));
        rev.files.push(Entry {
            path: path.clone(),
            size: id.size,
            sha256: id.sha256,
            source: Source::Blob { hash },
        });
        paths.insert(path, frozen);
    }
    rev.files.sort_by(|a, b| a.path.cmp(&b.path));
    rev.snapshot = snapshot::capture_selection(paths)?;
    rev.validate()?;
    Ok(())
}
fn configure(storage: &Storage, rev: &Revision, root: &Path) -> Result<()> {
    let personal = storage.root.join("personal");
    fs::create_dir_all(&personal)?;
    for folder in ["documents", "local"] {
        fs::create_dir_all(root.join(folder))?;
        fs::create_dir_all(root.join("seed").join(folder))?;
        fs::create_dir_all(personal.join(folder))?;
    }
    for (names, folder) in [
        (snapshot::DOCUMENTS.as_slice(), "documents"),
        (snapshot::LOCAL.as_slice(), "local"),
    ] {
        for name in names {
            let seed = files::require(&storage.instance(), &format!("config/{folder}/{name}"))?;
            files::copy(&seed, &root.join("seed").join(folder).join(name))?;
            let target = personal.join(folder).join(name);
            let current = root.join(folder).join(name);
            if !current.exists() {
                if *name == "SkyrimPrefs.ini" && target.is_file() {
                    let mut values = ini(&seed)?;
                    for ((s, k), v) in ini(&target)? {
                        if personal_key(name, &s) {
                            values.insert((s, k), v);
                        }
                    }
                    write_ini(&current, &values)?;
                } else {
                    files::copy(&seed, &current)?;
                }
            }
            files::copy(&current, &target)?;
        }
    }
    files::atomic_json(
        &storage.root.join("personal/config-revision.json"),
        &json!({"revision":rev.id()?}),
    )
}
fn effective(
    storage: &Storage,
    rev: &Revision,
    game: &Path,
    docs: &Path,
    local: &Path,
) -> Result<Value> {
    let got = snapshot::capture(game, Some(docs), Some(local))?;
    let comparison = snapshot::compare(&rev.snapshot, &got)?;
    let mut differences = comparison.differences;
    differences.retain(|d| {
        let Some(path) = d["path"].as_str() else {
            return true;
        };
        // New runtime logs are personal. A log already in the revision remains pinned.
        if path.starts_with("game/")
            && [".log", ".dmp"]
                .iter()
                .any(|s| path.to_lowercase().ends_with(s))
            && !rev
                .files
                .iter()
                .any(|e| files::fold(&e.path) == files::fold(path))
        {
            return false;
        }
        if let Some(name) = path.strip_prefix("config/documents/") {
            if snapshot::DOCUMENTS.contains(&name) {
                let seed = storage.instance().join(path);
                let actual = docs.join(name);
                return shared_ini(&seed, name)
                    .and_then(|a| Ok(a != shared_ini(&actual, name)?))
                    .unwrap_or(true);
            }
        }
        true
    });
    if !differences.is_empty() {
        crate::event("effective_differences", json!(differences));
        return Err(Error::new("effective_mismatch","Runtime overrides change shared content. Run status and publish selected changes (host), or move the conflicting runtime files aside. Installed content and runtime data are preserved."));
    }
    Ok(json!({"effective_match":true,"target_id":rev.snapshot.id()?,"runtime_verified":false}))
}
#[cfg(target_os = "linux")]
fn namespace_mount(storage: &Storage, root: &Path) -> Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } != 0 {
        return Err(Error::new("isolation_unavailable",format!("Cannot create user/mount namespace: {}. Enable user namespaces or choose a supported Linux setup.",io::Error::last_os_error())));
    }
    fs::write("/proc/self/setgroups", b"deny")?;
    fs::write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;
    fs::write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            (libc::MS_REC | libc::MS_PRIVATE) as _,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    mount_overlay(
        &storage.instance().join("game"),
        root,
        &root.join("view/game"),
    )
}
#[cfg(target_os = "linux")]
fn mount_overlay(lower: &Path, root: &Path, view: &Path) -> Result<PathBuf> {
    use std::ffi::CString;
    for path in [lower.to_path_buf(), root.join("upper"), root.join("work")] {
        if path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| b",:\\\n".contains(b))
        {
            return Err(Error::new("overlay_path_unsupported", "Overlay paths cannot contain commas, colons, backslashes or newlines. Choose a different owned storage path."));
        }
    }
    fs::create_dir_all(view)?;
    let opts = CString::new(format!(
        "lowerdir={},upperdir={},workdir={},userxattr",
        lower.display(),
        root.join("upper").display(),
        root.join("work").display()
    ))
    .map_err(|_| Error::new("invalid_path", "Invalid overlay path."))?;
    let dest = CString::new(view.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::new("invalid_path", "Invalid mount path."))?;
    if unsafe {
        libc::mount(
            c"overlay".as_ptr(),
            dest.as_ptr(),
            c"overlay".as_ptr(),
            0,
            opts.as_ptr().cast(),
        )
    } != 0
    {
        return Err(Error::new("overlay_unavailable",format!("Overlay mount failed: {}. Use compatible owned storage and enable unprivileged overlay mounts.",io::Error::last_os_error())));
    }
    Ok(view.to_path_buf())
}
#[cfg(not(target_os = "linux"))]
fn namespace_mount(_storage: &Storage, _root: &Path) -> Result<PathBuf> {
    Err(Error::new(
        "launch_unsupported",
        "Isolated play currently supports Linux only.",
    ))
}
#[cfg(target_os = "linux")]
fn tool_view(storage: &Storage, source: &Path, entrypoint: &str, kind: &str) -> Result<PathBuf> {
    let mut identity = source.as_os_str().as_encoded_bytes().to_vec();
    identity.extend(files::bounded(&source.join(entrypoint), 1024 * 1024)?);
    if source.join("version").is_file() {
        identity.extend(files::bounded(&source.join("version"), 1024 * 1024)?);
    }
    let root = storage
        .root
        .join("personal")
        .join(kind)
        .join(files::hash_bytes(&identity));
    for name in ["upper", "work", "view"] {
        private_directory(&root.join(name), &storage.root, false)?;
    }
    mount_overlay(source, &root, &root.join("view"))
}
#[cfg(not(target_os = "linux"))]
fn tool_view(_storage: &Storage, source: &Path, _entrypoint: &str, _kind: &str) -> Result<PathBuf> {
    Ok(source.to_path_buf())
}
pub fn helper(
    coop: &Path,
    steam: Option<&Path>,
    proton: Option<&Path>,
    runtime: Option<&Path>,
    probe: bool,
) -> Result<Value> {
    let storage = Storage::open(coop)?;
    let rev = revision::installed(&storage)?;
    let root = rt_root(&storage, &rev)?;
    // Persist final controller counters before the helper exits. A fast loader
    // can otherwise finish before the supervisor first discovers its cgroup.
    let _accounting = Accounting::new(&root)?;
    for dir in ["upper", "work", "view"] {
        let p = root.join(dir);
        private_directory(&p, &storage.root, false)?;
        files::root(&p)?;
    }
    let game = namespace_mount(&storage, &root)?;
    if probe {
        let marker = game.join(".skyrimsync/runtime-probe.txt");
        fs::create_dir_all(marker.parent().unwrap())?;
        fs::write(&marker, b"runtime isolated")?;
        let mut tools_isolated = true;
        for (source, entrypoint, kind) in [
            (proton, "proton", "proton"),
            (runtime, "run", "steam-runtime"),
        ] {
            if let Some(source) = source {
                let view = tool_view(&storage, source, entrypoint, kind)?;
                fs::write(view.join(".skyrimsync-probe"), b"private tool state")?;
                tools_isolated &= !source.join(".skyrimsync-probe").exists();
            }
        }
        return Ok(
            json!({"state":"isolation_probe","baseline_untouched":!storage.instance().join("game/.skyrimsync/runtime-probe.txt").exists(),"runtime_written":root.join("upper/.skyrimsync/runtime-probe.txt").is_file(),"tools_isolated":tools_isolated}),
        );
    }
    if std::env::var_os("SMS_LAUNCH_CAPPED").is_none() {
        return Err(Error::new(
            "memory_cap_required",
            "Invoke play to enforce a process-tree memory limit.",
        ));
    }
    if _accounting.is_none() {
        return Err(Error::new(
            "memory_cap_required",
            "Invoke play inside its verified process-tree scope.",
        ));
    }
    let steam = steam.ok_or_else(|| Error::new("steam_missing", "Select Steam."))?;
    let proton = proton.ok_or_else(|| Error::new("proton_missing", "Select Proton."))?;
    let runtime =
        runtime.ok_or_else(|| Error::new("runtime_missing", "Select the Steam Linux runtime."))?;
    configure(&storage, &rev, &root)?;
    effective(
        &storage,
        &rev,
        &game,
        &root.join("documents"),
        &root.join("local"),
    )?;
    let prefix = storage.root.join("personal/prefix");
    private_directory(&prefix, &storage.root, false)?;
    files::root(&prefix)?;
    let cfg = storage.root.join("personal");
    let proton_view = tool_view(&storage, proton, "proton", "proton")?;
    let proton = proton_view.as_path();
    let runtime_view = tool_view(&storage, runtime, "run", "steam-runtime")?;
    let runtime = runtime_view.as_path();
    // Keep cached runtime copies on the same overlay filesystem as their source:
    // pressure-vessel uses hardlinks, falling back to costly copies across mounts.
    let cache = runtime_view.join(".skyrimsync-cache");
    private_directory(&cache, &storage.root, false)?;
    let mut cmd = Command::new(
        crate::config::which("bwrap")
            .ok_or_else(|| Error::new("isolation_missing", "Install bubblewrap."))?,
    );
    let home = crate::config::home()?;
    cmd.args([
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--unshare-pid",
        "--die-with-parent",
        "--tmpfs",
    ])
    .arg(&home)
    .args(["--dev-bind-try", "/dev/dri", "/dev/dri"])
    .args([
        "--bind-try",
        "/dev/shm",
        "/dev/shm",
        "--ro-bind-try",
        "/tmp/steam",
        "/tmp/steam",
    ])
    .args([
        "--tmpfs",
        "/tmp",
        "--ro-bind-try",
        "/tmp/.X11-unix",
        "/tmp/.X11-unix",
    ])
    .arg("--bind")
    .arg(&cfg)
    .arg(&cfg)
    .arg("--bind")
    .arg(&root)
    .arg(&root)
    .arg("--ro-bind")
    .arg(steam)
    .arg(steam)
    .arg("--bind")
    .arg(proton)
    .arg(proton)
    .arg("--bind")
    .arg(runtime)
    .arg(runtime)
    .arg("--ro-bind")
    .arg(steam.join("linux64"))
    .arg(home.join(".steam/sdk64"))
    .arg("--ro-bind-try")
    .arg(steam.join("linux32"))
    .arg(home.join(".steam/sdk32"))
    .arg("--ro-bind-try")
    .arg(home.join(".steam/steam.pid"))
    .arg(home.join(".steam/steam.pid"))
    .arg("--ro-bind-try")
    .arg(home.join(".steam/steam.pipe"))
    .arg(home.join(".steam/steam.pipe"))
    .arg("--symlink")
    .arg(steam)
    .arg(home.join(".steam/root"))
    .arg("--symlink")
    .arg(steam)
    .arg(home.join(".steam/steam"))
    .arg("--chdir")
    .arg(&game)
    .arg("--");
    let mut devices = Command::new(cmd.get_program());
    let args = cmd.get_args().collect::<Vec<_>>();
    devices.args(&args[..args.len() - 1]);
    for entry in fs::read_dir("/dev")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("nvidia")
            && !entry.file_type()?.is_symlink()
        {
            devices
                .arg("--dev-bind")
                .arg(entry.path())
                .arg(entry.path());
        }
    }
    devices.arg("--");
    cmd = devices;
    if let Some(authority) = std::env::var_os("XAUTHORITY") {
        let authority = PathBuf::from(authority);
        if authority.starts_with(&home) && authority.is_file() {
            // Insert the display credential bind before the command separator.
            let mut display = Command::new(cmd.get_program());
            let args = cmd.get_args().collect::<Vec<_>>();
            display
                .args(&args[..args.len() - 1])
                .arg("--ro-bind")
                .arg(&authority)
                .arg(&authority)
                .arg("--");
            cmd = display;
        }
    }
    for (k, _) in std::env::vars_os() {
        let s = k.to_string_lossy();
        if s.starts_with("WINE")
            || s.starts_with("STEAM_COMPAT_")
            || s.starts_with("LD_")
            || s.starts_with("NEXUS_")
        {
            cmd.env_remove(k);
        }
    }
    cmd.env("PRESSURE_VESSEL_VARIABLE_DIR", &cache)
        .env("PRESSURE_VESSEL_FILESYSTEMS_RO", std::env::current_exe()?)
        .env("STEAM_COMPAT_INSTALL_PATH", &game)
        .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", steam)
        .env("STEAM_COMPAT_DATA_PATH", &prefix)
        .env("WINEPREFIX", prefix.join("pfx"))
        .env("SteamAppId", "489830")
        .env("SteamGameId", "489830")
        .env("STEAM_COMPAT_APP_ID", "489830");
    if files::locate(&game, "d3dx9_42.dll")?.is_some() {
        cmd.env("WINEDLLOVERRIDES", "d3dx9_42=n,b");
    }
    cmd.arg(runtime.join("run"))
        .arg("--")
        .arg(std::env::current_exe()?)
        .args(["--json", "__wine-session", "--proton"])
        .arg(proton)
        .arg("--runtime")
        .arg(runtime)
        .arg("--program");
    let user = prefix.join("pfx/drive_c/users/steamuser");
    if !user.is_dir() {
        let status = cmd
            .clone_for_probe()?
            .arg("wineboot.exe")
            .arg("--argument=-u")
            .status()?;
        if !status.success() {
            return Err(Error::new(
                "prefix_failed",
                "Private Proton initialization failed; inspect the launch log.",
            ));
        }
    }
    private_directory(&user, &prefix, false)?;
    let documents = user.join("Documents");
    private_directory(&documents, &prefix, true)?;
    let appdata = user.join("AppData/Local");
    private_directory(&appdata, &prefix, false)?;
    let doc_target = documents.join("My Games/Skyrim Special Edition");
    private_directory(&doc_target, &prefix, false)?;
    let local_target = appdata.join("Skyrim Special Edition");
    private_directory(&local_target, &prefix, false)?;
    for n in snapshot::DOCUMENTS {
        files::copy(
            &storage.root.join("personal/documents").join(n),
            &doc_target.join(n),
        )?;
    }
    for n in snapshot::LOCAL {
        files::copy(
            &storage.root.join("personal/local").join(n),
            &local_target.join(n),
        )?;
    }
    let saves = storage.root.join("personal/saves");
    fs::create_dir_all(&saves)?;
    let save_link = doc_target.join("Saves");
    if save_link.exists() || save_link.is_symlink() {
        if !save_link.is_symlink() || fs::read_link(&save_link)? != saves {
            return Err(Error::new(
                "unsafe_prefix",
                "Unexpected save directory; preserve it and choose a fresh owned prefix.",
            ));
        }
    } else {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&saves, &save_link)?;
    }
    let drives = prefix.join("pfx/dosdevices");
    private_directory(&drives, &prefix, false)?;
    let short = drives.join("g:");
    if short.exists() || short.is_symlink() {
        if !short.is_symlink() || !fs::read_link(&short)?.starts_with(storage.root.join("runtime"))
        {
            return Err(Error::new(
                "unsafe_prefix",
                "G: points outside owned runtime storage.",
            ));
        }
        fs::remove_file(&short)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&game, &short)?;
    let loader = files::require(&game, "skse64_loader.exe")?;
    let mut game_cmd = cmd.clone_for_probe()?;
    // Set the DOS working directory too: Linux chdir can translate back to a long
    // Z: path, even when the executable was named through the short G: alias.
    game_cmd.arg("cmd.exe").arg("--argument=/c").arg(format!(
        "--argument=cd /d G:\\ && {} -waitforclose",
        loader.file_name().unwrap().to_string_lossy()
    ));
    files::atomic_json(
        &storage.root.join("last-launch.json"),
        &json!({"revision":rev.id()?,"state":"running"}),
    )?;
    let status = game_cmd.status()?;
    for n in snapshot::DOCUMENTS {
        if let Some(p) = files::locate(&doc_target, n)? {
            files::copy(&p, &root.join("documents").join(n))?;
            files::copy(&p, &storage.root.join("personal/documents").join(n))?;
        }
    }
    for n in snapshot::LOCAL {
        if let Some(p) = files::locate(&local_target, n)? {
            files::copy(&p, &root.join("local").join(n))?;
        }
    }
    let match_after = effective(
        &storage,
        &rev,
        &game,
        &root.join("documents"),
        &root.join("local"),
    );
    files::atomic_json(
        &storage.root.join("last-launch.json"),
        &json!({"revision":rev.id()?,"exit_success":status.success()}),
    )?;
    let out = json!({"state":"game_exited","exit_success":status.success(),"effective_match":match_after.is_ok(),"runtime_changes":changes(&storage)?,"runtime_verified":false});
    if !status.success() {
        return Err(Error::new(
            "game_failed",
            "Game exited unsuccessfully. Runtime data is retained; inspect launch.log.",
        ));
    }
    Ok(out)
}
trait CloneCommand {
    fn clone_for_probe(&self) -> Result<Command>;
}
impl CloneCommand for Command {
    fn clone_for_probe(&self) -> Result<Command> {
        let mut c = Command::new(self.get_program());
        c.args(self.get_args());
        for (k, v) in self.get_envs() {
            if let Some(v) = v {
                c.env(k, v);
            } else {
                c.env_remove(k);
            }
        }
        if let Some(p) = self.get_current_dir() {
            c.current_dir(p);
        }
        Ok(c)
    }
}
pub fn play(
    storage: &Storage,
    steam: &Path,
    proton: &Path,
    runtime: &Path,
    memory: u64,
    timeout: Option<u64>,
) -> Result<Value> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(Error::new(
            "launch_unsupported",
            "Linux x86-64 launch only.",
        ));
    }
    if memory == 0 || memory > 65536 {
        return Err(Error::new(
            "invalid_memory_limit",
            "Choose a process-tree cap between 1 and 65536 MiB.",
        ));
    }
    for p in [steam, proton, runtime] {
        files::root(p)?;
    }
    for p in [proton.join("proton"), runtime.join("run")] {
        files::regular(&p)?;
    }
    files::regular(&steam.join("linux64/steamclient.so")).map_err(|_|Error::new("steam_sdk_missing", "Selected Steam has no native linux64/steamclient.so. Select the current native Steam installation with --steam."))?;
    if crate::config::which("systemd-run").is_none() {
        return Err(Error::new(
            "memory_cap_unavailable",
            "A working systemd user session is required to enforce the game memory cap.",
        ));
    }
    let _ = crate::authoring::refresh(storage)?;
    let _lock = files::Lock::take(&storage.root.join("locks/target.lock"))?;
    crate::target::running(&storage.instance())?;
    storage.recover()?;
    let rev = revision::installed(storage)?;
    if crate::target::verify(&rev.legacy(), &storage.instance())?["files_match"] != true {
        return Err(Error::new(
            "instance_mismatch",
            "Repair the installed revision before play.",
        ));
    }
    fs::create_dir_all(storage.root.join("personal"))?;
    let root = rt_root(storage, &rev)?;
    fs::create_dir_all(&root)?;
    let log = root.join("launch.log");
    let output = fs::File::create(&log)?;
    let unit = format!(
        "sms-play-{}-{}",
        std::process::id(),
        crate::target::unique()
    );
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--scope", "--quiet"])
        .arg(format!("--unit={unit}"))
        .arg("-p")
        .arg(format!("MemoryMax={}M", memory))
        .args(["-p", "MemorySwapMax=0"])
        .arg(std::env::current_exe()?)
        .args(["--json", "__launch-helper", "--coop"])
        .arg(&storage.root)
        .arg("--steam")
        .arg(steam)
        .arg("--proton")
        .arg(proton)
        .arg("--runtime")
        .arg(runtime)
        .env("SMS_LAUNCH_CAPPED", "1")
        .env("SMS_LAUNCH_UNIT", &unit)
        .stdout(Stdio::from(output.try_clone()?))
        .stderr(Stdio::from(output));
    let mut child = command.spawn()?;
    let started = Instant::now();
    let mut heartbeat = Instant::now();
    let mut cgroup = None;
    let mut peak = 0_u64;
    let mut anon_peak = 0_u64;
    let mut oom_kills = 0_u64;
    crate::event(
        "game_launching",
        json!({"revision":rev.id()?,"memory_limit_mib":memory,"log":log}),
    );
    let status = loop {
        if cgroup.is_none() {
            if let Ok(output) = Command::new("systemctl")
                .args(["--user", "show", "--property=ControlGroup", "--value"])
                .arg(format!("{unit}.scope"))
                .output()
            {
                let relative = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if output.status.success()
                    && relative.starts_with('/')
                    && !relative.contains("..")
                    && relative.ends_with(&format!("/{unit}.scope"))
                {
                    cgroup =
                        Some(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')));
                }
            }
        }
        let current = cgroup
            .as_ref()
            .and_then(|p| fs::read_to_string(p.join("memory.current")).ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let kernel_peak = cgroup
            .as_ref()
            .and_then(|p| fs::read_to_string(p.join("memory.peak")).ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0);
        peak = peak.max(current).max(kernel_peak);
        if let Some(path) = &cgroup {
            let stats = fs::read_to_string(path.join("memory.stat")).unwrap_or_default();
            let anonymous: u64 = stats
                .lines()
                .filter_map(|l| l.split_once(' '))
                .filter(|(k, _)| ["anon", "shmem"].contains(k))
                .filter_map(|(_, v)| v.parse::<u64>().ok())
                .sum();
            anon_peak = anon_peak.max(anonymous);
            let events = fs::read_to_string(path.join("memory.events")).unwrap_or_default();
            oom_kills = oom_kills.max(
                events
                    .lines()
                    .find_map(|l| l.strip_prefix("oom_kill "))
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0),
            );
        }
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if crate::check_cancel().is_err()
            || timeout.is_some_and(|t| started.elapsed().as_secs() >= t)
        {
            let _ = Command::new("systemctl")
                .args(["--user", "kill", "--signal=SIGTERM"])
                .arg(format!("{unit}.scope"))
                .output();
            std::thread::sleep(Duration::from_secs(2));
            if let Some(path) = &cgroup {
                let _ = fs::write(path.join("cgroup.kill"), b"1");
            }
            let _ = Command::new("systemctl")
                .args(["--user", "kill", "--signal=SIGKILL"])
                .arg(format!("{unit}.scope"))
                .output();
            let _ = child.wait();
            let _ = Command::new("systemctl")
                .args(["--user", "stop"])
                .arg(format!("{unit}.scope"))
                .output();
            preserve_failed_session(storage, &rev, &root, "interrupted")?;
            files::atomic_json(
                &root.join("launch-metrics.json"),
                &json!({"memory_limit_mib":memory,"peak_bytes":peak,"anonymous_peak_bytes":anon_peak,"oom_kills":oom_kills,"elapsed_seconds":started.elapsed().as_secs(),"state":"interrupted"}),
            )?;
            if crate::check_cancel().is_err() {
                return crate::check_cancel().map(|_| json!({}));
            }
            return Err(Error::new(
                "launch_timeout",
                format!(
                    "Owned game session timed out; runtime data remains. Inspect {}.",
                    log.display()
                ),
            ));
        }
        if heartbeat.elapsed() > Duration::from_secs(20) {
            crate::event(
                "game_running",
                json!({"elapsed_seconds":started.elapsed().as_secs(),"memory_limit_mib":memory,"memory_current_bytes":current,"memory_peak_bytes":peak,"anonymous_peak_bytes":anon_peak,"oom_kills":oom_kills,"log_bytes":fs::metadata(&log)?.len()}),
            );
            heartbeat = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // A loader can return zero after its game child was OOM-killed. The cgroup
    // event, rather than the loader's exit code, decides this failure.
    if let Some(path) = &cgroup {
        let events = fs::read_to_string(path.join("memory.events")).unwrap_or_default();
        oom_kills = oom_kills.max(
            events
                .lines()
                .find_map(|l| l.strip_prefix("oom_kill "))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0),
        );
    }
    let accounting = root.join("launch-accounting.json");
    let mut accounting_recorded = false;
    if accounting.exists() {
        let final_stats: Value = files::read_json(&accounting)?;
        if final_stats["unit"] == unit {
            accounting_recorded = true;
            oom_kills = oom_kills.max(final_stats["oom_kills"].as_u64().unwrap_or(0));
            peak = peak.max(final_stats["peak_bytes"].as_u64().unwrap_or(0));
        }
    }
    let _ = Command::new("systemctl")
        .args(["--user", "stop"])
        .arg(format!("{unit}.scope"))
        .output();
    files::atomic_json(
        &root.join("launch-metrics.json"),
        &json!({"memory_limit_mib":memory,"peak_bytes":peak,"anonymous_peak_bytes":anon_peak,"oom_kills":oom_kills,"elapsed_seconds":started.elapsed().as_secs(),"exit_success":status.success() && oom_kills==0 && accounting_recorded}),
    )?;
    if status.success() && oom_kills == 0 && !accounting_recorded {
        return Err(Error::new("launch_accounting_missing", format!("Private launch did not produce its final resource report. Check free space and permissions, then retry. Runtime data is retained; inspect {}.",log.display())));
    }
    if !status.success() || oom_kills > 0 {
        preserve_failed_session(storage, &rev, &root, "failed")?;
        if oom_kills > 0 {
            return Err(Error::new("memory_limit_exceeded", format!("The owned game session exceeded {} MiB. Runtime data remains; inspect {} and reduce the pack's memory requirements before retrying.", memory, log.display())));
        }
        if let Some(error) = helper_failure(&log)? {
            return Err(error);
        }
        return Err(Error::new("game_failed",format!("Owned game session failed. Inspect {} for the setup or game error. Runtime data remains.",log.display())));
    }
    Ok(
        json!({"state":"game_exited","revision":rev.id()?,"elapsed_seconds":started.elapsed().as_secs(),"memory_peak_bytes":peak,"log":log,"runtime_changes":changes(storage)?,"runtime_verified":false}),
    )
}

struct Accounting {
    unit: String,
    cgroup: PathBuf,
    output: PathBuf,
}
impl Accounting {
    fn new(root: &Path) -> Result<Option<Self>> {
        let Ok(unit) = std::env::var("SMS_LAUNCH_UNIT") else {
            return Ok(None);
        };
        if !unit.starts_with("sms-play-")
            || !unit.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::new(
                "invalid_launch_unit",
                "Invalid owned launch scope.",
            ));
        }
        let relative = fs::read_to_string("/proc/self/cgroup")?
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .ok_or_else(|| {
                Error::new(
                    "memory_cap_unavailable",
                    "A cgroup v2 process-tree limit is required.",
                )
            })?
            .to_owned();
        if !relative.starts_with('/')
            || relative.contains("..")
            || !relative.ends_with(&format!("/{unit}.scope"))
        {
            return Err(Error::new(
                "memory_cap_unavailable",
                "Launch helper is outside its owned memory scope.",
            ));
        }
        let cgroup = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
        let limit = fs::read_to_string(cgroup.join("memory.max"))?
            .trim()
            .parse::<u64>()
            .ok();
        if !limit.is_some_and(|n| n > 0 && n <= 65536 * 1024 * 1024)
            || fs::read_to_string(cgroup.join("memory.swap.max"))?.trim() != "0"
        {
            return Err(Error::new(
                "memory_cap_required",
                "A finite memory limit with zero swap is required; invoke play.",
            ));
        }
        Ok(Some(Self {
            unit,
            cgroup,
            output: root.join("launch-accounting.json"),
        }))
    }
}
impl Drop for Accounting {
    fn drop(&mut self) {
        let events = fs::read_to_string(self.cgroup.join("memory.events")).unwrap_or_default();
        let oom = events
            .lines()
            .find_map(|l| l.strip_prefix("oom_kill "))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let peak = fs::read_to_string(self.cgroup.join("memory.peak"))
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let _ = files::atomic_json(
            &self.output,
            &json!({"unit":self.unit,"oom_kills":oom,"peak_bytes":peak}),
        );
    }
}

fn helper_failure(log: &Path) -> Result<Option<Error>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(log)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(1024 * 1024)))?;
    let mut tail = Vec::new();
    file.take(1024 * 1024).read_to_end(&mut tail)?;
    for line in String::from_utf8_lossy(&tail).lines().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if value["state"] == "failed" {
                if let (Some(reason), Some(message)) =
                    (value["reason"].as_str(), value["message"].as_str())
                {
                    return Ok(Some(Error::new(
                        reason,
                        format!("{message} Details: {}", log.display()),
                    )));
                }
            }
        }
    }
    Ok(None)
}

// Only read a terminated, owned prefix. A crash must not hide reviewable INI edits.
fn preserve_failed_session(
    storage: &Storage,
    rev: &Revision,
    root: &Path,
    state: &str,
) -> Result<()> {
    let marker = storage.root.join("last-launch.json");
    if !marker.exists() {
        return Ok(());
    }
    let previous: Value = files::read_json(&marker)?;
    if previous["revision"] != rev.id()? {
        return Ok(());
    }
    let prefix = storage.root.join("personal/prefix");
    let user = prefix.join("pfx/drive_c/users/steamuser");
    for (relative, target, names) in [
        (
            "Documents/My Games/Skyrim Special Edition",
            "documents",
            snapshot::DOCUMENTS.as_slice(),
        ),
        (
            "AppData/Local/Skyrim Special Edition",
            "local",
            snapshot::LOCAL.as_slice(),
        ),
    ] {
        let source = user.join(relative);
        if source.is_dir() {
            private_directory(&source, &prefix, false)?;
            for name in names {
                if let Some(path) = files::locate(&source, name)? {
                    files::copy(&path, &root.join(target).join(name))?;
                }
            }
        }
    }
    files::atomic_json(
        &marker,
        &json!({"revision":rev.id()?,"state":state,"exit_success":false}),
    )
}

pub fn diagnostics() -> Value {
    let tools = ["bwrap", "systemd-run", "systemctl"]
        .into_iter()
        .map(|name| (name, crate::config::which(name)))
        .collect::<BTreeMap<_, _>>();
    json!({"supported_platform":cfg!(all(target_os="linux",target_arch="x86_64")),"tools":tools,"cgroup_v2":Path::new("/sys/fs/cgroup/cgroup.controllers").is_file(),"user_namespaces_configured":fs::read_to_string("/proc/sys/user/max_user_namespaces").ok().and_then(|v|v.trim().parse::<u64>().ok()).is_some_and(|v|v>0),"kernel_overlay_configured":fs::read_to_string("/proc/filesystems").unwrap_or_default().contains("overlay"),"default_memory_limit_mib":4096,"swap_limit_bytes":0,"setup_verified":false,"remedy":"Use native Linux with bubblewrap, a systemd user session, user namespaces and OverlayFS; supply --steam, --proton and --runtime if discovery is ambiguous. Play performs actual mount and capped launch checks."})
}

/// Run Proton and wait for this sandbox's private Wine session before returning.
/// Executed inside bubblewrap so wineserver sees the same private /tmp socket.
pub fn wine_session(proton: &Path, _runtime: &Path, program: &str, args: &[String]) -> Result<()> {
    if std::env::var_os("SMS_LAUNCH_CAPPED").is_none() {
        return Err(Error::new(
            "memory_cap_required",
            "Use play for a capped private session.",
        ));
    }
    let status = Command::new(proton.join("proton"))
        .arg("run")
        .arg(program)
        .args(args)
        .status()?;
    let server = proton.join("files/bin/wineserver");
    if server.is_file() {
        let wait = Command::new(&server).arg("-w").status()?;
        if !wait.success() {
            return Err(Error::new(
                "prefix_wait_failed",
                "Private Wine session did not exit cleanly.",
            ));
        }
    }
    if !status.success() {
        return Err(Error::new(
            "game_failed",
            "Proton process failed. Inspect launch.log.",
        ));
    }
    Ok(())
}
