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
        return procs
            .filter_map(|e| e.ok())
            .any(|e| std::fs::read(e.path().join("cmdline")).is_ok_and(|c| is_skyrim(&c)));
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

/// A /proc cmdline whose program (not just any argument) is SkyrimSE.exe; under Wine/Proton the program
/// is the game's Windows path.
fn is_skyrim(cmdline: &[u8]) -> bool {
    let program = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    let program = String::from_utf8_lossy(program).to_lowercase();
    program.rsplit(['/', '\\']).next() == Some("skyrimse.exe")
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

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_program_counts_not_an_argument() {
        assert!(super::is_skyrim(
            b"Z:\\games\\Skyrim Special Edition\\SkyrimSE.exe\0"
        ));
        assert!(super::is_skyrim(b"/games/skyrim/SkyrimSE.exe\0-foo\0"));
        assert!(!super::is_skyrim(
            b"/bin/bash\0-c\0touch game/SkyrimSE.exe\0"
        ));
        assert!(!super::is_skyrim(b"C:\\skse64_loader.exe\0"));
    }
}
