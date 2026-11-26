//! The host: a mod list resolved into a pack. Each mod's archive is downloaded once and listed by hash;
//! the pack names every installed file and the archive member it comes from.
use crate::{
    archive, config,
    deploy::{self, Game},
    files::{self, fold},
    hash,
    list::{self, ModList},
    net::Net,
    pack::{self, Archive, Entry, From, Origin, Pack},
    peer::{self, Published},
    rules, Error, Result,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const MASTERS: [&str; 5] = [
    "Skyrim.esm",
    "Update.esm",
    "Dawnguard.esm",
    "HearthFires.esm",
    "Dragonborn.esm",
];

/// One mod's line in what `publish` reports.
#[derive(Debug, Serialize)]
pub struct ModReport {
    pub name: String,
    pub source: String,
    pub files: usize,
    pub bytes: u64,
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
    /// The secret gist this host shares its pack in.
    pub fn gist(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("gist"))
            .ok()
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty())
    }
    pub fn set_gist(&self, id: &str) -> Result<()> {
        files::write_atomic(&self.dir.join("gist"), id.as_bytes())
    }
    pub fn downloads(&self) -> PathBuf {
        self.dir.join("cache/downloads")
    }

    /// Resolves the list against `game` (the host's own Skyrim folder, for the licensed files).
    pub fn resolve(
        &self,
        list: &ModList,
        list_dir: &Path,
        game: &Path,
        net: &Net,
    ) -> Result<(Published, Vec<ModReport>)> {
        let game = files::root(game)?;
        let folder = Game {
            root: game.clone(),
            local: PathBuf::new(),
            documents: PathBuf::new(),
        };
        if folder.state() != deploy::State::Clean {
            return Err(Error::new(
                "pack_loaded",
                "Your game folder has a pack loaded: `sms disable` first.",
            ));
        }
        let mut cache = hash::Cache::open(&self.dir.join("cache/hashes.json"));
        let mut chosen: BTreeMap<String, Entry> = BTreeMap::new();
        let base: Vec<(String, PathBuf)> = files::walk(&game)?
            .into_iter()
            .filter(|(rel, _)| rules::protected(rel) && !rules::stays(rel))
            .collect();
        let hashes = cache.many(&base.iter().map(|b| b.1.clone()).collect::<Vec<_>>())?;
        let mut local = BTreeMap::new();
        for ((path, real), (size, hash)) in base.into_iter().zip(hashes) {
            if rules::host_sends(&path) {
                local.insert(hash.clone(), real);
            }
            chosen.insert(
                fold(&path),
                Entry {
                    path,
                    size,
                    hash,
                    r#mod: None,
                    from: From::Base,
                },
            );
        }

        // Every archive first, so the hand downloads still needed are listed together.
        let mut fetched = BTreeMap::new();
        let mut missing = vec![];
        for (i, m) in list
            .mods
            .iter()
            .enumerate()
            .filter(|(_, m)| m.local.is_none())
        {
            match self.archive(m, net, list_dir) {
                Ok(a) => {
                    fetched.insert(i, a);
                }
                Err(e) if e.code == "manual_download" => missing.push(e.message),
                Err(e) => return Err(e),
            }
        }
        if !missing.is_empty() {
            return Err(Error::new(
                "manual_download",
                format!(
                    "Without Nexus Premium, download these by hand into {} and publish again:\n  {}",
                    list_dir.join("downloads").display(),
                    missing.join("\n  ")
                ),
            ));
        }
        let mut members = BTreeMap::new();
        let mut archives = BTreeMap::new();
        let mut reports = vec![];
        for (i, m) in list.mods.iter().enumerate() {
            // (path inside the mod, size, hash, where the bytes are on this machine, the archive)
            let mut found: Vec<(String, u64, String, From)> = vec![];
            let source;
            if let Some(dir) = &m.local {
                source = "host".to_owned();
                let walked = files::walk(&list_dir.join(dir))?;
                let paths: Vec<PathBuf> = walked.values().cloned().collect();
                for ((rel, path), (size, hash)) in walked.into_iter().zip(cache.many(&paths)?) {
                    local.insert(hash.clone(), path);
                    found.push((rel, size, hash, From::Host));
                }
            } else {
                let (path, origin, name) = fetched.remove(&i).expect("fetched above");
                source = match &origin {
                    Origin::Nexus { .. } => "nexus".to_owned(),
                    Origin::Url { .. } => "url".to_owned(),
                };
                let (size, hash) = cache.one(&path)?;
                for member in self.members(&path, &hash)? {
                    members.insert(member.hash.clone(), (path.clone(), member.name.clone()));
                    let from = From::Archive {
                        archive: hash.clone(),
                        member: member.name.clone(),
                    };
                    found.push((member.name, member.size, member.hash, from));
                }
                archives.insert(
                    hash.clone(),
                    Archive {
                        hash,
                        size,
                        name,
                        origin,
                    },
                );
            }
            let data = list::has_data(m, found.iter().map(|f| f.0.as_str()));
            let mut report = ModReport {
                name: m.name.clone(),
                source,
                files: 0,
                bytes: 0,
            };
            for (rel, size, hash, from) in found {
                let Some(path) = list::target(m, &rel, data) else {
                    continue;
                };
                if rules::ignored(&path) {
                    continue;
                }
                if rules::protected(&path) || rules::executable(&path) {
                    return Err(Error::new(
                        "invalid_list",
                        format!("{} would replace the game's {path}.", m.name),
                    ));
                }
                report.files += 1;
                report.bytes += size;
                chosen.insert(
                    fold(&path),
                    Entry {
                        path,
                        size,
                        hash,
                        r#mod: Some(m.name.clone()),
                        from,
                    },
                );
            }
            if report.files == 0 {
                return Err(Error::new(
                    "invalid_list",
                    format!("{} installs nothing: check its folder and root.", m.name),
                ));
            }
            reports.push(report);
        }

        let used: std::collections::BTreeSet<&str> = chosen
            .values()
            .filter_map(|e| match &e.from {
                From::Archive { archive, .. } => Some(archive.as_str()),
                _ => None,
            })
            .collect();
        let plugins: Vec<String> = list
            .plugins
            .iter()
            .map(|p| p.trim_start_matches('*').to_owned())
            .collect();
        let masters = MASTERS
            .iter()
            .filter(|m| chosen.contains_key(&fold(&format!("Data/{m}"))))
            .map(|m| m.to_string());
        let pack = Pack {
            schema: pack::SCHEMA,
            name: list.name.clone(),
            host: self.key()?.public().to_string(),
            loadorder: masters.chain(plugins.iter().cloned()).collect(),
            plugins: plugins.iter().map(|p| format!("*{p}")).collect(),
            ini: list.ini.clone(),
            archives: archives
                .into_values()
                .filter(|a| used.contains(a.hash.as_str()))
                .collect(),
            files: chosen.into_values().collect(),
        };
        pack.validate()?;
        members.retain(|h, _| pack.files.iter().any(|e| &e.hash == h));
        local.retain(|h, _| {
            pack.files
                .iter()
                .any(|e| &e.hash == h && matches!(e.from, From::Host | From::Base))
        });
        let published = Published {
            pack,
            local,
            members,
            extracted: self.dir.join("cache/extracted"),
        };
        Ok((published, reports))
    }

    /// A mod's archive: downloaded once, remembered by source. Without Nexus Premium, a Nexus archive
    /// is taken from `downloads/` next to the list, where the host saved it by hand.
    fn archive(
        &self,
        m: &list::Mod,
        net: &Net,
        list_dir: &Path,
    ) -> Result<(PathBuf, Origin, String)> {
        let (key, origin) = match (&m.nexus, &m.url, &m.github) {
            (Some(n), _, _) => (
                format!("nexus:{}/{}/{}", n.game, n.r#mod, n.file),
                Origin::Nexus {
                    game: n.game.clone(),
                    r#mod: n.r#mod,
                    file: n.file,
                },
            ),
            (_, Some(u), _) => (u.clone(), Origin::Url { url: u.clone() }),
            (_, _, Some(g)) => {
                let url = net.github_asset(&g.repo, g.asset.as_deref())?;
                (url.clone(), Origin::Url { url })
            }
            _ => unreachable!("validated by ModList::read"),
        };
        let index = self.dir.join("cache/sources.json");
        let mut known: BTreeMap<String, String> = files::read_json(&index).unwrap_or_default();
        if let Some(p) = known
            .get(&key)
            .map(|h| self.downloads().join(h))
            .filter(|p| p.is_file())
        {
            let name = known
                .get(&format!("name:{key}"))
                .cloned()
                .unwrap_or_else(|| key.clone());
            return Ok((p, origin, name));
        }
        let (path, name) = match &origin {
            Origin::Nexus { game, r#mod, file } => match net.nexus_link(game, *r#mod, *file) {
                Ok(url) => {
                    crate::event("downloading", json!({"mod": m.name}));
                    (
                        net.download(&url, &self.downloads(), None)?,
                        file_name(&url),
                    )
                }
                Err(e) if e.code == "nexus_not_premium" || e.code == "nexus_key_required" => {
                    let name = net
                        .nexus_file_name(game, *r#mod, *file)
                        .unwrap_or_else(|_| format!("the file {file} of mod {mod}"));
                    let manual = list_dir.join("downloads").join(&name);
                    if !manual.is_file() {
                        return Err(Error::new(
                            "manual_download",
                            format!("{name}\n    from https://www.nexusmods.com/{game}/mods/{mod}?tab=files&file_id={file}"),
                        ));
                    }
                    (self.keep(&manual)?, name)
                }
                Err(e) => return Err(e),
            },
            Origin::Url { url } => {
                crate::event("downloading", json!({"mod": m.name}));
                (net.download(url, &self.downloads(), None)?, file_name(url))
            }
        };
        known.insert(
            key.clone(),
            path.file_name().unwrap().to_string_lossy().into_owned(),
        );
        known.insert(format!("name:{key}"), name.clone());
        files::write_json(&index, &known)?;
        Ok((path, origin, name))
    }

    /// A hand-downloaded archive in the downloads cache under its hash (linked, else copied).
    fn keep(&self, path: &Path) -> Result<PathBuf> {
        let (_, hash) = hash::file(path)?;
        let dest = self.downloads().join(&hash);
        if !dest.is_file() {
            std::fs::create_dir_all(self.downloads())?;
            if std::fs::hard_link(path, &dest).is_err() {
                std::fs::copy(path, &dest)?;
            }
        }
        Ok(dest)
    }

    /// An archive's members, listed once per archive hash.
    fn members(&self, path: &Path, hash: &str) -> Result<Vec<archive::Member>> {
        let listed = self.dir.join("cache/archives").join(format!("{hash}.json"));
        if let Ok(m) = files::read_json(&listed) {
            return Ok(m);
        }
        let m = archive::members(path)?;
        files::write_json(&listed, &m)?;
        Ok(m)
    }

    /// Resolves and publishes the list: what `serve` sends and what host play loads.
    pub fn publish(
        &self,
        list_path: &Path,
        game: &Path,
        net: &Net,
        output: Option<&Path>,
    ) -> Result<Value> {
        let list = ModList::read(list_path)?;
        let dir = list_path.parent().unwrap_or(Path::new("."));
        let (published, mods) = self.resolve(&list, dir, game, net)?;
        files::write_json(&self.dir.join("published.json"), &published)?;
        if let Some(out) = output {
            files::write_atomic(out, &serde_json::to_vec_pretty(&published.pack)?)?;
        }
        let pack = &published.pack;
        Ok(json!({
            "state": "published",
            "code": pack.host,
            "pack_file": output,
            "files": pack.files.len(),
            "bytes": pack.bytes(),
            "from_host": pack.files.iter().filter(|e| e.from == From::Host).count(),
            "mods": mods,
        }))
    }

    /// Adds a Nexus mod to a list from the link on its Files tab.
    pub fn add(&self, list_path: &Path, link: &str, net: &Net) -> Result<Value> {
        let (game, r#mod, file) = list::parse_nexus_link(link).ok_or_else(|| {
            Error::new(
                "invalid_link",
                "Use the link of one file (Files tab → the file's name), with file_id= in it.",
            )
        })?;
        let mut list = if list_path.exists() {
            ModList::read(list_path)?
        } else {
            ModList {
                name: "My pack".into(),
                mods: vec![],
                plugins: vec![],
                ini: Default::default(),
            }
        };
        let name = net.nexus_mod_name(&game, r#mod)?;
        list.mods.push(list::Mod {
            name: name.clone(),
            nexus: Some(list::Nexus { game, r#mod, file }),
            url: None,
            github: None,
            local: None,
            folder: None,
            root: vec![],
        });
        list.write(list_path)?;
        Ok(
            json!({"state": "added", "mod": name, "note": "Add its plugins to \"plugins\" if it has any."}),
        )
    }
}

fn file_name(url: &str) -> String {
    url.split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or(url)
        .to_owned()
}
