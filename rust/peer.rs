//! The host's P2P service over iroh (which authenticates the host's key, crosses home routers and relays
//! when it must). One stream per request: `pack` → the pack JSON; `file <hash> <offset>` → `1` and the
//! file's bytes from where they are on the host's disk, or `0`. Only files the published pack names.
//! Reports go the other way: `offer` + JSON waits for the host's yes (`1` + the offsets to resume
//! from) or no (`0`); then `put <index> <offset>` + bytes, only on the connection that was accepted.
use crate::{
    files,
    pack::Pack,
    reports::{self, Offer},
    store::Store,
    Error, Result,
};
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
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

const ALPN: &[u8] = b"sms/5";
const STREAMS: usize = 8;

/// What the host serves: its pack, its own files (`local`), and for the rest the archive member each
/// is in, extracted on first request into `extracted` (the host is everyone's fallback).
#[derive(Debug, Serialize, Deserialize)]
pub struct Published {
    pub pack: Pack,
    pub local: BTreeMap<String, PathBuf>,
    pub members: BTreeMap<String, (PathBuf, String)>,
    pub extracted: PathBuf,
}

impl Published {
    /// Where a file's bytes are on this machine (extracting an archive member the first time).
    pub fn file(&self, hash: &str) -> Result<Option<PathBuf>> {
        if let Some(p) = self.local.get(hash) {
            return Ok(Some(p.clone()));
        }
        let Some((archive, member)) = self.members.get(hash) else {
            return Ok(None);
        };
        let done = self.extracted.join(hash);
        if !done.is_file() {
            std::fs::create_dir_all(&self.extracted)?;
            let temp = tempfile::NamedTempFile::new_in(&self.extracted)?;
            crate::archive::extract(archive, &[member.clone()].into(), |_, r| {
                let mut out = crate::hash::Writer {
                    inner: temp.as_file(),
                    hasher: blake3::Hasher::new(),
                };
                std::io::copy(r, &mut out)?;
                if out.hasher.finalize().to_hex().as_str() != hash {
                    return Err(Error::new(
                        "archive_changed",
                        format!("{} no longer holds {member}.", archive.display()),
                    ));
                }
                Ok(())
            })?;
            temp.persist(&done).map_err(|e| Error::from(e.error))?;
        }
        Ok(Some(done))
    }
}

fn failure(e: impl std::fmt::Display) -> Error {
    Error::new(
        "peer_unavailable",
        format!("Couldn't reach the host ({e}). The host must have `sms host` running."),
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

/// What one friend's connection took.
#[derive(Default)]
struct Sent {
    files: AtomicU64,
    bytes: AtomicU64,
}

/// Where reports from friends go, and who decides: `ask` sees each offer and says yes or no.
pub struct Inbox {
    pub dir: PathBuf,
    pub ask: Box<dyn Fn(&Offer) -> bool + Send + Sync>,
}

/// One connection's accepted offer: the files it may now send, and where each goes.
type Accepted = Mutex<Option<(Offer, Vec<PathBuf>)>>;
/// An accepted offer, where each file goes, and the offset each resumes from.
type Decided = (Offer, Vec<PathBuf>, Vec<u64>);

#[derive(Clone)]
struct Handler(Arc<(Vec<u8>, Published, Option<Inbox>)>);

impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handler")
    }
}

/// A request's first line (up to `\n` or the stream's end) and the bytes read past it.
async fn header(recv: &mut iroh::endpoint::RecvStream) -> Result<(String, Vec<u8>)> {
    let mut got = vec![];
    let mut buf = [0u8; 512];
    while !got.contains(&b'\n') && got.len() < 512 {
        match recv.read(&mut buf).await.map_err(failure)? {
            Some(n) => got.extend_from_slice(&buf[..n]),
            None => break,
        }
    }
    let end = got.iter().position(|b| *b == b'\n').unwrap_or(got.len());
    let rest = got.get(end + 1..).unwrap_or_default().to_vec();
    got.truncate(end);
    Ok((String::from_utf8_lossy(&got).into_owned(), rest))
}

impl Handler {
    async fn answer(
        &self,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        sent: &Sent,
        accepted: &Accepted,
    ) -> Result<()> {
        let (request, rest) = header(&mut recv).await?;
        let mut words = request.split_whitespace();
        let first = words.next();
        if first == Some("offer") {
            return self.offer(send, recv, rest, accepted).await;
        }
        if first == Some("put") {
            let index = words.next().and_then(|w| w.parse::<usize>().ok());
            let offset = words.next().and_then(|w| w.parse::<u64>().ok());
            return self.put(send, recv, rest, index, offset, accepted).await;
        }
        match (
            first,
            words.next(),
            words.next().and_then(|o| o.parse::<u64>().ok()),
        ) {
            (Some("pack"), None, None) => send.write_all(&self.0 .0).await.map_err(failure)?,
            (Some("file"), Some(hash), Some(offset)) => {
                let (me, hash) = (self.clone(), hash.to_owned());
                let path = tokio::task::spawn_blocking(move || me.0 .1.file(&hash))
                    .await
                    .map_err(failure)??;
                match path {
                    Some(path) => {
                        let mut file = tokio::fs::File::open(path).await?;
                        file.seek(std::io::SeekFrom::Start(offset)).await?;
                        send.write_all(b"1").await.map_err(failure)?;
                        if sent.files.fetch_add(1, Ordering::Relaxed) == 0 {
                            crate::say(
                                "sending",
                                "A friend is getting files from you...",
                                serde_json::json!({}),
                            );
                        }
                        let n = tokio::io::copy(&mut file, &mut send).await?;
                        sent.bytes.fetch_add(n, Ordering::Relaxed);
                    }
                    None => send.write_all(b"0").await.map_err(failure)?,
                }
            }
            _ => return Err(Error::new("bad_request", "Unknown request.")),
        }
        send.finish().map_err(failure)?;
        Ok(())
    }

    /// A friend's offer: the host's yes or no, then where each file would resume.
    async fn offer(
        &self,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        mut body: Vec<u8>,
        accepted: &Accepted,
    ) -> Result<()> {
        let mut buf = [0u8; 16 << 10];
        while let Some(n) = recv.read(&mut buf).await.map_err(failure)? {
            body.extend_from_slice(&buf[..n]);
            if body.len() > 1 << 20 {
                return Err(Error::new("invalid_offer", "The offer is too long."));
            }
        }
        let offer: Offer = serde_json::from_slice(&body)?;
        let me = self.clone();
        let decided = tokio::task::spawn_blocking(
            move || -> Result<Option<Decided>> {
                offer.validate()?;
                let Some(inbox) = &me.0 .2 else {
                    return Ok(None);
                };
                if !(inbox.ask)(&offer) {
                    return Ok(None);
                }
                let (mut paths, mut offsets) = (vec![], vec![]);
                for f in &offer.files {
                    let (path, here) = reports::destination(&inbox.dir, &offer.from, f)?;
                    let part = std::fs::metadata(path.with_extension("part"))
                        .map(|m| m.len())
                        .unwrap_or(0);
                    offsets.push(if here { f.size } else { part.min(f.size) });
                    paths.push(path);
                }
                Ok(Some((offer, paths, offsets)))
            },
        )
        .await
        .map_err(failure)??;
        match decided {
            Some((offer, paths, offsets)) => {
                *accepted.lock().unwrap() = Some((offer, paths));
                send.write_all(b"1").await.map_err(failure)?;
                send.write_all(&serde_json::to_vec(&offsets)?)
                    .await
                    .map_err(failure)?;
            }
            None => send.write_all(b"0").await.map_err(failure)?,
        }
        send.finish().map_err(failure)?;
        Ok(())
    }

    /// One accepted file's bytes from `offset`; checked and put in place once complete.
    async fn put(
        &self,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
        first: Vec<u8>,
        index: Option<usize>,
        offset: Option<u64>,
        accepted: &Accepted,
    ) -> Result<()> {
        let target = {
            let a = accepted.lock().unwrap();
            match (a.as_ref(), index, offset) {
                (Some((offer, paths)), Some(i), Some(o)) if i < paths.len() => Some((
                    offer.from.clone(),
                    offer.files[i].clone(),
                    paths[i].clone(),
                    o,
                )),
                _ => None,
            }
        };
        let Some((from, file, dest, offset)) = target else {
            send.write_all(b"0").await.map_err(failure)?;
            send.finish().map_err(failure)?;
            return Err(Error::new(
                "not_accepted",
                "The host has not accepted that file.",
            ));
        };
        tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
        let part = dest.with_extension("part");
        let mut out = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .await?;
        let have = out.metadata().await?.len().min(offset);
        out.set_len(have).await?;
        out.seek(std::io::SeekFrom::Start(have)).await?;
        let mut written = have;
        let mut chunk = first;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if written + chunk.len() as u64 > file.size {
                return Err(Error::new(
                    "too_long",
                    format!("{} is longer than offered.", file.name),
                ));
            }
            out.write_all(&chunk).await?;
            written += chunk.len() as u64;
            match recv.read(&mut buf).await.map_err(failure)? {
                Some(n) => chunk = buf[..n].to_vec(),
                None => break,
            }
        }
        out.sync_all().await?;
        drop(out);
        let ok =
            written == file.size && crate::hash::file(&part).is_ok_and(|(_, h)| h == file.hash);
        if ok {
            tokio::fs::rename(&part, &dest).await?;
            crate::say(
                "received",
                &format!(
                    "Got {} from {from} ({}): {}",
                    file.name,
                    crate::size(file.size),
                    dest.display()
                ),
                serde_json::json!({"from": from, "file": file.name, "path": dest}),
            );
        } else if written == file.size {
            let _ = tokio::fs::remove_file(&part).await;
        }
        send.write_all(if ok { b"1" } else { b"0" })
            .await
            .map_err(failure)?;
        send.finish().map_err(failure)?;
        Ok(())
    }
}

impl ProtocolHandler for Handler {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let sent = Arc::new(Sent::default());
        let accepted = Arc::new(Accepted::default());
        let mut streams = tokio::task::JoinSet::new();
        while let Ok((send, recv)) = connection.accept_bi().await {
            let (me, sent, accepted) = (self.clone(), sent.clone(), accepted.clone());
            streams.spawn(async move { me.answer(send, recv, &sent, &accepted).await });
        }
        while streams.join_next().await.is_some() {}
        let files = sent.files.load(Ordering::Relaxed);
        if files > 0 {
            let bytes = sent.bytes.load(Ordering::Relaxed);
            crate::say(
                "sent",
                &format!("Sent a friend {files} files ({}).", crate::size(bytes)),
                serde_json::json!({"files": files, "bytes": bytes}),
            );
        }
        Ok(())
    }
}

/// Serves `published` (and takes reports into `inbox`, if any) until `stop` says so; `ready` gets
/// the code (the endpoint id) once listening.
pub fn serve(
    key: SecretKey,
    published: Published,
    inbox: Option<Inbox>,
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
            .accept(ALPN, Handler(Arc::new((pack, published, inbox))))
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
    let total: u64 = wanted.iter().map(|w| w.1).sum();
    crate::say(
        "from_host",
        &format!(
            "Connecting to the host for {} files ({})...",
            wanted.len(),
            crate::size(total)
        ),
        serde_json::json!({"files": wanted.len(), "bytes": total}),
    );
    runtime()?.block_on(async {
        let (endpoint, conn) = connect(code, direct).await?;
        let meter = Arc::new(crate::Meter::new("From the host", total));
        let gate = Arc::new(tokio::sync::Semaphore::new(STREAMS));
        let mut jobs = tokio::task::JoinSet::new();
        for (hash, size) in wanted.iter().cloned() {
            let (conn, gate, meter) = (conn.clone(), gate.clone(), meter.clone());
            let dest = store.object(&hash);
            jobs.spawn(async move {
                let _turn = gate.acquire().await;
                fetch_one(&conn, &hash, size, &dest, &meter).await
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
        if result.is_ok() {
            meter.finish();
            crate::say(
                "from_host_done",
                &format!("Got {} files from the host.", wanted.len()),
                serde_json::json!({"files": wanted.len()}),
            );
        }
        result
    })
}

async fn fetch_one(
    conn: &Connection,
    hash: &str,
    size: u64,
    dest: &Path,
    meter: &crate::Meter,
) -> Result<()> {
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
    meter.add(have);
    let mut recv = ask(conn, &format!("file {hash} {have}")).await?;
    let mut flag = [0u8];
    recv.read_exact(&mut flag).await.map_err(failure)?;
    if flag[0] != b'1' {
        return Err(Error::new(
            "host_missing_file",
            format!("The host no longer has {hash}: ask them to publish again."),
        ));
    }
    // Written from where the part ends (not append mode: Windows will not truncate through it).
    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part)
        .await?;
    out.set_len(have).await?;
    out.seek(std::io::SeekFrom::Start(have)).await?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = recv.read(&mut buf).await.map_err(failure)?.unwrap_or(0);
        if n == 0 {
            break;
        }
        crate::check_cancel()?;
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await?;
        meter.add(n as u64);
    }
    out.sync_all().await?;
    drop(out); // Windows will not rename a file still open
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

/// Offers reports to the host and, once accepted, sends them (resuming cut files). `waiting` runs
/// while the host decides. False when the host said no.
pub fn send_reports(
    code: &str,
    direct: Option<SocketAddr>,
    offer: &Offer,
    paths: &[PathBuf],
    waiting: impl FnOnce(),
) -> Result<bool> {
    runtime()?.block_on(async {
        let (endpoint, conn) = connect(code, direct).await?;
        let mut body = b"offer\n".to_vec();
        body.extend(serde_json::to_vec(offer)?);
        let (mut send, mut recv) = conn.open_bi().await.map_err(failure)?;
        send.write_all(&body).await.map_err(failure)?;
        send.finish().map_err(failure)?;
        waiting();
        let answer = tokio::time::timeout(Duration::from_secs(600), recv.read_to_end(1 << 20))
            .await
            .map_err(|_| {
                Error::new(
                    "host_no_answer",
                    "The host did not answer within 10 minutes.",
                )
            })?
            .map_err(failure)?;
        if answer.first() != Some(&b'1') {
            conn.close(0u32.into(), b"done");
            endpoint.close().await;
            return Ok(false);
        }
        let offsets: Vec<u64> = serde_json::from_slice(&answer[1..])?;
        let total: u64 = offer
            .files
            .iter()
            .zip(&offsets)
            .map(|(f, o)| f.size - o.min(&f.size))
            .sum();
        let meter = crate::Meter::new("To the host", total);
        for (i, (f, path)) in offer.files.iter().zip(paths).enumerate() {
            crate::check_cancel()?;
            let offset = offsets.get(i).copied().unwrap_or(0).min(f.size);
            if offset == f.size {
                continue; // the host has it
            }
            let (mut send, mut recv) = conn.open_bi().await.map_err(failure)?;
            send.write_all(format!("put {i} {offset}\n").as_bytes())
                .await
                .map_err(failure)?;
            let mut file = tokio::fs::File::open(path).await?;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut file, &mut buf).await?;
                if n == 0 {
                    break;
                }
                crate::check_cancel()?;
                send.write_all(&buf[..n]).await.map_err(failure)?;
                meter.add(n as u64);
            }
            send.finish().map_err(failure)?;
            let ok = recv.read_to_end(16).await.map_err(failure)?;
            if ok != b"1" {
                return Err(Error::new(
                    "report_failed",
                    format!("{} did not arrive whole: send again (it resumes).", f.name),
                ));
            }
        }
        meter.finish();
        conn.close(0u32.into(), b"done");
        endpoint.close().await;
        Ok(true)
    })
}
