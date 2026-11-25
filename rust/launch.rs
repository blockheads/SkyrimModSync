//! Starting the game through Steam: with a pack loaded, Steam's launcher slot holds SKSE's loader, so
//! Steam (and on Linux its Proton setup) starts Skyrim with SKSE.
use crate::{snapshot, steam, Error, Result};
use std::{path::Path, process::Command};

/// Whether a Skyrim process is running (load/unload must not touch a running game).
pub fn game_running() -> bool {
    if cfg!(windows) {
        return Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq SkyrimSE.exe", "/NH"])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains("skyrimse.exe")
            });
    }
    if cfg!(target_os = "linux") {
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return false;
        };
        return procs.filter_map(|e| e.ok()).any(|e| {
            std::fs::read(e.path().join("cmdline")).is_ok_and(|c| {
                String::from_utf8_lossy(&c)
                    .to_lowercase()
                    .contains("skyrimse.exe")
            })
        });
    }
    Command::new("ps")
        .args(["-A", "-o", "command"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_lowercase()
                .contains("skyrimse.exe")
        })
}

pub fn refuse_if_running() -> Result<()> {
    if game_running() {
        return Err(Error::new(
            "game_running",
            "Skyrim is running: quit it first.",
        ));
    }
    Ok(())
}

/// The friend's Skyrim must be the version the pack was made with.
pub fn check_version(game: &Path, runtime: &str) -> Result<()> {
    let have = snapshot::runtime(game)?;
    if have != runtime {
        return Err(Error::new(
            "wrong_game_version",
            format!("Your Skyrim is {have}; this pack needs {runtime}."),
        ));
    }
    Ok(())
}

/// Asks Steam to start Skyrim.
pub fn start() -> Result<()> {
    let url = format!("steam://run/{}", steam::APP_ID);
    let started = if cfg!(windows) {
        Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn()
            .is_ok()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(&url).spawn().is_ok()
    } else {
        Command::new("steam").arg(&url).spawn().is_ok()
            || Command::new("xdg-open").arg(&url).spawn().is_ok()
    };
    if !started {
        return Err(Error::new(
            "steam_unavailable",
            "Couldn't ask Steam to start Skyrim: is Steam installed and running?",
        ));
    }
    Ok(())
}
