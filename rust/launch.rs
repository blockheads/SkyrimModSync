//! Whether Skyrim is running. With a pack enabled, Steam's launcher slot holds SKSE's loader, so Play in
//! Steam (and on Linux its Proton setup) starts Skyrim with SKSE.
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether this game folder's Skyrim is running (load/unload must not touch a running game). On Linux a
/// Skyrim run from another folder (a test copy) does not count; elsewhere any SkyrimSE.exe does.
pub fn game_running(root: &Path) -> bool {
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
            .any(|e| std::fs::read(e.path().join("cmdline")).is_ok_and(|c| runs_from(&c, root)));
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

/// A Skyrim cmdline whose program is in `root`. A Windows path on a drive other than Z: (Wine's
/// root) cannot be placed, so it counts.
fn runs_from(cmdline: &[u8], root: &Path) -> bool {
    if !is_skyrim(cmdline) {
        return false;
    }
    let program = cmdline.split(|b| *b == 0).next().unwrap_or_default();
    let program = String::from_utf8_lossy(program);
    let unix = match program.get(..2) {
        Some(d) if d.eq_ignore_ascii_case("z:") => PathBuf::from(program[2..].replace('\\', "/")),
        Some(d) if d.ends_with(':') => return true,
        _ => PathBuf::from(program.as_ref()),
    };
    let folder = unix.parent().unwrap_or(Path::new(""));
    let same = |a: &Path, b: &Path| {
        a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
    };
    same(folder, root)
}

pub fn refuse_if_running(root: &Path) -> Result<()> {
    if game_running(root) {
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

    #[test]
    fn a_skyrim_in_another_folder_does_not_count() {
        let root = std::path::Path::new("/games/Skyrim Special Edition");
        assert!(super::runs_from(
            b"Z:\\games\\Skyrim Special Edition\\SkyrimSE.exe\0",
            root
        ));
        assert!(super::runs_from(
            b"/games/Skyrim Special Edition/SkyrimSE.exe\0",
            root
        ));
        assert!(!super::runs_from(
            b"Z:\\work\\build\\coop-test\\game\\SkyrimSE.exe\0",
            root
        ));
        assert!(super::runs_from(b"C:\\Games\\SkyrimSE.exe\0", root));
    }
}
