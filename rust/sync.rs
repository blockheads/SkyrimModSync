//! Sync: making the store hold every file of a pack the friend's game lacks. Archives come from Nexus
//! (the friend's Premium key) or their URL, only the needed members are kept; the rest from the host.
use crate::{
    archive,
    channel::{self, Channel, Head},
    config, deploy, hashes, net, nexus,
    pack::{Origin, Pack, Source},
    peer,
    store::{Current, Store},
    Error, Result,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::Path,
};

/// Where a pack comes from: a host's code, or a pack file.
pub fn resolve(
    code_or_file: &str,
    store: &Store,
    direct: Option<SocketAddr>,
) -> Result<(Pack, Head)> {
    let path = Path::new(code_or_file);
    if path.is_file() {
        let (pack, head) = channel::read_file(path)?;
        store.accept(&head)?;
        return Ok((pack, head));
    }
    let channel = Channel::parse(code_or_file.trim())?;
    peer::fetch_pack(&channel, store, None, direct)
}

/// Fetches what the store and the game lack for `pack`; on success it becomes the store's pack.
pub fn sync(
    pack: &Pack,
    head: &Head,
    store: &Store,
    game: &deploy::Game,
    direct: Option<SocketAddr>,
    local: Option<&BTreeMap<String, std::path::PathBuf>>,
) -> Result<Value> {
    let _lock = crate::files::Lock::take(&store.root.join("locks/sync.lock"))?;
    let mut cache = hashes::Cache::open(&store.root.join("hashes.json"))?;
    let originals = game.originals()?;
    let mut base_differs = vec![];
    let mut need = vec![];
    for e in &pack.files {
        let original = match originals.get(&e.path)? {
            Some(p) => cache.holds(&p, &e.sha256)?,
            None => false,
        };
        if original {
            continue;
        }
        if e.source == Source::Base {
            base_differs.push(e.path.clone());
        } else if !store.has(&e.sha256, e.size) {
            need.push(e);
        }
    }
    cache.save()?;
    if !base_differs.is_empty() {
        return Err(Error::new(
            "game_differs",
            format!(
                "Your game's own files differ from the host's ({} files, first: {}). Check you both have the same Skyrim version and edition (Anniversary content), then verify the game's files in Steam.",
                base_differs.len(),
                base_differs[0]
            ),
        ));
    }

    // The host playing its own pack: every file is already on this machine.
    let mut from_local = 0;
    if let Some(local) = local {
        for e in &need {
            if let Some(path) = local.get(&e.sha256) {
                store.take_local(path, &e.sha256, e.size)?;
                from_local += 1;
            }
        }
    }
    let need = need
        .into_iter()
        .filter(|e| !store.has(&e.sha256, e.size))
        .collect::<Vec<_>>();
    let mut by_archive: BTreeMap<&str, BTreeMap<String, (&str, u64)>> = BTreeMap::new();
    let mut from_host = vec![];
    for e in &need {
        match &e.source {
            Source::Archive { archive, member } => {
                by_archive
                    .entry(archive)
                    .or_default()
                    .insert(member.clone(), (&e.sha256, e.size));
            }
            Source::Host { blob } => from_host.push((blob.clone(), e.sha256.clone(), e.size)),
            Source::Base => {}
        }
    }
    let nexus_needed = by_archive
        .keys()
        .filter_map(|a| pack.archive(a))
        .filter(|a| {
            matches!(a.origin, Origin::Nexus { .. }) && !store.downloads().join(&a.sha256).is_file()
        })
        .collect::<Vec<_>>();
    let key = config::nexus_key()?;
    if !nexus_needed.is_empty() && key.is_none() {
        return Err(Error::new(
            "nexus_key_required",
            format!(
                "{} archives come from Nexus (first: {}). Run `sms nexus-login` with a Premium account's API key, or ask the host to send them (`sms host source <mod> host`).",
                nexus_needed.len(),
                nexus_needed[0].name
            ),
        ));
    }
    let client = net::client()?;
    let mut downloaded = 0u64;
    for (sha, members) in &by_archive {
        let a = pack
            .archive(sha)
            .ok_or_else(|| Error::new("invalid_pack", "Unknown archive."))?;
        crate::event("archive", json!({"name": a.name, "size": a.size}));
        let cached = store.downloads().join(&a.sha256);
        let path = if cached.is_file() {
            cached
        } else {
            let url = match &a.origin {
                Origin::Url { url } => url.clone(),
                Origin::Nexus { game, r#mod, file } => {
                    nexus::download_link(key.as_deref().unwrap(), game, *r#mod, *file)?
                }
            };
            downloaded += a.size;
            net::download(&client, &url, &store.downloads(), Some((a.size, &a.sha256)))?
        };
        let wanted = members.keys().cloned().collect::<BTreeSet<_>>();
        let folded = members
            .iter()
            .map(|(m, v)| (crate::files::fold(m), *v))
            .collect::<BTreeMap<_, _>>();
        archive::extract(&path, &wanted, |name, r| {
            let (sha256, size) = folded[&crate::files::fold(name)];
            store.put(r, sha256, size)
        })?;
    }
    let host_bytes: u64 = from_host.iter().map(|(_, _, s)| s).sum();
    if !from_host.is_empty() {
        peer::fetch_objects(&head.channel, store, &from_host, direct)?;
    }
    store.set_current(&Current {
        pack: pack.clone(),
        head: head.clone(),
    })?;
    Ok(json!({
        "state": "synced",
        "pack": pack.name,
        "revision": head.revision,
        "sequence": head.sequence,
        "files_fetched": need.len(),
        "files_local": from_local,
        "archives": by_archive.len(),
        "downloaded_bytes": downloaded,
        "host_files": from_host.len(),
        "host_bytes": host_bytes,
    }))
}
