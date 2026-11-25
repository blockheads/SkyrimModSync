//! The host: an MO2 profile turned into a pack. A file found by hash in the archive its mod was installed
//! from is downloaded by friends (Nexus or a URL); everything else the host sends.
use crate::{
    archive, config,
    deploy::{self, Game},
    files::{self, fold},
    hash, ini, mo2,
    net::{self, Net},
    pack::{self, Archive, Entry, From, Origin, Pack},
    peer::{self, Published},
    rules, Error, Result,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Where a mod's files come from, when not its MO2 record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Host,
    Url {
        url: String,
    },
    /// The newest GitHub release's archive, picked at each publish.
    Github {
        repo: String,
        asset: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub name: String,
    pub mo2: PathBuf,
    pub profile: String,
    pub game: Option<PathBuf>,
    /// Mod name (folded) -> source.
    #[serde(default)]
    pub sources: BTreeMap<String, Source>,
}

/// One mod's line in `sms host status`.
#[derive(Debug, Serialize)]
pub struct ModReport {
    pub name: String,
    pub files: usize,
    pub bytes: u64,
    pub host_bytes: u64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub struct Draft {
    pub published: Published,
    pub mods: Vec<ModReport>,
}

pub struct Host {
    pub dir: PathBuf,
}

impl Host {
    pub fn open() -> Result<Self> {
        Ok(Self {
            dir: config::directory()?.join("host"),
        })
    }
    pub fn binding(&self) -> Result<Binding> {
        files::read_json(&self.dir.join("host.json")).map_err(|_| {
            Error::new(
                "host_setup_required",
                "Run `sms host init --mo2 DIR --profile NAME` first.",
            )
        })
    }
    pub fn init(&self, b: Binding) -> Result<Value> {
        let profile = mo2::read(&b.mo2, &b.profile, b.game.as_deref())?;
        files::write_json(&self.dir.join("host.json"), &b)?;
        Ok(json!({"state": "host_ready", "game": profile.game, "mods": profile.mods.len()}))
    }
    pub fn set_source(&self, r#mod: &str, source: Option<Source>) -> Result<Value> {
        let mut b = self.binding()?;
        let profile = mo2::read(&b.mo2, &b.profile, b.game.as_deref())?;
        if !profile.mods.iter().any(|m| fold(&m.name) == fold(r#mod)) {
            return Err(Error::new(
                "unknown_mod",
                format!("No enabled mod named {mod}."),
            ));
        }
        if let Some(Source::Github { repo, .. }) = &source {
            if !net::valid_repo(repo) {
                return Err(Error::new(
                    "invalid_source",
                    "Expected a GitHub owner/repo.",
                ));
            }
        }
        match &source {
            Some(s) => b.sources.insert(fold(r#mod), s.clone()),
            None => b.sources.remove(&fold(r#mod)),
        };
        files::write_json(&self.dir.join("host.json"), &b)?;
        Ok(json!({"state": "saved", "mod": r#mod, "source": source}))
    }
    pub fn key(&self) -> Result<iroh::SecretKey> {
        peer::secret(&self.dir.join("key"))
    }
    pub fn published(&self) -> Result<Option<Published>> {
        let p = self.dir.join("published.json");
        if !p.is_file() {
            return Ok(None);
        }
        Ok(Some(files::read_json(&p)?))
    }

    /// Reads the profile and finds every file's source.
    pub fn draft(&self, net: &Net) -> Result<Draft> {
        let b = self.binding()?;
        let profile = mo2::read(&b.mo2, &b.profile, b.game.as_deref())?;
        let folder = Game {
            root: profile.game.clone(),
            local: PathBuf::new(),
            documents: PathBuf::new(),
        };
        if folder.state() != deploy::State::Clean {
            return Err(Error::new(
                "pack_loaded",
                "Your game folder has a pack loaded: `sms unload` before publishing.",
            ));
        }
        let mut cache = hash::Cache::open(&self.dir.join("cache/hashes.json"));
        let (sources, notes) = self.sources(&b, &profile, net, &mut cache)?;
        let winners: Vec<&mo2::Winner> = profile.files.values().collect();
        let paths: Vec<PathBuf> = winners.iter().map(|w| w.file.clone()).collect();
        let hashes = cache.many(&paths)?;

        let mut files = vec![];
        let mut local = BTreeMap::new();
        let mut used = BTreeMap::new();
        let mut reports: BTreeMap<String, ModReport> = BTreeMap::new();
        for (w, (size, hash)) in winners.iter().zip(hashes) {
            let in_archive = w
                .r#mod
                .as_ref()
                .and_then(|m| sources.get(&fold(m)))
                .and_then(|(a, members)| members.get(&hash).map(|n| (a, n)));
            let from = match in_archive {
                _ if rules::protected(&w.path) => From::Base,
                Some((a, member)) => {
                    used.insert(a.hash.clone(), a.clone());
                    From::Archive {
                        archive: a.hash.clone(),
                        member: member.clone(),
                    }
                }
                None => From::Host,
            };
            if from != From::Base {
                local.insert(hash.clone(), w.file.clone());
                let name = w.r#mod.clone().unwrap_or_else(|| "(game folder)".into());
                let r = reports.entry(name.clone()).or_insert_with(|| ModReport {
                    note: notes.get(&name).cloned(),
                    source: match sources.get(&fold(&name)).map(|s| &s.0.origin) {
                        Some(Origin::Nexus { .. }) => "nexus".into(),
                        Some(Origin::Url { .. }) => "url".into(),
                        None => "host".into(),
                    },
                    name,
                    files: 0,
                    bytes: 0,
                    host_bytes: 0,
                });
                r.files += 1;
                r.bytes += size;
                if from == From::Host {
                    r.host_bytes += size;
                }
            }
            files.push(Entry {
                path: w.path.clone(),
                size,
                hash,
                r#mod: w.r#mod.clone(),
                from,
            });
        }
        let mut ini = pack::Ini::new();
        for (name, path) in &profile.inis {
            let s = ini::shared(path, name)?;
            if !s.is_empty() {
                ini.insert(name.clone(), s);
            }
        }
        let pack = Pack {
            schema: pack::SCHEMA,
            name: b.name.clone(),
            host: self.key()?.public().to_string(),
            plugins: profile.plugins,
            loadorder: profile.loadorder,
            ini,
            archives: used.into_values().collect(),
            files,
        };
        pack.validate()?;
        let mut mods: Vec<ModReport> = reports.into_values().collect();
        mods.sort_by_key(|m| std::cmp::Reverse(m.host_bytes));
        Ok(Draft {
            published: Published { pack, local },
            mods,
        })
    }

    /// Each mod's archive (record, member hash -> member name), and why the others have none.
    #[allow(clippy::type_complexity)]
    fn sources(
        &self,
        b: &Binding,
        profile: &mo2::Profile,
        net: &Net,
        cache: &mut hash::Cache,
    ) -> Result<(
        BTreeMap<String, (Archive, BTreeMap<String, String>)>,
        BTreeMap<String, String>,
    )> {
        let mut notes = BTreeMap::new();
        let mut found: Vec<(String, PathBuf, Origin, String)> = vec![];
        for m in &profile.mods {
            let url = |url: String| -> Result<Option<(PathBuf, Origin, String)>> {
                let name = url.rsplit('/').next().unwrap_or(&url).to_owned();
                let path = self.download(net, &url)?;
                Ok(Some((path, Origin::Url { url }, name)))
            };
            let choice = match b.sources.get(&fold(&m.name)) {
                Some(Source::Host) => None,
                Some(Source::Url { url: u }) => url(u.clone())?,
                Some(Source::Github { repo, asset }) => {
                    url(net.github_asset(repo, asset.as_deref())?)?
                }
                None => match (&m.archive, &m.nexus) {
                    (Some(a), Some((game, id, file))) => Some((
                        a.clone(),
                        Origin::Nexus {
                            game: game.clone(),
                            r#mod: *id,
                            file: *file,
                        },
                        a.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    )),
                    (None, Some(_)) => {
                        notes.insert(
                            m.name.clone(),
                            "its archive is not in MO2's downloads folder".to_owned(),
                        );
                        None
                    }
                    _ => {
                        notes.insert(
                            m.name.clone(),
                            "no Nexus record (installed by hand?)".to_owned(),
                        );
                        None
                    }
                },
            };
            if let Some((path, origin, name)) = choice {
                found.push((fold(&m.name), path, origin, name));
            }
        }
        let paths: Vec<PathBuf> = found.iter().map(|f| f.1.clone()).collect();
        let hashes = cache.many(&paths)?;
        let listed: Vec<Result<Vec<archive::Member>>> = found
            .par_iter()
            .zip(&hashes)
            .map(|(f, (_, h))| self.members(&f.1, h))
            .collect();
        let mut sources = BTreeMap::new();
        for ((key, _, origin, name), ((size, hash), members)) in
            found.into_iter().zip(hashes.into_iter().zip(listed))
        {
            match members {
                Ok(members) => {
                    let members = members.into_iter().map(|m| (m.hash, m.name)).collect();
                    sources.insert(
                        key,
                        (
                            Archive {
                                hash,
                                size,
                                name,
                                origin,
                            },
                            members,
                        ),
                    );
                }
                Err(e) if e.code == "unsupported_archive" => {
                    let real = profile
                        .mods
                        .iter()
                        .find(|m| fold(&m.name) == key)
                        .map(|m| m.name.clone())
                        .unwrap_or(key);
                    notes.insert(real, "RAR archive (unsupported)".to_owned());
                }
                Err(e) => return Err(e),
            }
        }
        Ok((sources, notes))
    }

    /// An archive's members, listed once per archive hash.
    fn members(&self, path: &Path, hash: &str) -> Result<Vec<archive::Member>> {
        let listed = self.dir.join("cache/archives").join(format!("{hash}.json"));
        if let Ok(m) = files::read_json(&listed) {
            return Ok(m);
        }
        crate::event(
            "listing_archive",
            json!({"archive": path.file_name().map(|n| n.to_string_lossy().into_owned())}),
        );
        let m = archive::members(path)?;
        files::write_json(&listed, &m)?;
        Ok(m)
    }

    /// A URL's archive, downloaded once (a release asset's URL names one file for good).
    fn download(&self, net: &Net, url: &str) -> Result<PathBuf> {
        let index = self.dir.join("cache/urls.json");
        let mut known: BTreeMap<String, String> = files::read_json(&index).unwrap_or_default();
        let dir = self.dir.join("cache/downloads");
        if let Some(p) = known.get(url).map(|h| dir.join(h)).filter(|p| p.is_file()) {
            return Ok(p);
        }
        crate::event("downloading", json!({"url": url}));
        let p = net.download(url, &dir, None)?;
        known.insert(
            url.into(),
            p.file_name().unwrap().to_string_lossy().into_owned(),
        );
        files::write_json(&index, &known)?;
        Ok(p)
    }

    pub fn status(&self, net: &Net) -> Result<Value> {
        let d = self.draft(net)?;
        let pack = &d.published.pack;
        Ok(json!({
            "name": pack.name,
            "files": pack.files.len(),
            "archives": pack.archives.len(),
            "bytes": pack.bytes(),
            "host_bytes": d.mods.iter().map(|m| m.host_bytes).sum::<u64>(),
            "mods": d.mods,
        }))
    }

    /// Publishes the profile: what `serve` sends and what host play loads. The code never changes.
    pub fn publish(&self, net: &Net) -> Result<Value> {
        let d = self.draft(net)?;
        files::write_json(&self.dir.join("published.json"), &d.published)?;
        let pack = &d.published.pack;
        Ok(json!({
            "state": "published",
            "code": pack.host,
            "files": pack.files.len(),
            "host_files": pack.files.iter().filter(|e| e.from == From::Host).count(),
            "note": "Friends run `sms play <code>`; keep `sms host serve` running while they sync.",
        }))
    }
}
