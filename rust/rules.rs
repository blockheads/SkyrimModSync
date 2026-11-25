//! What a pack may carry: licensed files are matched, never sent; logs, saves and executables never travel.
use crate::{
    files::{self, fold},
    Error, Result,
};
use std::path::Path;

/// The INIs under Documents/My Games/Skyrim Special Edition the pack carries (shared sections).
pub const DOCUMENTS: [&str; 3] = ["Skyrim.ini", "SkyrimPrefs.ini", "SkyrimCustom.ini"];
/// Licensed game content: always the friend's own copy, never sent or downloaded.
pub fn protected(path: &str) -> bool {
    let name = fold(path);
    ["skyrimse.exe", "steam_api64.dll", "bink2w64.dll"].contains(&name.as_str())
        || ["skyrim", "update", "dawnguard", "hearthfires", "dragonborn"]
            .iter()
            .any(|n| {
                name == format!("data/{n}.esm")
                    || name == format!("data/{n}.bsa")
                    || name.starts_with(&format!("data/{n} - ")) && name.ends_with(".bsa")
            })
        || name.starts_with("data/cc")
            && (name.ends_with(".esm") || name.ends_with(".esl") || name.ends_with(".bsa"))
        || name == "data/_resourcepack.esl"
        || name == "data/_resourcepack.bsa"
}

/// Game executables and the launcher slot: no pack source may name one (except SkyrimSE.exe as base).
/// Covers saved copies such as SkyrimSE_1.6.1170.exe.
pub fn executable(path: &str) -> bool {
    let n = fold(path);
    !n.contains('/') && n.starts_with("skyrimse") && n.contains(".exe")
}

/// What a game folder itself may contribute: the licensed files (matched, never sent) and SKSE's own
/// root files. Anything else there may be Bethesda's or private; it must be an MO2 mod to be shared.
pub fn from_game_folder(path: &str) -> bool {
    let n = fold(path);
    protected(path)
        || !n.contains('/')
            && n.starts_with("skse64_")
            && (n.ends_with(".dll") || n.ends_with(".exe"))
}

/// Data/ files that ship with the game or are its output: load leaves them where they are.
pub fn stays(path: &str) -> bool {
    let n = fold(path);
    n == "data/marketplacetextures.bsa"
        || n.starts_with("data/video/")
        || n.starts_with("data/skse/plugins/logs/")
}

/// Lines of a small text file, without comments, blanks or a BOM.
pub fn lines(path: Option<&Path>) -> Result<Vec<String>> {
    let Some(path) = path else { return Ok(vec![]) };
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("invalid_configuration", "Configuration must use UTF-8."))?
        .trim_start_matches('\u{feff}');
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with(['#', ';']))
        .map(str::to_owned)
        .collect())
}

pub fn plugin_name(name: &str) -> Result<()> {
    files::relative(name)?;
    if name.contains('/')
        || !["esm", "esp", "esl"]
            .contains(&fold(name.rsplit('.').next().unwrap_or_default()).as_str())
    {
        return Err(Error::new(
            "invalid_configuration",
            format!("Invalid plugin filename: {name}"),
        ));
    }
    Ok(())
}

/// Paths (relative to the game folder) a pack never carries: saves, logs, our own state.
pub fn ignored(name: &str) -> bool {
    let n = fold(name);
    n == "data/skse/plugins/openanimationreplacer_imgui.ini"
        || n.starts_with("saves/")
        || n.starts_with(".skyrim-mod-sync/")
        || (n.starts_with("logs/") && (n.ends_with(".log") || n.ends_with(".dmp")))
        || n.starts_with("data/skse/plugins/logs/")
        || n.starts_with("data/logs/")
        || n.ends_with(".log")
        || n.ends_with(".dmp")
        || n.ends_with(".pdb")
        || n.ends_with(".ess")
        || n.ends_with(".skse")
        || n.ends_with(".mohidden")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_names_the_licensed_files_only() {
        assert!(protected("Data/Skyrim.esm"));
        assert!(protected("data/Skyrim - Textures0.bsa"));
        assert!(protected("Data/ccBGSSSE001-Fish.esm"));
        assert!(protected("SkyrimSE.exe"));
        assert!(!protected("Data/SkyrimCoopStart.esp"));
        assert!(!protected("skse64_loader.exe"));
    }
    #[test]
    fn ignored_skips_saves_logs_and_our_state() {
        assert!(ignored("Data/SKSE/Plugins/BugFixesSSE.log"));
        assert!(ignored("Saves/a.ess"));
        assert!(ignored(".skyrim-mod-sync/journal"));
        assert!(!ignored("Data/SKSE/Plugins/BugFixesSSE.dll"));
        assert!(ignored("Data/SKSE/Plugins/logs/tp_client.1.log"));
        assert!(ignored("Data/SKSE/Plugins/logs/tp_client.log.1"));
    }
    #[test]
    fn a_game_folder_gives_only_licensed_matches_and_skse() {
        assert!(from_game_folder("skse64_1_7_104.dll") && from_game_folder("skse64_loader.exe"));
        assert!(from_game_folder("Data/Skyrim.esm"));
        for p in [
            "SkyrimSE_1.7.104.exe",
            "SkyrimSELauncher.exe",
            "Data/Video/BGS_Logo.bik",
            "Data/Mine.esp",
            "notes.txt",
        ] {
            assert!(!from_game_folder(p), "{p}");
        }
        assert!(executable("SkyrimSE_1.7.104.exe") && executable("SkyrimSELauncher.exe"));
    }
}
