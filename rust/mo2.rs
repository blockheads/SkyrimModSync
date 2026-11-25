//! Reading a Mod Organizer 2 instance (Wabbajack lists are MO2 instances too): the effective files a
//! profile deploys, which mod each comes from, and where each mod was downloaded. Read-only.
use crate::{
    files::{self, fold},
    ini, snapshot, Error, Result,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// One enabled mod.
#[derive(Clone, Debug, PartialEq)]
pub struct Mod {
    pub name: String,
    /// The archive it was installed from, if still in the downloads folder.
    pub archive: Option<PathBuf>,
    /// Its Nexus page and file: (game slug, mod id, file id).
    pub nexus: Option<(String, u64, u64)>,
}

/// The file that wins a path: the last mod (highest priority) providing it, else the game folder.
#[derive(Clone, Debug)]
pub struct Winner {
    /// Relative to the game folder ("Data/foo.esp"; a mod's Root/ files land in the game root).
    pub path: String,
    pub file: PathBuf,
    pub r#mod: Option<String>,
}

#[derive(Debug)]
pub struct Profile {
    pub game: PathBuf,
    /// Folded path -> winner.
    pub files: BTreeMap<String, Winner>,
    /// Enabled mods, lowest priority first.
    pub mods: Vec<Mod>,
    pub plugins: Vec<String>,
    pub loadorder: Vec<String>,
    /// INI file name -> its path, for the INIs the profile uses.
    pub inis: BTreeMap<String, PathBuf>,
}

/// ModOrganizer.ini's values (keys folded, section dropped), paths as MO2 writes them.
fn settings(instance: &Path) -> Result<BTreeMap<String, String>> {
    let p = files::require(instance, "ModOrganizer.ini").map_err(|_| {
        Error::new(
            "not_mo2",
            format!("No ModOrganizer.ini in {}.", instance.display()),
        )
    })?;
    let text = String::from_utf8_lossy(&files::bounded(&p, 4 * 1024 * 1024)?).into_owned();
    Ok(ini::parse(&text)
        .into_values()
        .flatten()
        .map(|(k, v)| (fold(&k), v))
        .collect())
}

/// An MO2 path value as a path on this machine: `@ByteArray(...)`, `%BASE_DIR%`, Windows separators,
/// and under Wine the `Z:` drive (the Unix root).
pub fn native_path(instance: &Path, value: &str) -> Result<PathBuf> {
    let v = value.trim().trim_matches('"');
    let v = v
        .strip_prefix("@ByteArray(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(v);
    let v = v
        .replace("\\\\", "/")
        .replace('\\', "/")
        .replace("%BASE_DIR%", &instance.to_string_lossy());
    if cfg!(unix) {
        if let Some(rest) = v.strip_prefix("Z:/").or_else(|| v.strip_prefix("z:/")) {
            return Ok(PathBuf::from(format!("/{rest}")));
        }
        if v.as_bytes().get(1) == Some(&b':') {
            return Err(Error::new(
                "mo2_path_unmapped",
                format!("MO2 points at a Windows drive ({v}); pass the folder with --game."),
            ));
        }
    }
    let p = PathBuf::from(&v);
    Ok(if p.is_absolute() { p } else { instance.join(p) })
}

fn folder(
    instance: &Path,
    s: &BTreeMap<String, String>,
    key: &str,
    default: &str,
) -> Result<PathBuf> {
    match s.get(key).filter(|v| !v.trim().is_empty()) {
        Some(v) => native_path(instance, v),
        None => Ok(instance.join(default)),
    }
}

/// Nexus's slug for MO2's game name.
fn nexus_game(name: &str) -> Option<String> {
    match fold(name).as_str() {
        "skyrimse" | "skyrimspecialedition" | "skyrim special edition" => {
            Some("skyrimspecialedition".into())
        }
        "skyrim" => Some("skyrim".into()),
        _ => None,
    }
}

fn read_ini(path: &Path) -> Result<ini::Sections> {
    if !path.is_file() {
        return Ok(ini::Sections::new());
    }
    Ok(ini::parse(&String::from_utf8_lossy(&files::bounded(
        path,
        1024 * 1024,
    )?)))
}

fn value<'a>(s: &'a ini::Sections, section: &str, key: &str) -> Option<&'a str> {
    s.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(section))?
        .1
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

fn id(v: Option<&str>) -> Option<u64> {
    v?.trim().parse().ok().filter(|n| *n > 0)
}

/// Where a mod came from: its meta.ini and its archive's .meta in the downloads folder.
fn origin(name: &str, folder: &Path, downloads: &Path, default_game: &str) -> Result<Mod> {
    let meta = read_ini(&folder.join("meta.ini"))?;
    let archive = value(&meta, "General", "installationFile")
        .filter(|f| !f.trim().is_empty())
        .map(|f| {
            let f = f.replace('\\', "/");
            let p = PathBuf::from(&f);
            if p.is_absolute() {
                p
            } else {
                downloads.join(Path::new(&f).file_name().unwrap_or_default())
            }
        })
        .filter(|p| p.is_file());
    let archive_meta = match &archive {
        Some(a) => read_ini(&PathBuf::from(format!("{}.meta", a.display())))?,
        None => ini::Sections::new(),
    };
    let game = value(&archive_meta, "General", "gameName")
        .or(value(&meta, "General", "gameName"))
        .unwrap_or(default_game);
    let mod_id =
        id(value(&archive_meta, "General", "modID")).or(id(value(&meta, "General", "modid")));
    let file_id = id(value(&archive_meta, "General", "fileID")).or(id(value(
        &meta,
        "installedFiles",
        "1\\fileid",
    )));
    let nexus = match (nexus_game(game), mod_id, file_id) {
        (Some(g), Some(m), Some(f)) => Some((g, m, f)),
        _ => None,
    };
    Ok(Mod {
        name: name.into(),
        archive,
        nexus,
    })
}

fn add(winners: &mut BTreeMap<String, Winner>, dir: &Path, r#mod: Option<&str>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for (rel, file) in files::inventory(dir)? {
        let first = fold(rel.split('/').next().unwrap());
        let path = match r#mod {
            None => rel.clone(),
            Some(_) => {
                if fold(&rel) == "meta.ini"
                    || ["calientetools", "nemesis_engine"].contains(&first.as_str())
                {
                    continue;
                }
                match rel.split_once('/') {
                    Some((root, rest)) if fold(root) == "root" => rest.to_owned(),
                    _ => format!("Data/{rel}"),
                }
            }
        };
        winners.insert(
            fold(&path),
            Winner {
                path,
                file,
                r#mod: r#mod.map(str::to_owned),
            },
        );
    }
    Ok(())
}

/// The profile's effective install. `game` overrides MO2's game path; `excluded` mods are left out.
pub fn read(
    instance: &Path,
    profile: &str,
    game: Option<&Path>,
    excluded: &BTreeSet<String>,
) -> Result<Profile> {
    let instance = files::root(instance)?;
    let s = settings(&instance)?;
    files::relative(profile)?;
    if profile.contains('/') {
        return Err(Error::new("invalid_profile", "A profile is a folder name."));
    }
    let game = match game {
        Some(g) => files::root(g)?,
        None => files::root(&match s.get("gamepath") {
            Some(v) => native_path(&instance, v)?,
            None => instance.join("Stock Game"),
        })?,
    };
    if files::locate(&game, "SkyrimSE.exe")?.is_none() {
        return Err(Error::new(
            "not_skyrim",
            format!("No SkyrimSE.exe in {}.", game.display()),
        ));
    }
    let mods_dir = folder(&instance, &s, "mod_directory", "mods")?;
    let downloads = folder(&instance, &s, "download_directory", "downloads")?;
    let selected =
        files::root(&folder(&instance, &s, "profiles_directory", "profiles")?.join(profile))?;
    let default_game = s
        .get("gamename")
        .cloned()
        .unwrap_or_else(|| "SkyrimSE".into());

    let mut winners = BTreeMap::new();
    add(&mut winners, &game, None)?;
    let list = snapshot::lines(Some(&files::require(&selected, "modlist.txt")?))?;
    let mut seen = BTreeSet::new();
    let mut mods = vec![];
    // modlist.txt lists the highest priority first; apply lowest first so higher ones overwrite.
    for name in list.iter().rev().filter_map(|l| l.strip_prefix('+')) {
        files::relative(name)?;
        if name.contains('/') || !seen.insert(fold(name)) {
            return Err(Error::new(
                "invalid_profile",
                format!("Invalid or repeated mod: {name}"),
            ));
        }
        if excluded.contains(&fold(name)) || name.ends_with("_separator") {
            continue;
        }
        let dir = mods_dir.join(name);
        add(&mut winners, &dir, Some(name))?;
        mods.push(origin(name, &dir, &downloads, &default_game)?);
    }
    let overwrite = folder(&instance, &s, "overwrite_directory", "overwrite")?;
    add(&mut winners, &overwrite, Some("Overwrite"))?;
    let logs = snapshot::known_logs(winners.values().map(|w| w.path.clone()));
    winners.retain(|_, w| !snapshot::ignored(&w.path, &logs));

    let plugins = snapshot::lines(files::locate(&selected, "plugins.txt")?.as_deref())?;
    let loadorder = snapshot::lines(files::locate(&selected, "loadorder.txt")?.as_deref())?;
    let mut inis = BTreeMap::new();
    for n in snapshot::DOCUMENTS {
        if let Some(p) = files::locate(&selected, n)? {
            inis.insert(n.to_owned(), p);
        }
    }
    Ok(Profile {
        game,
        files: winners,
        mods,
        plugins,
        loadorder,
        inis,
    })
}

#[cfg(test)]
pub(crate) mod fixture {
    use std::{fs, path::Path};
    /// An MO2 instance: a game folder, mods A (low) and B (high, overrides A's shared file), a
    /// disabled mod C, B's Root/ file, an archive for A in downloads with its .meta.
    pub fn instance(root: &Path) {
        let w = |p: &str, b: &[u8]| {
            let p = root.join(p);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b).unwrap();
        };
        w("game/SkyrimSE.exe", b"exe");
        w("game/Data/Skyrim.esm", b"esm");
        w(
            "mo2/ModOrganizer.ini",
            format!(
                "[General]\ngameName=SkyrimSE\ngamePath=@ByteArray({})\n",
                root.join("game").display()
            )
            .as_bytes(),
        );
        w(
            "mo2/profiles/Default/modlist.txt",
            b"# comment\n+B\n-C\n+A\n",
        );
        w("mo2/profiles/Default/plugins.txt", b"*A.esp\n*B.esp\n");
        w(
            "mo2/profiles/Default/loadorder.txt",
            b"Skyrim.esm\nA.esp\nB.esp\n",
        );
        w(
            "mo2/profiles/Default/Skyrim.ini",
            b"[General]\nuGridsToLoad=5\n",
        );
        w("mo2/mods/A/A.esp", b"a-plugin");
        w("mo2/mods/A/textures/shared.dds", b"from-a");
        w("mo2/mods/A/meta.ini", b"[General]\nmodid=100\ninstallationFile=A-100-1-0.zip\n[installedFiles]\n1\\modid=100\n1\\fileid=200\n");
        w("mo2/mods/B/B.esp", b"b-plugin");
        w("mo2/mods/B/textures/shared.dds", b"from-b");
        w("mo2/mods/B/Root/d3dx9_42.dll", b"root-file");
        w("mo2/mods/C/C.esp", b"c-plugin");
        w("mo2/overwrite/SKSE/Plugins/gen.ini", b"generated");
        w(
            "mo2/downloads/A-100-1-0.zip.meta",
            b"[General]\ngameName=skyrimse\nmodID=100\nfileID=200\n",
        );
        let f = fs::File::create(root.join("mo2/downloads/A-100-1-0.zip")).unwrap();
        let mut z = zip::ZipWriter::new(f);
        for (n, b) in [
            ("A.esp", &b"a-plugin"[..]),
            ("textures/shared.dds", b"from-a"),
            ("readme.txt", b"hi"),
        ] {
            z.start_file(n, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut z, b).unwrap();
        }
        z.finish().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_priorities_root_files_and_origins() {
        let dir = tempfile::tempdir().unwrap();
        fixture::instance(dir.path());
        let p = read(&dir.path().join("mo2"), "Default", None, &BTreeSet::new()).unwrap();
        let shared = &p.files["data/textures/shared.dds"];
        assert_eq!(shared.r#mod.as_deref(), Some("B"));
        assert_eq!(std::fs::read(&shared.file).unwrap(), b"from-b");
        assert_eq!(p.files["d3dx9_42.dll"].r#mod.as_deref(), Some("B"));
        assert!(p.files.contains_key("data/skse/plugins/gen.ini"));
        assert!(!p.files.contains_key("data/c.esp"));
        assert!(!p.files.contains_key("data/meta.ini"));
        let a = p.mods.iter().find(|m| m.name == "A").unwrap();
        assert_eq!(a.nexus, Some(("skyrimspecialedition".into(), 100, 200)));
        assert!(a.archive.is_some());
        assert_eq!(p.plugins, ["*A.esp", "*B.esp"]);
        assert!(p.inis.contains_key("Skyrim.ini"));
    }
    #[test]
    fn excluded_mods_leave_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        fixture::instance(dir.path());
        let p = read(
            &dir.path().join("mo2"),
            "Default",
            None,
            &["b".to_string()].into(),
        )
        .unwrap();
        assert_eq!(
            p.files["data/textures/shared.dds"].r#mod.as_deref(),
            Some("A")
        );
    }
    #[test]
    fn wine_paths_map_to_the_unix_root() {
        if cfg!(unix) {
            let p = native_path(Path::new("/x"), "@ByteArray(Z:\\\\home\\\\me\\\\Skyrim)").unwrap();
            assert_eq!(p, PathBuf::from("/home/me/Skyrim"));
            assert!(native_path(Path::new("/x"), "C:\\Games").is_err());
        }
    }
}
