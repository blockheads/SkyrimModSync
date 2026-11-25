//! HTTPS downloads, checked against their SHA-256 before they are kept.
use crate::{files, Error, Result};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The HTTP client: HTTPS only (`SMS_ALLOW_HTTP=1` lets tests use a local server).
pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(None)
        .user_agent(concat!("SkyrimModSync/", env!("CARGO_PKG_VERSION")))
        .https_only(std::env::var_os("SMS_ALLOW_HTTP").is_none())
        .build()?)
}

/// Downloads `url` into `dir` as `<sha256>`; with `expect` (size, sha256) anything else is refused.
pub fn download(
    client: &reqwest::blocking::Client,
    url: &str,
    dir: &Path,
    expect: Option<(u64, &str)>,
) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    if let Some((size, sha)) = expect {
        let done = dir.join(sha);
        if done.is_file() && fs::metadata(&done)?.len() == size && files::sha(&done)? == sha {
            return Ok(done);
        }
        files::space(dir, size)?;
    }
    let mut src = client.get(url).send()?.error_for_status()?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    let mut count = 0u64;
    let mut last = std::time::Instant::now();
    let mut buf = vec![0; 256 * 1024];
    loop {
        crate::check_cancel()?;
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if expect.is_some_and(|(size, _)| count > size) {
            return Err(Error::new(
                "download_mismatch",
                format!("{url} is larger than expected."),
            ));
        }
        temp.write_all(&buf[..n])?;
        if last.elapsed() > Duration::from_secs(2) {
            crate::event(
                "downloading",
                serde_json::json!({"bytes": count, "size": expect.map(|e| e.0)}),
            );
            last = std::time::Instant::now();
        }
    }
    temp.as_file().sync_all()?;
    let sha = files::sha(temp.path())?;
    if let Some((size, want)) = expect {
        if count != size || sha != want {
            return Err(Error::new(
                "download_mismatch",
                format!("{url} differs from what the pack expects (changed upstream?)."),
            ));
        }
    }
    let done = dir.join(&sha);
    temp.persist(&done).map_err(|e| Error::from(e.error))?;
    Ok(done)
}
