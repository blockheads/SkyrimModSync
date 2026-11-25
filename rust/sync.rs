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
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::{Path, PathBuf},
};

const DOWNLOADS: usize = 3;

/// Fetches a pack from wherever `source` names.
pub fn fetch_pack(source: &str, net: &Net, direct: Option<SocketAddr>) -> Result<Pack> {
    let bytes = if let Some(repo) = source.strip_prefix("github:") {
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
    Ok(pack)
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
    let mut cache = store.hashes();
    let plan = deploy::plan(pack, game, &mut cache, |e| {
        store.has(&e.hash, e.size).then(|| store.object(&e.hash))
    })?;
    if let Some(p) = plan.differs.first() {
        return Err(Error::new("game_differs", format!("Your game's {p} differs from the host's: same Skyrim version and edition? Verify the game's files in Steam.")));
    }
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
    let mut failed = vec![];
    for chunk in jobs.chunks(DOWNLOADS) {
        let results: Vec<Result<()>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|(a, es)| s.spawn(|| fetch_archive(a, es, store, net)))
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
                crate::event(
                    "archive_from_host",
                    json!({"archive": a.name, "why": e.message}),
                );
                failed.push(a.name.clone());
                from_host.extend(es.iter().copied());
            }
        }
    }
    let wanted: BTreeSet<(String, u64)> =
        from_host.iter().map(|e| (e.hash.clone(), e.size)).collect();
    let wanted: Vec<(String, u64)> = wanted.into_iter().collect();
    if !wanted.is_empty() {
        crate::event(
            "from_host",
            json!({"files": wanted.len(), "note": "the host must be running `sms host serve`"}),
        );
    }
    peer::fetch_files(&pack.host, direct, store, &wanted)?;
    store.set_pack(pack)?;
    Ok(json!({
        "state": "synced",
        "pack": pack.name,
        "files_fetched": plan.missing.len(),
        "archives": jobs.len() - failed.len(),
        "archives_from_host": failed,
        "host_files": wanted.len(),
        "host_bytes": wanted.iter().map(|w| w.1).sum::<u64>(),
    }))
}

/// One archive: from the store's downloads, else downloaded; the needed members into the store.
fn fetch_archive(a: &Archive, entries: &[&Entry], store: &Store, net: &Net) -> Result<()> {
    let cached: PathBuf = store.downloads().join(&a.hash);
    let path = if cached.is_file() {
        cached
    } else {
        let url = match &a.origin {
            Origin::Url { url } => url.clone(),
            Origin::Nexus { game, r#mod, file } => net.nexus_link(game, *r#mod, *file)?,
        };
        crate::event("downloading", json!({"archive": a.name, "size": a.size}));
        net.download(&url, &store.downloads(), Some((a.size, &a.hash)))?
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
