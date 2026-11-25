//! The supported stock-game adapter flattens an exact reconstructed recipe profile.
use crate::{
    acquisition::wj_hash,
    files::{self, fold},
    package::{self, Artifact, Instruction, Manifest, Source, SourceHint},
    snapshot, Error, Result,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
};
use zip::ZipArchive;
#[derive(Debug)]
pub struct Recipe {
    pub data: Value,
    pub artifact: PathBuf,
    pub paths: BTreeMap<String, String>,
}
impl Recipe {
    /// Internal acquisition recipe: retain exact directives and their parent archives.
    /// The published manifest's SHA-256 remains the authority for every repaired file.
    pub fn select(&self, paths: &BTreeSet<String>) -> Result<Value> {
        let mut data = self.data.clone();
        let directives = self.data["Directives"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| paths.contains(&fold(&d["To"].as_str().unwrap().replace('\\', "/"))))
            .cloned()
            .collect::<Vec<_>>();
        if directives.len() != paths.len() {
            return Err(Error::new(
                "source_missing",
                "Required output is absent from the pinned recipe.",
            ));
        }
        if directives
            .iter()
            .any(|d| d["$type"] == "RemappedInlineFile")
        {
            return Err(Error::new(
                "unsupported_layout",
                "Machine-remapped outputs cannot be shared runtime files.",
            ));
        }
        let hashes = directives
            .iter()
            .filter_map(|d| d["ArchiveHashPath"][0].as_str())
            .collect::<BTreeSet<_>>();
        let archives = self.data["Archives"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["Hash"].as_str().is_some_and(|h| hashes.contains(h)))
            .cloned()
            .collect::<Vec<_>>();
        if hashes
            .iter()
            .any(|h| !archives.iter().any(|a| a["Hash"] == *h))
        {
            return Err(Error::new(
                "invalid_recipe",
                "Required parent archive is absent.",
            ));
        }
        data["Archives"] = json!(archives);
        data["Directives"] = json!(directives);
        Ok(data)
    }
    pub fn subset(&self, paths: &BTreeSet<String>, output: &Path) -> Result<Self> {
        use std::io::Write;
        let data = self.select(paths)?;
        let entries = data["Directives"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|d| [d["SourceDataID"].as_str(), d["PatchID"].as_str()])
            .flatten()
            .collect::<BTreeSet<_>>();
        let mut source = ZipArchive::new(File::open(&self.artifact)?)?;
        let mut dest = zip::ZipWriter::new(File::create(output)?);
        for name in entries {
            files::relative(name)?;
            let mut entry = source.by_name(name)?;
            dest.start_file(name, zip::write::SimpleFileOptions::default())?;
            std::io::copy(&mut entry, &mut dest)?;
        }
        dest.start_file("modlist", zip::write::SimpleFileOptions::default())?;
        dest.write_all(&serde_json::to_vec(&data)?)?;
        dest.finish()?.sync_all()?;
        Self::load(output)
    }
    pub fn selected_bytes(&self, paths: &BTreeSet<String>) -> Result<u64> {
        let data = self.select(paths)?;
        let mut zip = ZipArchive::new(File::open(&self.artifact)?)?;
        data["Directives"]
            .as_array()
            .unwrap()
            .iter()
            .try_fold(0u64, |total, d| {
                let size = if d["$type"] == "InlineFile" {
                    zip.by_name(d["SourceDataID"].as_str().unwrap_or(""))?
                        .size()
                } else {
                    d["Size"].as_u64().unwrap()
                };
                total
                    .checked_add(size)
                    .ok_or_else(|| Error::new("invalid_recipe", "Source working size overflow."))
            })
    }
    /// Inline payloads require no installer, Nexus authentication or archive cache.
    pub fn extract_inline(
        &self,
        paths: &BTreeSet<String>,
        output: &Path,
    ) -> Result<BTreeSet<String>> {
        use std::io::{Read, Write};
        let data = self.select(paths)?;
        let mut remaining = paths.clone();
        let mut zip = ZipArchive::new(File::open(&self.artifact)?)?;
        for d in data["Directives"].as_array().unwrap() {
            if d["$type"] != "InlineFile" {
                continue;
            }
            let rel = d["To"].as_str().unwrap().replace('\\', "/");
            let p = files::destination(output, &rel)?;
            fs::create_dir_all(p.parent().unwrap())?;
            let mut entry = zip.by_name(d["SourceDataID"].as_str().unwrap_or(""))?;
            // Some MO2 modlist directives have stale size/hash metadata. Runtime
            // output SHA-256 is still checked before publication of a repair.
            if entry.size() > 1024u64.pow(4) {
                return Err(Error::new(
                    "invalid_recipe",
                    "Inline payload exceeds file limit.",
                ));
            }
            let mut dest = File::create(&p)?;
            let expected = entry.size();
            let mut total = 0u64;
            let mut buffer = vec![0; 1024 * 1024];
            loop {
                crate::check_cancel()?;
                let n = entry.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                if total > expected {
                    return Err(Error::new(
                        "invalid_recipe",
                        "Inline payload exceeds declared ZIP size.",
                    ));
                }
                dest.write_all(&buffer[..n])?;
            }
            if total != expected {
                return Err(Error::new("invalid_recipe", "Inline payload is truncated."));
            }
            dest.sync_all()?;
            remaining.remove(&fold(&rel));
        }
        Ok(remaining)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut zip = ZipArchive::new(File::open(path)?)?;
        let data: Value =
            crate::json::value(&package::zip_bytes(&mut zip, "modlist", files::MAX_JSON)?)?;
        let dirs = data["Directives"]
            .as_array()
            .ok_or_else(|| Error::new("invalid_recipe", "Missing recipe directives."))?;
        if dirs.len() > files::MAX_FILES
            || data["Archives"].as_array().is_none_or(|a| a.len() > 10000)
        {
            return Err(Error::new(
                "invalid_recipe",
                "Invalid archive/directive count.",
            ));
        }
        let mut paths = BTreeMap::new();
        for d in dirs {
            let kind = d["$type"].as_str().unwrap_or("");
            if ![
                "InlineFile",
                "RemappedInlineFile",
                "FromArchive",
                "PatchedFromArchive",
            ]
            .contains(&kind)
            {
                return Err(Error::new(
                    "unsupported_recipe",
                    format!("Unsupported directive: {kind}"),
                ));
            }
            let to = d["To"].as_str().unwrap_or("").replace('\\', "/");
            files::relative(&to)?;
            if paths.insert(fold(&to), kind.to_owned()).is_some() {
                return Err(Error::new(
                    "case_collision",
                    "Duplicate recipe output path.",
                ));
            }
            if d["Size"].as_u64().is_none_or(|s| s > 1024u64.pow(4)) {
                return Err(Error::new("invalid_recipe", "Invalid directive size."));
            }
        }
        Ok(Self {
            data,
            artifact: path.to_owned(),
            paths,
        })
    }
    pub fn check_base(&self, base: &Path) -> Result<Vec<Value>> {
        let mut problems = vec![];
        for a in self.data["Archives"].as_array().unwrap() {
            let s = &a["State"];
            if !s["$type"].as_str().unwrap_or("").contains("GameFileSource") {
                continue;
            }
            let rel = s["GameFile"].as_str().unwrap_or("").replace('\\', "/");
            files::relative(&rel)?;
            let p = files::locate(base, &rel)?;
            if s["Game"] != "SkyrimSpecialEdition"
                || p.as_ref().is_none_or(|p| {
                    fs::metadata(p)
                        .map(|m| Some(m.len()) != a["Size"].as_u64())
                        .unwrap_or(true)
                })
                || p.as_ref()
                    .map(|p| wj_hash(p))
                    .transpose()?
                    .is_none_or(|h| Some(h.as_str()) != a["Hash"].as_str())
            {
                problems.push(json!({"path":rel,"reason":"base_mismatch"}));
            }
        }
        Ok(problems)
    }
    pub fn verify(&self, dir: &Path) -> Result<Vec<Value>> {
        let mut differences = Vec::new();
        let mut zip = ZipArchive::new(File::open(&self.artifact)?)?;
        for (i, d) in self.data["Directives"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            crate::check_cancel()?;
            let kind = d["$type"].as_str().unwrap();
            if kind == "RemappedInlineFile" {
                continue;
            }
            let rel = d["To"].as_str().unwrap().replace('\\', "/");
            let path = files::locate(dir, &rel)?;
            let same = if let Some(p) = path {
                if kind == "InlineFile" && fold(&rel).ends_with("/modlist.txt") {
                    let entry = d["SourceDataID"]
                        .as_str()
                        .ok_or_else(|| Error::new("invalid_recipe", "Missing inline source."))?;
                    files::bounded(&p, files::MAX_JSON)?
                        == package::zip_bytes(&mut zip, entry, files::MAX_JSON)?
                } else {
                    Some(fs::metadata(&p)?.len()) == d["Size"].as_u64()
                        && Some(wj_hash(&p)?.as_str()) == d["Hash"].as_str()
                }
            } else {
                false
            };
            if !same {
                differences.push(json!({"path":rel,"reason":"reconstruction_mismatch"}));
            }
            if i % 1000 == 0 {
                crate::event(
                    "verifying_reconstruction",
                    json!({"completed":i+1,"total":self.paths.len()}),
                );
            }
        }
        Ok(differences)
    }
    pub fn archives(&self, base: &Path, downloads: &Path) -> Result<Value> {
        let mut present = Vec::new();
        let mut missing = Vec::new();
        let mut unsupported = Vec::new();
        let mut missing_bytes = 0u64;
        let cache = if downloads.is_dir() {
            files::inventory(downloads)?
        } else {
            BTreeMap::new()
        };
        for a in self.data["Archives"].as_array().unwrap() {
            let state = &a["State"];
            let kind = state["$type"].as_str().unwrap_or("unknown");
            if kind.contains("GameFileSource") {
                continue;
            }
            let size = a["Size"]
                .as_u64()
                .ok_or_else(|| Error::new("invalid_recipe", "Invalid archive size."))?;
            let name = a["Name"].as_str().unwrap_or("Unnamed archive");
            let expected = a["Hash"]
                .as_str()
                .ok_or_else(|| Error::new("invalid_recipe", "Missing archive hash."))?;
            let mut matches = false;
            // Alternate cache filenames are accepted only after checking actual content.
            for p in cache.values() {
                if fs::metadata(p)?.len() == size && wj_hash(p)? == expected {
                    matches = true;
                    break;
                }
            }
            let url = source_url(state);
            let info = json!({"name":name,"size":size,"kind":kind,"url":url,"hash":expected});
            if matches {
                present.push(info);
            } else {
                missing_bytes = missing_bytes
                    .checked_add(size)
                    .ok_or_else(|| Error::new("invalid_recipe", "Archive size overflow."))?;
                if !kind.contains("Nexus") && !kind.contains("Http") {
                    unsupported.push(info.clone());
                }
                missing.push(info);
            }
        }
        let expanded = self.data["Directives"]
            .as_array()
            .unwrap()
            .iter()
            .try_fold(0u64, |n, d| {
                n.checked_add(d["Size"].as_u64().unwrap())
                    .ok_or_else(|| Error::new("invalid_recipe", "Expanded size overflow."))
            })?;
        Ok(
            json!({"verified_cached":present,"missing":missing,"missing_bytes":missing_bytes,"unsupported_missing_sources":unsupported,"base_problems":self.check_base(base)?,"reconstruction_bytes":expanded}),
        )
    }
}
fn source_url(state: &Value) -> String {
    if state["$type"].as_str().unwrap_or("").contains("Nexus")
        && state["GameName"] == "SkyrimSpecialEdition"
    {
        if let Some(id) = state["ModID"].as_u64() {
            return format!("https://www.nexusmods.com/skyrimspecialedition/mods/{id}");
        }
    }
    let raw = state["Url"]
        .as_str()
        .or(state["URL"].as_str())
        .or(state["ManualURL"].as_str())
        .unwrap_or("");
    if let Ok(mut u) = reqwest::Url::parse(raw) {
        if u.scheme() == "https" {
            u.set_query(None);
            u.set_fragment(None);
            let _ = u.set_username("");
            let _ = u.set_password(None);
            return u.to_string();
        }
    }
    String::new()
}
fn write_generated(dir: &Path, rel: &str, bytes: &[u8]) -> Result<PathBuf> {
    files::relative(rel)?;
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap())?;
    if p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(Error::new(
            "path_escape",
            "Generated settings cannot be links.",
        ));
    }
    fs::write(&p, bytes)?;
    Ok(p)
}
pub(crate) fn merge_inis(dir: &Path, profile: &str) -> Result<BTreeMap<String, PathBuf>> {
    merge_selected_inis(dir, profile, None)
}
fn merge_selected_inis(
    dir: &Path,
    profile: &str,
    only: Option<&str>,
) -> Result<BTreeMap<String, PathBuf>> {
    let selected = dir.join("profiles").join(profile);
    let tweak = files::locate(&selected, "initweaks.ini")?;
    let tweaks = if let Some(p) = tweak {
        snapshot::lines(Some(&p))?
    } else {
        vec![]
    };
    let mut overrides = Vec::new();
    let mut section = String::new();
    for line in tweaks {
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(Error::new(
                "unsupported_layout",
                "Unsupported profile INI tweak.",
            ));
        };
        if fold(&section) != "archive"
            || !["binvalidateolderfiles", "sresourcedatadirsfinal"]
                .contains(&fold(key.trim()).as_str())
        {
            return Err(Error::new(
                "unsupported_layout",
                "Profile has unsupported INI tweaks; only archive invalidation is supported.",
            ));
        }
        overrides.push((
            section.clone(),
            key.trim().to_owned(),
            value.trim().to_owned(),
        ));
    }
    let mut result = BTreeMap::new();
    for n in snapshot::DOCUMENTS {
        if only.is_some_and(|name| name != n) {
            continue;
        }
        let Some(src) = files::locate(&selected, n)? else {
            continue;
        };
        let mut raw = files::bounded(&src, 4 * 1024 * 1024)?;
        if n == "Skyrim.ini" && !overrides.is_empty() {
            let text = std::str::from_utf8(&raw)
                .map_err(|_| Error::new("invalid_configuration", "Profile INIs must use UTF-8."))?
                .trim_start_matches('\u{feff}');
            let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
            for line in text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with(['#', ';']))
            {
                if line.starts_with('[') && line.ends_with(']') {
                    sections.push((line[1..line.len() - 1].to_owned(), vec![]));
                } else if let Some((k, v)) = line.split_once('=') {
                    let Some(s) = sections.last_mut() else {
                        return Err(Error::new(
                            "invalid_configuration",
                            "INI property outside a section.",
                        ));
                    };
                    s.1.push((k.trim().to_owned(), v.trim().to_owned()));
                } else {
                    return Err(Error::new(
                        "unsupported_layout",
                        "Multiline or malformed INI is unsupported by the selected adapter.",
                    ));
                }
            }
            for (section, key, value) in &overrides {
                let i = sections
                    .iter()
                    .position(|s| fold(&s.0) == fold(section))
                    .unwrap_or_else(|| {
                        sections.push((section.clone(), vec![]));
                        sections.len() - 1
                    });
                if let Some(k) = sections[i].1.iter_mut().find(|kv| fold(&kv.0) == fold(key)) {
                    k.1 = value.clone();
                } else {
                    sections[i].1.push((key.clone(), value.clone()));
                }
            }
            let mut output = String::new();
            for (s, kvs) in sections {
                output.push_str(&format!("[{s}]\n"));
                for (k, v) in kvs {
                    output.push_str(&format!("{k}={v}\n"));
                }
                output.push('\n');
            }
            raw = output.into_bytes();
        }
        let rel = format!(".skyrimsync-generated/profiles/{profile}/{n}");
        result.insert(n.to_owned(), write_generated(dir, &rel, &raw)?);
    }
    Ok(result)
}
#[derive(Clone)]
struct Winner {
    target: String,
    path: PathBuf,
    source: Source,
}
fn overlay(
    winners: &mut BTreeMap<String, Winner>,
    dir: &Path,
    source_prefix: &str,
    mod_files: bool,
    proof: &Recipe,
) -> Result<()> {
    if !dir.is_dir() {
        return Err(Error::new(
            "missing_source",
            format!("Missing reconstructed folder: {}", dir.display()),
        ));
    }
    for (rel, path) in files::inventory(dir)? {
        let first = fold(rel.split('/').next().unwrap());
        let lower = fold(&rel);
        if mod_files
            && (lower == "meta.ini"
                || lower.ends_with(".mohidden")
                || ["calientetools", "nemesis_engine"].contains(&first.as_str()))
        {
            continue;
        }
        if first == "ini tweaks" {
            return Err(Error::new(
                "unsupported_layout",
                "Mod INI tweaks need an explicit supported merge recipe.",
            ));
        }
        let target = if mod_files && first == "root" {
            format!(
                "game/{}",
                rel.split_once('/')
                    .ok_or_else(|| Error::new("unsupported_layout", "Invalid root-builder output"))?
                    .1
            )
        } else if mod_files {
            format!("game/Data/{rel}")
        } else {
            format!("game/{rel}")
        };
        let source = format!("{source_prefix}{rel}");
        if proof
            .paths
            .get(&fold(&source))
            .is_none_or(|k| k == "RemappedInlineFile")
        {
            return Err(Error::new(
                "untracked_output",
                format!(
                    "Runtime file is not reconstructed deterministically by the recipe: {source}"
                ),
            ));
        }
        winners.insert(
            fold(&target),
            Winner {
                target,
                path,
                source: Source {
                    kind: "wabbajack".into(),
                    path: source,
                },
            },
        );
    }
    Ok(())
}
pub fn materialize(
    recipe: &Recipe,
    reconstructed: &Path,
    profile: &str,
    destination: &Path,
) -> Result<Manifest> {
    files::relative(profile)?;
    if profile.contains('/') {
        return Err(Error::new(
            "invalid_profile",
            "Profile must be one directory name.",
        ));
    }
    let selected = files::require(reconstructed, &format!("profiles/{profile}/modlist.txt"))?;
    if reconstructed.join("overwrite").is_dir()
        && !files::inventory(&reconstructed.join("overwrite"))?.is_empty()
    {
        return Err(Error::new(
            "untracked_output",
            "Put required overwrite output into a recipe mod before publication.",
        ));
    }
    let mut winners = BTreeMap::new();
    overlay(
        &mut winners,
        &reconstructed.join("Stock Game"),
        "Stock Game/",
        false,
        recipe,
    )?;
    let mods = snapshot::lines(Some(&selected))?
        .into_iter()
        .filter_map(|s| s.strip_prefix('+').map(str::to_owned))
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    for m in &mods {
        files::relative(m)?;
        if m.contains('/') || !seen.insert(fold(m)) {
            return Err(Error::new(
                "invalid_profile",
                "Duplicate or invalid enabled mod name.",
            ));
        }
    }
    for m in mods.iter().rev() {
        overlay(
            &mut winners,
            &reconstructed.join("mods").join(m),
            &format!("mods/{m}/"),
            true,
            recipe,
        )?;
    }
    for name in snapshot::DOCUMENTS.into_iter().chain(["initweaks.ini"]) {
        let rel = format!("profiles/{profile}/{name}");
        if files::locate(reconstructed, &rel)?.is_some()
            && recipe
                .paths
                .get(&fold(&rel))
                .is_none_or(|k| k == "RemappedInlineFile")
        {
            return Err(Error::new("untracked_output",format!("Profile configuration must be reconstructed deterministically by the recipe: {rel}")));
        }
    }
    let inis = merge_inis(reconstructed, profile)?;
    for (name, path) in inis {
        let target = format!("config/documents/{name}");
        let rel = path
            .strip_prefix(reconstructed)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        winners.insert(
            fold(&target),
            Winner {
                target,
                path,
                source: Source {
                    kind: "wabbajack".into(),
                    path: rel,
                },
            },
        );
    }
    for name in snapshot::LOCAL {
        if let Some(path) = files::locate(&reconstructed.join("profiles").join(profile), name)? {
            let source = format!("profiles/{profile}/{name}");
            if recipe
                .paths
                .get(&fold(&source))
                .is_none_or(|k| k == "RemappedInlineFile")
            {
                return Err(Error::new(
                    "untracked_output",
                    "Activation must be declared by the recipe.",
                ));
            }
            let target = format!("config/local/{name}");
            winners.insert(
                fold(&target),
                Winner {
                    target,
                    path,
                    source: Source {
                        kind: "wabbajack".into(),
                        path: source,
                    },
                },
            );
        }
    }
    let oar = winners
        .get("game/data/skse/plugins/openanimationreplacer.dll")
        .cloned();
    if let Some(dll) = oar {
        let key = "game/data/skse/plugins/openanimationreplacer.ini";
        if !winners.contains_key(key) {
            if files::sha(&dll.path)?
                != "a908eca3a068c3ea67edc3495e8baa80038c3db345b7e1a051c80b9ad093a1b7"
            {
                return Err(Error::new(
                    "unsupported_default",
                    "This OpenAnimationReplacer version needs an explicit recipe INI.",
                ));
            }
            let source = ".skyrimsync-generated/runtime/oar-2.3.6.ini";
            let path = write_generated(
                reconstructed,
                source,
                include_bytes!("assets/oar-2.3.6.ini"),
            )?;
            let target = dll.target.strip_suffix(".dll").unwrap().to_owned() + ".ini";
            winners.insert(
                key.into(),
                Winner {
                    target,
                    path,
                    source: Source {
                        kind: "wabbajack".into(),
                        path: source.into(),
                    },
                },
            );
        }
    }
    let mut directories = BTreeMap::new();
    let mut instructions = Vec::new();
    for w in winners.values() {
        let mut parts = Vec::new();
        let segments = w.target.split('/').collect::<Vec<_>>();
        for part in &segments[..segments.len() - 1] {
            let key = fold(&format!("{}/{}", parts.join("/"), part));
            parts.push(
                directories
                    .entry(key)
                    .or_insert_with(|| (*part).to_owned())
                    .clone(),
            );
        }
        parts.push(segments.last().unwrap().to_string());
        let target = parts.join("/");
        snapshot::portable_configuration(&target, &w.path)?;
        let identity = files::identity(target.clone(), &w.path)?;
        files::copy(&w.path, &destination.join(&target))?;
        instructions.push(Instruction {
            path: target,
            size: identity.size,
            sha256: identity.sha256,
            source: w.source.clone(),
        });
    }
    let snapshot = snapshot::capture_install(destination)?;
    if !snapshot.warnings.is_empty() {
        return Err(Error::new(
            "incomplete_target",
            format!(
                "Target has unresolved plugin/configuration diagnostics: {:?}",
                snapshot.warnings
            ),
        ));
    }
    let hints = recipe.data["Archives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| SourceHint {
            name: a["Name"].as_str().unwrap_or("Unnamed archive").to_owned(),
            kind: a["State"]["$type"].as_str().unwrap_or("unknown").to_owned(),
            url: source_url(&a["State"]),
        })
        .collect();
    let manifest = Manifest {
        schema: 2,
        name: recipe.data["Name"]
            .as_str()
            .unwrap_or("Skyrim setup")
            .to_owned(),
        artifact: Artifact {
            sha256: files::sha(&recipe.artifact)?,
            size: fs::metadata(&recipe.artifact)?.len(),
            url: None,
        },
        files: instructions,
        sources: hints,
        snapshot: Some(snapshot),
        profile: Some(profile.into()),
        layout: Some("stock-mo2".into()),
    };
    manifest.validate()?;
    Ok(manifest)
}

/// Legacy v1 stores deterministic generated config paths as ordinary instructions.
pub fn regenerate_legacy(directory: &Path, manifest: &Manifest) -> Result<()> {
    let mut profiles = BTreeSet::new();
    for f in &manifest.files {
        if let Some(tail) = f
            .source
            .path
            .strip_prefix(".skyrimsync-generated/profiles/")
        {
            let (profile, name) = tail.split_once('/').ok_or_else(|| {
                Error::new("unsupported_default", "Invalid generated profile source.")
            })?;
            if !snapshot::DOCUMENTS.contains(&name) {
                return Err(Error::new(
                    "unsupported_default",
                    "Unknown generated configuration.",
                ));
            }
            profiles.insert(profile.to_owned());
        }
        if f.source.path == ".skyrimsync-generated/runtime/oar-2.3.6.ini" {
            // The exact expected default bytes are checked by the v1 instruction hash.
            write_generated(
                directory,
                &f.source.path,
                include_bytes!("assets/oar-2.3.6.ini"),
            )?;
        }
    }
    for profile in profiles {
        merge_inis(directory, &profile)?;
    }
    Ok(())
}

pub fn generated_dependencies(source: &str, recipe: &Recipe) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    if let Some(tail) = source.strip_prefix(".skyrimsync-generated/profiles/") {
        let (profile, name) = tail
            .split_once('/')
            .ok_or_else(|| Error::new("unsupported_default", "Invalid profile source."))?;
        if !snapshot::DOCUMENTS.contains(&name) {
            return Err(Error::new(
                "unsupported_default",
                "Unknown generated configuration.",
            ));
        }
        paths.insert(fold(&format!("profiles/{profile}/{name}")));
        let tweaks = fold(&format!("profiles/{profile}/initweaks.ini"));
        if recipe.paths.contains_key(&tweaks) {
            paths.insert(tweaks);
        }
    } else if source != ".skyrimsync-generated/runtime/oar-2.3.6.ini" {
        paths.insert(fold(source));
    }
    Ok(paths)
}
pub fn generate_selected(directory: &Path, source: &str) -> Result<()> {
    if let Some(tail) = source.strip_prefix(".skyrimsync-generated/profiles/") {
        let (profile, name) = tail
            .split_once('/')
            .ok_or_else(|| Error::new("unsupported_default", "Invalid profile source."))?;
        merge_selected_inis(directory, profile, Some(name))?;
    } else if source == ".skyrimsync-generated/runtime/oar-2.3.6.ini" {
        write_generated(directory, source, include_bytes!("assets/oar-2.3.6.ini"))?;
    }
    Ok(())
}
