use crate::{
    files,
    package::{public_url, Artifact, Code},
    Error, Result,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use xxhash_rust::xxh64::Xxh64;
pub fn wj_hash(path: &Path) -> Result<String> {
    files::regular(path)?;
    let mut src = File::open(path)?;
    let mut hash = Xxh64::new(0);
    let mut b = vec![0; 1024 * 1024];
    loop {
        crate::check_cancel()?;
        let n = src.read(&mut b)?;
        if n == 0 {
            break;
        }
        hash.update(&b[..n]);
    }
    Ok(STANDARD.encode(hash.digest().to_le_bytes()))
}
pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(120))
        .user_agent("SkyrimModSync/0.2")
        .https_only(true)
        .build()?)
}
pub fn download(url: &str, to: &Path, size: u64, sha256: &str) -> Result<()> {
    download_with(&client()?, url, to, size, sha256)
}
fn download_with(
    client: &reqwest::blocking::Client,
    url: &str,
    to: &Path,
    size: u64,
    sha256: &str,
) -> Result<()> {
    if to.is_file() && fs::metadata(to)?.len() == size && files::sha(to)? == sha256 {
        return Ok(());
    }
    fs::create_dir_all(to.parent().unwrap())?;
    files::space(to.parent().unwrap(), size)?;
    let mut src = client.get(url).send()?.error_for_status()?;
    let mut temp = tempfile::NamedTempFile::new_in(to.parent().unwrap())?;
    let mut count = 0;
    let mut buf = vec![0; 256 * 1024];
    loop {
        crate::check_cancel()?;
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > size {
            return Err(Error::new(
                "download_mismatch",
                "Download exceeded pinned size.",
            ));
        }
        temp.write_all(&buf[..n])?;
    }
    if count != size || files::sha(temp.path())? != sha256 {
        return Err(Error::new(
            "download_mismatch",
            "Download failed pinned SHA-256 or size check.",
        ));
    }
    temp.as_file().sync_all()?;
    temp.persist(to).map_err(|e| Error::from(e.error))?;
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Definition {
    size: u64,
    hash: String,
    parts: Vec<Part>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Part {
    index: usize,
    offset: u64,
    size: u64,
    hash: String,
}
fn valid_hash(s: &str) -> bool {
    STANDARD.decode(s).is_ok_and(|b| b.len() == 8)
}
pub fn download_recipe(url: &str, to: &Path, identity: &Artifact) -> Result<()> {
    public_url(url, true)?;
    let response = client()?
        .get(format!("{}/definition.json.gz", url.trim_end_matches('/')))
        .send()?
        .error_for_status()?;
    let mut compressed = Vec::new();
    response
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut compressed)?;
    if compressed.len() > 2 * 1024 * 1024 {
        return Err(Error::new(
            "invalid_download",
            "Recipe definition too large.",
        ));
    }
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 4 * 1024 * 1024 {
        return Err(Error::new(
            "invalid_download",
            "Expanded definition too large.",
        ));
    }
    let definition: Definition = serde_json::from_slice(&raw)?;
    if definition.size != identity.size
        || definition.parts.is_empty()
        || definition.parts.len() > 10000
        || !valid_hash(&definition.hash)
    {
        return Err(Error::new("invalid_download", "Invalid recipe definition."));
    }
    let mut offset = 0u64;
    for (i, p) in definition.parts.iter().enumerate() {
        if p.index != i
            || p.offset != offset
            || p.size == 0
            || p.size > 16 * 1024 * 1024
            || !valid_hash(&p.hash)
        {
            return Err(Error::new(
                "invalid_download",
                "Overlapping or invalid recipe parts.",
            ));
        }
        offset += p.size;
    }
    if offset != definition.size {
        return Err(Error::new(
            "invalid_download",
            "Recipe part sizes disagree.",
        ));
    }
    let cache = to
        .parent()
        .unwrap()
        .join("recipe-parts")
        .join(files::hash_bytes(&raw));
    fs::create_dir_all(&cache)?;
    files::space(&cache, 2 * identity.size)?;
    for p in &definition.parts {
        crate::check_cancel()?;
        let part = cache.join(p.index.to_string());
        if part.is_file() && fs::metadata(&part)?.len() == p.size && wj_hash(&part)? == p.hash {
            continue;
        }
        let response = client()?
            .get(format!("{}/parts/{}", url.trim_end_matches('/'), p.index))
            .send()?
            .error_for_status()?;
        let mut temp = tempfile::NamedTempFile::new_in(&cache)?;
        std::io::copy(&mut response.take(p.size + 1), temp.as_file_mut())?;
        if temp.as_file().metadata()?.len() != p.size || wj_hash(temp.path())? != p.hash {
            return Err(Error::new(
                "download_mismatch",
                "Recipe part hash mismatch.",
            ));
        }
        temp.persist(&part).map_err(|e| Error::from(e.error))?;
        crate::event(
            "downloading_recipe",
            serde_json::json!({"completed":p.offset+p.size,"total":identity.size}),
        );
    }
    let mut temp = tempfile::NamedTempFile::new_in(to.parent().unwrap())?;
    for p in &definition.parts {
        let mut src = File::open(cache.join(p.index.to_string()))?;
        let mut b = vec![0; 1024 * 1024];
        loop {
            crate::check_cancel()?;
            let n = src.read(&mut b)?;
            if n == 0 {
                break;
            }
            temp.write_all(&b[..n])?;
        }
    }
    if wj_hash(temp.path())? != definition.hash || files::sha(temp.path())? != identity.sha256 {
        return Err(Error::new(
            "artifact_mismatch",
            "Public recipe changed from its pinned identity.",
        ));
    }
    temp.as_file().sync_all()?;
    temp.persist(to).map_err(|e| Error::from(e.error))?;
    Ok(())
}
pub fn import_code(code: &str, storage: &Path) -> Result<PathBuf> {
    let code = Code::decode(code)?;
    let dest = storage
        .join("packages")
        .join(format!("{}.skyrimsync", code.sha256));
    let _lock = files::Lock::take(&storage.join("locks/package.lock"))?;
    download(&code.url, &dest, code.size, &code.sha256)?;
    crate::package::load(&dest)?;
    Ok(dest)
}
pub fn validate_nexus(key: &str) -> Result<bool> {
    let response = client()?
        .get("https://api.nexusmods.com/v1/users/validate.json")
        .header("apikey", key)
        .send()?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(Error::new(
            "nexus_auth_failed",
            "Nexus rejected the key. Regenerate a personal API key and run nexus-login again.",
        ));
    }
    let value: serde_json::Value = response.error_for_status()?.json()?;
    Ok(value
        .get("is_premium")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn server(bytes: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            socket.write_all(&bytes).unwrap();
        });
        (format!("http://{address}/package"), handle)
    }
    #[test]
    fn pinned_stream_download_and_offline_cache_reuse() {
        let bytes = b"stand-in package bytes".to_vec();
        let (url, handle) = server(bytes.clone());
        let d = tempfile::tempdir().unwrap();
        let to = d.path().join("package");
        let client = reqwest::blocking::Client::new();
        let hash = files::hash_bytes(&bytes);
        download_with(&client, &url, &to, bytes.len() as u64, &hash).unwrap();
        handle.join().unwrap();
        assert_eq!(fs::read(&to).unwrap(), bytes);
        // The server has stopped; success proves cache reuse still freshly verifies.
        download_with(&client, &url, &to, bytes.len() as u64, &hash).unwrap();
    }
    #[test]
    fn altered_same_size_truncated_and_oversized_downloads_are_not_published() {
        for (body, size) in [
            (b"evil".to_vec(), 4),
            (b"abc".to_vec(), 4),
            (b"extra bytes".to_vec(), 4),
        ] {
            let (url, handle) = server(body);
            let d = tempfile::tempdir().unwrap();
            let to = d.path().join("package");
            let e = download_with(
                &reqwest::blocking::Client::new(),
                &url,
                &to,
                size,
                &files::hash_bytes(b"good"),
            )
            .unwrap_err();
            handle.join().unwrap();
            assert_eq!(e.code, "download_mismatch");
            assert!(!to.exists());
        }
    }
    #[test]
    fn production_client_refuses_plain_http() {
        let d = tempfile::tempdir().unwrap();
        let e = download(
            "http://127.0.0.1:9/package",
            &d.path().join("package"),
            4,
            &files::hash_bytes(b"good"),
        )
        .unwrap_err();
        assert_eq!(e.code, "network_error");
    }
}
