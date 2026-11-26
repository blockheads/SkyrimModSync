//! Load and unload, Vortex-style. A load is planned in full, written to a journal, then carried out;
//! unload walks the journal backwards, each step tolerating one that never ran, so a crash mid-load is
//! undone by the next command and the game folder always comes back as it was.
use crate::{
    files::{self, fold},
    hash, ini,
    pack::{Entry, From, Pack},
    rules, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

const DIR: &str = ".skyrim-mod-sync";
const LAUNCHER: &str = "SkyrimSELauncher.exe";
const LOADER: &str = "skse64_loader.exe";
/// Files a mod may rewrite while the game runs: copied, never hardlinked.
const COPY: [&str; 8] = ["ini", "json", "toml", "txt", "yaml", "yml", "xml", "cfg"];

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
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// A folder made for the pack (game-relative).
    Dir { path: String },
    /// A file of the friend's moved out of the way.
    Aside { path: String },
    /// A pack file put in place; `original` when a file of the friend's was there.
    Place {
        path: String,
        original: bool,
        source: PathBuf,
    },
    /// A settings file outside the game folder rewritten.
    User {
        file: PathBuf,
        original: bool,
        text: String,
    },
}

#[derive(Debug, PartialEq)]
pub enum State {
    Clean,
    /// A load that did not finish: undone before anything else.
    Interrupted,
    Loaded(String),
}

/// What a load would do, and what stops it.
#[derive(Debug, Default)]
pub struct Plan {
    pub ops: Vec<Op>,
    /// Pack files neither the game nor a source has.
    pub missing: Vec<Entry>,
    /// Licensed files whose own copy differs (another Skyrim version or edition).
    pub differs: Vec<String>,
}

impl Game {
    fn dir(&self) -> PathBuf {
        self.root.join(DIR)
    }
    fn backup(&self, rel: &str) -> PathBuf {
        self.dir().join("backup").join(rel)
    }
    fn user_backup(&self, i: usize) -> PathBuf {
        self.dir().join("user").join(i.to_string())
    }
    pub fn state(&self) -> State {
        match (
            self.dir().join("journal.json").is_file(),
            fs::read_to_string(self.dir().join("loaded")),
        ) {
            (false, _) => State::Clean,
            (true, Ok(name)) => State::Loaded(name),
            (true, Err(_)) => State::Interrupted,
        }
    }
}

/// Plans putting `pack` into a clean game folder; `source` says where a file the game lacks comes from.
pub fn plan(
    pack: &Pack,
    game: &Game,
    cache: &mut hash::Cache,
    source: impl Fn(&Entry) -> Option<PathBuf>,
) -> Result<Plan> {
    // One walk: folded path -> real path, for files and the folders above them.
    let mut index: HashMap<String, String> = HashMap::new();
    let mut dirs: HashMap<String, String> = HashMap::new();
    for rel in files::walk(&game.root)?
        .into_keys()
        .filter(|r| !fold(r).starts_with(DIR))
    {
        let mut at = 0;
        while let Some(i) = rel[at..].find('/') {
            at += i;
            dirs.insert(fold(&rel[..at]), rel[..at].to_owned());
            at += 1;
        }
        index.insert(fold(&rel), rel);
    }
    let wanted: HashMap<String, &Entry> = pack.files.iter().map(|e| (fold(&e.path), e)).collect();
    let present: Vec<(&String, &String)> = index
        .iter()
        .filter(|(k, _)| wanted.contains_key(*k))
        .collect();
    let paths: Vec<PathBuf> = present.iter().map(|(_, rel)| game.root.join(rel)).collect();
    let same: BTreeMap<&str, bool> = present
        .iter()
        .zip(cache.many(&paths)?)
        .map(|((k, _), (_, h))| (k.as_str(), h == wanted[*k].hash))
        .collect();

    let mut plan = Plan::default();
    let mut places = vec![];
    for e in &pack.files {
        let key = fold(&e.path);
        if same.get(key.as_str()) == Some(&true) {
            continue;
        }
        match (&e.from, source(e)) {
            (From::Base, _) => plan.differs.push(e.path.clone()),
            (_, None) => plan.missing.push(e.clone()),
            (_, Some(src)) => places.push((e.path.clone(), src)),
        }
    }
    if let Some(loader) = pack.files.iter().find(|e| fold(&e.path) == LOADER) {
        let own = (same.get(LOADER) == Some(&true)).then(|| game.root.join(&index[LOADER]));
        if let Some(src) = source(loader).or(own) {
            places.push((LAUNCHER.to_owned(), src));
        }
    }
    for (key, rel) in &index {
        if key.starts_with("data/") && !wanted.contains_key(key) && !rules::stays(rel) {
            plan.ops.push(Op::Aside { path: rel.clone() });
        }
    }
    for (path, source) in places {
        let (real, made) = resolve(&path, &index, &mut dirs);
        plan.ops
            .extend(made.into_iter().map(|path| Op::Dir { path }));
        let original = index.contains_key(&fold(&path));
        plan.ops.push(Op::Place {
            path: real,
            original,
            source,
        });
    }
    // Folders first, then asides; places keep their order after.
    plan.ops.sort_by_key(|op| match op {
        Op::Dir { .. } => 0,
        Op::Aside { .. } => 1,
        _ => 2,
    });
    let lines = |l: &[String]| l.join("\r\n") + "\r\n";
    plan.ops
        .push(user(&game.local, "Plugins.txt", lines(&pack.plugins)));
    plan.ops
        .push(user(&game.local, "loadorder.txt", lines(&pack.loadorder)));
    for name in rules::DOCUMENTS {
        let values = pack
            .ini
            .iter()
            .find(|(f, _)| f.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone());
        if values.is_some()
            || (files::find(&game.documents, name).is_none() && name != "SkyrimCustom.ini")
        {
            plan.ops.push(user(
                &game.documents,
                name,
                ini_text(game, name, &values.unwrap_or_default())?,
            ));
        }
    }
    Ok(plan)
}

/// `path` with the game folder's own casing for folders that exist, and the folders to make.
fn resolve(
    path: &str,
    index: &HashMap<String, String>,
    dirs: &mut HashMap<String, String>,
) -> (String, Vec<String>) {
    if let Some(real) = index.get(&fold(path)) {
        return (real.clone(), vec![]);
    }
    let mut real = String::new();
    let mut made = vec![];
    let parts: Vec<&str> = path.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let next = if real.is_empty() {
            part.to_string()
        } else {
            format!("{real}/{part}")
        };
        if i + 1 == parts.len() {
            return (next, made);
        }
        real = match dirs.get(&fold(&next)) {
            Some(existing) => existing.clone(),
            None => {
                dirs.insert(fold(&next), next.clone());
                made.push(next.clone());
                next
            }
        };
    }
    (real, made)
}

fn user(dir: &Path, name: &str, text: String) -> Op {
    let file = files::find(dir, name).unwrap_or_else(|| dir.join(name));
    Op::User {
        original: file.is_file(),
        file,
        text,
    }
}

/// The friend's INI with the pack's settings written in (the game's defaults if they have none).
fn ini_text(game: &Game, name: &str, values: &ini::Sections) -> Result<String> {
    let default = match fold(name).as_str() {
        "skyrim.ini" => files::find(&game.root, "Skyrim_Default.ini"),
        "skyrimprefs.ini" => files::find(&game.root, "High.ini"),
        _ => None,
    };
    let text = match files::find(&game.documents, name).or(default) {
        Some(p) => String::from_utf8_lossy(&fs::read(p)?).into_owned(),
        None => String::new(),
    };
    Ok(ini::merge(&text, values))
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to.parent().unwrap())?;
    fs::rename(from, to)?;
    Ok(())
}

fn link_or_copy(source: &Path, dest: &Path) -> Result<()> {
    let ext = dest
        .extension()
        .map(|e| fold(&e.to_string_lossy()))
        .unwrap_or_default();
    if COPY.contains(&ext.as_str()) || fs::hard_link(source, dest).is_err() {
        fs::copy(source, dest)?;
    }
    Ok(())
}

/// Carries out a plan on a clean game folder.
pub fn load(game: &Game, plan: &Plan, name: &str) -> Result<Value> {
    if game.state() != State::Clean {
        return Err(Error::new(
            "not_clean",
            "A pack is loaded: `sms disable` first.",
        ));
    }
    if let Some(e) = plan.missing.first() {
        return Err(Error::new(
            "pack_incomplete",
            format!(
                "{} files are not synced yet (first: {}).",
                plan.missing.len(),
                e.path
            ),
        ));
    }
    if let Some(p) = plan.differs.first() {
        return Err(Error::new(
            "game_differs",
            format!("Your game's {p} differs from the host's: same Skyrim version and edition?"),
        ));
    }
    files::write_json(&game.dir().join("journal.json"), &plan.ops)?;
    let (mut placed, mut aside) = (0, 0);
    for (i, op) in plan.ops.iter().enumerate() {
        crate::check_cancel()?;
        match op {
            Op::Dir { path } => fs::create_dir_all(game.root.join(path))?,
            Op::Aside { path } => {
                move_file(&game.root.join(path), &game.backup(path))?;
                aside += 1;
            }
            Op::Place {
                path,
                original,
                source,
            } => {
                let dest = game.root.join(path);
                if *original {
                    move_file(&dest, &game.backup(path))?;
                }
                link_or_copy(source, &dest)?;
                placed += 1;
            }
            Op::User {
                file,
                original,
                text,
            } => {
                if *original {
                    files::write_atomic(&game.user_backup(i), &fs::read(file)?)?;
                }
                files::write_atomic(file, text.as_bytes())?;
            }
        }
    }
    files::write_atomic(&game.dir().join("loaded"), name.as_bytes())?;
    Ok(json!({"state": "loaded", "pack": name, "placed": placed, "set_aside": aside}))
}

/// Takes a loaded or half-loaded pack out, putting back everything of the friend's.
pub fn unload(game: &Game) -> Result<Value> {
    let journal = game.dir().join("journal.json");
    if !journal.is_file() {
        return Ok(json!({"state": "clean"}));
    }
    let ops: Vec<Op> = files::read_json(&journal)?;
    for (i, op) in ops.iter().enumerate().rev() {
        match op {
            Op::Place { path, original, .. } => {
                let (dest, backup) = (game.root.join(path), game.backup(path));
                // An original with no backup was never moved: it is still in place.
                if !*original || backup.exists() {
                    let _ = fs::remove_file(&dest);
                }
                if backup.exists() {
                    fs::rename(&backup, &dest)?;
                }
            }
            Op::Aside { path } => {
                if game.backup(path).exists() {
                    move_file(&game.backup(path), &game.root.join(path))?;
                }
            }
            Op::Dir { path } => {
                let _ = fs::remove_dir(game.root.join(path));
            }
            Op::User { file, original, .. } => match (*original, game.user_backup(i)) {
                (true, b) if b.is_file() => files::write_atomic(file, &fs::read(b)?)?,
                (true, _) => {}
                (false, _) => {
                    let _ = fs::remove_file(file);
                }
            },
        }
    }
    fs::remove_dir_all(game.dir())?;
    Ok(json!({"state": "clean", "restored": ops.len()}))
}
