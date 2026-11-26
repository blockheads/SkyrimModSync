//! Sync: the store gets every pack file the game lacks. Archives come from Nexus (a Premium key) or
//! their URL, three at a time, keeping only the files needed; anything else, or any archive that fails,
//! comes from the host.
use crate::{
    archive,
    deploy::{self, Game},
    files,
    net::Net,
    pack::{Archive, Entry, From, Origin, Pack},
    peer::{self, Published},
    store::Store,
    Error, Result,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
};

const DOWNLOADS: usize = 3;

/// What a player pastes, as a source: a bare gist id or gist link becomes `gist:ID`.
pub fn source(input: &str) -> String {
    let s = input.trim();
    let id = s
        .strip_prefix("https://gist.github.com/")
        .map(|rest| rest.trim_end_matches('/').rsplit('/').next().unwrap_or(""))
        .unwrap_or(s);
    let hex = |t: &str| matches!(t.len(), 20 | 32) && t.chars().all(|c| c.is_ascii_hexdigit());
    if hex(id) {
        format!("gist:{id}")
    } else {
        s.to_owned()
    }
}

/// Fetches a pack from wherever `source` names.
pub fn fetch_pack(source: &str, net: &Net, direct: Option<SocketAddr>) -> Result<Pack> {
    let source = &self::source(source);
    let from = if let Some(id) = source.strip_prefix("gist:") {
        format!("gist {id}")
    } else if let Some(repo) = source.strip_prefix("github:") {
        format!("the {repo} release")
    } else if source.contains("://") || Path::new(source).is_file() {
        source.clone()
    } else {
        "the host".into()
    };
    crate::say(
        "fetching_pack",
        &format!("Getting the pack from {from}..."),
        json!({"source": source}),
    );
    let bytes = if let Some(id) = source.strip_prefix("gist:") {
        net.gist_file(id, "pack.json")?
    } else if let Some(repo) = source.strip_prefix("github:") {
        net.get(&net.github_asset(repo, Some("pack.json"))?)?
    } else if source.starts_with("https://") || source.starts_with("http://") {
        net.get(source)?
    } else if Path::new(source).is_file() {
        std::fs::read(source)?
    } else {
        return peer::fetch_pack(source, direct);
    };
    let pack: Pack = serde_json::from_slice(&bytes)?;
    pack.validate()?;
    said_pack(&pack);
    Ok(pack)
}

fn said_pack(pack: &Pack) {
    crate::say(
        "pack",
        &format!(
            "Pack \"{}\": {} files, {} plugins.",
            pack.name,
            pack.files.len(),
            pack.plugins.len()
        ),
        json!({"pack": pack.name, "files": pack.files.len()}),
    );
}

fn comparing() {
    crate::say(
        "comparing",
        "Comparing your game and downloads with the pack...",
        json!({}),
    );
}

/// One mod's files the store still needs.
#[derive(Debug, Serialize)]
pub struct ModChange {
    pub name: String,
    pub files: usize,
    pub bytes: u64,
    /// "nexus", "download" (a URL) or "host".
    pub from: &'static str,
}

/// A licensed game file the host sends.
#[derive(Debug, Serialize)]
pub struct GameFile {
    pub path: String,
    pub bytes: u64,
    /// "missing" or "different" (another version).
    pub state: &'static str,
}

/// What a player's game and store lack for a pack: shown before anything downloads.
#[derive(Debug, Serialize)]
pub struct Check {
    pub pack: String,
    pub mods: Vec<ModChange>,
    pub game_files: Vec<GameFile>,
    /// About what downloads: whole archives not cached yet, and the host's files.
    pub download_bytes: u64,
    /// The player's own Data/ files moved aside while the pack is on.
    pub set_aside: Vec<String>,
}

impl Check {
    pub fn needs_download(&self) -> bool {
        !self.mods.is_empty() || !self.game_files.is_empty()
    }
}

fn differs(plan: &deploy::Plan) -> Result<()> {
    match plan.differs.first() {
        Some(p) => Err(Error::new("game_differs", format!("Your game's {p} differs from the host's: same Skyrim version and edition? Verify the game's files in Steam."))),
        None => Ok(()),
    }
}

/// Lists what is out of sync, downloading nothing.
pub fn check(pack: &Pack, game: &Game, store: &Store) -> Result<Check> {
    comparing();
    let plan = deploy::plan(pack, game, &mut store.hashes(), |e| {
        store.has(&e.hash, e.size).then(|| store.object(&e.hash))
    })?;
    differs(&plan)?;
    let changed: BTreeSet<String> = plan.changed.iter().map(|p| files::fold(p)).collect();
    let mut mods: Vec<ModChange> = vec![];
    let mut game_files = vec![];
    let mut archives = BTreeSet::new();
    let mut download_bytes = 0;
    for e in &plan.missing {
        let from = match &e.from {
            From::Base => {
                game_files.push(GameFile {
                    path: e.path.clone(),
                    bytes: e.size,
                    state: if changed.contains(&files::fold(&e.path)) {
                        "different"
                    } else {
                        "missing"
                    },
                });
                download_bytes += e.size;
                continue;
            }
            From::Host => {
                download_bytes += e.size;
                "host"
            }
            From::Archive { archive, .. } => match pack.archive(archive) {
                Some(a) => {
                    if archives.insert(a.hash.clone()) && !store.downloads().join(&a.hash).is_file()
                    {
                        download_bytes += a.size;
                    }
                    match a.origin {
                        Origin::Nexus { .. } => "nexus",
                        Origin::Url { .. } => "download",
                    }
                }
                None => "host",
            },
        };
        let name = e.r#mod.clone().unwrap_or_else(|| "(other)".into());
        match mods.iter_mut().find(|m| m.name == name) {
            Some(m) => {
                m.files += 1;
                m.bytes += e.size;
            }
            None => mods.push(ModChange {
                name,
                files: 1,
                bytes: e.size,
                from,
            }),
        }
    }
    let set_aside = plan
        .ops
        .iter()
        .filter_map(|op| match op {
            deploy::Op::Aside { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    Ok(Check {
        pack: pack.name.clone(),
        mods,
        game_files,
        download_bytes,
        set_aside,
    })
}

/// `own`: the host syncing its own pack, every file taken from this machine.
pub fn sync(
    pack: &Pack,
    game: &Game,
    store: &Store,
    net: &Net,
    direct: Option<SocketAddr>,
    own: Option<&Published>,
) -> Result<Value> {
    let _lock = files::Lock::take(&store.root.join("lock"))?;
    comparing();
    let mut cache = store.hashes();
    let plan = deploy::plan(pack, game, &mut cache, |e| {
        store.has(&e.hash, e.size).then(|| store.object(&e.hash))
    })?;
    differs(&plan)?;
    if let Some(own) = own {
        for e in &plan.missing {
            let path = own.file(&e.hash)?.ok_or_else(|| {
                Error::new(
                    "not_published",
                    format!("{} is not in your published files: publish again.", e.path),
                )
            })?;
            store.take(&path, &e.hash)?;
        }
        store.set_pack(pack)?;
        return Ok(
            json!({"state": "synced", "pack": pack.name, "files_local": plan.missing.len()}),
        );
    }
    let mut by_archive: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    let mut from_host: Vec<&Entry> = vec![];
    for e in &plan.missing {
        match &e.from {
            From::Archive { archive, .. } => by_archive.entry(archive).or_default().push(e),
            _ => from_host.push(e),
        }
    }
    let jobs: Vec<(&Archive, Vec<&Entry>)> = by_archive
        .into_iter()
        .filter_map(|(h, es)| pack.archive(h).map(|a| (a, es)))
        .collect();
    let to_download: u64 = jobs
        .iter()
        .filter(|(a, _)| !store.downloads().join(&a.hash).is_file())
        .map(|(a, _)| a.size)
        .sum();
    let meter = (to_download > 0).then(|| crate::Meter::new("Downloading mods", to_download));
    let mut failed = vec![];
    for chunk in jobs.chunks(DOWNLOADS) {
        let results: Vec<Result<()>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(a, es)| s.spawn(|| fetch_archive(a, es, store, net, meter.as_ref())))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(Error::new("crashed", "A download thread failed.")))
                })
                .collect()
        });
        for ((a, es), r) in chunk.iter().zip(results) {
            if let Err(e) = r {
                crate::say(
                    "archive_from_host",
                    &format!("{}: {} The host sends it instead.", a.name, e.message),
                    json!({"archive": a.name, "why": e.message}),
                );
                failed.push(a.name.clone());
                from_host.extend(es.iter().copied());
            }
        }
    }
    if let Some(m) = &meter {
        m.finish();
    }
    let wanted: BTreeSet<(String, u64)> =
        from_host.iter().map(|e| (e.hash.clone(), e.size)).collect();
    let wanted: Vec<(String, u64)> = wanted.into_iter().collect();
    let host_bytes: u64 = wanted.iter().map(|w| w.1).sum();
    peer::fetch_files(&pack.host, direct, store, &wanted)?;
    store.set_pack(pack)?;
    Ok(json!({
        "state": "synced",
        "pack": pack.name,
        "files_fetched": plan.missing.len(),
        "archives": jobs.len() - failed.len(),
        "archives_from_host": failed,
        "host_files": wanted.len(),
        "host_bytes": host_bytes,
    }))
}

/// One archive: from the store's downloads, else downloaded; the needed members into the store.
fn fetch_archive(
    a: &Archive,
    entries: &[&Entry],
    store: &Store,
    net: &Net,
    meter: Option<&crate::Meter>,
) -> Result<()> {
    let cached: PathBuf = store.downloads().join(&a.hash);
    let path = if cached.is_file() {
        cached
    } else {
        let (url, from) = match &a.origin {
            Origin::Url { url } => (url.clone(), "its link"),
            Origin::Nexus { game, r#mod, file } => (net.nexus_link(game, *r#mod, *file)?, "Nexus"),
        };
        crate::say(
            "downloading",
            &format!(
                "Downloading {} ({}) from {from}",
                a.name,
                crate::size(a.size)
            ),
            json!({"archive": a.name, "size": a.size}),
        );
        net.download(&url, &store.downloads(), Some((a.size, &a.hash)), meter)?
    };
    let members: BTreeMap<String, &str> = entries
        .iter()
        .filter_map(|e| match &e.from {
            From::Archive { member, .. } => Some((files::fold(member), e.hash.as_str())),
            _ => None,
        })
        .collect();
    let wanted: BTreeSet<String> = members.keys().cloned().collect();
    archive::extract(&path, &wanted, |name, r| {
        store.put(r, members[&files::fold(name)])
    })
}
