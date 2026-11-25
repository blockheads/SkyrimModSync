use crate::{
    files::{self, fold, Identity, MAX_FILES},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
};

pub const DOCUMENTS: [&str; 3] = ["Skyrim.ini", "SkyrimPrefs.ini", "SkyrimCustom.ini"];
pub const LOCAL: [&str; 2] = ["plugins.txt", "loadorder.txt"];
const MASTERS: [&str; 5] = [
    "Skyrim.esm",
    "Update.esm",
    "Dawnguard.esm",
    "HearthFires.esm",
    "Dragonborn.esm",
];
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub name: String,
    pub light: Option<bool>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Versions {
    pub skyrim: Option<String>,
    pub skse: Option<String>,
    pub coop_sha256: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    pub reason: String,
    pub path: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: u32,
    pub kind: String,
    pub policy: String,
    pub files: Vec<Identity>,
    pub versions: Versions,
    pub plugins: Vec<Plugin>,
    pub warnings: Vec<Warning>,
}
#[derive(Debug, Serialize)]
pub struct Comparison {
    pub state: String,
    pub files_match: bool,
    pub runtime_verified: bool,
    pub expected_id: String,
    pub actual_id: String,
    pub differences: Vec<Value>,
    pub warnings: Vec<Warning>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 2
            || self.kind != "deployed-skyrim-snapshot"
            || self.policy != "deployed-exact-v1"
            || self.files.is_empty()
            || self.files.len() > MAX_FILES
        {
            return Err(Error::new(
                "invalid_snapshot",
                "Unsupported snapshot schema or matching policy.",
            ));
        }
        let mut seen = BTreeSet::new();
        for f in &self.files {
            files::relative(&f.path)?;
            if !f.path.starts_with("game/")
                && !DOCUMENTS
                    .iter()
                    .any(|n| f.path == format!("config/documents/{n}"))
                && !LOCAL.iter().any(|n| f.path == format!("config/local/{n}"))
            {
                return Err(Error::new(
                    "invalid_snapshot",
                    "File is outside supported game/configuration roots.",
                ));
            }
            if f.size > 1024u64.pow(4) || !files::digest_valid(&f.sha256) {
                return Err(Error::new("invalid_snapshot", "Invalid file identity."));
            }
            if !seen.insert(fold(&f.path)) {
                return Err(Error::new(
                    "case_collision",
                    format!("Duplicate path: {}", f.path),
                ));
            }
        }
        for n in &seen {
            let mut p = n.as_str();
            while let Some((parent, _)) = p.rsplit_once('/') {
                if seen.contains(parent) {
                    return Err(Error::new(
                        "invalid_snapshot",
                        "A path is both file and directory.",
                    ));
                }
                p = parent;
            }
        }
        for v in [&self.versions.skyrim, &self.versions.skse]
            .into_iter()
            .flatten()
        {
            if v.split('.').count() != 4 || v.split('.').any(|p| p.parse::<u16>().is_err()) {
                return Err(Error::new("invalid_snapshot", "Invalid binary version."));
            }
        }
        if self
            .versions
            .coop_sha256
            .as_ref()
            .is_some_and(|s| !files::digest_valid(s))
        {
            return Err(Error::new(
                "invalid_snapshot",
                "Invalid co-op binary identity.",
            ));
        }
        let coop = self
            .files
            .iter()
            .find(|f| fold(&f.path) == "game/data/skse/plugins/skyrimcoop/skyrimtogetherclient.dll")
            .or_else(|| {
                self.files
                    .iter()
                    .find(|f| fold(&f.path) == "game/data/skse/plugins/skyrimtogetherclient.dll")
            })
            .map(|f| f.sha256.clone());
        if coop != self.versions.coop_sha256 {
            return Err(Error::new(
                "invalid_snapshot",
                "Co-op hash disagrees with file inventory.",
            ));
        }
        let mut plugins = BTreeSet::new();
        if self.plugins.len() > 65535 || self.warnings.len() > 65536 {
            return Err(Error::new(
                "invalid_snapshot",
                "Too many plugin entries or diagnostics.",
            ));
        }
        for p in &self.plugins {
            plugin_name(&p.name)?;
            if !plugins.insert(fold(&p.name)) {
                return Err(Error::new("invalid_snapshot", "Duplicate plugin entry."));
            }
        }
        for w in &self.warnings {
            files::relative(&w.path)?;
            if w.reason.len() > 200 {
                return Err(Error::new("invalid_snapshot", "Invalid diagnostic."));
            }
        }
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        let mut normalized = self.clone();
        normalized.warnings.clear();
        for f in &mut normalized.files {
            f.path = fold(&f.path);
        }
        normalized.files.sort_by(|a, b| a.path.cmp(&b.path));
        for p in &mut normalized.plugins {
            p.name = fold(&p.name);
        }
        // Value uses sorted map keys, preserving the Python prototype's canonical format.
        Ok(files::hash_bytes(&serde_json::to_vec(
            &serde_json::to_value(normalized)?,
        )?))
    }
}
pub fn load(path: &Path) -> Result<Snapshot> {
    let data: Snapshot = files::read_json(path)?;
    data.validate()?;
    Ok(data)
}
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
fn plugin_name(name: &str) -> Result<()> {
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
pub fn plugin_list(game: &Path, local: Option<&Path>) -> Result<(Vec<Plugin>, Vec<Warning>)> {
    let activation = if let Some(r) = local {
        files::locate(r, "plugins.txt")?
    } else {
        None
    };
    let order = if let Some(r) = local {
        lines(files::locate(r, "loadorder.txt")?.as_deref())?
    } else {
        vec![]
    };
    plugin_selection(|rel| files::locate(game, rel), activation.as_deref(), order)
}
fn plugin_selection(
    lookup: impl Fn(&str) -> Result<Option<std::path::PathBuf>>,
    activation: Option<&Path>,
    order: Vec<String>,
) -> Result<(Vec<Plugin>, Vec<Warning>)> {
    let entries = lines(activation)?;
    let starred = entries.iter().any(|s| s.starts_with('*'));
    let active = entries
        .iter()
        .filter(|s| !starred || s.starts_with('*'))
        .map(|s| s.trim_start_matches('*').to_owned())
        .collect::<Vec<_>>();
    let mut requested = Vec::new();
    let mut seen = BTreeSet::new();
    for n in MASTERS
        .into_iter()
        .map(str::to_owned)
        .chain(lines(lookup("Skyrim.ccc")?.as_deref())?)
        .chain(["_ResourcePack.esl".to_owned()])
    {
        plugin_name(&n)?;
        if lookup(&format!("Data/{n}"))?.is_some() && seen.insert(fold(&n)) {
            requested.push(n);
        }
    }
    let mut activated = BTreeSet::new();
    for n in active {
        plugin_name(&n)?;
        if !activated.insert(fold(&n)) {
            return Err(Error::new(
                "invalid_configuration",
                "Duplicate plugin activation entry.",
            ));
        }
        if seen.insert(fold(&n)) {
            requested.push(n);
        }
    }
    let mut ordered = Vec::new();
    let mut order_seen = BTreeSet::new();
    for n in order {
        plugin_name(&n)?;
        if !order_seen.insert(fold(&n)) {
            return Err(Error::new(
                "invalid_configuration",
                "Duplicate load-order entry.",
            ));
        }
        if let Some(i) = requested.iter().position(|v| fold(v) == fold(&n)) {
            ordered.push(requested.remove(i));
        }
    }
    ordered.extend(requested);
    let mut result = Vec::new();
    let mut warnings = Vec::new();
    for name in ordered {
        let path = format!("Data/{name}");
        let mut light = None;
        if let Some(p) = lookup(&path)? {
            let mut h = [0; 12];
            if fs::File::open(p)?.read_exact(&mut h).is_ok() && &h[..4] == b"TES4" {
                light = Some(u32::from_le_bytes(h[8..12].try_into().unwrap()) & 0x200 != 0);
            } else {
                warnings.push(Warning {
                    reason: "invalid_plugin_header".into(),
                    path: format!("game/{path}"),
                });
            }
        } else {
            warnings.push(Warning {
                reason: "missing_active_plugin".into(),
                path: format!("game/{path}"),
            });
        }
        result.push(Plugin { name, light });
    }
    if activation.is_none() {
        warnings.push(Warning {
            reason: "activation_unavailable".into(),
            path: "config/local/plugins.txt".into(),
        });
    }
    Ok((result, warnings))
}
pub fn known_logs(names: impl Iterator<Item = String>) -> BTreeSet<String> {
    let names = names.map(|s| fold(&s)).collect::<BTreeSet<_>>();
    let mut logs = BTreeSet::new();
    if names.contains("game/d3dx9_42.dll") {
        logs.insert("game/d3dx9_42.log".into());
    }
    if names.contains("game/data/skse/plugins/openanimationreplacer.dll") {
        logs.insert("game/data/skse/plugins/openanimationreplacer_imgui.ini".into());
    }
    // SkyrimCoop's development loader creates per-process/generation copies from
    // the separately hashed source client. Their filenames are runtime state.
    if names.contains("game/data/skse/plugins/skyrimcooploader.dll")
        && names.contains("game/data/skse/plugins/skyrimcoop/skyrimtogetherclient.dll")
    {
        for name in &names {
            if let Some(stem) = name
                .strip_prefix("game/data/skse/plugins/skyrimcoop/loaded/")
                .and_then(|n| n.strip_suffix(".dll"))
            {
                let parts = stem.rsplitn(3, '.').collect::<Vec<_>>();
                if parts.len() == 3
                    && parts[0].parse::<u32>().is_ok()
                    && parts[1].parse::<u32>().is_ok()
                    && (parts[2] == "skyrimtogetherclient"
                        || parts[2].starts_with("skyrimtogetherclient-"))
                {
                    logs.insert(name.clone());
                }
            }
        }
    }
    for n in [
        "actorlimitfix",
        "barterlimitfix",
        "bugfixessse",
        "scrambledbugs",
    ] {
        let p = format!("game/data/skse/plugins/{n}");
        if names.contains(&format!("{p}.dll")) {
            logs.insert(format!("{p}.log"));
        }
    }
    logs
}
pub fn ignored(name: &str, logs: &BTreeSet<String>) -> bool {
    let n = fold(name);
    logs.contains(&n)
        || n.starts_with("game/saves/")
        || (n.starts_with("game/logs/") && (n.ends_with(".log") || n.ends_with(".dmp")))
        || n.starts_with("game/.skyrimsync/")
        || n.ends_with(".ess")
        || n.ends_with(".skse")
}
pub fn portable_configuration(name: &str, path: &Path) -> Result<()> {
    if !name.to_lowercase().ends_with(".ini") {
        return Ok(());
    }
    let raw = files::bounded(path, 4 * 1024 * 1024)?;
    let text = std::str::from_utf8(&raw).map_err(|_| {
        Error::new(
            "unportable_configuration",
            format!("INI must use UTF-8: {name}"),
        )
    })?;
    for line in text
        .lines()
        .filter(|s| !s.trim_start().starts_with([';', '#']))
    {
        let Some((key, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches(['\"', '\'']);
        // Crash Logger's shipped cache hint is a logical Windows path, with no
        // machine identity; retain its bytes exactly. Other absolute paths fail.
        if fold(name).ends_with("/crashlogger.ini")
            && fold(key.trim()) == "symcache directory"
            && fold(v) == "c:\\symcache"
        {
            continue;
        }
        // Machine paths cannot be shipped as exact shared settings. Windows separators in
        // game-relative resource lists are fine; absolute drives/home paths are not.
        if v.starts_with('/')
            || v.starts_with("\\\\")
            || v.as_bytes().get(1) == Some(&b':')
            || v.contains("/home/")
            || v.contains("\\Users\\")
        {
            return Err(Error::new("unportable_configuration",format!("{name} contains a machine-specific absolute path. Move it to local configuration or use a relative path before publishing.")));
        }
    }
    Ok(())
}
pub fn capture(game: &Path, documents: Option<&Path>, local: Option<&Path>) -> Result<Snapshot> {
    let game = files::root(game)?;
    let initial = files::inventory(&game)?;
    let logs = known_logs(initial.keys().map(|n| format!("game/{n}")));
    let mut selected = initial
        .iter()
        .filter(|(n, _)| !ignored(&format!("game/{n}"), &logs))
        .map(|(n, p)| (format!("game/{n}"), p.clone()))
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    if !initial.keys().any(|n| fold(n) == "skyrimse.exe") {
        warnings.push(Warning {
            reason: "missing_game_binary".into(),
            path: "game/SkyrimSE.exe".into(),
        });
    }
    for (root, names, prefix) in [
        (documents, DOCUMENTS.as_slice(), "config/documents"),
        (local, LOCAL.as_slice(), "config/local"),
    ] {
        if let Some(root) = root {
            files::root(root)?;
            for n in names {
                if let Some(path) = files::locate(root, n)? {
                    selected.push((format!("{prefix}/{n}"), path));
                } else {
                    warnings.push(Warning {
                        reason: "configuration_missing".into(),
                        path: format!("{prefix}/{n}"),
                    });
                }
            }
        } else {
            warnings.push(Warning {
                reason: "configuration_unavailable".into(),
                path: format!("{prefix}/{}", names[0]),
            });
        }
    }
    selected.sort_by(|a, b| a.0.cmp(&b.0));
    let mut identities = Vec::new();
    let mut stable = Vec::new();
    for (i, (n, p)) in selected.iter().enumerate() {
        identities.push(files::identity(n.clone(), p)?);
        let m = fs::metadata(p)?;
        stable.push((p.clone(), m.len(), m.modified()?));
        if i % 100 == 0 {
            crate::event("capturing", json!({"completed":i+1,"total":selected.len()}));
        }
    }
    if initial != files::inventory(&game)? {
        return Err(Error::new(
            "source_changed",
            "Files appeared/disappeared during capture. Stop Skyrim and retry.",
        ));
    }
    let (plugins, plugin_warnings) = plugin_list(&game, local)?;
    warnings.extend(plugin_warnings);
    let coop_sha256 = identities
        .iter()
        .find(|f| fold(&f.path) == "game/data/skse/plugins/skyrimcoop/skyrimtogetherclient.dll")
        .or_else(|| {
            identities
                .iter()
                .find(|f| fold(&f.path) == "game/data/skse/plugins/skyrimtogetherclient.dll")
        })
        .map(|f| f.sha256.clone());
    let snap = Snapshot {
        schema: 2,
        kind: "deployed-skyrim-snapshot".into(),
        policy: "deployed-exact-v1".into(),
        files: identities,
        versions: Versions {
            skyrim: binary_version(files::locate(&game, "SkyrimSE.exe")?.as_deref()),
            skse: binary_version(files::locate(&game, "skse64_loader.exe")?.as_deref()),
            coop_sha256,
        },
        plugins,
        warnings,
    };
    for (path, size, modified) in stable {
        let m = fs::metadata(path)?;
        if m.len() != size || m.modified()? != modified {
            return Err(Error::new(
                "source_changed",
                "A captured file changed during the scan. Close Skyrim and retry.",
            ));
        }
    }
    snap.validate()?;
    Ok(snap)
}
pub fn capture_install(root: &Path) -> Result<Snapshot> {
    let documents = root.join("config/documents");
    let local = root.join("config/local");
    capture(
        &root.join("game"),
        documents.is_dir().then_some(documents.as_path()),
        local.is_dir().then_some(local.as_path()),
    )
}
pub fn compare(expected: &Snapshot, actual: &Snapshot) -> Result<Comparison> {
    expected.validate()?;
    actual.validate()?;
    let mut differences = Vec::new();
    for (n, a, b) in [
        ("skyrim", &expected.versions.skyrim, &actual.versions.skyrim),
        ("skse", &expected.versions.skse, &actual.versions.skse),
        (
            "coop_sha256",
            &expected.versions.coop_sha256,
            &actual.versions.coop_sha256,
        ),
    ] {
        if a != b {
            differences.push(json!({"kind":"version","component":n,"expected":a,"actual":b}));
        }
    }
    let want = expected
        .files
        .iter()
        .map(|f| (fold(&f.path), f))
        .collect::<BTreeMap<_, _>>();
    let have = actual
        .files
        .iter()
        .map(|f| (fold(&f.path), f))
        .collect::<BTreeMap<_, _>>();
    for key in want.keys().chain(have.keys()).collect::<BTreeSet<_>>() {
        match (want.get(key), have.get(key)) {
            (Some(a), None) => differences.push(json!({"kind":"missing","path":a.path})),
            (None, Some(b)) => differences.push(json!({"kind":"extra","path":b.path})),
            (Some(a), Some(b)) if a.size != b.size || a.sha256 != b.sha256 => differences.push(
                json!({"kind":"changed","path":a.path,"expected":a.sha256,"actual":b.sha256}),
            ),
            _ => {}
        }
    }
    let host = expected
        .plugins
        .iter()
        .map(|p| (fold(&p.name), p))
        .collect::<BTreeMap<_, _>>();
    let peer = actual
        .plugins
        .iter()
        .map(|p| (fold(&p.name), p))
        .collect::<BTreeMap<_, _>>();
    for p in &expected.plugins {
        match peer.get(&fold(&p.name)) {
            None => differences.push(json!({"kind":"plugin_missing","plugin":p.name})),
            Some(b) if p.light != b.light => differences.push(
                json!({"kind":"plugin_light","plugin":p.name,"expected":p.light,"actual":b.light}),
            ),
            _ => {}
        }
    }
    for p in &actual.plugins {
        if !host.contains_key(&fold(&p.name)) {
            differences.push(json!({"kind":"plugin_extra","plugin":p.name}));
        }
    }
    let a = expected
        .plugins
        .iter()
        .filter(|p| peer.contains_key(&fold(&p.name)))
        .collect::<Vec<_>>();
    let b = actual
        .plugins
        .iter()
        .filter(|p| host.contains_key(&fold(&p.name)))
        .collect::<Vec<_>>();
    let a_common = &a;
    for (i, (a, b)) in a.iter().zip(&b).enumerate() {
        if fold(&a.name) != fold(&b.name) {
            differences.push(json!({"kind":"plugin_order","expected":a.name,"actual":b.name,"after":if i==0{""}else{&a_common[i-1].name}}));
            break;
        }
    }
    Ok(Comparison {
        state: if differences.is_empty() {
            "files_match"
        } else {
            "different"
        }
        .into(),
        files_match: differences.is_empty(),
        runtime_verified: false,
        expected_id: expected.id()?,
        actual_id: actual.id()?,
        differences,
        warnings: actual.warnings.clone(),
    })
}
pub fn binary_version(path: Option<&Path>) -> Option<String> {
    let bytes = files::bounded(path?, 128 * 1024 * 1024).ok()?;
    use pelite::pe64::Pe;
    let pe = pelite::pe64::PeFile::from_bytes(&bytes).ok()?;
    let info = pe.resources().ok()?.version_info().ok()?;
    let fixed = info.fixed()?;
    let v = fixed.dwFileVersion;
    Some(format!("{}.{}.{}.{}", v.Major, v.Minor, v.Patch, v.Build))
}

/// Snapshot an imported effective view without copying its complete game tree.
pub fn capture_selection(paths: &BTreeMap<String, std::path::PathBuf>) -> Result<Snapshot> {
    let folded = paths
        .iter()
        .map(|(n, p)| (fold(n), p.clone()))
        .collect::<BTreeMap<_, _>>();
    if folded.len() != paths.len() {
        return Err(Error::new("case_collision", "Ambiguous effective files."));
    }
    let lookup = |n: &str| folded.get(&fold(n)).cloned();
    let activation = lookup("config/local/plugins.txt");
    let (plugins, mut warnings) = plugin_selection(
        |rel| Ok(lookup(&format!("game/{rel}"))),
        activation.as_deref(),
        lines(lookup("config/local/loadorder.txt").as_deref())?,
    )?;
    let mut identities = Vec::new();
    for (i, (name, path)) in paths.iter().enumerate() {
        identities.push(files::identity(name.clone(), path)?);
        if i % 100 == 0 {
            crate::event(
                "import_capturing",
                json!({"completed":i+1,"total":paths.len()}),
            );
        }
    }
    if lookup("game/SkyrimSE.exe").is_none() {
        warnings.push(Warning {
            reason: "missing_game_binary".into(),
            path: "game/SkyrimSE.exe".into(),
        });
    }
    for (names, prefix) in [
        (DOCUMENTS.as_slice(), "config/documents"),
        (LOCAL.as_slice(), "config/local"),
    ] {
        for n in names {
            if lookup(&format!("{prefix}/{n}")).is_none() {
                warnings.push(Warning {
                    reason: "configuration_missing".into(),
                    path: format!("{prefix}/{n}"),
                });
            }
        }
    }
    let coop_sha256 = identities
        .iter()
        .find(|f| fold(&f.path) == "game/data/skse/plugins/skyrimcoop/skyrimtogetherclient.dll")
        .or_else(|| {
            identities
                .iter()
                .find(|f| fold(&f.path) == "game/data/skse/plugins/skyrimtogetherclient.dll")
        })
        .map(|f| f.sha256.clone());
    let snap = Snapshot {
        schema: 2,
        kind: "deployed-skyrim-snapshot".into(),
        policy: "deployed-exact-v1".into(),
        files: identities,
        versions: Versions {
            skyrim: binary_version(lookup("game/SkyrimSE.exe").as_deref()),
            skse: binary_version(lookup("game/skse64_loader.exe").as_deref()),
            coop_sha256,
        },
        plugins,
        warnings,
    };
    snap.validate()?;
    Ok(snap)
}
