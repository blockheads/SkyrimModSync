//! GitHub releases as a mod's source: the newest release's archive, resolved when the host publishes
//! (the pack pins that asset's URL and hash, so friends get exactly what the host has).
use crate::{net, Error, Result};
use serde_json::Value;

fn base() -> String {
    std::env::var("SMS_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into())
}

/// "owner/repo", nothing else.
pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && !p.starts_with('.')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// The newest release's (prereleases included) archive: the asset whose name contains `asset`, else
/// its only .zip or .7z. Returns (download URL, asset name).
pub fn latest_asset(repo: &str, asset: Option<&str>) -> Result<(String, String)> {
    if !valid_repo(repo) {
        return Err(Error::new(
            "invalid_source",
            format!("Expected owner/repo, got {repo}."),
        ));
    }
    let releases: Vec<Value> = net::client()?
        .get(format!("{}/repos/{repo}/releases?per_page=10", base()))
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json()?;
    let release = releases
        .iter()
        .find(|r| r["draft"] != true)
        .ok_or_else(|| Error::new("no_release", format!("{repo} has no published release.")))?;
    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let named = |a: &Value| a["name"].as_str().unwrap_or_default().to_owned();
    let candidates: Vec<&Value> = assets
        .iter()
        .filter(|a| match asset {
            Some(p) => named(a).contains(p),
            None => {
                let n = named(a).to_lowercase();
                n.ends_with(".zip") || n.ends_with(".7z")
            }
        })
        .collect();
    match candidates.as_slice() {
        [one] => Ok((
            one["browser_download_url"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            named(one),
        )),
        [] => Err(Error::new(
            "no_asset",
            format!(
                "{repo}'s release {} has no matching archive.",
                release["tag_name"].as_str().unwrap_or("?")
            ),
        )),
        many => Err(Error::new(
            "ambiguous_asset",
            format!(
                "{repo}'s release has several archives ({}): name one.",
                many.iter().map(|a| named(a)).collect::<Vec<_>>().join(", ")
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repos_are_owner_and_name_only() {
        assert!(valid_repo("blockheads/SkyrimCoop"));
        assert!(!valid_repo("blockheads"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("../x"));
        assert!(!valid_repo("a/b?x=1"));
    }
}
