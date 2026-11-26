//! Whether Skyrim is running. With a pack enabled, Steam's launcher slot holds SKSE's loader, so Play in
//! Steam (and on Linux its Proton setup) starts Skyrim with SKSE.
use crate::{Error, Result};
use std::process::Command;

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
