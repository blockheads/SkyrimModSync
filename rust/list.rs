//! The mod list: what the host writes (or a converter will, later). Each mod has one source and a simple
//! install rule; `resolve` turns it into the exact files a pack lists.
use crate::{
    files::{self, fold},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Nexus {
    #[serde(default = "skyrim_se")]
    pub game: String,
    pub r#mod: u64,
    pub file: u64,
}
fn skyrim_se() -> String {
    "skyrimspecialedition".into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Github {
    pub repo: String,
    /// Part of the asset's name, when the release has several archives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mod {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nexus: Option<Nexus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<Github>,
    /// A folder or archive (next to the list, or absolute; `*` in its name: the newest match); the host
    /// sends its files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// Only this folder inside the archive is installed (archives that wrap their contents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Files that go next to SkyrimSE.exe instead of into Data/ (paths inside the mod).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModList {
    pub name: String,
    pub mods: Vec<Mod>,
    /// Active plugins in load order.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Shared INI settings: file -> section -> key -> value.
    #[serde(default, skip_serializing_if = "crate::pack::Ini::is_empty")]
    pub ini: crate::pack::Ini,
    /// Files friends can send you (`sms send`): `Documents/…` or `Game/…`, `*` in the file name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<String>,
}

impl ModList {
    pub fn read(path: &Path) -> Result<Self> {
        let list: Self = files::read_json(path).map_err(|e| {
            Error::new("invalid_list", format!("{}: {}", path.display(), e.message))
        })?;
        for r in &list.reports {
            crate::reports::valid_pattern(r)?;
        }
        for m in &list.mods {
            let sources = [
                m.nexus.is_some(),
                m.url.is_some(),
                m.github.is_some(),
                m.local.is_some(),
            ];
            if sources.iter().filter(|s| **s).count() != 1 {
                return Err(Error::new(
                    "invalid_list",
                    format!("{} needs exactly one of nexus, url, github, local.", m.name),
                ));
            }
            if let Some(g) = &m.github {
                if !crate::net::valid_repo(&g.repo) {
                    return Err(Error::new(
                        "invalid_list",
                        format!("{}: expected github owner/repo.", m.name),
                    ));
                }
            }
        }
        Ok(list)
    }
    pub fn write(&self, path: &Path) -> Result<()> {
        files::write_atomic(
            path,
            (serde_json::to_string_pretty(self)? + "\n").as_bytes(),
        )
    }
}

/// A path inside the mod with `folder` (if any) taken off; None for paths outside it.
fn inside(m: &Mod, rel: &str) -> Option<String> {
    let Some(f) = &m.folder else {
        return Some(rel.to_owned());
    };
    let prefix = format!("{}/", fold(f.trim_matches('/')));
    let n = prefix.len();
    rel.get(..n)
        .filter(|p| fold(p) == prefix)
        .map(|_| rel[n..].to_owned())
}

/// Whether a mod keeps its game files under a Data/ folder (then nothing else outside it is installed).
pub fn has_data<'a>(m: &Mod, names: impl Iterator<Item = &'a str>) -> bool {
    names
        .filter_map(|n| inside(m, n))
        .any(|r| fold(&r).starts_with("data/"))
}

/// Where a file of a mod lands in the game folder: `root` files next to SkyrimSE.exe, the rest in
/// Data/; None for installer metadata, paths outside `folder`, or extras beside a Data/ folder.
pub fn target(m: &Mod, rel: &str, has_data: bool) -> Option<String> {
    let rel = inside(m, rel)?;
    let folded = fold(&rel);
    if m.root.iter().any(|r| fold(r) == folded) {
        return Some(rel);
    }
    if folded.starts_with("fomod/") {
        return None;
    }
    if folded.starts_with("data/") {
        return Some(format!("Data/{}", &rel[5..]));
    }
    (!has_data).then(|| format!("Data/{rel}"))
}

/// A Nexus file link (the Files tab, or a file's own link) → (game, mod, file).
pub fn parse_nexus_link(link: &str) -> Option<(String, u64, u64)> {
    let rest = link.split("nexusmods.com/").nth(1)?;
    let mut parts = rest.split(['/', '?', '&', '#']);
    let game = parts.next()?.to_owned();
    if parts.next()? != "mods" {
        return None;
    }
    let r#mod = parts.next()?.parse().ok()?;
    let file = link
        .split(['?', '&'])
        .find_map(|p| p.strip_prefix("file_id="))?
        .parse()
        .ok()?;
    Some((game, r#mod, file))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn m(folder: Option<&str>, root: &[&str]) -> Mod {
        Mod {
            name: "m".into(),
            nexus: None,
            url: None,
            github: None,
            local: Some(".".into()),
            folder: folder.map(Into::into),
            root: root.iter().map(|r| r.to_string()).collect(),
        }
    }
    #[test]
    fn install_rules() {
        // A plain mod: everything into Data/.
        assert_eq!(
            target(&m(None, &[]), "A.esp", false).as_deref(),
            Some("Data/A.esp")
        );
        assert_eq!(target(&m(None, &[]), "fomod/info.xml", false), None);
        // SKSE's layout: a wrapping folder, root files, Data/, and source code that is skipped.
        let skse = m(Some("skse64_2_03_01"), &["skse64_loader.exe"]);
        let names = [
            "skse64_2_03_01/skse64_loader.exe",
            "skse64_2_03_01/Data/Scripts/a.pex",
            "skse64_2_03_01/src/x.cpp",
        ];
        let data = has_data(&skse, names.into_iter());
        assert!(data);
        assert_eq!(
            target(&skse, names[0], data).as_deref(),
            Some("skse64_loader.exe")
        );
        assert_eq!(
            target(&skse, names[1], data).as_deref(),
            Some("Data/Scripts/a.pex")
        );
        assert_eq!(target(&skse, names[2], data), None);
        assert_eq!(target(&skse, "other/thing.esp", data), None);
    }
    #[test]
    fn nexus_links() {
        assert_eq!(
            parse_nexus_link(
                "https://www.nexusmods.com/skyrimspecialedition/mods/272?tab=files&file_id=729409"
            ),
            Some(("skyrimspecialedition".into(), 272, 729409))
        );
        assert_eq!(
            parse_nexus_link("https://www.nexusmods.com/skyrimspecialedition/mods/272"),
            None
        );
    }
}
