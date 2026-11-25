use crate::{acquisition, config, files, recipe::Recipe, Error, Result};
use flate2::read::GzDecoder;
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};
const ENGINE_SHA: &str = "10480a3d452f6611d236219aab450e43bdcd7eab2f9c5cde96fb43e55ce5625d";
const ENGINE_SIZE: u64 = 58_345_382;
const ENGINE_URL:&str="https://github.com/Omni-guides/dev-jackify-engine/releases/download/v0.5.10/jackify-engine-0.5.10-linux-x64.tar.gz";
fn verify_inventory(root: &Path, expected: &BTreeSet<String>) -> Result<()> {
    let actual = files::inventory(root)?;
    // NLog writes relative to the executable directory, including startup rotation.
    // Allow only these log names; binaries and all other unexpected files still fail.
    let runtime_log = |name: &str| {
        name.strip_prefix("logs/wabbajack-cli.")
            .and_then(|s| s.strip_suffix(".log"))
            .is_some_and(|s| {
                s == "current"
                    || (!s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()))
            })
    };
    if actual
        .keys()
        .any(|n| !expected.contains(n) && !runtime_log(n))
        || expected.iter().any(|n| !actual.contains_key(n))
    {
        return Err(Error::new("engine_changed", "Unexpected or missing files in the managed engine. Preserve logs and reinstall the pinned engine folder."));
    }
    Ok(())
}
pub fn ensure(storage: &Path) -> Result<PathBuf> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(Error::new("engine_unsupported","Recipe reconstruction currently supports Linux x86-64. Capture and compare are portable; acquisition on this platform is not validated."));
    }
    let _lock = files::Lock::take(&storage.join("locks/engine.lock"))?;
    let archive = storage
        .join("tools/downloads")
        .join(format!("{ENGINE_SHA}.tar.gz"));
    acquisition::download(ENGINE_URL, &archive, ENGINE_SIZE, ENGINE_SHA)?;
    let dest = storage.join("tools/jackify-engine-0.5.10");
    let existing = dest.is_dir();
    let temp = tempfile::tempdir_in(storage.join("tools"))?;
    let stage = temp.path().join("files");
    fs::create_dir(&stage)?;
    let mut tar = tar::Archive::new(GzDecoder::new(File::open(&archive)?));
    let mut seen = BTreeSet::new();
    let mut files_seen = BTreeSet::new();
    let mut total = 0u64;
    for (i, entry) in tar.entries()?.enumerate() {
        crate::check_cancel()?;
        if i > 2000 {
            return Err(Error::new(
                "invalid_engine",
                "Too many engine archive entries.",
            ));
        }
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        let path = path.strip_prefix("./").unwrap_or(&path);
        if (path == "." || path.is_empty()) && entry.header().entry_type().is_dir() {
            continue;
        }
        let path = if entry.header().entry_type().is_dir() {
            path.strip_suffix('/').unwrap_or(path)
        } else {
            path
        };
        files::relative(path)?;
        if !seen.insert(files::fold(path)) {
            return Err(Error::new(
                "invalid_engine",
                "Duplicate engine archive entry.",
            ));
        }
        let typ = entry.header().entry_type();
        if typ.is_dir() {
            continue;
        }
        if !typ.is_file() {
            return Err(Error::new(
                "invalid_engine",
                "Engine archive contains links or special files.",
            ));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| Error::new("invalid_engine", "Engine size overflow."))?;
        if total > 512 * 1024 * 1024 {
            return Err(Error::new(
                "invalid_engine",
                "Expanded engine exceeds 512 MiB.",
            ));
        }
        files_seen.insert(path.to_owned());
        if existing {
            let installed = files::require(&dest, path)?;
            if fs::metadata(&installed)?.len() != entry.size()
                || files::sha_reader(&mut entry)? != files::sha(&installed)?
            {
                return Err(Error::new("engine_changed","Managed engine differs from the pinned release. Remove its tools/jackify-engine-0.5.10 folder to reinstall."));
            }
        } else {
            let target = stage.join(path);
            fs::create_dir_all(target.parent().unwrap())?;
            let mut f = File::create(&target)?;
            std::io::copy(&mut entry, &mut f)?;
            f.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &target,
                    fs::Permissions::from_mode(if entry.header().mode()? & 0o111 != 0 {
                        0o755
                    } else {
                        0o644
                    }),
                )?;
            }
        }
    }
    if existing {
        verify_inventory(&dest, &files_seen)?;
    }
    if !existing {
        files::require(&stage, "jackify-engine")?;
        fs::rename(&stage, &dest)?;
        files::sync_dir(dest.parent().unwrap())?;
    }
    Ok(dest.join("jackify-engine"))
}
pub fn namespace(
    storage: &Path,
    job: &Path,
    base: &Path,
    downloads: &Path,
    steam: Option<&Path>,
    proton: Option<&Path>,
) -> Result<Command> {
    let bwrap = config::which("bwrap").ok_or_else(|| {
        Error::new(
            "isolation_missing",
            "Install bubblewrap (bwrap), then retry. The installer needs a private home directory.",
        )
    })?;
    let home = config::home()?;
    let cfg = storage.join("engine-config");
    let data = storage.join("engine-data");
    fs::create_dir_all(&cfg)?;
    fs::create_dir_all(&data)?;
    let mut settings = json!({"jackify_data_dir":data});
    if let Some(p) = proton {
        settings["proton_path"] = json!(p);
    }
    files::atomic_json(&cfg.join("config.json"), &settings)?;
    let mut cmd = Command::new(bwrap);
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
    .arg("--dir")
    .arg(home.join(".config"));
    let mut mounts = BTreeSet::new();
    for p in [Some(base), steam, proton].into_iter().flatten() {
        let p = p.canonicalize()?;
        if mounts.insert(p.clone()) {
            cmd.arg("--ro-bind").arg(&p).arg(&p);
        }
    }
    cmd.arg("--bind")
        .arg(storage)
        .arg(storage)
        .arg("--bind")
        .arg(downloads)
        .arg(downloads)
        .arg("--bind")
        .arg(&cfg)
        .arg(home.join(".config/jackify"))
        .arg("--bind")
        .arg(&data)
        .arg(home.join("Jackify"));
    let engine = storage.join("tools/jackify-engine-0.5.10");
    if engine.exists() {
        files::root(&engine)?;
        let log_mount = engine.join("logs");
        let logs = job.join("engine-logs");
        for p in [&log_mount, &logs] {
            if p.exists() {
                files::root(p)?;
            } else {
                fs::create_dir_all(p)?;
            }
        }
        // Only the job's log mount is writable inside the pinned engine tree.
        cmd.arg("--ro-bind")
            .arg(&engine)
            .arg(&engine)
            .arg("--bind")
            .arg(&logs)
            .arg(&log_mount);
    }
    cmd.arg("--chdir").arg(job).arg("--");
    // Engine hardcodes ~/.config/jackify. Preserve HOME, provide its private mounts,
    // and prevent inherited launch/Nexus credentials from selecting another identity.
    for (k, _) in std::env::vars_os() {
        let kstr = k.to_string_lossy();
        if kstr.starts_with("WINE")
            || kstr.starts_with("STEAM_COMPAT_")
            || kstr.starts_with("LD_")
            || kstr.starts_with("NEXUS_")
        {
            cmd.env_remove(k);
        }
    }
    Ok(cmd)
}
pub fn reconstruct(
    recipe: &Recipe,
    storage: &Path,
    base: &Path,
    downloads: &Path,
    steam: Option<&Path>,
    proton: Option<&Path>,
) -> Result<PathBuf> {
    let job = storage.join("jobs").join(files::sha(&recipe.artifact)?);
    fs::create_dir_all(&job)?;
    let output = job.join("reconstructed");
    fs::create_dir_all(&output)?;
    let _lock = files::Lock::take(&job.join("writer.lock"))?;
    let plan = recipe.archives(base, downloads)?;
    if !plan["base_problems"].as_array().unwrap().is_empty() {
        crate::event("base_requirements", plan["base_problems"].clone());
        return Err(Error::new("base_mismatch","Required licensed game files differ. Select the required Skyrim edition/version with --base."));
    }
    let missing = plan["missing"].as_array().unwrap();
    if !plan["unsupported_missing_sources"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        crate::event("manual_downloads_required", plan.clone());
        return Err(Error::new("manual_downloads_required","Some recipe sources cannot be acquired automatically. Download the listed archives into --downloads, then retry."));
    }
    let needs_nexus = missing
        .iter()
        .any(|a| a["kind"].as_str().unwrap_or("").contains("Nexus"));
    let key = config::load_key(needs_nexus)?;
    if needs_nexus && !acquisition::validate_nexus(key.as_deref().unwrap())? {
        crate::event("manual_downloads_required", json!(missing));
        return Err(Error::new("manual_downloads_required","Missing Nexus archives require Premium for unattended acquisition. Download the listed files manually into --downloads and retry; cached archives are verified by content."));
    }
    let expanded = plan["reconstruction_bytes"].as_u64().unwrap();
    let working = expanded
        .checked_mul(2)
        .and_then(|n| n.checked_add(plan["missing_bytes"].as_u64().unwrap()))
        .ok_or_else(|| Error::new("invalid_recipe", "Required space overflow."))?;
    files::space(storage, working)?;
    files::space(downloads, plan["missing_bytes"].as_u64().unwrap())?;
    let engine = ensure(storage)?;
    let mut cmd = namespace(storage, &job, base, downloads, steam, proton)?;
    cmd.arg(engine)
        .args(["install", "-w"])
        .arg(&recipe.artifact)
        .arg("-o")
        .arg(&output)
        .arg("-d")
        .arg(downloads)
        .arg("-g")
        .arg(base)
        .arg("--show-file-progress");
    if let Some(k) = key {
        cmd.env("NEXUS_API_KEY", k);
    }
    let log = job.join("engine.log");
    let file = File::create(&log)?;
    cmd.stdout(Stdio::from(file.try_clone()?))
        .stderr(Stdio::from(file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    crate::event("reconstructing", json!({"log":log}));
    let mut child = cmd.spawn()?;
    let started = std::time::Instant::now();
    let mut heartbeat = started;
    loop {
        if heartbeat.elapsed() >= Duration::from_secs(20) {
            crate::event(
                "reconstructing",
                json!({"elapsed_seconds":started.elapsed().as_secs(),"log_bytes":fs::metadata(&log)?.len(),"log":log}),
            );
            heartbeat = std::time::Instant::now();
        }
        if crate::check_cancel().is_err() {
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGTERM);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
            for _ in 0..20 {
                if child.try_wait()?.is_some() {
                    return crate::check_cancel().map(|_| output);
                }
                thread::sleep(Duration::from_millis(100));
            }
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            return crate::check_cancel().map(|_| output);
        }
        if let Some(s) = child.try_wait()? {
            if !s.success() {
                return Err(Error::new("reconstruction_failed",format!("Installer exited with {s}; inspect {}. Downloads remain cached. Correct the reported source/configuration issue and retry.",log.display())));
            }
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_installer_logs_are_allowed_but_extra_binaries_are_rejected() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("jackify-engine"), b"pinned fixture").unwrap();
        fs::create_dir(d.path().join("logs")).unwrap();
        fs::write(d.path().join("logs/wabbajack-cli.current.log"), b"current").unwrap();
        fs::write(d.path().join("logs/wabbajack-cli.01.log"), b"rotated").unwrap();
        let expected = BTreeSet::from(["jackify-engine".to_owned()]);
        verify_inventory(d.path(), &expected).unwrap();
        fs::write(d.path().join("logs/injected.dll"), b"unexpected").unwrap();
        assert_eq!(
            verify_inventory(d.path(), &expected).unwrap_err().code,
            "engine_changed"
        );
    }
}
