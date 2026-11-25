//! Skyrim's INIs: reading the host's shared settings, writing them into a friend's file in place.
use crate::{files, Result};
use std::{collections::BTreeMap, path::Path};

pub type Sections = BTreeMap<String, BTreeMap<String, String>>;

/// Settings that stay each player's own (screen, controls).
pub fn personal(file: &str, section: &str) -> bool {
    file.eq_ignore_ascii_case("SkyrimPrefs.ini")
        && ["display", "controls", "launcher"].contains(&files::fold(section).as_str())
}

/// Sections and keys as written; a repeated key keeps its last value (as the game does).
pub fn parse(text: &str) -> Sections {
    let mut out = Sections::new();
    let mut section = String::new();
    for l in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if l.is_empty() || l.starts_with(['#', ';']) {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            section = l[1..l.len() - 1].trim().to_owned();
        } else if let Some((k, v)) = l.split_once('=') {
            let keys = out.entry(section.clone()).or_default();
            let k = k.trim();
            keys.retain(|old, _| !old.eq_ignore_ascii_case(k));
            keys.insert(k.to_owned(), v.trim().to_owned());
        }
    }
    out.retain(|_, k| !k.is_empty());
    out
}

/// The shared settings of an INI file (none if it does not exist).
pub fn shared(path: &Path, file: &str) -> Result<Sections> {
    if !path.exists() {
        return Ok(Sections::new());
    }
    let mut s = parse(&String::from_utf8_lossy(
        &std::fs::read(path).map_err(crate::Error::from)?,
    ));
    s.retain(|section, _| !section.is_empty() && !personal(file, section));
    Ok(s)
}

/// `text` with `values` written in: existing keys changed in place, missing ones added to their
/// section, missing sections appended. Everything else of the friend's stays.
pub fn merge(text: &str, values: &Sections) -> String {
    let mut pending: Vec<(String, Vec<(String, String)>)> = values
        .iter()
        .map(|(s, k)| {
            (
                s.clone(),
                k.iter().map(|(a, b)| (a.clone(), b.clone())).collect(),
            )
        })
        .collect();
    let take = |pending: &mut Vec<(String, Vec<(String, String)>)>,
                section: &str,
                key: &str|
     -> Option<String> {
        let (_, keys) = pending
            .iter_mut()
            .find(|(s, _)| s.eq_ignore_ascii_case(section))?;
        let i = keys.iter().position(|(k, _)| k.eq_ignore_ascii_case(key))?;
        Some(keys.remove(i).1)
    };
    let flush = |pending: &mut Vec<(String, Vec<(String, String)>)>,
                 section: &str,
                 out: &mut Vec<String>| {
        if let Some((_, keys)) = pending
            .iter_mut()
            .find(|(s, _)| s.eq_ignore_ascii_case(section))
        {
            out.extend(keys.drain(..).map(|(k, v)| format!("{k}={v}")));
        }
    };
    let mut out = vec![];
    let mut section = String::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            flush(&mut pending, &section, &mut out);
            section = l[1..l.len() - 1].trim().to_owned();
            out.push(line.to_owned());
        } else if let Some((k, _)) = l.split_once('=').filter(|_| !l.starts_with(['#', ';'])) {
            match take(&mut pending, &section, k.trim()) {
                Some(v) => out.push(format!("{}={v}", k.trim())),
                None => out.push(line.to_owned()),
            }
        } else {
            out.push(line.to_owned());
        }
    }
    flush(&mut pending, &section, &mut out);
    for (s, keys) in pending.into_iter().filter(|(_, k)| !k.is_empty()) {
        out.push(format!("[{s}]"));
        out.extend(keys.into_iter().map(|(k, v)| format!("{k}={v}")));
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_changes_in_place_and_keeps_the_rest() {
        let friend = "[General]\nsLanguage=ENGLISH\nuGridsToLoad=5\n\n[Display]\niSize W=2560\n";
        let mut values = Sections::new();
        values
            .entry("general".into())
            .or_default()
            .insert("ugridstoload".into(), "7".into());
        values
            .entry("Archive".into())
            .or_default()
            .insert("bInvalidateOlderFiles".into(), "1".into());
        let merged = merge(friend, &values);
        let read = parse(&merged);
        assert_eq!(
            read["General"]["uGridsToLoad"], "7",
            "the friend's key keeps its spelling"
        );
        assert_eq!(read["General"]["sLanguage"], "ENGLISH");
        assert_eq!(read["Display"]["iSize W"], "2560");
        assert_eq!(read["Archive"]["bInvalidateOlderFiles"], "1");
    }
    #[test]
    fn screen_and_controls_stay_personal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("SkyrimPrefs.ini");
        std::fs::write(
            &p,
            "[Display]\niSize W=1920\n[Grass]\nbAllowCreateGrass=1\n",
        )
        .unwrap();
        let s = shared(&p, "SkyrimPrefs.ini").unwrap();
        assert!(!s.contains_key("Display"));
        assert_eq!(s["Grass"]["bAllowCreateGrass"], "1");
    }
}
