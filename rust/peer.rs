//! The host's P2P service over iroh (which authenticates the host's key, crosses home routers and relays
//! when it must). One stream per request: `pack` → the pack JSON; `file <hash> <offset>` → `1` and the
//! file's bytes from where they are on the host's disk, or `0`. Only files the published pack names.
use crate::{files, pack::Pack, store::Store, Error, Result};
use iroh::{
    endpoint::{presets, Connection, RelayMode},
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr, EndpointId, SecretKey,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

const ALPN: &[u8] = b"sms/5";
const STREAMS: usize = 8;

/// What the host serves: its pack and where each of its files is on its disk.
#[derive(Serialize, Deserialize)]
pub struct Published {
    pub pack: Pack,
    pub local: BTreeMap<String, PathBuf>,
}

fn failure(e: impl std::fmt::Display) -> Error {
    Error::new(
        "peer_unavailable",
        format!("Couldn't reach the host ({e}). The host must run `sms host serve`."),
    )
}
fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

/// The host's iroh key, made on first use.
pub fn secret(path: &Path) -> Result<SecretKey> {
    if let Ok(bytes) = std::fs::read(path) {
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::new("invalid_key", "The host key file is damaged."))?;
        return Ok(SecretKey::from_bytes(&key));
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| Error::new("io_error", e.to_string()))?;
    files::write_atomic(path, &key)?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(SecretKey::from_bytes(&key))
}

#[derive(Clone, Debug)]
struct Handler(Arc<(Vec<u8>, BTreeMap<String, PathBuf>)>);

impl Handler {
    async fn answer(
        &self,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
    ) -> Result<()> {
        let request = recv.read_to_end(256).await.map_err(failure)?;
        let request = String::from_utf8_lossy(&request);
        let mut words = request.split_whitespace();
        match (
            words.next(),
            words.next(),
            words.next().and_then(|o| o.parse::<u64>().ok()),
        ) {
            (Some("pack"), None, None) => send.write_all(&self.0 .0).await.map_err(failure)?,
            (Some("file"), Some(hash), Some(offset)) => match self.0 .1.get(hash) {
                Some(path) => {
                    let mut file = tokio::fs::File::open(path).await?;
                    file.seek(std::io::SeekFrom::Start(offset)).await?;
                    send.write_all(b"1").await.map_err(failure)?;
                    tokio::io::copy(&mut file, &mut send).await?;
                }
                None => send.write_all(b"0").await.map_err(failure)?,
            },
            _ => return Err(Error::new("bad_request", "Unknown request.")),
        }
        send.finish().map_err(failure)?;
        Ok(())
    }
}

impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        while let Ok((send, recv)) = connection.accept_bi().await {
            let me = self.clone();
            tokio::spawn(async move { me.answer(send, recv).await });
        }
        Ok(())
    }
}

/// Serves `published` until `stop` says so; `ready` gets the code (the endpoint id) once listening.
pub fn serve(
    key: SecretKey,
    published: Published,
    bind: Option<SocketAddr>,
    ready: impl FnOnce(&str),
    stop: impl Fn() -> bool,
) -> Result<()> {
    let pack = serde_json::to_vec(&published.pack)?;
    runtime()?.block_on(async {
        let mut builder = Endpoint::builder(presets::N0).secret_key(key);
        if let Some(addr) = bind {
            builder = builder
                .relay_mode(RelayMode::Disabled)
                .bind_addr(addr)
                .map_err(failure)?;
        }
        let endpoint = builder.bind().await.map_err(failure)?;
        let id = endpoint.id().to_string();
        let router = Router::builder(endpoint)
            .accept(ALPN, Handler(Arc::new((pack, published.local))))
            .spawn();
        ready(&id);
        while !stop() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        router.shutdown().await.map_err(failure)?;
        Ok(())
    })
}

async fn connect(code: &str, direct: Option<SocketAddr>) -> Result<(Endpoint, Connection)> {
    let id: EndpointId = code
        .trim()
        .parse()
        .map_err(|_| Error::new("invalid_code", "That is not an sms code."))?;
    let mut builder = Endpoint::builder(presets::N0);
    let mut addr = EndpointAddr::new(id);
    if let Some(a) = direct {
        builder = builder.relay_mode(RelayMode::Disabled);
        addr = addr.with_ip_addr(a);
    }
    let endpoint = builder.bind().await.map_err(failure)?;
    let conn = tokio::time::timeout(Duration::from_secs(30), endpoint.connect(addr, ALPN))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    Ok((endpoint, conn))
}

async fn ask(conn: &Connection, request: &str) -> Result<iroh::endpoint::RecvStream> {
    let (mut send, recv) = conn.open_bi().await.map_err(failure)?;
    send.write_all(request.as_bytes()).await.map_err(failure)?;
    send.finish().map_err(failure)?;
    Ok(recv)
}

/// The host's pack.
pub fn fetch_pack(code: &str, direct: Option<SocketAddr>) -> Result<Pack> {
    runtime()?.block_on(async {
        let (endpoint, conn) = connect(code, direct).await?;
        let bytes = ask(&conn, "pack")
            .await?
            .read_to_end(256 << 20)
            .await
            .map_err(failure)?;
        conn.close(0u32.into(), b"done");
        endpoint.close().await;
        let pack: Pack = serde_json::from_slice(&bytes)?;
        pack.validate()?;
        Ok(pack)
    })
}

/// Fetches files (hash, size) from the host into the store, several at once; a cut file resumes.
pub fn fetch_files(
    code: &str,
    direct: Option<SocketAddr>,
    store: &Store,
    wanted: &[(String, u64)],
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    runtime()?.block_on(async {
        let (endpoint, conn) = connect(code, direct).await?;
        let gate = Arc::new(tokio::sync::Semaphore::new(STREAMS));
        let mut jobs = tokio::task::JoinSet::new();
        for (hash, size) in wanted.iter().cloned() {
            let (conn, gate) = (conn.clone(), gate.clone());
            let dest = store.object(&hash);
            jobs.spawn(async move {
                let _turn = gate.acquire().await;
                fetch_one(&conn, &hash, size, &dest).await
            });
        }
        let mut result = Ok(());
        while let Some(done) = jobs.join_next().await {
            if let Err(e) = done.map_err(failure).and_then(|r| r) {
                result = Err(e);
            }
        }
        conn.close(0u32.into(), b"done");
        endpoint.close().await;
        result
    })
}

async fn fetch_one(conn: &Connection, hash: &str, size: u64, dest: &Path) -> Result<()> {
    crate::check_cancel()?;
    tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
    let part = dest.with_extension("part");
    let mut hasher = blake3::Hasher::new();
    let have = match tokio::fs::read(&part).await {
        Ok(bytes) if (bytes.len() as u64) < size => {
            hasher.update(&bytes);
            bytes.len() as u64
        }
        _ => 0,
    };
    let mut recv = ask(conn, &format!("file {hash} {have}")).await?;
    let mut flag = [0u8];
    recv.read_exact(&mut flag).await.map_err(failure)?;
    if flag[0] != b'1' {
        return Err(Error::new(
            "host_missing_file",
            format!("The host no longer has {hash}: ask them to publish again."),
        ));
    }
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .await?;
    if have == 0 {
        out.set_len(0).await?;
    }
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = recv.read(&mut buf).await.map_err(failure)?.unwrap_or(0);
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await?;
    }
    out.sync_all().await?;
    if hasher.count() != size || hasher.finalize().to_hex().as_str() != hash {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(Error::new(
            "object_mismatch",
            format!("{hash} arrived damaged; run sync again."),
        ));
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}
