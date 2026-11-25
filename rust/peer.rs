//! Host-only content service. Channel heads are signed; blob storage is read-only
//! over the wire and contains only objects reachable from published revisions.
use crate::{
    files,
    revision::{self, Channel, Entry, Head, Revision, Source},
    target::Storage,
    Error, Result,
};

use iroh::{
    endpoint::{presets, Connection, RelayMode},
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr,
};

use iroh_blobs::{store::fs::FsStore, BlobsProtocol};

use serde::{Deserialize, Serialize};

use serde_json::{json, Value};

use std::{
    collections::BTreeSet,
    fs,
    future::IntoFuture,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const ALPN: &[u8] = b"skyrim-mod-sync/channel/3";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    pack: String,
    revision: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub head: Head,
    pub revision: Revision,
    pub recipe_blob: Option<String>,
}
fn failure(_: impl std::fmt::Display) -> Error {
    Error::new("peer_unavailable","Peer transfer failed. Keep the host's serve command running, check connectivity/relay settings and retry. Verified partial downloads are retained.")
}
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(Into::into)
}
#[derive(Debug, Clone)]
struct Control {
    root: PathBuf,
    channel: Channel,
    store: FsStore,
}
impl Control {
    async fn reply(&self, request: Request) -> Result<Reply> {
        if request.pack != self.channel.pack {
            return Err(Error::new(
                "channel_mismatch",
                "Requested pack is not published by this endpoint.",
            ));
        }
        let path = if let Some(id) = request.revision {
            if !files::digest_valid(&id) {
                return Err(Error::new(
                    "invalid_revision",
                    "Invalid requested revision.",
                ));
            }
            self.root.join("published").join(format!("{id}.json"))
        } else {
            self.root.join("channel-head.json")
        };

        let head: Head = files::read_json(&path)?;

        head.validate()?;

        if head.channel.pack != self.channel.pack
            || head.channel.publisher != self.channel.publisher
        {
            return Err(Error::new(
                "channel_mismatch",
                "Publisher identity changed.",
            ));
        }
        let revision = revision::load(
            &self
                .root
                .join("revisions")
                .join(format!("{}.json", head.revision)),
        )?;

        if revision.id()? != head.revision {
            return Err(Error::new(
                "invalid_revision",
                "Published manifest differs.",
            ));
        }
        let recipe_blob = if let Some(a) = &revision.recipe {
            Some(revision::freeze(
                &self.root,
                &self
                    .root
                    .join("recipes")
                    .join(format!("{}.wabbajack", a.sha256)),
            )?)
        } else {
            None
        };

        let mut objects = revision.objects();

        objects.extend(recipe_blob.clone());

        for hash in objects {
            let parsed = hash.parse::<iroh_blobs::Hash>().map_err(failure)?;
            if self.store.blobs().has(parsed).await.map_err(failure)? {
                continue;
            }
            let path = revision::object_path(&self.root, &hash)?;

            if revision::blob_hash(&path)? != hash {
                return Err(Error::new("object_corrupt", "Published object differs."));
            }
            let tag = self.store.blobs().add_path(path).await.map_err(failure)?;

            if tag.hash.to_string() != hash {
                return Err(Error::new("object_corrupt", "Stored peer object differs."));
            }
            self.store
                .tags()
                .set(hash.as_bytes(), tag.hash)
                .await
                .map_err(failure)?;
        }
        Ok(Reply {
            head,
            revision,
            recipe_blob,
        })
    }
}
impl ProtocolHandler for Control {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;

        let raw = recv
            .read_to_end(16384)
            .await
            .map_err(AcceptError::from_err)?;

        let request: Request = serde_json::from_slice(&raw).map_err(AcceptError::from_err)?;

        let reply = self.reply(request).await.map_err(AcceptError::from_err)?;

        let bytes = serde_json::to_vec(&reply).map_err(AcceptError::from_err)?;

        if bytes.len() > files::MAX_JSON as usize {
            return Err(AcceptError::from_err(Error::new(
                "invalid_revision",
                "Revision control response exceeds limit.",
            )));
        }
        send.write_all(&bytes)
            .await
            .map_err(AcceptError::from_err)?;

        send.finish()?;

        connection.closed().await;

        Ok(())
    }
}
pub fn serve(
    storage: &Storage,
    no_relay: bool,
    bind: Option<std::net::SocketAddr>,
    relay: Option<&str>,
) -> Result<Value> {
    let _lock = files::Lock::take(&storage.root.join("locks/peer-server.lock"))?;

    let mut private = crate::authoring::private_channel(storage)?;

    if let Some(r) = relay {
        r.parse::<iroh::RelayUrl>()
            .map_err(|_| Error::new("invalid_relay", "Invalid relay URL."))?;

        private.relay = Some(r.into());

        files::atomic_json(&storage.root.join("channel-private.json"), &private)?;
    }
    let channel = crate::authoring::channel(&private)?;

    let head: Head = files::read_json(&storage.root.join("channel-head.json"))?;

    head.validate()?;

    if head.channel.publisher != channel.publisher || head.channel.pack != channel.pack {
        return Err(Error::new(
            "channel_mismatch",
            "Private key does not match the published channel.",
        ));
    }
    runtime()?.block_on(async {let mut builder=Endpoint::builder(presets::N0).secret_key(iroh::SecretKey::from_bytes(&revision::decode32(&private.endpoint_key)?));
if no_relay{builder=builder.relay_mode(RelayMode::Disabled);
}else if let Some(r)=&private.relay{builder=builder.relay_mode(RelayMode::custom([r.parse::<iroh::RelayUrl>().map_err(failure)?]));
}
if let Some(addr)=bind{builder=builder.bind_addr(addr).map_err(failure)?;
}
let endpoint=builder.bind().await.map_err(failure)?;
let store=FsStore::load(storage.root.join("peer/server")).await.map_err(failure)?;
let control=Control{root:storage.root.clone(),channel:channel.clone(),store:store.clone()};
control.reply(Request{pack:channel.pack.clone(),revision:None}).await?;
let router=Router::builder(endpoint).accept(ALPN,control).accept(iroh_blobs::ALPN,BlobsProtocol::new(&store,None)).spawn();
let code=channel.code()?;
println!("{}",json!({"state":"serving","code":code,"addresses":router.endpoint().addr(),"sequence":head.sequence}));
std::io::stdout().flush()?;
while crate::check_cancel().is_ok(){tokio::time::sleep(Duration::from_millis(200)).await;
}router.shutdown().await.map_err(failure)?;
Ok(json!({"state":"stopped","code":code}))})
}
async fn client_endpoint(
    channel: &Channel,
    address: Option<std::net::SocketAddr>,
) -> Result<(Endpoint, EndpointAddr)> {
    let mut builder = Endpoint::builder(presets::N0);

    if address.is_some() {
        builder = builder.relay_mode(RelayMode::Disabled);
    }
    let endpoint = builder.bind().await.map_err(failure)?;

    let id = channel
        .endpoint
        .parse::<iroh::EndpointId>()
        .map_err(failure)?;

    let mut addr = EndpointAddr::new(id);

    if let Some(r) = &channel.relay {
        addr = addr.with_relay_url(r.parse().map_err(failure)?);
    }
    if let Some(a) = address {
        addr = addr.with_ip_addr(a);
    }
    Ok((endpoint, addr))
}
async fn wait<T, E: std::fmt::Display>(
    future: impl std::future::Future<Output = std::result::Result<T, E>>,
    phase: &str,
) -> Result<T> {
    tokio::pin!(future);

    let mut heartbeat = Instant::now();

    loop {
        tokio::select! {r=&mut future=>return r.map_err(failure),_=tokio::time::sleep(Duration::from_millis(200))=>{crate::check_cancel()?;
        if heartbeat.elapsed()>Duration::from_secs(20){crate::event(phase,json!({"status":"waiting_or_transferring"}));
        heartbeat=Instant::now();
        }}}
    }
}
pub fn fetch(
    channel: &Channel,
    storage: &Storage,
    pin: Option<&str>,
    address: Option<std::net::SocketAddr>,
) -> Result<Reply> {
    channel.validate()?;
    runtime()?.block_on(async {
        let (endpoint, addr) = client_endpoint(channel, address).await?;
        let store = FsStore::load(storage.root.join("peer/client")).await.map_err(failure)?;
        let result = async {
            let conn = wait(
                tokio::time::timeout(Duration::from_secs(30), endpoint.connect(addr, ALPN)),
                "peer_connecting",
            ).await?.map_err(failure)?;
            let (mut send, mut recv) = conn.open_bi().await.map_err(failure)?;
            send.write_all(&serde_json::to_vec(&Request {
                pack: channel.pack.clone(), revision: pin.map(str::to_owned),
            })?).await.map_err(failure)?;
            send.finish().map_err(failure)?;
            let raw = wait(
                tokio::time::timeout(Duration::from_secs(120), recv.read_to_end(files::MAX_JSON as usize)),
                "peer_revision",
            ).await?.map_err(failure)?;
            let reply: Reply = serde_json::from_slice(&raw)?;
            reply.head.validate()?;
            reply.revision.validate()?;
            if reply.head.channel.publisher != channel.publisher
                || reply.head.channel.endpoint != channel.endpoint
                || reply.head.channel.pack != channel.pack
                || reply.head.revision != reply.revision.id()?
                || pin.is_some_and(|p| p != reply.head.revision) {
                return Err(Error::new("channel_mismatch", "Peer response does not match the code or pinned revision."));
            }
            conn.close(0u32.into(), b"received");
            let highest = storage.root.join("peer/accepted").join(format!("{}.json", channel.pack));
            if highest.exists() {
                let seen: Head = files::read_json(&highest)?;
                seen.validate()?;
                if seen.channel.publisher != channel.publisher {
                    return Err(Error::new("channel_mismatch", "This pack was previously bound to another publisher."));
                }
                if pin.is_none() && (reply.head.sequence < seen.sequence
                    || (reply.head.sequence == seen.sequence && reply.head.revision != seen.revision)) {
                    return Err(Error::new("revision_replay", "Host advertised an older or conflicting revision. Use an explicit --revision for intentional rollback."));
                }
            }
            if let Some(a) = &reply.revision.recipe {
                let path = storage.root.join("recipes").join(format!("{}.wabbajack", a.sha256));
                if !path.exists() || fs::metadata(&path)?.len() != a.size || files::sha(&path)? != a.sha256 {
                    let hash = reply.recipe_blob.as_deref().ok_or_else(|| Error::new("source_missing", "Host did not supply the pinned recipe."))?;
                    get_blob(&store, &endpoint, channel, address, hash, storage, a.size).await?;
                    let source = revision::object_path(&storage.root, hash)?;
                    if files::sha(&source)? != a.sha256 || fs::metadata(&source)?.len() != a.size {
                        return Err(Error::new("artifact_mismatch", "Peer recipe differs from its SHA-256 pin."));
                    }
                    files::copy(&source, &path)?;
                }
            }
            revision::save(&storage.root, &reply.revision)?;
            if pin.is_none() { files::atomic_json(&highest, &reply.head)?; }
            Ok(reply)
        }.await;
        let shutdown = store.shutdown().await.map_err(failure);
        endpoint.close().await;
        let reply = result?;
        shutdown?;
        Ok(reply)
    })
}
async fn get_blob(
    store: &FsStore,
    endpoint: &Endpoint,
    channel: &Channel,
    address: Option<std::net::SocketAddr>,
    hash: &str,
    storage: &Storage,
    size: u64,
) -> Result<()> {
    let path = revision::object_path(&storage.root, hash)?;

    if path.is_file() && fs::metadata(&path)?.len() == size && revision::blob_hash(&path)? == hash {
        return Ok(());
    }
    files::space(
        &storage.root,
        size.checked_mul(2)
            .ok_or_else(|| Error::new("invalid_revision", "Object size overflow."))?,
    )?;

    let mut addr = EndpointAddr::new(
        channel
            .endpoint
            .parse::<iroh::EndpointId>()
            .map_err(failure)?,
    );

    if let Some(r) = &channel.relay {
        addr = addr.with_relay_url(r.parse().map_err(failure)?);
    }
    if let Some(a) = address {
        addr = addr.with_ip_addr(a);
    }
    let hash = hash.parse::<iroh_blobs::Hash>().map_err(failure)?;

    crate::event(
        "peer_downloading",
        json!({"hash":hash.to_string(),"size":size}),
    );

    let connection = wait(
        tokio::time::timeout(
            Duration::from_secs(30),
            endpoint.connect(addr, iroh_blobs::ALPN),
        ),
        "peer_connecting",
    )
    .await?
    .map_err(failure)?;

    wait(
        store.remote().fetch(connection, hash).into_future(),
        "peer_downloading",
    )
    .await?;

    fs::create_dir_all(storage.root.join("objects"))?;

    let tmp = tempfile::NamedTempFile::new_in(storage.root.join("objects"))?;

    wait(
        store.blobs().export(hash, tmp.path()).into_future(),
        "peer_exporting",
    )
    .await?;

    if tmp.as_file().metadata()?.len() != size
        || revision::blob_hash(tmp.path())? != hash.to_string()
    {
        return Err(Error::new(
            "object_mismatch",
            "Peer object does not match its declared size/hash.",
        ));
    }
    tmp.as_file().sync_all()?;

    tmp.persist(&path).map_err(|e| Error::from(e.error))?;

    files::sync_dir(path.parent().unwrap())?;

    store
        .tags()
        .set(hash.to_string().as_bytes(), hash)
        .await
        .map_err(failure)?;

    Ok(())
}
fn acquire(
    rev: &Revision,
    head: &Head,
    storage: &Storage,
    address: Option<std::net::SocketAddr>,
    needed: &BTreeSet<String>,
) -> Result<()> {
    let entries = rev
        .files
        .iter()
        .filter(|e| needed.contains(&e.path))
        .filter_map(|e| {
            if let Source::Blob { hash } = &e.source {
                Some((hash, e.size))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if entries.is_empty() {
        return Ok(());
    }
    runtime()?.block_on(async {
        let (endpoint, _) = client_endpoint(&head.channel, address).await?;

        let store = FsStore::load(storage.root.join("peer/client"))
            .await
            .map_err(failure)?;

        let result = async {
            for (hash, size) in entries {
                get_blob(
                    &store,
                    &endpoint,
                    &head.channel,
                    address,
                    hash,
                    storage,
                    size,
                )
                .await?;
            }
            Ok(())
        }
        .await;

        store.shutdown().await.map_err(failure)?;

        endpoint.close().await;

        result
    })
}
#[allow(clippy::too_many_arguments)]
pub fn sync(
    rev: &Revision,
    head: &Head,
    storage: &Storage,
    base: &Path,
    downloads: &Path,
    steam: Option<&Path>,
    proton: Option<&Path>,
    address: Option<std::net::SocketAddr>,
) -> Result<Value> {
    rev.validate()?;

    head.validate()?;

    if rev.id()? != head.revision {
        return Err(Error::new(
            "invalid_revision",
            "Signed head does not match revision.",
        ));
    }
    let plan = revision::prepare_install(storage, rev)?;
    acquire(rev, head, storage, address, &plan.changed)?;

    let recipe = rev
        .recipe
        .as_ref()
        .map(|a| {
            crate::recipe::Recipe::load(
                &storage
                    .root
                    .join("recipes")
                    .join(format!("{}.wabbajack", a.sha256)),
            )
        })
        .transpose()?;

    fs::create_dir_all(storage.root.join("stages"))?;

    let work = tempfile::tempdir_in(storage.root.join("stages"))?;

    let needed = &plan.changed;

    if let Some(r) = &recipe {
        let mut paths = BTreeSet::new();

        for e in rev.files.iter().filter(|e| needed.contains(&e.path)) {
            if let Source::Recipe { path } = &e.source {
                let cache = storage
                    .root
                    .join("jobs")
                    .join(rev.recipe.as_ref().unwrap().sha256.clone())
                    .join("reconstructed");

                if let Some(src) = files::locate(&cache, path)? {
                    if revision::matches(&src, e)? {
                        files::copy(&src, &files::destination(work.path(), path)?)?;

                        continue;
                    }
                }
                paths.extend(crate::recipe::generated_dependencies(path, r)?);
            }
        }
        if !paths.is_empty() {
            let pending = r.extract_inline(&paths, work.path())?;

            if !pending.is_empty() {
                let subset = r.subset(&pending, &work.path().join("sources.wabbajack"))?;

                let output = crate::engine::reconstruct(
                    &subset,
                    &storage.root,
                    base,
                    downloads,
                    steam,
                    proton,
                )?;

                for p in pending {
                    files::copy(
                        &files::require(&output, &p)?,
                        &files::destination(work.path(), &p)?,
                    )?;
                }
            }
        }
    }
    let result = revision::install_prepared(storage, rev, &plan, |e: &Entry| match &e.source {
        Source::Base { path } => files::require(base, path),
        Source::Blob { hash } => revision::object_path(&storage.root, hash),
        Source::Recipe { path } => {
            crate::recipe::generate_selected(work.path(), path)?;

            files::require(work.path(), path)
        }
    })?;

    files::atomic_json(&storage.root.join("installed-head.json"), head)?;

    files::atomic_json(
        &storage.root.join("machine-paths.json"),
        &json!({"base":base,"downloads":downloads,"steam":steam,"proton":proton}),
    )?;

    Ok(result)
}
