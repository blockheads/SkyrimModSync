//! Mod archives (zip, 7z): which files they hold, by hash, and taking out the ones a pack needs.
//! RAR is not supported: a mod installed from a RAR archive is sent by the host instead.
use crate::{files, Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs::File, io::Read, path::Path};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// '/'-separated, as stored.
    pub name: String,
    pub size: u64,
    pub hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Zip,
    SevenZ,
}

fn kind(path: &Path) -> Result<Kind> {
    let mut magic = [0u8; 6];
    let n = File::open(path)?.read(&mut magic)?;
    match &magic[..n] {
        [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..] => Ok(Kind::Zip),
        [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C] => Ok(Kind::SevenZ),
        [b'R', b'a', b'r', b'!', ..] => Err(Error::new(
            "unsupported_archive",
            "RAR archives are not supported.",
        )),
        _ => Err(Error::new(
            "unsupported_archive",
            format!("Not a zip or 7z archive: {}", path.display()),
        )),
    }
}

fn normal(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn sevenz_error(e: sevenz_rust2::Error) -> Error {
    Error::new("invalid_archive", e.to_string())
}

/// Calls `each` with every file member and a reader over its bytes, in archive order.
fn walk(path: &Path, mut each: impl FnMut(&str, u64, &mut dyn Read) -> Result<()>) -> Result<()> {
    match kind(path)? {
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(File::open(path)?)?;
            for i in 0..z.len() {
                crate::check_cancel()?;
                let mut f = z.by_index(i)?;
                if f.is_dir() {
                    continue;
                }
                let name = normal(f.name());
                let size = f.size();
                each(&name, size, &mut f)?;
            }
            Ok(())
        }
        Kind::SevenZ => {
            let mut r = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())
                .map_err(sevenz_error)?;
            let mut failure = None;
            r.for_each_entries(|entry, reader| {
                if entry.is_directory() || failure.is_some() {
                    return Ok(true);
                }
                if let Err(e) = crate::check_cancel()
                    .and_then(|_| each(&normal(entry.name()), entry.size(), reader))
                {
                    failure = Some(e);
                }
                Ok(true)
            })
            .map_err(sevenz_error)?;
            failure.map_or(Ok(()), Err)
        }
    }
}

/// Every file in the archive with its size and BLAKE3 (reads the whole archive).
pub fn members(path: &Path) -> Result<Vec<Member>> {
    let mut out = vec![];
    walk(path, |name, _, r| {
        let mut h = blake3::Hasher::new();
        std::io::copy(r, &mut h)?;
        out.push(Member {
            name: name.into(),
            size: h.count(),
            hash: h.finalize().to_hex().to_string(),
        });
        Ok(())
    })?;
    Ok(out)
}

/// Hands each wanted member (by name, compared without case) to `sink`.
pub fn extract(
    path: &Path,
    wanted: &BTreeSet<String>,
    mut sink: impl FnMut(&str, &mut dyn Read) -> Result<()>,
) -> Result<()> {
    let wanted = wanted
        .iter()
        .map(|w| files::fold(&normal(w)))
        .collect::<BTreeSet<_>>();
    let mut found = BTreeSet::new();
    walk(path, |name, _, r| {
        let key = files::fold(name);
        if wanted.contains(&key) && found.insert(key) {
            sink(name, r)?;
        }
        Ok(())
    })?;
    if let Some(missing) = wanted.difference(&found).next() {
        return Err(Error::new(
            "archive_member_missing",
            format!("{} has no {missing}.", path.display()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lists_and_extracts_zip_and_7z_members() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("textures")).unwrap();
        std::fs::write(src.join("A.esp"), b"plugin").unwrap();
        std::fs::write(src.join("textures/t.dds"), b"texture").unwrap();
        let seven = dir.path().join("a.7z");
        sevenz_rust2::compress_to_path(&src, &seven).unwrap();
        let zip_path = dir.path().join("a.zip");
        let mut z = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        for (n, b) in [("A.esp", &b"plugin"[..]), ("textures\\t.dds", b"texture")] {
            z.start_file(n, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut z, b).unwrap();
        }
        z.finish().unwrap();
        for a in [&seven, &zip_path] {
            let m = members(a).unwrap();
            let t = m
                .iter()
                .find(|m| m.name.ends_with("textures/t.dds"))
                .unwrap();
            assert_eq!(t.hash, blake3::hash(b"texture").to_hex().to_string());
            let mut got = vec![];
            extract(a, &[t.name.to_uppercase()].into(), |_, r| {
                r.read_to_end(&mut got)?;
                Ok(())
            })
            .unwrap();
            assert_eq!(got, b"texture");
        }
    }
    #[test]
    fn rar_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.rar");
        std::fs::write(&p, b"Rar!\x1a\x07\x00").unwrap();
        assert_eq!(members(&p).unwrap_err().code, "unsupported_archive");
    }
}
