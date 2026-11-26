//! Updating sms itself: the newest GitHub release, checked at every start; with the person's
//! yes, its binary for this system (checked against SHA256SUMS) replaces the running one.
use crate::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const REPO: &str = "blockheads/SkyrimModSync";
/// The release asset for this system.
pub fn asset_name() -> &'static str {
    if cfg!(windows) {
        "sms-windows-x86_64.exe"
    } else {
        "sms-linux-x86_64"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub binary: String,
    pub sums: String,
}

/// x.y.z compared as numbers; true when `latest` is above `current`.
pub fn newer(latest: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(latest) > parts(current)
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("SkyrimModSync/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// The newest release, when it is newer than this program and has a binary for this system.
pub fn check(api: &str) -> Result<Option<Release>> {
    let v: Value = client()?
        .get(format!("{api}/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(5))
        .send()?
        .error_for_status()?
        .json()?;
    let version = v["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_owned();
    if !newer(&version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let url = |name: &str| {
        v["assets"].as_array().into_iter().flatten().find_map(|a| {
            (a["name"] == name).then(|| {
                a["browser_download_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
        })
    };
    Ok(match (url(asset_name()), url("SHA256SUMS")) {
        (Some(binary), Some(sums)) => Some(Release {
            version,
            binary,
            sums,
        }),
        _ => None,
    })
}

/// Whether to look for a newer release at start (`SMS_NO_UPDATE` turns it off).
pub fn enabled() -> bool {
    std::env::var_os("SMS_NO_UPDATE").is_none()
}

/// `binary` when SHA256SUMS lists it under `name` with its hash.
pub fn verify(binary: &[u8], sums: &str, name: &str) -> Result<()> {
    let want = sums
        .lines()
        .find_map(|l| {
            let (hash, file) = l.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| hash.to_lowercase())
        })
        .ok_or_else(|| Error::new("update_failed", format!("SHA256SUMS has no {name}.")))?;
    let got = Sha256::digest(binary)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != want {
        return Err(Error::new(
            "update_failed",
            "The download does not match SHA256SUMS.",
        ));
    }
    Ok(())
}

/// Puts `binary` where `exe` is. Windows cannot overwrite a running program, but can rename it: the
/// old one becomes `<exe>.old`, removed on the next start.
pub fn replace(exe: &Path, binary: &[u8]) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| Error::new("update_failed", "No folder for sms."))?;
    let fresh = dir.join(format!(".sms-update-{}", std::process::id()));
    std::fs::write(&fresh, binary).map_err(|e| {
        Error::new(
            "update_failed",
            format!(
                "Can't write in {} ({e}): update by hand from the releases page.",
                dir.display()
            ),
        )
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(&fresh, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    if cfg!(windows) {
        let old = old_path(exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
        if let Err(e) = std::fs::rename(&fresh, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e.into());
        }
    } else {
        std::fs::rename(&fresh, exe)?;
    }
    Ok(())
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Downloads `release`, checks it and replaces this program.
pub fn install(release: &Release) -> Result<()> {
    crate::say(
        "updating",
        &format!("Downloading sms {}...", release.version),
        serde_json::json!({"version": release.version}),
    );
    let c = client()?;
    let binary = c.get(&release.binary).send()?.error_for_status()?.bytes()?;
    let sums = c.get(&release.sums).send()?.error_for_status()?.text()?;
    verify(&binary, &sums, asset_name())?;
    replace(&std::env::current_exe()?, &binary)?;
    crate::say(
        "updated",
        &format!("Updated to sms {}.", release.version),
        serde_json::json!({"version": release.version}),
    );
    Ok(())
}

/// The previous binary left by an update on Windows.
pub fn tidy() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_compare_as_numbers() {
        assert!(newer("0.10.0", "0.9.1"));
        assert!(newer("v1.0.0", "0.99.0"));
        assert!(!newer("0.9.0", "0.9.0"));
        assert!(!newer("0.8.9", "0.9.0"));
    }
    #[test]
    fn a_checked_binary_replaces_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("sms");
        std::fs::write(&exe, b"old").unwrap();
        let new = b"new build";
        let hash: String = Sha256::digest(new)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sums = format!("{hash}  {}\n{}  other\n", asset_name(), "0".repeat(64));
        verify(new, &sums, asset_name()).unwrap();
        assert_eq!(
            verify(b"tampered", &sums, asset_name()).unwrap_err().code,
            "update_failed"
        );
        replace(&exe, new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), new);
    }
}
