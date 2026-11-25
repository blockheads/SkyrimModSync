//! The host's P2P service (iroh): its signed pack and the files only the host has. Read-only over
//! the wire; only objects a published pack names are served.
use crate::{
    channel::{self, Channel, Head, PrivateChannel},
    files,
    pack::Pack,
    store::Store,
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
    fs,
    future::IntoFuture,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const ALPN: &[u8] = b"skyrim-mod-sync/channel/4";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    pack: String,
    revision: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    head: Head,
    pack: Pack,
}

fn failure(_: impl std::fmt::Display) -> Error {
    Error::new(
        "peer_unavailable",
        "Couldn't reach the host. The host must keep `sms host serve` running; check the connection and retry (finished files are kept).",
    )
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
                "This host does not publish that pack.",
            ));
        }
        let path = match request.revision {
            Some(id) if files::digest_valid(&id) => {
                self.root.join("published").join(format!("{id}.json"))
            }
            Some(_) => return Err(Error::new("invalid_pack", "Invalid requested revision.")),
            None => self.root.join("channel-head.json"),
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
        let pack: Pack = files::read_json(
            &self
                .root
                .join("packs")
                .join(format!("{}.json", head.revision)),
        )?;
        if pack.id()? != head.revision {
            return Err(Error::new(
                "invalid_pack",
                "The published pack differs from its head.",
            ));
        }
        for hash in pack.host_objects() {
            let parsed = hash.parse::<iroh_blobs::Hash>().map_err(failure)?;
            if self.store.blobs().has(parsed).await.map_err(failure)? {
                continue;
            }
            let path = channel::object_path(&self.root, &hash)?;
            if channel::blob_hash(&path)? != hash {
                return Err(Error::new(
                    "object_corrupt",
                    "A published host file changed on disk; publish again.",
                ));
            }
            let tag = self.store.blobs().add_path(path).await.map_err(failure)?;
            if tag.hash.to_string() != hash {
                return Err(Error::new("object_corrupt", "Stored host file differs."));
            }
            self.store
                .tags()
                .set(hash.as_bytes(), tag.hash)
                .await
                .map_err(failure)?;
        }
        Ok(Reply { head, pack })
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
        send.write_all(&bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}

/// Serves the host's latest pack until cancelled (Ctrl-C).
pub fn serve(root: &Path, no_relay: bool, bind: Option<SocketAddr>) -> Result<Value> {
    serve_until(
        root,
        no_relay,
        bind,
        |ready| {
            println!("{ready}");
            let _ = std::io::stdout().flush();
        },
        || crate::check_cancel().is_err(),
    )
}

/// Serves until `stop` says so; `ready` gets the code and addresses once listening.
pub fn serve_until(
    root: &Path,
    no_relay: bool,
    bind: Option<SocketAddr>,
    ready: impl FnOnce(&Value),
    stop: impl Fn() -> bool,
) -> Result<Value> {
    let _lock = files::Lock::take(&root.join("locks/serve.lock"))?;
    let private = PrivateChannel::load_or_create(&root.join("channel-private.json"))?;
    let channel = private.channel()?;
    let head: Head = files::read_json(&root.join("channel-head.json"))
        .map_err(|_| Error::new("not_published", "Publish a pack first: `sms host publish`."))?;
    head.validate()?;
    runtime()?.block_on(async {
        let mut builder = Endpoint::builder(presets::N0).secret_key(iroh::SecretKey::from_bytes(&channel::decode32(&private.endpoint_key)?));
        if no_relay {
            builder = builder.relay_mode(RelayMode::Disabled);
        } else if let Some(r) = &private.relay {
            builder = builder.relay_mode(RelayMode::custom([r.parse::<iroh::RelayUrl>().map_err(failure)?]));
        }
        if let Some(addr) = bind {
            builder = builder.bind_addr(addr).map_err(failure)?;
        }
        let endpoint = builder.bind().await.map_err(failure)?;
        let store = FsStore::load(root.join("peer/server")).await.map_err(failure)?;
        let control = Control { root: root.into(), channel: channel.clone(), store: store.clone() };
        control.reply(Request { pack: channel.pack.clone(), revision: None }).await?;
        let router = Router::builder(endpoint)
            .accept(ALPN, control)
            .accept(iroh_blobs::ALPN, BlobsProtocol::new(&store, None))
            .spawn();
        let code = channel.code()?;
        ready(&json!({"state": "serving", "code": code, "addresses": router.endpoint().addr(), "sequence": head.sequence}));
        while !stop() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        router.shutdown().await.map_err(failure)?;
        Ok(json!({"state": "stopped", "code": code}))
    })
}

fn address(channel: &Channel, direct: Option<SocketAddr>) -> Result<EndpointAddr> {
    let mut addr = EndpointAddr::new(
        channel
            .endpoint
            .parse::<iroh::EndpointId>()
            .map_err(failure)?,
    );
    if let Some(r) = &channel.relay {
        addr = addr.with_relay_url(r.parse().map_err(failure)?);
    }
    if let Some(a) = direct {
        addr = addr.with_ip_addr(a);
    }
    Ok(addr)
}
async fn endpoint(direct: Option<SocketAddr>) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0);
    if direct.is_some() {
        builder = builder.relay_mode(RelayMode::Disabled);
    }
    builder.bind().await.map_err(failure)
}
async fn wait<T, E: std::fmt::Display>(
    future: impl std::future::Future<Output = std::result::Result<T, E>>,
    phase: &str,
) -> Result<T> {
    tokio::pin!(future);
    let mut heartbeat = Instant::now();
    loop {
        tokio::select! {
            r = &mut future => return r.map_err(failure),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                crate::check_cancel()?;
                if heartbeat.elapsed() > Duration::from_secs(20) {
                    crate::event(phase, json!({"status": "waiting_or_transferring"}));
                    heartbeat = Instant::now();
                }
            }
        }
    }
}

/// Asks the host for its latest (or a pinned) pack.
pub fn fetch_pack(
    channel: &Channel,
    store: &Store,
    pin: Option<&str>,
    direct: Option<SocketAddr>,
) -> Result<(Pack, Head)> {
    channel.validate()?;
    runtime()?.block_on(async {
        let ep = endpoint(direct).await?;
        let result = async {
            let conn = wait(
                tokio::time::timeout(
                    Duration::from_secs(30),
                    ep.connect(address(channel, direct)?, ALPN),
                ),
                "peer_connecting",
            )
            .await?
            .map_err(failure)?;
            let (mut send, mut recv) = conn.open_bi().await.map_err(failure)?;
            send.write_all(&serde_json::to_vec(&Request {
                pack: channel.pack.clone(),
                revision: pin.map(str::to_owned),
            })?)
            .await
            .map_err(failure)?;
            send.finish().map_err(failure)?;
            let raw = wait(
                tokio::time::timeout(
                    Duration::from_secs(120),
                    recv.read_to_end(files::MAX_JSON as usize),
                ),
                "peer_pack",
            )
            .await?
            .map_err(failure)?;
            conn.close(0u32.into(), b"received");
            let reply: Reply = serde_json::from_slice(&raw)?;
            reply.head.validate()?;
            if reply.head.channel.publisher != channel.publisher
                || reply.head.channel.endpoint != channel.endpoint
                || reply.head.channel.pack != channel.pack
                || reply.head.revision != reply.pack.id()?
                || pin.is_some_and(|p| p != reply.head.revision)
            {
                return Err(Error::new(
                    "channel_mismatch",
                    "The host's answer does not match the code.",
                ));
            }
            if pin.is_none() {
                store.accept(&reply.head)?;
            }
            Ok((reply.pack, reply.head))
        }
        .await;
        ep.close().await;
        result
    })
}

/// Fetches host files (blob, sha256, size) into the store.
pub fn fetch_objects(
    channel: &Channel,
    store: &Store,
    wanted: &[(String, String, u64)],
    direct: Option<SocketAddr>,
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    runtime()?.block_on(async {
        let ep = endpoint(direct).await?;
        let blobs = FsStore::load(store.root.join("peer"))
            .await
            .map_err(failure)?;
        let result = async {
            let staging = store.root.join("staging");
            fs::create_dir_all(&staging)?;
            for (blob, sha256, size) in wanted {
                if store.has(sha256, *size) {
                    continue;
                }
                let hash = blob.parse::<iroh_blobs::Hash>().map_err(failure)?;
                crate::event("host_file", json!({"sha256": sha256, "size": size}));
                let conn = wait(
                    tokio::time::timeout(
                        Duration::from_secs(30),
                        ep.connect(address(channel, direct)?, iroh_blobs::ALPN),
                    ),
                    "peer_connecting",
                )
                .await?
                .map_err(failure)?;
                wait(
                    blobs.remote().fetch(conn, hash).into_future(),
                    "peer_downloading",
                )
                .await?;
                let tmp = tempfile::NamedTempFile::new_in(&staging)?;
                wait(
                    blobs.blobs().export(hash, tmp.path()).into_future(),
                    "peer_exporting",
                )
                .await?;
                store.put(&mut fs::File::open(tmp.path())?, sha256, *size)?;
            }
            Ok::<(), Error>(())
        }
        .await;
        blobs.shutdown().await.map_err(failure)?;
        ep.close().await;
        result?;
        // Every file is in the store now; the transfer cache only served resuming.
        let _ = fs::remove_dir_all(store.root.join("peer"));
        Ok(())
    })
}
