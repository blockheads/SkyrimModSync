//! Load and Unload: putting a synced pack into the game folder and taking it out again, Vortex-style.
//! Every change is written to a journal before it is made, so an interrupted load (a crash, a killed
//! process) is undone by the next command and the game folder always comes back as it was.
use crate::{
    files::{self, fold},
    hashes, ini,
    pack::{Pack, Source},
    snapshot,
    store::Store,
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

const DIR: &str = ".skyrim-mod-sync";
const LAUNCHER: &str = "SkyrimSELauncher.exe";
const LOADER: &str = "skse64_loader.exe";
/// Files a mod may rewrite while the game runs: copied, never hardlinked to the store.
const COPY: [&str; 9] = [
    "ini", "json", "toml", "txt", "yaml", "yml", "xml", "cfg", "log",
];

/// The folders a load touches.
#[derive(Clone, Debug)]
pub struct Game {
    pub root: PathBuf,
    /// AppData/Local/Skyrim Special Edition.
    pub local: PathBuf,
    /// Documents/My Games/Skyrim Special Edition.
    pub documents: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Op {
    Begin {
        pack: String,
        name: String,
        sequence: u64,
    },
    /// A folder we made (game-relative).
    Mkdir {
        path: String,
    },
    /// A file of the friend's moved out of the way (game-relative, on-disk casing).
    Aside {
        path: String,
    },
    /// A pack file put in place; `original` when a file of the friend's was there.
    Place {
        path: String,
        original: bool,
    },
    /// A settings file outside the game folder rewritten; `backup` names its copy.
    User {
        file: PathBuf,
        original: bool,
        backup: String,
    },
    Loaded,
}

/// What the game folder holds now.
#[derive(Debug, PartialEq)]
pub enum State {
    Clean,
    /// A load or unload that did not finish: the next command undoes it.
    Interrupted,
    Loaded {
        pack: String,
        name: String,
        sequence: u64,
    },
}

impl Game {
    fn dir(&self) -> PathBuf {
        self.root.join(DIR)
    }
    fn journal(&self) -> PathBuf {
        self.dir().join("journal.jsonl")
    }
    fn backup(&self, rel: &str) -> PathBuf {
        self.dir().join("backup/game").join(rel)
    }
    fn ops(&self) -> Result<Vec<Op>> {
        let p = self.journal();
        if !p.is_file() {
            return Ok(vec![]);
        }
        let mut ops = vec![];
        for line in BufReader::new(File::open(p)?).lines() {
            let line = line?;
            match serde_json::from_str(&line) {
                Ok(op) => ops.push(op),
                // A torn last line from a crash mid-write: what it described never started.
                Err(_) => break,
            }
        }
        Ok(ops)
    }
    pub fn state(&self) -> Result<State> {
        let ops = self.ops()?;
        Ok(match (ops.first(), ops.last()) {
            (None, _) => {
                if self.journal().exists() {
                    State::Interrupted
                } else {
                    State::Clean
                }
            }
            (
                Some(Op::Begin {
                    pack,
                    name,
                    sequence,
                }),
                Some(Op::Loaded),
            ) => State::Loaded {
                pack: pack.clone(),
                name: name.clone(),
                sequence: *sequence,
            },
            _ => State::Interrupted,
        })
    }

    /// Where the friend's own files are now: their backups for what a loaded pack replaced.
    pub fn originals(&self) -> Result<Originals> {
        let mut moved = std::collections::BTreeMap::new();
        for op in self.ops()? {
            match op {
                Op::Aside { path }
                | Op::Place {
                    path,
                    original: true,
                } => {
                    let b = self.backup(&path);
                    moved.insert(fold(&path), b.is_file().then_some(b));
                }
                Op::Place {
                    path,
                    original: false,
                } => {
                    moved.insert(fold(&path), None);
                }
                _ => {}
            }
        }
        Ok(Originals {
            root: self.root.clone(),
            moved,
        })
    }
}

/// The friend's own copy of each path, wherever it is while a pack is loaded.
pub struct Originals {
    root: PathBuf,
    moved: std::collections::BTreeMap<String, Option<PathBuf>>,
}
impl Originals {
    pub fn get(&self, rel: &str) -> Result<Option<PathBuf>> {
        match self.moved.get(&fold(rel)) {
            Some(p) => Ok(p.clone()),
            None => files::locate(&self.root, rel),
        }
    }
}

struct Journal {
    file: File,
}
impl Journal {
    fn create(game: &Game) -> Result<Self> {
        fs::create_dir_all(game.dir())?;
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(game.journal())?;
        Ok(Self { file })
    }
    /// Durable before the change it describes is made.
    fn write(&mut self, op: &Op) -> Result<()> {
        let mut line = serde_json::to_vec(op)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()?;
        Ok(())
    }
}

/// `rel` under `root`, matching existing folders without case and journaling the ones made.
fn resolve(root: &Path, rel: &str, journal: &mut Journal) -> Result<(String, PathBuf)> {
    files::relative(rel)?;
    let parts = rel.split('/').collect::<Vec<_>>();
    let mut actual = vec![];
    let mut current = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        let found = if current.is_dir() {
            fs::read_dir(&current)?
                .filter_map(|e| e.ok())
                .find(|e| fold(&e.file_name().to_string_lossy()) == fold(part))
                .map(|e| e.file_name().to_string_lossy().into_owned())
        } else {
            None
        };
        let name = found.unwrap_or_else(|| part.to_string());
        actual.push(name.clone());
        current.push(&name);
        if !last && !current.is_dir() {
            journal.write(&Op::Mkdir {
                path: actual.join("/"),
            })?;
            fs::create_dir(&current)?;
        }
    }
    Ok((actual.join("/"), current))
}

fn link_or_copy(object: &Path, dest: &Path) -> Result<()> {
    let ext = dest
        .extension()
        .map(|e| fold(&e.to_string_lossy()))
        .unwrap_or_default();
    if COPY.contains(&ext.as_str()) || fs::hard_link(object, dest).is_err() {
        files::copy(object, dest)?;
    }
    Ok(())
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to.parent().unwrap())?;
    fs::rename(from, to)?;
    Ok(())
}

/// Every file (or link) under `dir`, relative, '/'-separated; links are not followed.
fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
    for e in fs::read_dir(dir.join(rel))? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if e.file_type()?.is_dir() {
            walk(dir, &path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn find(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    files::locate(dir, name)
}

/// Writes a settings file outside the game folder, journaled with a copy of the friend's.
fn write_user(
    game: &Game,
    journal: &mut Journal,
    n: &mut usize,
    dir: &Path,
    name: &str,
    text: &str,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let file = find(dir, name)?.unwrap_or_else(|| dir.join(name));
    let original = file.is_file();
    *n += 1;
    let backup = format!("user/{n}-{name}");
    journal.write(&Op::User {
        file: file.clone(),
        original,
        backup: backup.clone(),
    })?;
    if original {
        let b = game.dir().join("backup").join(&backup);
        fs::create_dir_all(b.parent().unwrap())?;
        // Whole or absent: a crash mid-copy must not leave a torn backup to restore.
        let part = b.with_extension("part");
        fs::copy(&file, &part)?;
        File::open(&part)?.sync_all()?;
        fs::rename(&part, &b)?;
    }
    let mut out = File::create(&file)?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    Ok(())
}

/// The friend's INI text with the pack's settings written in (the game's defaults if they have none).
fn ini_text(game: &Game, name: &str, values: &ini::Sections) -> Result<String> {
    let current = find(&game.documents, name)?;
    let default = match fold(name).as_str() {
        "skyrim.ini" => files::locate(&game.root, "Skyrim_Default.ini")?,
        "skyrimprefs.ini" => files::locate(&game.root, "High.ini")?,
        _ => None,
    };
    let text = match current.or(default) {
        Some(p) => String::from_utf8_lossy(&files::bounded(&p, 4 * 1024 * 1024)?).into_owned(),
        None => String::new(),
    };
    Ok(ini::merge(&text, values))
}

/// Checks the store and the friend's game hold everything the pack needs.
pub fn missing(
    game: &Game,
    store: &Store,
    pack: &Pack,
    cache: &mut hashes::Cache,
) -> Result<Vec<String>> {
    let originals = game.originals()?;
    let mut out = vec![];
    for e in &pack.files {
        let have_original = match originals.get(&e.path)? {
            Some(p) => cache.holds(&p, &e.sha256)?,
            None => false,
        };
        if !have_original && (e.source == Source::Base || !store.has(&e.sha256, e.size)) {
            out.push(e.path.clone());
        }
    }
    Ok(out)
}

/// Puts the pack into the game. The store must hold it completely (`sms sync`).
pub fn load(game: &Game, store: &Store, pack: &Pack, sequence: u64) -> Result<Value> {
    let id = pack.id()?;
    match game.state()? {
        State::Loaded { pack: p, .. } if p == id => {
            return Ok(json!({"state": "loaded", "pack": id, "changed": 0}))
        }
        State::Clean => {}
        _ => {
            unload(game)?;
        }
    }
    let mut cache = hashes::Cache::open(&store.root.join("hashes.json"))?;
    let missing = missing(game, store, pack, &mut cache)?;
    if !missing.is_empty() {
        return Err(Error::new(
            "pack_incomplete",
            format!(
                "{} files are not synced yet (first: {}); run `sms sync`.",
                missing.len(),
                missing[0]
            ),
        ));
    }
    let loader = pack.files.iter().find(|e| fold(&e.path) == fold(LOADER));
    let wanted = pack
        .files
        .iter()
        .map(|e| fold(&e.path))
        .collect::<BTreeSet<_>>();

    let mut journal = Journal::create(game)?;
    journal.write(&Op::Begin {
        pack: id.clone(),
        name: pack.name.clone(),
        sequence,
    })?;
    let mut set_aside = 0;
    if let Some(data) = files::locate_dir(&game.root, "Data")? {
        let mut found = vec![];
        walk(&data, "", &mut found)?;
        let data_name = data.file_name().unwrap().to_string_lossy().into_owned();
        for rel in found {
            let path = format!("{data_name}/{rel}");
            if !wanted.contains(&fold(&path)) && !snapshot::stays(&path) {
                journal.write(&Op::Aside { path: path.clone() })?;
                move_file(&game.root.join(&path), &game.backup(&path))?;
                set_aside += 1;
            }
        }
    }
    let mut placed = 0;
    let mut placements: Vec<(&str, &str)> = pack
        .files
        .iter()
        .filter(|e| e.source != Source::Base)
        .map(|e| (e.path.as_str(), e.sha256.as_str()))
        .collect();
    if let Some(l) = loader {
        placements.push((LAUNCHER, l.sha256.as_str()));
    }
    for (path, sha256) in placements {
        // The store holds what the game lacked; a file the game already had (SKSE's loader in the host's
        // own game folder) is taken from the game.
        let source = match store.object(sha256) {
            o if o.is_file() => o,
            _ => match files::locate(&game.root, if path == LAUNCHER { LOADER } else { path })? {
                Some(own) if cache.holds(&own, sha256)? => own,
                _ => {
                    return Err(Error::new(
                        "pack_incomplete",
                        format!("{path} is not synced; run `sms sync`."),
                    ))
                }
            },
        };
        let current = files::locate(&game.root, path)?;
        if path != LAUNCHER {
            if let Some(c) = &current {
                if cache.holds(c, sha256)? {
                    continue;
                }
            }
        }
        let (actual, dest) = resolve(&game.root, path, &mut journal)?;
        journal.write(&Op::Place {
            path: actual.clone(),
            original: current.is_some(),
        })?;
        if current.is_some() {
            move_file(&dest, &game.backup(&actual))?;
        }
        link_or_copy(&source, &dest)?;
        placed += 1;
    }
    let mut n = 0;
    write_user(
        game,
        &mut journal,
        &mut n,
        &game.local,
        "Plugins.txt",
        &(pack.plugins.join("\r\n") + "\r\n"),
    )?;
    write_user(
        game,
        &mut journal,
        &mut n,
        &game.local,
        "loadorder.txt",
        &(pack.loadorder.join("\r\n") + "\r\n"),
    )?;
    for name in snapshot::DOCUMENTS {
        let values = pack
            .ini
            .iter()
            .find(|(f, _)| f.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if !values.is_empty()
            || find(&game.documents, name)?.is_none() && name != "SkyrimCustom.ini"
        {
            let text = ini_text(game, name, &values)?;
            write_user(game, &mut journal, &mut n, &game.documents, name, &text)?;
        }
    }
    journal.write(&Op::Loaded)?;
    cache.save()?;
    Ok(json!({
        "state": "loaded",
        "pack": id,
        "name": pack.name,
        "placed": placed,
        "set_aside": set_aside,
        "skse_launcher": loader.is_some(),
    }))
}

/// Takes a loaded (or half-loaded) pack out, putting back everything of the friend's.
pub fn unload(game: &Game) -> Result<Value> {
    let ops = game.ops()?;
    if ops.is_empty() && !game.journal().exists() {
        return Ok(json!({"state": "clean", "restored": 0}));
    }
    let mut restored = 0;
    for op in ops.iter().rev() {
        match op {
            Op::Place { path, original } => {
                let target = game.root.join(path);
                let backup = game.backup(path);
                if *original {
                    if backup.exists() {
                        if target.exists() {
                            fs::remove_file(&target)?;
                        }
                        fs::rename(&backup, &target)?;
                    }
                } else if target.exists() {
                    fs::remove_file(&target)?;
                }
                restored += 1;
            }
            Op::Aside { path } => {
                let backup = game.backup(path);
                if backup.exists() || backup.symlink_metadata().is_ok() {
                    move_file(&backup, &game.root.join(path))?;
                }
                restored += 1;
            }
            Op::Mkdir { path } => {
                let _ = fs::remove_dir(game.root.join(path));
            }
            Op::User {
                file,
                original,
                backup,
            } => {
                let b = game.dir().join("backup").join(backup);
                if *original {
                    if b.is_file() {
                        fs::copy(&b, file)?;
                    }
                } else if file.exists() {
                    fs::remove_file(file)?;
                }
            }
            Op::Begin { .. } | Op::Loaded => {}
        }
    }
    fs::remove_file(game.journal())?;
    let backup = game.dir().join("backup");
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    let _ = fs::remove_dir(game.dir());
    Ok(json!({"state": "clean", "restored": restored}))
}

/// Undoes an interrupted load before anything else touches the game folder.
pub fn recover(game: &Game) -> Result<Option<Value>> {
    if game.state()? == State::Interrupted {
        crate::event("recovering", json!({"game": game.root}));
        return unload(game).map(Some);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn game(root: &Path) -> Game {
        Game {
            root: root.join("game"),
            local: root.join("local"),
            documents: root.join("docs"),
        }
    }
    fn journal(g: &Game, ops: &[Op]) {
        fs::create_dir_all(g.dir()).unwrap();
        let text = ops
            .iter()
            .map(|o| serde_json::to_string(o).unwrap() + "\n")
            .collect::<String>();
        fs::write(g.journal(), text).unwrap();
    }
    #[test]
    fn a_crash_between_moving_the_original_and_placing_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let g = game(dir.path());
        fs::create_dir_all(g.root.join("Data")).unwrap();
        let begin = Op::Begin {
            pack: "p".into(),
            name: "n".into(),
            sequence: 1,
        };
        journal(
            &g,
            &[
                begin,
                Op::Place {
                    path: "Data/a.esp".into(),
                    original: true,
                },
            ],
        );
        fs::create_dir_all(g.backup("Data").as_path()).unwrap();
        fs::write(g.backup("Data/a.esp"), b"mine").unwrap();
        assert_eq!(g.state().unwrap(), State::Interrupted);
        recover(&g).unwrap();
        assert_eq!(fs::read(g.root.join("Data/a.esp")).unwrap(), b"mine");
        assert!(!g.dir().exists());
    }
    #[test]
    fn a_crash_before_moving_the_original_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let g = game(dir.path());
        fs::create_dir_all(g.root.join("Data")).unwrap();
        fs::write(g.root.join("Data/a.esp"), b"mine").unwrap();
        let begin = Op::Begin {
            pack: "p".into(),
            name: "n".into(),
            sequence: 1,
        };
        journal(
            &g,
            &[
                begin,
                Op::Place {
                    path: "Data/a.esp".into(),
                    original: true,
                },
            ],
        );
        recover(&g).unwrap();
        assert_eq!(fs::read(g.root.join("Data/a.esp")).unwrap(), b"mine");
    }
    #[test]
    fn folders_made_for_the_pack_go_away_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let g = game(dir.path());
        fs::create_dir_all(g.root.join("Data/meshes/x")).unwrap();
        fs::write(g.root.join("Data/meshes/x/a.nif"), b"ours").unwrap();
        let begin = Op::Begin {
            pack: "p".into(),
            name: "n".into(),
            sequence: 1,
        };
        journal(
            &g,
            &[
                begin,
                Op::Mkdir {
                    path: "Data/meshes".into(),
                },
                Op::Mkdir {
                    path: "Data/meshes/x".into(),
                },
                Op::Place {
                    path: "Data/meshes/x/a.nif".into(),
                    original: false,
                },
                Op::Loaded,
            ],
        );
        unload(&g).unwrap();
        assert!(!g.root.join("Data/meshes").exists());
        assert!(g.root.join("Data").exists());
    }
}
