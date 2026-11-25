//! The host: an MO2 profile turned into a pack. Each file's source is found by hash (a member of the
//! archive its mod was installed from, downloadable from Nexus or a URL), else the host sends it.
use crate::{
    archive,
    channel::{self, Head, PrivateChannel},
    config, deploy,
    files::{self, fold},
    github, hashes, ini, mo2, net, nexus,
    pack::{self, Archive, Entry, Origin, Pack, Source},
    snapshot, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// The status line for files in the game folder itself (not from a mod): friends with the same vanilla
/// files skip them; anything else there (leftovers of hand installs) is sent by the host.
const GAME_FOLDER: &str = "(game folder)";

/// A pack's files on the host's machine: SHA-256 -> path.
pub type LocalFiles = BTreeMap<String, PathBuf>;

/// What the host chose beyond MO2: per mod, where its files come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Override {
    /// Always sent by the host.
    Host,
    /// An HTTPS archive holding (some of) the mod's files.
    Url { url: String },
    /// The newest GitHub release's archive (resolved at each publish).
    Github {
        repo: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset: Option<String>,
    },
    /// Left out of the pack.
    Exclude,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub name: String,
    pub mo2: PathBuf,
    pub profile: String,
    #[serde(default)]
    pub game: Option<PathBuf>,
    /// Mod name (folded) -> override.
    #[serde(default)]
    pub overrides: BTreeMap<String, Override>,
}

pub struct Host {
    pub dir: PathBuf,
}

/// A mod's line in `sms host status`.
#[derive(Debug, Serialize)]
pub struct ModReport {
    pub name: String,
    pub files: usize,
    pub bytes: u64,
    /// "nexus", "url", "host", or "nexus+host" when some files differ from the archive.
    pub source: String,
    pub host_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A pack before it is signed, with the host files it needs frozen.
pub struct Draft {
    pub pack: Pack,
    pub host_files: BTreeMap<String, PathBuf>,
    /// Every non-base file by SHA-256 -> where it is on the host (the host plays from these).
    pub local: BTreeMap<String, PathBuf>,
    pub mods: Vec<ModReport>,
}

impl Host {
    pub fn open() -> Result<Self> {
        Ok(Self {
            dir: config::host_directory()?,
        })
    }
    fn binding_path(&self) -> PathBuf {
        self.dir.join("host.json")
    }
    pub fn binding(&self) -> Result<Binding> {
        if !self.binding_path().is_file() {
            return Err(Error::new(
                "host_setup_required",
                "Run `sms host init --mo2 DIR --profile NAME` first.",
            ));
        }
        files::read_json(&self.binding_path())
    }
    pub fn init(&self, b: Binding) -> Result<Value> {
        let profile = mo2::read(&b.mo2, &b.profile, b.game.as_deref(), &BTreeSet::new())?;
        files::atomic_json(&self.binding_path(), &b)?;
        Ok(
            json!({"state": "host_ready", "mo2": b.mo2, "profile": b.profile, "game": profile.game, "mods": profile.mods.len()}),
        )
    }
    pub fn set_override(&self, r#mod: &str, o: Option<Override>) -> Result<Value> {
        let mut b = self.binding()?;
        let profile = mo2::read(&b.mo2, &b.profile, b.game.as_deref(), &BTreeSet::new())?;
        if !profile.mods.iter().any(|m| fold(&m.name) == fold(r#mod)) {
            return Err(Error::new(
                "unknown_mod",
                format!("No enabled mod named {mod} in the profile."),
            ));
        }
        match &o {
            Some(Override::Url { url }) if !url.starts_with("https://") => {
                return Err(Error::new("invalid_url", "Archive URLs must be HTTPS."));
            }
            Some(Override::Github { repo, .. }) if !github::valid_repo(repo) => {
                return Err(Error::new(
                    "invalid_source",
                    "Expected a GitHub owner/repo.",
                ));
            }
            _ => {}
        }
        match &o {
            Some(o) => b.overrides.insert(fold(r#mod), o.clone()),
            None => b.overrides.remove(&fold(r#mod)),
        };
        files::atomic_json(&self.binding_path(), &b)?;
        Ok(json!({"state": "saved", "mod": r#mod, "override": o}))
    }

    /// A URL's archive, downloaded once (a release asset's URL names one file for good).
    fn fetch(&self, client: &reqwest::blocking::Client, url: &str) -> Result<PathBuf> {
        let index = self.dir.join("downloads/urls.json");
        let mut known: BTreeMap<String, String> = if index.is_file() {
            files::read_json(&index)?
        } else {
            BTreeMap::new()
        };
        if let Some(sha) = known.get(url) {
            let p = self.dir.join("downloads").join(sha);
            if p.is_file() {
                return Ok(p);
            }
        }
        crate::event("downloading", json!({"url": url}));
        let p = net::download(client, url, &self.dir.join("downloads"), None)?;
        known.insert(
            url.to_owned(),
            p.file_name().unwrap().to_string_lossy().into_owned(),
        );
        files::atomic_json(&index, &known)?;
        Ok(p)
    }

    /// The archive's members by hash, cached by the archive's own hash.
    fn members(
        &self,
        cache: &mut hashes::Cache,
        path: &Path,
    ) -> Result<(u64, String, BTreeMap<String, String>)> {
        let (size, sha) = cache.sha256(path)?;
        let listed = self.dir.join("archives").join(format!("{sha}.json"));
        let members: Vec<archive::Member> = if listed.is_file() {
            files::read_json(&listed)?
        } else {
            crate::event(
                "listing_archive",
                json!({"archive": path.file_name().map(|n| n.to_string_lossy().into_owned())}),
            );
            let m = archive::members(path)?;
            files::atomic_json(&listed, &m)?;
            m
        };
        Ok((
            size,
            sha,
            members.into_iter().map(|m| (m.sha256, m.name)).collect(),
        ))
    }

    /// Reads the profile and finds every file's source.
    pub fn draft(&self) -> Result<Draft> {
        let b = self.binding()?;
        let excluded = b
            .overrides
            .iter()
            .filter(|(_, o)| **o == Override::Exclude)
            .map(|(m, _)| m.clone())
            .collect();
        let profile = mo2::read(&b.mo2, &b.profile, b.game.as_deref(), &excluded)?;
        let deployed = deploy::Game {
            root: profile.game.clone(),
            local: PathBuf::new(),
            documents: PathBuf::new(),
        };
        if deployed.state()? != deploy::State::Clean {
            return Err(Error::new(
                "pack_loaded",
                "Your game folder has a pack loaded: `sms unload` before publishing, or its copies would be read as game files.",
            ));
        }
        let runtime = snapshot::runtime(&profile.game)?;
        let key = config::nexus_key()?;
        let mut cache = hashes::Cache::open(&self.dir.join("hashes.json"))?;
        let client = net::client()?;

        // Each mod's archive: (archive record, member hash -> member name), or why it has none.
        let mut sources: BTreeMap<String, (Archive, BTreeMap<String, String>)> = BTreeMap::new();
        let mut notes: BTreeMap<String, String> = BTreeMap::new();
        for m in &profile.mods {
            let found: Result<Option<(PathBuf, Origin)>> = match b.overrides.get(&fold(&m.name)) {
                Some(Override::Host) => Ok(None),
                Some(Override::Url { url }) => self
                    .fetch(&client, url)
                    .map(|p| Some((p, Origin::Url { url: url.clone() }))),
                Some(Override::Github { repo, asset }) => {
                    let (url, _) = github::latest_asset(repo, asset.as_deref())?;
                    self.fetch(&client, &url)
                        .map(|p| Some((p, Origin::Url { url })))
                }
                _ => match (&m.archive, &m.nexus) {
                    (Some(a), Some((game, id, file))) => match &key {
                        Some(k) if !nexus::available(k, game, *id, *file)? => {
                            notes
                                .insert(m.name.clone(), "no longer downloadable from Nexus".into());
                            Ok(None)
                        }
                        _ => Ok(Some((
                            a.clone(),
                            Origin::Nexus {
                                game: game.clone(),
                                r#mod: *id,
                                file: *file,
                            },
                        ))),
                    },
                    (None, Some(_)) => {
                        notes.insert(
                            m.name.clone(),
                            "its archive is not in MO2's downloads folder".into(),
                        );
                        Ok(None)
                    }
                    _ => {
                        notes.insert(
                            m.name.clone(),
                            "no Nexus record (installed by hand?)".into(),
                        );
                        Ok(None)
                    }
                },
            };
            let Some((path, origin)) = found? else {
                continue;
            };
            match self.members(&mut cache, &path) {
                Ok((size, sha256, members)) => {
                    let name = match &origin {
                        Origin::Url { url } => url.rsplit('/').next().unwrap_or(url).to_owned(),
                        Origin::Nexus { .. } => path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    };
                    sources.insert(
                        fold(&m.name),
                        (
                            Archive {
                                sha256,
                                size,
                                name,
                                origin,
                            },
                            members,
                        ),
                    );
                }
                Err(e) if e.code == "unsupported_archive" => {
                    notes.insert(m.name.clone(), "RAR archive (unsupported)".into());
                }
                Err(e) => return Err(e),
            }
        }

        let mut entries = vec![];
        let mut host_files = BTreeMap::new();
        let mut local = BTreeMap::new();
        let mut used = BTreeMap::new();
        let mut reports: BTreeMap<String, ModReport> = BTreeMap::new();
        for w in profile.files.values() {
            let base = fold(&w.path);
            if base == "skyrimselauncher.exe" || base == "skyrimselauncher.bethesda.exe" {
                continue; // `sms load` puts SKSE's loader there
            }
            let (size, sha256) = cache.sha256(&w.file)?;
            let source = if snapshot::protected(&w.path) {
                Source::Base
            } else if let Some((a, member)) = w
                .r#mod
                .as_ref()
                .and_then(|m| sources.get(&fold(m)))
                .and_then(|(a, members)| members.get(&sha256).map(|n| (a, n)))
            {
                used.insert(a.sha256.clone(), a.clone());
                Source::Archive {
                    archive: a.sha256.clone(),
                    member: member.clone(),
                }
            } else {
                let blob = match cache.blake3(&w.file)? {
                    Some(b) => b,
                    None => {
                        let b = channel::blob_hash(&w.file)?;
                        cache.set_blake3(&w.file, &b)?;
                        b
                    }
                };
                host_files.insert(blob.clone(), w.file.clone());
                Source::Host { blob }
            };
            if source != Source::Base {
                let m = w.r#mod.clone().unwrap_or_else(|| GAME_FOLDER.to_owned());
                let r = reports.entry(m.clone()).or_insert_with(|| ModReport {
                    name: m.clone(),
                    files: 0,
                    bytes: 0,
                    source: String::new(),
                    host_bytes: 0,
                    note: notes.get(&m).cloned(),
                });
                r.files += 1;
                r.bytes += size;
                if matches!(source, Source::Host { .. }) {
                    r.host_bytes += size;
                }
            }
            if source != Source::Base {
                local.insert(sha256.clone(), w.file.clone());
            }
            entries.push(Entry {
                path: w.path.clone(),
                size,
                sha256,
                r#mod: w.r#mod.clone(),
                source,
            });
        }
        cache.save()?;
        for r in reports.values_mut() {
            let origin = sources.get(&fold(&r.name)).map(|(a, _)| match a.origin {
                Origin::Nexus { .. } => "nexus",
                Origin::Url { .. } => "url",
            });
            r.source = match (origin, r.host_bytes) {
                (Some(o), 0) => o.into(),
                (Some(o), h) if h == r.bytes => {
                    r.note
                        .get_or_insert_with(|| "no file matches its archive".into());
                    let _ = o;
                    "host".into()
                }
                (Some(o), _) => format!("{o}+host"),
                (None, _) => "host".into(),
            };
        }
        entries.sort_by(|a, b| fold(&a.path).cmp(&fold(&b.path)));
        let mut ini_values = pack::Ini::new();
        for (name, path) in &profile.inis {
            let s: ini::Sections = ini::shared(path, name)?;
            if !s.is_empty() {
                ini_values.insert(name.clone(), s);
            }
        }
        let pack = Pack {
            schema: pack::SCHEMA,
            name: b.name.clone(),
            runtime,
            plugins: profile.plugins,
            loadorder: profile.loadorder,
            ini: ini_values,
            archives: used.into_values().collect(),
            files: entries,
        };
        pack.validate()?;
        let mut mods = reports.into_values().collect::<Vec<_>>();
        mods.sort_by_key(|m| std::cmp::Reverse(m.host_bytes));
        Ok(Draft {
            pack,
            host_files,
            local,
            mods,
        })
    }

    /// The host's latest published pack, with where each of its files is on this machine: the host
    /// syncs from its own files to play its pack.
    pub fn own_pack(&self) -> Result<Option<(Pack, Head, LocalFiles)>> {
        let head_path = self.dir.join("channel-head.json");
        if !head_path.is_file() {
            return Ok(None);
        }
        let head: Head = files::read_json(&head_path)?;
        head.validate()?;
        let packs = self.dir.join("packs");
        let pack: Pack = files::read_json(&packs.join(format!("{}.json", head.revision)))?;
        if pack.id()? != head.revision {
            return Err(Error::new(
                "invalid_pack",
                "The published pack differs from its head.",
            ));
        }
        let local = files::read_json(&packs.join(format!("{}.local.json", head.revision)))?;
        Ok(Some((pack, head, local)))
    }

    pub fn status(&self) -> Result<Value> {
        let d = self.draft()?;
        let total: u64 = d
            .pack
            .files
            .iter()
            .filter(|e| e.source != Source::Base)
            .map(|e| e.size)
            .sum();
        let host: u64 = d.mods.iter().map(|m| m.host_bytes).sum();
        Ok(json!({
            "state": "draft",
            "name": d.pack.name,
            "runtime": d.pack.runtime,
            "files": d.pack.files.len(),
            "plugins": d.pack.plugins.len(),
            "archives": d.pack.archives.len(),
            "bytes": total,
            "host_bytes": host,
            "mods": d.mods,
        }))
    }

    /// Freezes the host's files, signs the pack as the channel's next head; with `output`, also
    /// writes a pack file.
    pub fn publish(&self, output: Option<&Path>) -> Result<Value> {
        let _lock = files::Lock::take(&self.dir.join("locks/publish.lock"))?;
        let d = self.draft()?;
        for (blob, file) in &d.host_files {
            let object = channel::object_path(&self.dir, blob)?;
            if !object.is_file() && channel::freeze(&self.dir, file)? != *blob {
                return Err(Error::new(
                    "source_changed",
                    format!("{} changed while publishing.", file.display()),
                ));
            }
        }
        let private = PrivateChannel::load_or_create(&self.dir.join("channel-private.json"))?;
        let head_path = self.dir.join("channel-head.json");
        let id = d.pack.id()?;
        let old: Option<Head> = if head_path.is_file() {
            Some(files::read_json(&head_path)?)
        } else {
            None
        };
        let head = match &old {
            Some(o) if o.revision == id => o.clone(),
            Some(o) => Head::sign(
                private.channel()?,
                o.sequence + 1,
                id.clone(),
                Some(o.revision.clone()),
                &private.signing_key()?,
            )?,
            None => Head::sign(
                private.channel()?,
                1,
                id.clone(),
                None,
                &private.signing_key()?,
            )?,
        };
        files::atomic_json(&self.dir.join("packs").join(format!("{id}.json")), &d.pack)?;
        files::atomic_json(
            &self.dir.join("packs").join(format!("{id}.local.json")),
            &d.local,
        )?;
        files::atomic_json(
            &self.dir.join("published").join(format!("{id}.json")),
            &head,
        )?;
        files::atomic_json(&head_path, &head)?;
        if let Some(out) = output {
            channel::export(out, &d.pack, &head)?;
        }
        let host_bytes: u64 = d
            .pack
            .files
            .iter()
            .filter(|e| matches!(e.source, Source::Host { .. }))
            .map(|e| e.size)
            .sum();
        Ok(json!({
            "state": "published",
            "code": head.channel.code()?,
            "revision": id,
            "sequence": head.sequence,
            "pack_file": output,
            "host_files": d.host_files.len(),
            "host_bytes": host_bytes,
            "needs_serve": !d.host_files.is_empty(),
        }))
    }
}
