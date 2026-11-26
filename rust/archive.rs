//! Mod archives (zip, 7z, rar): which files they hold, by hash, and taking out the ones a pack needs.
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
    Rar,
}

fn kind(path: &Path) -> Result<Kind> {
    let mut magic = [0u8; 6];
    let n = File::open(path)?.read(&mut magic)?;
    match &magic[..n] {
        [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..] => Ok(Kind::Zip),
        [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C] => Ok(Kind::SevenZ),
        [b'R', b'a', b'r', b'!', ..] => Ok(Kind::Rar),
        _ => Err(Error::new(
            "unsupported_archive",
            format!("Not a zip, 7z or rar archive: {}", path.display()),
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

fn rar_error(path: &Path, e: unrar::error::UnrarError) -> Error {
    Error::new("invalid_archive", format!("{}: {e}", path.display()))
}

/// RAR members bigger than this are unpacked to a temporary file instead of memory (unrar has no
/// streaming reader).
const RAR_IN_MEMORY: u64 = 256 << 20;

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
                // Every entry read to its end: a solid block checks each entry's CRC, skipped ones too.
                if let Err(e) = crate::check_cancel()
                    .and_then(|_| each(&normal(entry.name()), entry.size(), reader))
                    .and_then(|_| Ok(std::io::copy(reader, &mut std::io::sink()).map(|_| ())?))
                {
                    failure = Some(e);
                }
                Ok(true)
            })
            .map_err(sevenz_error)?;
            failure.map_or(Ok(()), Err)
        }
        Kind::Rar => {
            let mut open = unrar::Archive::new(path)
                .open_for_processing()
                .map_err(|e| rar_error(path, e))?;
            // Each entry is read (unpacked) or skipped; solid archives unpack skipped ones too.
            while let Some(header) = open.read_header().map_err(|e| rar_error(path, e))? {
                crate::check_cancel()?;
                let entry = header.entry();
                if !entry.is_file() {
                    open = header.skip().map_err(|e| rar_error(path, e))?;
                    continue;
                }
                let name = normal(&entry.filename.to_string_lossy());
                let size = entry.unpacked_size;
                if size <= RAR_IN_MEMORY {
                    let (bytes, next) = header.read().map_err(|e| rar_error(path, e))?;
                    each(&name, size, &mut bytes.as_slice())?;
                    open = next;
                } else {
                    let tmp = tempfile::NamedTempFile::new()?;
                    open = header
                        .extract_to(tmp.path())
                        .map_err(|e| rar_error(path, e))?;
                    each(&name, size, &mut File::open(tmp.path())?)?;
                }
            }
            Ok(())
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
    fn one_member_comes_out_of_a_solid_7z() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..5 {
            std::fs::write(
                src.join(format!("f{i}.txt")),
                format!("file {i}").repeat(1000),
            )
            .unwrap();
        }
        let seven = dir.path().join("a.7z");
        let mut w = sevenz_rust2::ArchiveWriter::create(&seven).unwrap();
        w.push_source_path(&src, |_| true).unwrap(); // solid, as mod archives are
        w.finish().unwrap();
        let m = members(&seven).unwrap();
        assert_eq!(m.len(), 5);
        for pick in [&m[0], &m[2], &m[4]] {
            let mut got = vec![];
            extract(&seven, &[pick.name.clone()].into(), |_, r| {
                r.read_to_end(&mut got)?;
                Ok(())
            })
            .unwrap();
            assert_eq!(blake3::hash(&got).to_hex().to_string(), pick.hash);
        }
    }
    #[test]
    fn lists_and_extracts_a_solid_rar() {
        // rar a -ma5 -s: A.esp ("plugin"), textures/t.dds ("texture")
        let rar = Path::new(env!("CARGO_MANIFEST_DIR")).join("rust/testdata/solid.rar");
        let m = members(&rar).unwrap();
        assert_eq!(m.len(), 2);
        let t = m.iter().find(|m| m.name == "textures/t.dds").unwrap();
        assert_eq!(t.hash, blake3::hash(b"texture").to_hex().to_string());
        let mut got = vec![];
        extract(&rar, &["A.ESP".to_owned()].into(), |_, r| {
            r.read_to_end(&mut got)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(got, b"plugin");
    }
}
