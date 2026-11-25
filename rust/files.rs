use crate::{check_cancel, Error, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_JSON: u64 = 64 * 1024 * 1024;
pub const MAX_FILES: usize = 500_000;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
pub fn fold(s: &str) -> String {
    s.to_lowercase()
}
pub fn relative(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 2048
        || s.split('/').any(|p| {
            let stem = fold(p.split('.').next().unwrap_or_default());
            p.is_empty()
                || p == "."
                || p == ".."
                || p.ends_with(['.', ' '])
                || p.chars()
                    .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
                || ["con", "prn", "aux", "nul"].contains(&stem.as_str())
                || ((stem.starts_with("com") || stem.starts_with("lpt"))
                    && stem.len() == 4
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
    {
        return Err(Error::new(
            "invalid_path",
            format!("Unsafe Windows-relative path: {s:?}"),
        ));
    }
    Ok(())
}
pub fn digest_valid(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn sha_reader(mut r: impl Read) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        check_cancel()?;
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn sha(path: &Path) -> Result<String> {
    regular(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    sha_reader(options.open(path)?)
}
pub fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn regular(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path).map_err(|e| {
        Error::new(
            "missing_file",
            format!(
                "Cannot read {}: {e}. Correct the selected path and retry.",
                path.display()
            ),
        )
    })?;
    if !m.is_file() {
        return Err(Error::new(
            "unsafe_file",
            format!("Expected ordinary file: {}", path.display()),
        ));
    }
    Ok(())
}
pub fn root(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path)
        .map_err(|e| {
            Error::new(
                "missing_path",
                format!(
                    "Cannot access {}: {e}. Correct the selected path and retry.",
                    path.display()
                ),
            )
        })?
        .file_type()
        .is_symlink()
    {
        return Err(Error::new(
            "path_escape",
            "Selected root cannot be a symbolic link.",
        ));
    }
    Ok(path.canonicalize()?)
}
pub fn locate(root: &Path, rel: &str) -> Result<Option<PathBuf>> {
    relative(rel)?;
    let mut current = root.to_path_buf();
    for part in rel.split('/') {
        if !current.is_dir() {
            return Ok(None);
        }
        let matches = fs::read_dir(&current)?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| fold(&e.file_name().to_string_lossy()) == fold(part))
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(Error::new(
                "case_collision",
                format!("Ambiguous path: {rel}"),
            ));
        }
        let Some(entry) = matches.first() else {
            return Ok(None);
        };
        current = entry.path();
        if entry.file_type()?.is_symlink() {
            return Err(Error::new("path_escape", format!("Symbolic link in {rel}")));
        }
    }
    if current.is_file() {
        Ok(Some(current))
    } else {
        Ok(None)
    }
}
pub fn require(root: &Path, rel: &str) -> Result<PathBuf> {
    locate(root, rel)?.ok_or_else(|| Error::new("missing_file", format!("Missing file: {rel}")))
}
/// Resolve output directories with Windows casing before writing on Linux.
/// Mixed inline/archive sources must share one physical directory tree.
pub fn destination(root: &Path, rel: &str) -> Result<PathBuf> {
    relative(rel)?;
    fs::create_dir_all(root)?;
    let mut current = self::root(root)?;
    let parts = rel.split('/').collect::<Vec<_>>();
    for (i, part) in parts.iter().enumerate() {
        let matches = fs::read_dir(&current)?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| fold(&e.file_name().to_string_lossy()) == fold(part))
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(Error::new(
                "case_collision",
                format!("Ambiguous output: {rel}"),
            ));
        }
        let last = i + 1 == parts.len();
        if let Some(entry) = matches.first() {
            let ty = entry.file_type()?;
            if ty.is_symlink() || (!last && !ty.is_dir()) || (last && !ty.is_file()) {
                return Err(Error::new("path_escape", format!("Unsafe output: {rel}")));
            }
            current = entry.path();
        } else {
            current.push(part);
            if !last {
                fs::create_dir(&current)?;
            }
        }
    }
    Ok(current)
}
pub fn inventory(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    fn walk(root: &Path, rel: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
        check_cancel()?;
        let mut names = BTreeMap::new();
        for e in fs::read_dir(root.join(rel))? {
            let e = e?;
            let name = e
                .file_name()
                .into_string()
                .map_err(|_| Error::new("invalid_path", "File names must be Unicode."))?;
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            relative(&path)?;
            if names.insert(fold(&name), name).is_some() {
                return Err(Error::new(
                    "case_collision",
                    format!("Ambiguous directory: {rel}"),
                ));
            }
            let ty = e.file_type()?;
            if ty.is_symlink() {
                return Err(Error::new("path_escape", format!("Symbolic link: {path}")));
            }
            if ty.is_dir() {
                walk(root, &path, out)?;
            } else if ty.is_file() {
                out.insert(path, e.path());
            } else {
                return Err(Error::new(
                    "unsafe_file",
                    format!("Not an ordinary file: {path}"),
                ));
            }
            if out.len() > MAX_FILES {
                return Err(Error::new("too_many_files", "More than 500,000 files."));
            }
        }
        Ok(())
    }
    let root = self::root(root)?;
    if !root.is_dir() {
        return Err(Error::new(
            "missing_directory",
            format!("Select a folder rather than a file: {}", root.display()),
        ));
    }
    let mut out = BTreeMap::new();
    walk(&root, "", &mut out)?;
    Ok(out)
}
pub fn identity(name: String, path: &Path) -> Result<Identity> {
    regular(path)?;
    let before = fs::metadata(path)?;
    let sha256 = sha(path)?;
    let after = fs::metadata(path)?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(Error::new(
            "source_changed",
            format!("File changed while hashing: {name}. Stop Skyrim and retry."),
        ));
    }
    Ok(Identity {
        path: name,
        size: after.len(),
        sha256,
    })
}
pub fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            "size_limit",
            "File exceeds its supported size limit.",
        ));
    }
    Ok(bytes)
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&bounded(path, MAX_JSON)?)?)
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| Error::new("invalid_path", "Missing parent"))?,
    )?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer(&mut temp, value)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| Error::from(e.error))?;
    sync_dir(path.parent().unwrap())?;
    Ok(())
}
pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    let _ = path;
    Ok(())
}
pub fn copy(src: &Path, dst: &Path) -> Result<()> {
    regular(src)?;
    fs::create_dir_all(dst.parent().unwrap())?;
    if dst
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(Error::new(
            "path_escape",
            "Copy destination cannot be a symbolic link.",
        ));
    }
    let mut input = fs::OpenOptions::new();
    input.read(true);
    let mut output = fs::OpenOptions::new();
    output.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        input.custom_flags(libc::O_NOFOLLOW);
        output.custom_flags(libc::O_NOFOLLOW);
    }
    let mut from = input.open(src)?;
    let mut to = output.open(dst)?;
    let mut buf = vec![0; 1024 * 1024];
    loop {
        check_cancel()?;
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n])?;
    }
    to.sync_all()?;
    Ok(())
}
pub fn separate(a: &Path, b: &Path) -> Result<()> {
    if a.starts_with(b) || b.starts_with(a) {
        Err(Error::new(
            "overlapping_paths",
            "Base, managed storage and download cache must be separate folders.",
        ))
    } else {
        Ok(())
    }
}
pub struct Lock(File);
impl Lock {
    pub fn take(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&f).map_err(|_| {
            Error::new(
                "busy",
                "Another operation owns this target; wait for it to finish.",
            )
        })?;
        Ok(Self(f))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
pub fn space(path: &Path, required: u64) -> Result<()> {
    if fs2::available_space(path)? < required {
        return Err(Error::new(
            "disk_full",
            format!(
                "Need at least {required} bytes free in {}. Choose another folder or free space.",
                path.display()
            ),
        ));
    }
    Ok(())
}
