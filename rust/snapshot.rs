//! What a Skyrim install is made of: the files that count, the ones that never travel.
use crate::{
    files::{self, fold},
    Error, Result,
};
use std::{collections::BTreeSet, path::Path};

/// The INIs under Documents/My Games/Skyrim Special Edition the pack carries (shared sections).
pub const DOCUMENTS: [&str; 3] = ["Skyrim.ini", "SkyrimPrefs.ini", "SkyrimCustom.ini"];
/// The activation files under AppData/Local/Skyrim Special Edition.
pub const LOCAL: [&str; 2] = ["plugins.txt", "loadorder.txt"];

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
    let bytes = files::bounded(path, 4 * 1024 * 1024)?;
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

/// Runtime output some plugins write next to themselves: never part of a pack.
pub fn known_logs(names: impl Iterator<Item = String>) -> BTreeSet<String> {
    let names = names.map(|s| fold(&s)).collect::<BTreeSet<_>>();
    let mut logs = BTreeSet::new();
    if names.contains("d3dx9_42.dll") {
        logs.insert("d3dx9_42.log".into());
    }
    if names.contains("data/skse/plugins/openanimationreplacer.dll") {
        logs.insert("data/skse/plugins/openanimationreplacer_imgui.ini".into());
    }
    for n in [
        "actorlimitfix",
        "barterlimitfix",
        "bugfixessse",
        "scrambledbugs",
    ] {
        let p = format!("data/skse/plugins/{n}");
        if names.contains(&format!("{p}.dll")) {
            logs.insert(format!("{p}.log"));
        }
    }
    logs
}

/// Paths (relative to the game folder) a pack never carries: saves, logs, our own state.
pub fn ignored(name: &str, logs: &BTreeSet<String>) -> bool {
    let n = fold(name);
    logs.contains(&n)
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

/// SkyrimSE.exe's file version, "1.7.104.0".
pub fn binary_version(path: &Path) -> Option<String> {
    let bytes = files::bounded(path, 128 * 1024 * 1024).ok()?;
    use pelite::pe64::Pe;
    let pe = pelite::pe64::PeFile::from_bytes(&bytes).ok()?;
    let info = pe.resources().ok()?.version_info().ok()?;
    let v = info.fixed()?.dwFileVersion;
    Some(format!("{}.{}.{}.{}", v.Major, v.Minor, v.Patch, v.Build))
}

/// The game's runtime as a pack names it: SkyrimSE.exe's version, else (an unreadable executable)
/// its SHA-256, so two games still compare.
pub fn runtime(game: &Path) -> Result<String> {
    let exe = files::require(game, "SkyrimSE.exe")?;
    Ok(match binary_version(&exe) {
        Some(v) => v,
        None => format!("sha256:{}", &files::sha(&exe)?[..16]),
    })
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
        let logs = known_logs(["Data/SKSE/Plugins/BugFixesSSE.dll".to_string()].into_iter());
        assert!(ignored("Data/SKSE/Plugins/BugFixesSSE.log", &logs));
        assert!(ignored("Saves/a.ess", &logs));
        assert!(ignored(".skyrim-mod-sync/journal", &logs));
        assert!(!ignored("Data/SKSE/Plugins/BugFixesSSE.dll", &logs));
        assert!(ignored("Data/SKSE/Plugins/logs/tp_client.1.log", &logs));
        assert!(ignored("Data/SKSE/Plugins/logs/tp_client.log.1", &logs));
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
