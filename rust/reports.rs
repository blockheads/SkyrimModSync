//! Reports: files a friend sends the host (a game's exported logs, crash zips). The friend picks and
//! confirms what goes; the host accepts or refuses each offer. The list names the default files.
use crate::{files, hash, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// At most this many files per offer, each at most `MAX_SIZE`.
pub const MAX_FILES: usize = 20;
pub const MAX_SIZE: u64 = 2 << 30;
/// `sms send` without files: matches changed in the last week, newest first, at most this many.
const RECENT: Duration = Duration::from_secs(7 * 24 * 3600);
const NEWEST: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportFile {
    pub name: String,
    pub size: u64,
    pub hash: String,
}

/// What a friend asks to send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    /// The friend's name (their user name and a short id of their machine).
    pub from: String,
    pub files: Vec<ReportFile>,
}

impl Offer {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
    /// The offer as the host sees it: who, what, how much.
    pub fn describe(&self) -> String {
        let mut out = format!(
            "{} wants to send you {} files ({}):",
            self.from,
            self.files.len(),
            crate::size(self.bytes())
        );
        for f in &self.files {
            out.push_str(&format!("\n  {}  {}", f.name, crate::size(f.size)));
        }
        out
    }
    /// Refuses names that could leave the friend's folder, and oversized offers.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::new("invalid_offer", m));
        safe_name(&self.from)?;
        if self.files.is_empty() || self.files.len() > MAX_FILES {
            return bad(format!("1 to {MAX_FILES} files per offer."));
        }
        for f in &self.files {
            safe_name(&f.name)?;
            if f.size > MAX_SIZE {
                return bad(format!("{} is over {}.", f.name, crate::size(MAX_SIZE)));
            }
            if f.hash.len() != 64 || !f.hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return bad(format!("{} has no valid hash.", f.name));
            }
        }
        Ok(())
    }
}

/// A plain file name: no folders, no `..`, nothing hidden or odd.
pub fn safe_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || " -_.()+[]".contains(c));
    if ok {
        Ok(())
    } else {
        Err(Error::new(
            "invalid_offer",
            format!("Not a plain file name: {name:?}."),
        ))
    }
}

/// This machine's name for the host's folder: the user name and 4 hex of the user and computer.
pub fn sender() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "friend".into());
    let machine = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    let clean: String = user
        .chars()
        .filter(|c| c.is_alphanumeric() || "-_".contains(*c))
        .take(40)
        .collect();
    let id = blake3::hash(format!("{user}@{}", machine.trim()).as_bytes()).to_hex();
    format!(
        "{}-{}",
        if clean.is_empty() { "friend" } else { &clean },
        &id[..4]
    )
}

/// Is a list's report pattern well formed: `Documents/…` or `Game/…`, relative, `*` only in the last part.
pub fn valid_pattern(p: &str) -> Result<()> {
    let bad = || {
        Error::new(
            "invalid_list",
            format!("reports: {p} must start with Documents/ or Game/."),
        )
    };
    let rest = p
        .strip_prefix("Documents/")
        .or_else(|| p.strip_prefix("Game/"))
        .ok_or_else(bad)?;
    files::relative(&rest.replace('*', "x"))?;
    if rest
        .rsplit_once('/')
        .is_some_and(|(dirs, _)| dirs.contains('*'))
    {
        return Err(Error::new(
            "invalid_list",
            format!("reports: {p}: `*` only in the file name."),
        ));
    }
    Ok(())
}

/// The files the patterns name on this machine: changed in the last week, newest first.
/// `Documents/` is the game's Documents/My Games/Skyrim Special Edition, `Game/` the Skyrim folder.
pub fn pick(patterns: &[String], documents: &Path, game: &Path) -> Vec<PathBuf> {
    let now = SystemTime::now();
    let mut found = vec![];
    for p in patterns {
        let (base, rest) = match (p.strip_prefix("Documents/"), p.strip_prefix("Game/")) {
            (Some(r), _) => (documents, r),
            (_, Some(r)) => (game, r),
            _ => continue,
        };
        let path = base.join(rest);
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            continue;
        };
        let name = name.to_string_lossy().to_lowercase();
        let (before, after) = name.split_once('*').unwrap_or((&name, ""));
        let exact = !name.contains('*');
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            let hit = if exact {
                n == name
            } else {
                n.len() >= before.len() + after.len() && n.starts_with(before) && n.ends_with(after)
            };
            let Ok(meta) = e.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if hit && meta.is_file() && now.duration_since(modified).unwrap_or_default() <= RECENT {
                found.push((modified, e.path()));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.dedup_by(|a, b| a.1 == b.1);
    found.into_iter().take(NEWEST).map(|f| f.1).collect()
}

/// The offer for these files (each hashed).
pub fn offer(paths: &[PathBuf]) -> Result<Offer> {
    let mut files = vec![];
    for p in paths {
        let (size, hash) = hash::file(p)?;
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        files.push(ReportFile { name, size, hash });
    }
    let offer = Offer {
        from: sender(),
        files,
    };
    offer.validate()?;
    Ok(offer)
}

/// Where an accepted file lands: `<received>/<from>/<name>`, or `<name> (2)` and on when another
/// file already has that name.
pub fn destination(received: &Path, from: &str, f: &ReportFile) -> Result<(PathBuf, bool)> {
    let dir = received.join(from);
    let (stem, ext) = match f.name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (f.name.clone(), String::new()),
    };
    for n in 1..1000 {
        let name = if n == 1 {
            f.name.clone()
        } else {
            format!("{stem} ({n}){ext}")
        };
        let path = dir.join(&name);
        if !path.exists() {
            return Ok((path, false));
        }
        if hash::file(&path).is_ok_and(|(s, h)| s == f.size && h == f.hash) {
            return Ok((path, true)); // already here
        }
    }
    Err(Error::new(
        "invalid_offer",
        format!("Too many files named {}.", f.name),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offers_keep_to_plain_names_and_sizes() {
        let file = |name: &str, size| ReportFile {
            name: name.into(),
            size,
            hash: "a".repeat(64),
        };
        let offer = |files| Offer {
            from: "ann-1a2b".into(),
            files,
        };
        assert!(offer(vec![file("SkyrimCoop-2026.zip", 10)])
            .validate()
            .is_ok());
        for bad in ["../x.zip", "a/b.zip", "a\\b.zip", ".hidden", "", "con:x"] {
            assert!(offer(vec![file(bad, 1)]).validate().is_err(), "{bad}");
        }
        assert!(offer(vec![file("big.zip", MAX_SIZE + 1)])
            .validate()
            .is_err());
        assert!(offer(vec![]).validate().is_err());
        assert!(Offer {
            from: "../x".into(),
            files: vec![file("a.zip", 1)]
        }
        .validate()
        .is_err());
    }
    #[test]
    fn patterns_pick_recent_matches_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(docs.join("Mod")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(30 * 24 * 3600);
        for (name, age) in [
            ("Mod-1.zip", 60),
            ("Mod-2.zip", 0),
            ("Mod-old.zip", 0),
            ("other.txt", 0),
        ] {
            let p = docs.join("Mod").join(name);
            std::fs::write(&p, name).unwrap();
            let t = if name == "Mod-old.zip" {
                old
            } else {
                SystemTime::now() - Duration::from_secs(age)
            };
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        let got = pick(&["Documents/Mod/Mod-*.zip".into()], &docs, dir.path());
        let names: Vec<_> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Mod-2.zip", "Mod-1.zip"]);
        assert!(valid_pattern("Documents/Mod/Mod-*.zip").is_ok());
        assert!(valid_pattern("/etc/passwd").is_err());
        assert!(valid_pattern("Documents/../x").is_err());
        assert!(valid_pattern("Game/*/x.zip").is_err());
    }
    #[test]
    fn a_second_file_with_the_same_name_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        let f = |bytes: &[u8]| ReportFile {
            name: "a.zip".into(),
            size: bytes.len() as u64,
            hash: blake3::hash(bytes).to_hex().to_string(),
        };
        let (first, here) = destination(dir.path(), "ann", &f(b"one")).unwrap();
        assert!(!here);
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, b"one").unwrap();
        assert_eq!(
            destination(dir.path(), "ann", &f(b"one")).unwrap(),
            (first, true)
        );
        let (second, _) = destination(dir.path(), "ann", &f(b"two")).unwrap();
        assert!(second.ends_with("ann/a (2).zip"));
    }
}
