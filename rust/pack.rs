//! The pack: what a friend's game must hold. Files (by path and hash) with where each comes from, the
//! load order, and the shared INI settings. Public data: no host paths, no credentials.
use crate::{files, snapshot, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: u32 = 4;

/// Where a file's bytes come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// Licensed game content: the friend's own copy must match.
    Base,
    /// A member of a downloadable archive listed in `Pack::archives`.
    Archive { archive: String, member: String },
    /// Served by the host over P2P (BLAKE3 object).
    Host { blob: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Relative to the game folder, '/'-separated, the host's casing ("Data/foo.esp").
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// The MO2 mod it came from (display only); none for the game folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#mod: Option<String>,
    pub source: Source,
}

/// Where an archive is downloaded from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    /// Nexus Mods, through the friend's own API key.
    Nexus { game: String, r#mod: u64, file: u64 },
    /// A public HTTPS download.
    Url { url: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Archive {
    pub sha256: String,
    pub size: u64,
    /// The archive's file name, for display ("USSEP 4.3.9c-266-....7z").
    pub name: String,
    pub origin: Origin,
}

/// Shared INI settings: file -> section -> key -> value (sections and keys as the host wrote them).
pub type Ini = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub schema: u32,
    pub name: String,
    /// SkyrimSE.exe's version on the host ("1.7.104.0"): the friend's must match.
    pub runtime: String,
    /// plugins.txt lines as the host's game reads them ("*Foo.esp" active).
    pub plugins: Vec<String>,
    /// loadorder.txt lines.
    pub loadorder: Vec<String>,
    pub ini: Ini,
    pub archives: Vec<Archive>,
    pub files: Vec<Entry>,
}

impl Pack {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::new("invalid_pack", m));
        if self.schema != SCHEMA {
            return bad(format!("Unsupported pack schema {}.", self.schema));
        }
        if self.name.is_empty() || self.name.len() > 200 || self.runtime.len() > 40 {
            return bad("Invalid pack name or runtime.".into());
        }
        for p in self.plugins.iter().chain(&self.loadorder) {
            snapshot::plugin_name(p.trim_start_matches('*'))?;
        }
        let mut archives = BTreeSet::new();
        for a in &self.archives {
            if !files::digest_valid(&a.sha256) || !archives.insert(a.sha256.as_str()) {
                return bad(format!("Invalid or duplicate archive {}.", a.name));
            }
            if let Origin::Url { url } = &a.origin {
                // Tests serve plain HTTP on loopback (SMS_ALLOW_HTTP); nothing else.
                let test = std::env::var_os("SMS_ALLOW_HTTP").is_some()
                    && url.starts_with("http://127.0.0.1:");
                if !url.starts_with("https://") && !test {
                    return bad(format!("Archive URLs must be HTTPS: {url}"));
                }
            }
        }
        let mut paths = BTreeSet::new();
        for e in &self.files {
            files::relative(&e.path)?;
            if !files::digest_valid(&e.sha256) || !paths.insert(files::fold(&e.path)) {
                return bad(format!("Invalid or duplicate file {}.", e.path));
            }
            // A pack may not write over our journal, the launcher slot, saves or the game's executable.
            let licensed = snapshot::protected(&e.path);
            if snapshot::ignored(&e.path, &BTreeSet::new())
                || snapshot::executable(&e.path) && !licensed
                || licensed && e.source != Source::Base
            {
                return bad(format!("A pack cannot provide {}.", e.path));
            }
            match &e.source {
                Source::Base if !snapshot::protected(&e.path) => {
                    return bad(format!(
                        "Only licensed game files come from the base game: {}",
                        e.path
                    ))
                }
                Source::Host { blob } => {
                    if snapshot::protected(&e.path) {
                        return bad(format!("Licensed game content cannot be sent: {}", e.path));
                    }
                    blob.parse::<iroh_blobs::Hash>()
                        .map_err(|_| Error::new("invalid_pack", "Invalid host object hash."))?;
                }
                Source::Archive { archive, member } => {
                    if !archives.contains(archive.as_str()) || member.is_empty() {
                        return bad(format!("Unknown archive for {}.", e.path));
                    }
                }
                Source::Base => {}
            }
        }
        for (file, sections) in &self.ini {
            if !snapshot::DOCUMENTS
                .iter()
                .any(|d| d.eq_ignore_ascii_case(file))
            {
                return bad(format!("Unexpected INI {file}."));
            }
            for (section, keys) in sections {
                if section.contains(['\n', ']'])
                    || keys
                        .iter()
                        .any(|(k, v)| k.contains(['\n', '=']) || v.contains('\n'))
                {
                    return bad(format!("Invalid INI setting in {file}."));
                }
            }
        }
        Ok(())
    }
    /// The pack's identity: SHA-256 of its canonical JSON.
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(files::hash_bytes(&serde_json::to_vec(self)?))
    }
    pub fn archive(&self, sha256: &str) -> Option<&Archive> {
        self.archives.iter().find(|a| a.sha256 == sha256)
    }
    pub fn host_objects(&self) -> BTreeSet<String> {
        self.files
            .iter()
            .filter_map(|e| match &e.source {
                Source::Host { blob } => Some(blob.clone()),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) fn sample() -> Pack {
    Pack {
        schema: SCHEMA,
        name: "Test".into(),
        runtime: "1.7.104.0".into(),
        plugins: vec!["*A.esp".into()],
        loadorder: vec!["Skyrim.esm".into(), "A.esp".into()],
        ini: Ini::new(),
        archives: vec![Archive {
            sha256: "a".repeat(64),
            size: 1,
            name: "a.zip".into(),
            origin: Origin::Nexus {
                game: "skyrimspecialedition".into(),
                r#mod: 1,
                file: 2,
            },
        }],
        files: vec![
            Entry {
                path: "Data/A.esp".into(),
                size: 1,
                sha256: "b".repeat(64),
                r#mod: Some("A".into()),
                source: Source::Archive {
                    archive: "a".repeat(64),
                    member: "A.esp".into(),
                },
            },
            Entry {
                path: "Data/Skyrim.esm".into(),
                size: 1,
                sha256: "c".repeat(64),
                r#mod: None,
                source: Source::Base,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_valid_pack_has_a_stable_id() {
        let p = sample();
        assert_eq!(p.id().unwrap(), p.clone().id().unwrap());
    }
    #[test]
    fn licensed_files_never_come_from_the_host() {
        let mut p = sample();
        p.files[1].source = Source::Host {
            blob: iroh_blobs::Hash::new(b"x").to_string(),
        };
        assert_eq!(p.validate().unwrap_err().code, "invalid_pack");
    }
    #[test]
    fn a_pack_cannot_touch_the_journal_executables_or_licensed_files() {
        for path in [
            ".skyrim-mod-sync/journal.jsonl",
            "SkyrimSE.exe",
            "SkyrimSE_1.7.104.exe",
            "skyrimselauncher.EXE",
            "Data/Skyrim.esm",
            "Saves/a.ess",
        ] {
            let mut p = sample();
            p.files.remove(1);
            p.files[0].path = path.into();
            assert!(p.validate().is_err(), "{path}");
        }
    }
    #[test]
    fn mod_files_never_claim_to_be_base_game() {
        let mut p = sample();
        p.files[0].source = Source::Base;
        assert!(p.validate().is_err());
    }
    #[test]
    fn plain_http_urls_are_refused() {
        let mut p = sample();
        p.archives[0].origin = Origin::Url {
            url: "http://x/a.zip".into(),
        };
        assert!(p.validate().is_err());
    }
    #[test]
    fn duplicate_paths_differing_in_case_are_refused() {
        let mut p = sample();
        let mut dup = p.files[0].clone();
        dup.path = "data/a.ESP".into();
        p.files.push(dup);
        assert!(p.validate().is_err());
    }
}
