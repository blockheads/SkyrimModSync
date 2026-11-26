//! The pack: what a friend's game must hold. Files by path and BLAKE3, where each comes from, the load
//! order and the shared INI settings. Public data: no host paths, no credentials.
use crate::{files, rules, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: u32 = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum From {
    /// Licensed game content: the friend's own copy must match.
    Base,
    /// Sent by the host.
    Host,
    /// A member of an archive in `Pack::archives` (the host sends it if the download fails).
    Archive { archive: String, member: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the game folder, the host's casing ("Data/foo.esp").
    pub path: String,
    pub size: u64,
    pub hash: String,
    /// The MO2 mod it came from (display only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#mod: Option<String>,
    pub from: From,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Nexus { game: String, r#mod: u64, file: u64 },
    Url { url: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Archive {
    pub hash: String,
    pub size: u64,
    pub name: String,
    pub origin: Origin,
}

/// Shared INI settings: file -> section -> key -> value.
pub type Ini = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pack {
    pub schema: u32,
    pub name: String,
    /// The host's iroh endpoint id: where host files come from.
    pub host: String,
    /// plugins.txt lines ("*Foo.esp" active).
    pub plugins: Vec<String>,
    pub loadorder: Vec<String>,
    pub ini: Ini,
    pub archives: Vec<Archive>,
    pub files: Vec<Entry>,
    /// Files friends can send the host (`sms send`): `Documents/…` or `Game/…`, `*` in the name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<String>,
}

impl Pack {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::new("invalid_pack", m));
        if self.schema != SCHEMA {
            return bad(format!(
                "This pack needs another sms version (schema {}).",
                self.schema
            ));
        }
        for r in &self.reports {
            crate::reports::valid_pattern(r)?;
        }
        for p in self.plugins.iter().chain(&self.loadorder) {
            rules::plugin_name(p.trim_start_matches('*'))?;
        }
        let archives: BTreeSet<&str> = self.archives.iter().map(|a| a.hash.as_str()).collect();
        let mut paths = BTreeSet::new();
        for e in &self.files {
            files::relative(&e.path)?;
            if !paths.insert(files::fold(&e.path)) {
                return bad(format!("{} is listed twice.", e.path));
            }
            // Never our journal, saves, logs or an executable; licensed files only as base.
            let licensed = rules::protected(&e.path);
            if rules::ignored(&e.path)
                || (rules::executable(&e.path) && !licensed)
                || (licensed != (e.from == From::Base))
            {
                return bad(format!("A pack cannot provide {} that way.", e.path));
            }
            if let From::Archive { archive, .. } = &e.from {
                if !archives.contains(archive.as_str()) {
                    return bad(format!("{} names an unknown archive.", e.path));
                }
            }
        }
        for file in self.ini.keys() {
            if !rules::DOCUMENTS
                .iter()
                .any(|d| d.eq_ignore_ascii_case(file))
            {
                return bad(format!("Unexpected INI {file}."));
            }
        }
        Ok(())
    }
    pub fn archive(&self, hash: &str) -> Option<&Archive> {
        self.archives.iter().find(|a| a.hash == hash)
    }
    /// The total of non-base bytes (what a fresh friend fetches at most).
    pub fn bytes(&self) -> u64 {
        self.files
            .iter()
            .filter(|e| e.from != From::Base)
            .map(|e| e.size)
            .sum()
    }
}

#[cfg(test)]
pub(crate) fn sample() -> Pack {
    let entry = |path: &str, from| Entry {
        path: path.into(),
        size: 1,
        hash: "h".repeat(64),
        r#mod: None,
        from,
    };
    Pack {
        schema: SCHEMA,
        name: "Test".into(),
        host: String::new(),
        plugins: vec!["*A.esp".into()],
        loadorder: vec!["Skyrim.esm".into(), "A.esp".into()],
        reports: vec![],
        ini: Ini::new(),
        archives: vec![Archive {
            hash: "a".repeat(64),
            size: 1,
            name: "a.zip".into(),
            origin: Origin::Nexus {
                game: "skyrimspecialedition".into(),
                r#mod: 1,
                file: 2,
            },
        }],
        files: vec![
            entry(
                "Data/A.esp",
                From::Archive {
                    archive: "a".repeat(64),
                    member: "A.esp".into(),
                },
            ),
            entry("Data/Skyrim.esm", From::Base),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_sample_pack_is_valid() {
        sample().validate().unwrap();
    }
    #[test]
    fn licensed_files_only_as_base_and_mod_files_never() {
        let mut p = sample();
        p.files[1].from = From::Host;
        assert!(p.validate().is_err());
        let mut p = sample();
        p.files[0].from = From::Base;
        assert!(p.validate().is_err());
    }
    #[test]
    fn a_pack_cannot_touch_the_journal_executables_saves_or_repeat_a_path() {
        for path in [
            ".skyrim-mod-sync/journal.json",
            "SkyrimSE_1.7.104.exe",
            "skyrimselauncher.EXE",
            "Saves/a.ess",
            "data/a.ESP",
        ] {
            let mut p = sample();
            p.files.push(Entry {
                path: path.into(),
                size: 1,
                hash: "h".repeat(64),
                r#mod: None,
                from: From::Host,
            });
            assert!(p.validate().is_err(), "{path}");
        }
    }
}
