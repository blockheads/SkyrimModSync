//! The internet side: HTTPS downloads (hashed while written), Nexus Mods through the user's own API key,
//! and GitHub's newest release. Nexus downloads need a Premium key; nothing automates the website.
use crate::{hash, Error, Result};
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct Net {
    client: reqwest::blocking::Client,
    nexus: String,
    github: String,
    pub key: Option<String>,
}

impl Net {
    pub fn new(key: Option<String>) -> Result<Self> {
        Self::with(
            "https://api.nexusmods.com",
            "https://api.github.com",
            true,
            key,
        )
    }
    /// Tests point both APIs at a local plain-HTTP server.
    pub fn with(nexus: &str, github: &str, https_only: bool, key: Option<String>) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(None)
            .user_agent(concat!("SkyrimModSync/", env!("CARGO_PKG_VERSION")))
            .https_only(https_only)
            .build()?;
        Ok(Self {
            client,
            nexus: nexus.into(),
            github: github.into(),
            key,
        })
    }

    /// `url` saved in `dir` as its BLAKE3; with `expect` (size, hash) anything else is refused.
    pub fn download(
        &self,
        url: &str,
        dir: &Path,
        expect: Option<(u64, &str)>,
        meter: Option<&crate::Meter>,
    ) -> Result<PathBuf> {
        fs::create_dir_all(dir)?;
        if let Some((_, h)) = expect {
            if dir.join(h).is_file() {
                return Ok(dir.join(h));
            }
        }
        let mut src = self.client.get(url).send()?.error_for_status()?;
        let temp = tempfile::NamedTempFile::new_in(dir)?;
        let mut out = hash::Writer {
            inner: temp.as_file(),
            hasher: blake3::Hasher::new(),
        };
        let mut buf = vec![0; 1 << 20];
        loop {
            crate::check_cancel()?;
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            if let Some(m) = meter {
                m.add(n as u64);
            }
        }
        let (size, h) = (
            out.hasher.count(),
            out.hasher.finalize().to_hex().to_string(),
        );
        if expect.is_some_and(|(s, e)| s != size || e != h) {
            return Err(Error::new(
                "download_mismatch",
                format!("{url} changed upstream."),
            ));
        }
        temp.as_file().sync_all()?;
        let done = dir.join(&h);
        temp.persist(&done).map_err(|e| Error::from(e.error))?;
        Ok(done)
    }

    /// A small document (a pack).
    pub fn get(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self
            .client
            .get(url)
            .send()?
            .error_for_status()?
            .bytes()?
            .to_vec())
    }

    fn nexus_get(&self, path: &str) -> Result<reqwest::blocking::Response> {
        let key = self
            .key
            .as_deref()
            .ok_or_else(|| Error::new("nexus_key_required", "No Nexus API key."))?;
        Ok(self
            .client
            .get(format!("{}{path}", self.nexus))
            .header("apikey", key)
            .send()?)
    }
    /// The key's account name and whether it is Premium.
    pub fn nexus_user(&self) -> Result<(String, bool)> {
        let r = self.nexus_get("/v1/users/validate.json")?;
        if r.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::new("nexus_auth_failed", "Nexus refused the key."));
        }
        let v: Value = r.error_for_status()?.json()?;
        Ok((
            v["name"].as_str().unwrap_or("?").into(),
            v["is_premium"] == true,
        ))
    }
    /// A mod's name on Nexus.
    pub fn nexus_mod_name(&self, game: &str, r#mod: u64) -> Result<String> {
        let v: Value = self
            .nexus_get(&format!("/v1/games/{game}/mods/{mod}.json"))?
            .error_for_status()?
            .json()?;
        Ok(v["name"].as_str().unwrap_or("?").to_owned())
    }
    /// A download link for a mod file (Premium accounts only).
    pub fn nexus_link(&self, game: &str, r#mod: u64, file: u64) -> Result<String> {
        let r = self.nexus_get(&format!(
            "/v1/games/{game}/mods/{mod}/files/{file}/download_link.json"
        ))?;
        if r.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::new(
                "nexus_not_premium",
                "Downloading through the Nexus API needs a Premium account.",
            ));
        }
        let links: Vec<Value> = r.error_for_status()?.json()?;
        links
            .iter()
            .find_map(|l| l["URI"].as_str().map(str::to_owned))
            .ok_or_else(|| {
                Error::new(
                    "nexus_no_link",
                    format!("Nexus gave no link for mod {mod}."),
                )
            })
    }
    /// A mod file's archive name on Nexus (any account).
    pub fn nexus_file_name(&self, game: &str, r#mod: u64, file: u64) -> Result<String> {
        let v: Value = self
            .nexus_get(&format!("/v1/games/{game}/mods/{mod}/files/{file}.json"))?
            .error_for_status()?
            .json()?;
        Ok(v["file_name"].as_str().unwrap_or_default().to_owned())
    }

    /// A file of a gist, its newest version.
    pub fn gist_file(&self, id: &str, name: &str) -> Result<Vec<u8>> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::new("invalid_source", "Expected gist:<id>."));
        }
        let gist: Value = self
            .client
            .get(format!("{}/gists/{id}", self.github))
            .header("Accept", "application/vnd.github+json")
            .send()?
            .error_for_status()?
            .json()?;
        let url = gist["files"][name]["raw_url"]
            .as_str()
            .ok_or_else(|| Error::new("no_asset", format!("The gist has no {name}.")))?;
        self.get(url)
    }

    /// Saves files to a secret gist: a new one (`id` None) or a new version of `id`; returns its id.
    pub fn gist_save(
        &self,
        token: &str,
        id: Option<&str>,
        description: &str,
        files: &[(&str, String)],
    ) -> Result<String> {
        let files: serde_json::Map<String, Value> = files
            .iter()
            .map(|(n, c)| (n.to_string(), serde_json::json!({ "content": c })))
            .collect();
        let body = serde_json::json!({"description": description, "public": false, "files": files});
        let request = match id {
            Some(id) => self.client.patch(format!("{}/gists/{id}", self.github)),
            None => self.client.post(format!("{}/gists", self.github)),
        };
        let r = request
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()?;
        match r.status().as_u16() {
            401 | 403 => {
                return Err(Error::new(
                    "github_token",
                    "GitHub refused the token: it needs the gist scope.",
                ))
            }
            404 if id.is_some() => {
                return Err(Error::new("gist_missing", "The pack's gist is gone."))
            }
            _ => {}
        }
        let v: Value = r.error_for_status()?.json()?;
        v["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::new("github_error", "GitHub returned no gist id."))
    }

    /// The newest release's (prereleases too) archive whose name contains `asset`, else its only .zip/.7z.
    pub fn github_asset(&self, repo: &str, asset: Option<&str>) -> Result<String> {
        let releases: Vec<Value> = self
            .client
            .get(format!("{}/repos/{repo}/releases?per_page=10", self.github))
            .header("Accept", "application/vnd.github+json")
            .send()?
            .error_for_status()?
            .json()?;
        let release = releases
            .iter()
            .find(|r| r["draft"] != true)
            .ok_or_else(|| Error::new("no_release", format!("{repo} has no release.")))?;
        let name = |a: &Value| a["name"].as_str().unwrap_or_default().to_lowercase();
        let found: Vec<&Value> = release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| match asset {
                Some(p) => name(a).contains(&p.to_lowercase()),
                None => name(a).ends_with(".zip") || name(a).ends_with(".7z"),
            })
            .collect();
        match found.as_slice() {
            [one] => Ok(one["browser_download_url"]
                .as_str()
                .unwrap_or_default()
                .into()),
            [] => Err(Error::new(
                "no_asset",
                format!("{repo}'s newest release has no matching archive."),
            )),
            _ => Err(Error::new(
                "ambiguous_asset",
                format!("{repo}'s release has several archives: name one."),
            )),
        }
    }
}

/// "owner/repo", nothing else.
pub fn valid_repo(repo: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    matches!(repo.split('/').collect::<Vec<_>>().as_slice(), [a, b] if part(a) && part(b))
}
