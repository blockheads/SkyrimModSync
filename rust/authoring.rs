//! Read-only imports of editable installations; owned drafts and exports are snapshots.
use crate::{
    files,
    recipe::Recipe,
    revision::{self, Channel, Entry, Head, Revision, Source},
    snapshot,
    target::Storage,
    Error, Result,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub schema: u32,
    pub mo2: Option<PathBuf>,
    pub profile: Option<String>,
    pub game: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub local: Option<PathBuf>,
    pub recipe: Option<PathBuf>,
    pub base: PathBuf,
    pub downloads: PathBuf,
}
#[derive(Clone)]
struct Winner {
    target: String,
    path: PathBuf,
    recipe_path: Option<String>,
}
fn cfg(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = files::bounded(path, 4 * 1024 * 1024)?;
    let text = std::str::from_utf8(&raw)
        .map_err(|_| Error::new("invalid_configuration", "Profile settings require UTF-8."))?;
    let mut result = BTreeMap::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with(['#', ';']))
    {
        if let Some((k, v)) = line.split_once('=') {
            result.insert(files::fold(k.trim()), v.trim().trim_matches('"').to_owned());
        }
    }
    Ok(result)
}
fn directory(
    root: &Path,
    config: &BTreeMap<String, String>,
    key: &str,
    default: &str,
) -> Result<PathBuf> {
    let value = config
        .get(key)
        .map(String::as_str)
        .unwrap_or(default)
        .replace("%BASE_DIR%", &root.to_string_lossy())
        .replace('\\', "/");
    let p = PathBuf::from(value);
    let p = if p.is_absolute() {
        p
    } else if p.to_string_lossy().as_bytes().get(1) == Some(&b':') {
        return Err(Error::new("source_path_unmapped",format!("MO2 {key} uses a foreign Windows drive. Fix its configured path or provide an ordinary deployed --game import.")));
    } else {
        root.join(p)
    };
    files::root(&p)
}
fn add(
    winners: &mut BTreeMap<String, Winner>,
    dir: &Path,
    prefix: &str,
    mod_files: bool,
    recipe_prefix: Option<&str>,
) -> Result<()> {
    for (rel, path) in files::inventory(dir)? {
        let first = files::fold(rel.split('/').next().unwrap());
        let lower = files::fold(&rel);
        if mod_files
            && (lower == "meta.ini"
                || lower.split('/').any(|part| part.ends_with(".mohidden"))
                || ["calientetools", "nemesis_engine"].contains(&first.as_str()))
        {
            continue;
        }
        if first == "ini tweaks" {
            return Err(Error::new("unsupported_layout","Mod-specific INI tweaks need a supported adapter; materialize this profile before import."));
        }
        let target = if mod_files && first == "root" {
            format!(
                "game/{}",
                rel.split_once('/')
                    .ok_or_else(|| Error::new("unsupported_layout", "Invalid root output."))?
                    .1
            )
        } else {
            format!("{prefix}/{rel}")
        };
        files::relative(&target)?;
        winners.insert(
            files::fold(&target),
            Winner {
                target,
                path,
                recipe_path: recipe_prefix.map(|p| format!("{p}{rel}")),
            },
        );
    }
    Ok(())
}
#[derive(Debug)]
pub struct Capture {
    pub revision: Revision,
    pub paths: BTreeMap<String, PathBuf>,
    pub source_hashes: BTreeMap<String, String>,
    _generated: tempfile::TempDir,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    source_sha: Option<String>,
    entry: Option<Entry>,
}
fn overrides(storage: &Storage) -> Result<BTreeMap<String, Override>> {
    let p = storage.root.join("authoring-overrides.json");
    if p.exists() {
        files::read_json(&p)
    } else {
        Ok(BTreeMap::new())
    }
}
pub fn capture(binding: &Binding, storage: &Storage) -> Result<Capture> {
    capture_with_choices(binding, storage, &BTreeSet::new())
}
fn capture_with_choices(
    binding: &Binding,
    storage: &Storage,
    use_source: &BTreeSet<String>,
) -> Result<Capture> {
    if binding.schema != 1 {
        return Err(Error::new(
            "invalid_source",
            "Unknown import source schema.",
        ));
    }
    files::separate(&binding.base, &storage.root)?;
    fs::create_dir_all(storage.root.join("stages"))?;
    let generated = tempfile::tempdir_in(storage.root.join("stages"))?;
    let mut winners = BTreeMap::new();
    let mut profile_name = "Imported setup".to_owned();
    if let Some(root) = &binding.mo2 {
        let root = files::root(root)?;
        files::separate(&root, &storage.root)?;
        let settings = cfg(&root.join("ModOrganizer.ini"))?;
        if settings
            .get("load_mechanism")
            .is_some_and(|v| v != "0" && v != "1")
        {
            return Err(Error::new(
                "unsupported_layout",
                "Unsupported MO2 load mechanism.",
            ));
        }
        let name = binding
            .profile
            .as_deref()
            .ok_or_else(|| Error::new("profile_required", "Select an MO2 --profile."))?;
        files::relative(name)?;
        if name.contains('/') {
            return Err(Error::new(
                "invalid_profile",
                "Profile must be a directory name.",
            ));
        }
        profile_name = name.into();
        let mods = directory(&root, &settings, "mod_directory", "mods")?;
        let profiles = directory(&root, &settings, "profiles_directory", "profiles")?;
        let selected = profiles.join(name);
        files::root(&selected)?;
        let game = if let Some(game) = &binding.game {
            files::root(game)?
        } else if root.join("Stock Game").is_dir() {
            files::root(&root.join("Stock Game"))?
        } else {
            directory(&root, &settings, "gamepath", "Stock Game")?
        };
        add(&mut winners, &game, "game", false, Some("Stock Game/"))?;
        let list = snapshot::lines(Some(&files::require(&selected, "modlist.txt")?))?;
        let enabled = list
            .iter()
            .filter_map(|l| l.strip_prefix('+'))
            .collect::<Vec<_>>();
        let mut names = BTreeSet::new();
        for name in &enabled {
            files::relative(name)?;
            if name.contains('/') || !names.insert(files::fold(name)) {
                return Err(Error::new(
                    "invalid_profile",
                    "Invalid or duplicate enabled mod.",
                ));
            }
        }
        for name in enabled.iter().rev() {
            add(
                &mut winners,
                &mods.join(name),
                "game/Data",
                true,
                Some(&format!("mods/{name}/")),
            )?;
        }
        let overwrite_value = settings
            .get("overwrite_directory")
            .map(String::as_str)
            .unwrap_or("overwrite")
            .replace("%BASE_DIR%", &root.to_string_lossy())
            .replace('\\', "/");
        let overwrite = if Path::new(&overwrite_value).is_absolute() {
            PathBuf::from(&overwrite_value)
        } else {
            root.join(&overwrite_value)
        };
        if overwrite.exists() {
            let overwrite = directory(&root, &settings, "overwrite_directory", "overwrite")?;
            add(
                &mut winners,
                &files::root(&overwrite)?,
                "game/Data",
                true,
                None,
            )?;
        }
        let local_inis = settings
            .get("profile_local_inis")
            .is_none_or(|v| v == "true" || v == "1");
        let docs = if local_inis {
            selected.clone()
        } else {
            binding.documents.clone().ok_or_else(|| {
                Error::new(
                    "configuration_required",
                    "This profile uses global INIs. Supply --documents.",
                )
            })?
        };
        let scratch = generated.path().join("profiles").join(name);
        fs::create_dir_all(&scratch)?;
        for n in snapshot::DOCUMENTS.into_iter().chain(["initweaks.ini"]) {
            if let Some(p) = files::locate(&docs, n)? {
                files::copy(&p, &scratch.join(n))?;
            } else if n == "SkyrimCustom.ini" {
                fs::write(scratch.join(n), b"")?;
            }
        }
        for (n, path) in crate::recipe::merge_inis(generated.path(), name)? {
            let target = format!("config/documents/{n}");
            winners.insert(
                files::fold(&target),
                Winner {
                    target,
                    path,
                    recipe_path: None,
                },
            );
        }
        let local = binding.local.as_deref().unwrap_or(&selected);
        for n in snapshot::LOCAL {
            let path = files::require(local, n)?;
            let target = format!("config/local/{n}");
            winners.insert(
                files::fold(&target),
                Winner {
                    target,
                    path,
                    recipe_path: Some(format!("profiles/{name}/{n}")),
                },
            );
        }
    } else {
        let game = binding
            .game
            .as_deref()
            .ok_or_else(|| Error::new("source_required", "Supply --mo2 or --game."))?;
        files::separate(&files::root(game)?, &storage.root)?;
        add(&mut winners, game, "game", false, None)?;
        for (root, names, prefix) in [
            (
                binding.documents.as_deref(),
                snapshot::DOCUMENTS.as_slice(),
                "config/documents",
            ),
            (
                binding.local.as_deref(),
                snapshot::LOCAL.as_slice(),
                "config/local",
            ),
        ] {
            let root = root.ok_or_else(|| {
                Error::new(
                    "configuration_required",
                    "Deployed imports need --documents and --local.",
                )
            })?;
            for n in names {
                let path = files::require(root, n)?;
                let target = format!("{prefix}/{n}");
                winners.insert(
                    files::fold(&target),
                    Winner {
                        target,
                        path,
                        recipe_path: None,
                    },
                );
            }
        }
    }
    let logs = snapshot::known_logs(winners.values().map(|w| w.target.clone()));
    winners.retain(|_, w| !snapshot::ignored(&w.target, &logs));
    // A versioned deterministic first-run default is part of the shared installation.
    if let Some(oar) = winners.get("game/data/skse/plugins/openanimationreplacer.dll") {
        if !winners.contains_key("game/data/skse/plugins/openanimationreplacer.ini")
            && files::sha(&oar.path)?
                == "a908eca3a068c3ea67edc3495e8baa80038c3db345b7e1a051c80b9ad093a1b7"
        {
            let path = generated.path().join("oar.ini");
            fs::write(&path, include_bytes!("assets/oar-2.3.6.ini"))?;
            let target = "game/Data/SKSE/Plugins/OpenAnimationReplacer.ini".to_owned();
            winners.insert(
                files::fold(&target),
                Winner {
                    target,
                    path,
                    recipe_path: None,
                },
            );
        }
    }
    let recipe = binding
        .recipe
        .as_ref()
        .map(|p| Recipe::load(p))
        .transpose()?;
    let artifact = if let Some(r) = &recipe {
        let a = crate::package::Artifact {
            sha256: files::sha(&r.artifact)?,
            size: fs::metadata(&r.artifact)?.len(),
            url: None,
        };
        let dest = storage
            .root
            .join("recipes")
            .join(format!("{}.wabbajack", a.sha256));
        if !dest.exists() {
            files::copy(&r.artifact, &dest)?;
        }
        Some(a)
    } else {
        None
    };
    let base_catalog = recipe
        .as_ref()
        .map(|r| {
            r.data["Archives"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| {
                    a["State"]["$type"]
                        .as_str()
                        .unwrap_or("")
                        .contains("GameFileSource")
                })
                .filter_map(|a| a["State"]["GameFile"].as_str())
                .map(|p| files::fold(&p.replace('\\', "/")))
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut entries = Vec::new();
    let mut paths = BTreeMap::new();
    let mut directories = BTreeMap::new();
    for winner in winners.values() {
        crate::check_cancel()?;
        let mut parts = Vec::new();
        let split = winner.target.split('/').collect::<Vec<_>>();
        for part in &split[..split.len() - 1] {
            let key = files::fold(&format!("{}/{}", parts.join("/"), part));
            parts.push(
                directories
                    .entry(key)
                    .or_insert_with(|| part.to_string())
                    .clone(),
            );
        }
        parts.push(split.last().unwrap().to_string());
        let target = parts.join("/");
        snapshot::portable_configuration(&target, &winner.path)?;
        let id = files::identity(target.clone(), &winner.path)?;
        let game_rel = target.strip_prefix("game/");
        let base = game_rel
            .map(|p| files::locate(&binding.base, p))
            .transpose()?
            .flatten();
        let mut source = None;
        if let Some(p) = &base {
            if (revision::protected_base(&target)
                || game_rel.is_some_and(|p| base_catalog.contains(&files::fold(p))))
                && fs::metadata(p)?.len() == id.size
                && files::sha(p)? == id.sha256
            {
                source = Some(Source::Base {
                    path: game_rel.unwrap().into(),
                });
            }
        }
        if source.is_none() {
            if let (Some(r), Some(rel)) = (&recipe, &winner.recipe_path) {
                if let Some(d) = r.data["Directives"].as_array().unwrap().iter().find(|d| {
                    files::fold(&d["To"].as_str().unwrap().replace('\\', "/")) == files::fold(rel)
                }) {
                    if d["$type"] != "RemappedInlineFile"
                        && d["Size"].as_u64() == Some(id.size)
                        && crate::acquisition::wj_hash(&winner.path)?
                            == d["Hash"].as_str().unwrap_or("")
                    {
                        source = Some(Source::Recipe { path: rel.clone() });
                    }
                }
            }
        }
        let source = if let Some(s) = source {
            s
        } else {
            if revision::protected_base(&target) {
                return Err(Error::new("base_provenance_required",format!("Base content differs and has no verified recipe source: {target}. Select matching licensed files or the original recipe.")));
            }
            let hash = revision::freeze(&storage.root, &winner.path)?;
            let frozen = revision::object_path(&storage.root, &hash)?;
            if files::sha(&frozen)? != id.sha256 {
                return Err(Error::new(
                    "source_changed",
                    "Authoring file changed while being frozen. Close editing tools and retry.",
                ));
            }
            Source::Blob { hash }
        };
        paths.insert(target.clone(), winner.path.clone());
        entries.push(Entry {
            path: target,
            size: id.size,
            sha256: id.sha256,
            source,
        });
    }
    // Capture metadata without cloning the entire game: the snapshot module accepts
    // an explicit selected-file view, using the same plugin/version rules as capture.
    let source_hashes = entries
        .iter()
        .map(|e| (e.path.clone(), e.sha256.clone()))
        .collect::<BTreeMap<_, _>>();
    for (path, override_) in overrides(storage)? {
        files::relative(&path)?;
        if use_source.contains(&path) {
            continue;
        }
        let current = source_hashes.get(&path).cloned();
        if current != override_.source_sha {
            if override_
                .entry
                .as_ref()
                .is_some_and(|e| Some(&e.sha256) == current.as_ref())
            {
                continue;
            }
            return Err(Error::new("authoring_override_conflict",format!("Authoring source changed a previously promoted runtime file: {path}. Publish with --use-source {path} to use the new source; or move the desired runtime result back into your mod-manager profile.")));
        }
        entries.retain(|e| files::fold(&e.path) != files::fold(&path));
        paths.remove(&path);
        if let Some(e) = override_.entry {
            let Source::Blob { hash } = &e.source else {
                return Err(Error::new(
                    "invalid_override",
                    "Runtime override must be a frozen blob.",
                ));
            };
            let frozen = revision::object_path(&storage.root, hash)?;
            if !revision::matches(&frozen, &e)? {
                return Err(Error::new(
                    "object_corrupt",
                    "Promoted runtime content differs.",
                ));
            }
            paths.insert(path, frozen);
            entries.push(e);
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let snap = snapshot::capture_selection(&paths)?;
    let rev = Revision {
        schema: 3,
        name: profile_name,
        snapshot: snap,
        files: entries,
        recipe: artifact,
        policy: "isolated-personal-v1".into(),
    };
    rev.validate()?;
    Ok(Capture {
        revision: rev,
        paths,
        source_hashes,
        _generated: generated,
    })
}

pub fn import(storage: &Storage, binding: &Binding) -> Result<Value> {
    let c = capture(binding, storage)?;
    let result = revision::install(storage, &c.revision, |e| {
        c.paths
            .get(&e.path)
            .cloned()
            .ok_or_else(|| Error::new("source_missing", "Missing imported source."))
    })?;
    files::atomic_json(&storage.root.join("authoring-source.json"), binding)?;
    files::atomic_json(&storage.root.join("authoring-baseline.json"), &c.revision)?;
    Ok(
        json!({"state":"imported","source_bound":true,"installation":result,"peer_files":c.revision.objects().len(),"revision":c.revision.id()?,"runtime_verified":false}),
    )
}
pub fn refresh(storage: &Storage) -> Result<Option<Revision>> {
    let p = storage.root.join("authoring-source.json");
    if !p.exists() {
        return Ok(None);
    }
    let binding: Binding = files::read_json(&p)?;
    let c = capture(&binding, storage)?;
    revision::install(storage, &c.revision, |e| {
        c.paths
            .get(&e.path)
            .cloned()
            .ok_or_else(|| Error::new("source_missing", "Missing imported source."))
    })?;
    files::atomic_json(&storage.root.join("authoring-baseline.json"), &c.revision)?;
    Ok(Some(c.revision))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateChannel {
    pub schema: u32,
    pub endpoint_key: String,
    pub signing_key: String,
    pub pack: String,
    pub relay: Option<String>,
}
pub fn private_channel(storage: &Storage) -> Result<PrivateChannel> {
    let path = storage.root.join("channel-private.json");
    if path.exists() {
        return files::read_json(&path);
    }
    let mut e = [0u8; 32];
    let mut s = [0u8; 32];
    let mut p = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut e);
    rand::rngs::OsRng.fill_bytes(&mut s);
    rand::rngs::OsRng.fill_bytes(&mut p);
    let c = PrivateChannel {
        schema: 1,
        endpoint_key: URL_SAFE_NO_PAD.encode(e),
        signing_key: URL_SAFE_NO_PAD.encode(s),
        pack: files::hash_bytes(&p),
        relay: None,
    };
    files::atomic_json(&path, &c)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(c)
}
pub fn channel(private: &PrivateChannel) -> Result<Channel> {
    let key = iroh::SecretKey::from_bytes(&revision::decode32(&private.endpoint_key)?);
    let sign = SigningKey::from_bytes(&revision::decode32(&private.signing_key)?);
    Ok(Channel {
        schema: 3,
        endpoint: key.public().to_string(),
        publisher: URL_SAFE_NO_PAD.encode(sign.verifying_key().to_bytes()),
        pack: private.pack.clone(),
        relay: private.relay.clone(),
    })
}
pub fn publish(
    storage: &Storage,
    output: &Path,
    include: &[String],
    interactive: bool,
) -> Result<Value> {
    publish_with_choices(storage, output, include, &[], false, interactive)
}
pub fn publish_with_choices(
    storage: &Storage,
    output: &Path,
    include: &[String],
    use_source: &[String],
    keep_runtime: bool,
    interactive: bool,
) -> Result<Value> {
    let _lock = files::Lock::take(&storage.root.join("locks/publish.lock"))?;
    let binding: Binding = files::read_json(&storage.root.join("authoring-source.json"))?;
    let use_source = use_source.iter().cloned().collect::<BTreeSet<_>>();
    let mut c = capture_with_choices(&binding, storage, &use_source)?;
    let before = c
        .revision
        .files
        .iter()
        .map(|e| (e.path.clone(), e.sha256.clone()))
        .collect::<BTreeMap<_, _>>();
    crate::play::promote(
        storage,
        &mut c.revision,
        &mut c.paths,
        include,
        &use_source,
        keep_runtime,
        interactive,
    )?;
    let after = c
        .revision
        .files
        .iter()
        .map(|e| (e.path.clone(), e.sha256.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut persisted = overrides(storage)?;
    for p in &use_source {
        persisted.remove(p);
    }
    for path in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        if before.get(path) != after.get(path) {
            persisted.insert(
                path.clone(),
                Override {
                    source_sha: c.source_hashes.get(path).cloned(),
                    entry: c.revision.files.iter().find(|e| &e.path == path).cloned(),
                },
            );
        }
    }
    revision::install(storage, &c.revision, |e| {
        c.paths
            .get(&e.path)
            .cloned()
            .ok_or_else(|| Error::new("source_missing", "Missing candidate source."))
    })?;
    let private = private_channel(storage)?;
    let channel = channel(&private)?;
    let old: Option<Head> = if storage.root.join("channel-head.json").exists() {
        Some(files::read_json(&storage.root.join("channel-head.json"))?)
    } else {
        None
    };
    let id = c.revision.id()?;
    let head = if let Some(old) = &old {
        old.validate()?;
        if old.revision == id {
            old.clone()
        } else {
            Head::sign(
                channel,
                old.sequence
                    .checked_add(1)
                    .ok_or_else(|| Error::new("invalid_revision", "Revision sequence overflow."))?,
                id.clone(),
                Some(old.revision.clone()),
                &SigningKey::from_bytes(&revision::decode32(&private.signing_key)?),
            )?
        }
    } else {
        Head::sign(
            channel,
            1,
            id.clone(),
            None,
            &SigningKey::from_bytes(&revision::decode32(&private.signing_key)?),
        )?
    };
    revision::save(&storage.root, &c.revision)?;
    revision::export(output, &c.revision, &head, &storage.root)?;
    files::atomic_json(
        &storage.root.join("published").join(format!("{id}.json")),
        &head,
    )?;
    files::atomic_json(&storage.root.join("authoring-overrides.json"), &persisted)?;
    files::atomic_json(&storage.root.join("channel-head.json"), &head)?;
    files::atomic_json(&storage.root.join("authoring-baseline.json"), &c.revision)?;
    Ok(
        json!({"state":"published","revision":id,"sequence":head.sequence,"code":head.channel.code()?,"package":output,"peer_files":c.revision.objects().len(),"runtime_verified":false}),
    )
}
