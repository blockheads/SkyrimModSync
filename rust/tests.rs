//! Whole flows on fake games: a host publishes an MO2 profile, a friend syncs it (a fake Nexus and
//! GitHub, the host over a local iroh connection), loads, plays, unloads back to the same bytes.
use crate::{
    deploy::{self, Game},
    host::{Binding, Host, Source},
    mo2,
    net::Net,
    pack::{From, Origin},
    peer,
    store::Store,
    sync,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// Every file under the folders: relative path -> bytes.
fn tree(roots: &[&Path]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for (i, r) in roots.iter().enumerate() {
        for (rel, path) in crate::files::walk(r).unwrap() {
            out.insert(format!("{i}:{rel}"), fs::read(path).unwrap());
        }
    }
    out
}

/// A fake Nexus and GitHub on plain HTTP: mod 100 file 200 and repo fake/mod are the A archive.
fn fake_api(zip: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let link = format!("{base}/dl/A.zip");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let (status, body): (&str, Vec<u8>) = match first.split_whitespace().nth(1).unwrap_or("") {
                "/v1/games/skyrimspecialedition/mods/100/files/200/download_link.json" => {
                    ("200 OK", format!(r#"[{{"URI":"{link}"}}]"#).into_bytes())
                }
                "/dl/A.zip" => ("200 OK", zip.clone()),
                "/repos/fake/mod/releases?per_page=10" => ("200 OK", format!(
                    r#"[{{"draft":false,"assets":[{{"name":"notes.txt"}},{{"name":"A-1.0.zip","browser_download_url":"{link}"}}]}}]"#
                ).into_bytes()),
                _ => ("404 Not Found", b"{}".to_vec()),
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    base
}

/// An MO2 host with loose and private files that must never be shared, and a friend's own game.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    host: Host,
    api: String,
    friend: Game,
}

const PRIVATE: [&str; 5] = [
    "game/SkyrimSE_1.7.104.exe",
    "game/Data/Video/logo.bik",
    "game/Data/HandInstalled.esp",
    "game/Data/SKSE/Plugins/logs/tp_client.log",
    "mo2/overwrite/SKSE/Plugins/logs/tp_client.1.log",
];

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    mo2::fixture::instance(&root);
    write(&root, "game/skse64_loader.exe", b"loader");
    for p in PRIVATE {
        write(&root, p, b"private");
    }
    let mut zip = vec![];
    fs::File::open(root.join("mo2/downloads/A-100-1-0.zip"))
        .unwrap()
        .read_to_end(&mut zip)
        .unwrap();
    let host = Host {
        dir: root.join("host"),
    };
    host.init(Binding {
        name: "Test".into(),
        mo2: root.join("mo2"),
        profile: "Default".into(),
        game: None,
        sources: Default::default(),
    })
    .unwrap();
    let f = root.join("friend");
    write(&f, "game/SkyrimSE.exe", b"exe");
    write(&f, "game/Data/Skyrim.esm", b"esm");
    write(&f, "game/Data/Friend.esp", b"mine");
    write(&f, "game/Data/Textures/shared.dds", b"friend's");
    write(&f, "game/SkyrimSELauncher.exe", b"bethesda");
    write(
        &f,
        "game/Skyrim_Default.ini",
        b"[General]\nsLanguage=ENGLISH\n",
    );
    write(&f, "local/Plugins.txt", b"*Friend.esp\n");
    write(
        &f,
        "docs/Skyrim.ini",
        b"[General]\nuGridsToLoad=9\nsLanguage=ENGLISH\n",
    );
    write(&f, "docs/SkyrimPrefs.ini", b"[Display]\niSize W=1920\n");
    let friend = Game {
        root: f.join("game"),
        local: f.join("local"),
        documents: f.join("docs"),
    };
    World {
        _dir: dir,
        root,
        host,
        api: fake_api(zip),
        friend,
    }
}

impl World {
    fn net(&self, key: Option<&str>) -> Net {
        Net::with(&self.api, &self.api, false, key.map(str::to_owned)).unwrap()
    }
    fn store(&self) -> Store {
        Store::open(&self.root.join("friend/store")).unwrap()
    }
    /// Publishes and serves on a local port until the returned flag is set.
    fn serve(&self) -> (SocketAddr, Arc<AtomicBool>) {
        self.host.publish(&self.net(None)).unwrap();
        let addr: SocketAddr = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (key, published, flag) = (
            self.host.key().unwrap(),
            self.host.published().unwrap().unwrap(),
            stop.clone(),
        );
        std::thread::spawn(move || {
            peer::serve(
                key,
                published,
                Some(addr),
                |_| tx.send(()).unwrap(),
                || flag.load(Ordering::Relaxed),
            )
            .unwrap()
        });
        rx.recv().unwrap();
        (addr, stop)
    }
    fn sync(&self, key: Option<&str>, addr: SocketAddr) -> serde_json::Value {
        let pack =
            peer::fetch_pack(&self.host.key().unwrap().public().to_string(), Some(addr)).unwrap();
        sync::sync(
            &pack,
            &self.friend,
            &self.store(),
            &self.net(key),
            Some(addr),
        )
        .unwrap()
    }
    fn load(&self) -> serde_json::Value {
        let st = self.store();
        let pack = st.pack().unwrap().unwrap();
        let plan = deploy::plan(&pack, &self.friend, &mut st.hashes(), |e| {
            st.has(&e.hash, e.size).then(|| st.object(&e.hash))
        })
        .unwrap();
        deploy::load(&self.friend, &plan, &pack.name).unwrap()
    }
    fn friend_tree(&self) -> BTreeMap<String, Vec<u8>> {
        tree(&[
            &self.friend.root,
            &self.friend.local,
            &self.friend.documents,
        ])
    }
    fn read(&self, rel: &str) -> Vec<u8> {
        let real = crate::files::walk(&self.friend.root)
            .unwrap()
            .into_iter()
            .find(|(r, _)| r.eq_ignore_ascii_case(rel));
        fs::read(real.unwrap_or_else(|| panic!("no {rel}")).1).unwrap()
    }
}

#[test]
fn a_draft_finds_sources_by_hash_and_never_shares_private_files() {
    let w = world();
    let d = w.host.draft(&w.net(None)).unwrap();
    let from = |p: &str| {
        d.published
            .pack
            .files
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(p))
            .unwrap()
            .from
            .clone()
    };
    assert!(matches!(from("Data/A.esp"), From::Archive { .. }));
    assert_eq!(
        from("Data/textures/shared.dds"),
        From::Host,
        "B wins over A's copy; B has no archive"
    );
    assert_eq!(from("Data/Skyrim.esm"), From::Base);
    let private = blake3::hash(b"private").to_hex().to_string();
    assert!(d.published.pack.files.iter().all(|e| e.hash != private));
    let source = |m: &str| d.mods.iter().find(|r| r.name == m).unwrap().source.clone();
    assert_eq!((source("A"), source("B")), ("nexus".into(), "host".into()));
}

#[test]
fn a_github_source_is_the_newest_releases_archive() {
    let w = world();
    w.host
        .set_source(
            "A",
            Some(Source::Github {
                repo: "fake/mod".into(),
                asset: None,
            }),
        )
        .unwrap();
    let d = w.host.draft(&w.net(None)).unwrap();
    let a = &d.published.pack.archives[0];
    assert!(matches!(&a.origin, Origin::Url { url } if url.ends_with("/dl/A.zip")));
}

#[test]
fn without_a_key_everything_comes_from_the_host() {
    let w = world();
    let (addr, stop) = w.serve();
    let synced = w.sync(None, addr);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(synced["archives"], 0);
    assert_eq!(synced["archives_from_host"].as_array().unwrap().len(), 1);
    w.load();
    assert_eq!(w.read("Data/A.esp"), b"a-plugin");
}

#[test]
fn friend_syncs_loads_and_unloads_to_the_same_bytes() {
    let w = world();
    let before = w.friend_tree();
    let (addr, stop) = w.serve();
    let synced = w.sync(Some("premium"), addr);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(synced["archives"], 1, "A from the fake Nexus");
    assert_eq!(w.store().pack().unwrap().unwrap().name, "Test");

    let loaded = w.load();
    assert!(loaded["placed"].as_u64().unwrap() >= 5);
    assert_eq!(w.read("Data/A.esp"), b"a-plugin");
    assert_eq!(w.read("Data/textures/shared.dds"), b"from-b");
    assert_eq!(w.read("d3dx9_42.dll"), b"root-file");
    assert_eq!(
        w.read("SkyrimSELauncher.exe"),
        b"loader",
        "SKSE in Steam's launcher slot"
    );
    assert!(
        !w.friend.root.join("Data/Friend.esp").exists(),
        "the friend's own mod is set aside"
    );
    assert_eq!(
        fs::read_to_string(w.friend.local.join("Plugins.txt")).unwrap(),
        "*A.esp\r\n*B.esp\r\n"
    );
    let ini =
        crate::ini::parse(&fs::read_to_string(w.friend.documents.join("Skyrim.ini")).unwrap());
    assert_eq!(
        (
            ini["General"]["uGridsToLoad"].as_str(),
            ini["General"]["sLanguage"].as_str()
        ),
        ("5", "ENGLISH")
    );
    assert_eq!(
        fs::read(w.friend.documents.join("SkyrimPrefs.ini")).unwrap(),
        b"[Display]\niSize W=1920\n"
    );
    assert!(matches!(w.friend.state(), deploy::State::Loaded(_)));

    deploy::unload(&w.friend).unwrap();
    assert_eq!(w.friend_tree(), before);
}

#[test]
fn a_load_cut_short_is_undone() {
    let w = world();
    let before = w.friend_tree();
    let (addr, stop) = w.serve();
    w.sync(Some("premium"), addr);
    stop.store(true, Ordering::Relaxed);
    w.load();
    fs::remove_file(w.friend.root.join(".skyrim-mod-sync/loaded")).unwrap();
    assert_eq!(w.friend.state(), deploy::State::Interrupted);
    deploy::unload(&w.friend).unwrap();
    assert_eq!(w.friend_tree(), before);
}

#[test]
fn a_cut_host_transfer_resumes() {
    let w = world();
    let (addr, stop) = w.serve();
    let pack = peer::fetch_pack(&w.host.key().unwrap().public().to_string(), Some(addr)).unwrap();
    let shared = pack
        .files
        .iter()
        .find(|e| e.path.eq_ignore_ascii_case("Data/textures/shared.dds"))
        .unwrap();
    let st = w.store();
    let part = st.object(&shared.hash).with_extension("part");
    write(
        part.parent().unwrap(),
        part.file_name().unwrap().to_str().unwrap(),
        b"fro",
    );
    peer::fetch_files(
        &pack.host,
        Some(addr),
        &st,
        &[(shared.hash.clone(), shared.size)],
    )
    .unwrap();
    stop.store(true, Ordering::Relaxed);
    assert_eq!(fs::read(st.object(&shared.hash)).unwrap(), b"from-b");
    assert!(!part.exists());
}

#[test]
fn the_host_plays_its_own_pack_from_its_own_files() {
    let w = world();
    w.host.publish(&w.net(None)).unwrap();
    let published = w.host.published().unwrap().unwrap();
    let game = Game {
        root: w.root.join("game"),
        local: w.root.join("host-local"),
        documents: w.root.join("host-docs"),
    };
    let before = tree(&[&game.root]);
    let plan = deploy::plan(
        &published.pack,
        &game,
        &mut crate::hash::Cache::open(&w.root.join("h.json")),
        |e| published.local.get(&e.hash).cloned(),
    )
    .unwrap();
    deploy::load(&game, &plan, "Test").unwrap();
    assert_eq!(
        fs::read(game.root.join("Data/textures/shared.dds")).unwrap(),
        b"from-b"
    );
    assert_eq!(
        w.host.publish(&w.net(None)).unwrap_err().code,
        "pack_loaded"
    );
    deploy::unload(&game).unwrap();
    assert_eq!(tree(&[&game.root]), before);
}

#[test]
fn clean_keeps_only_what_the_pack_uses() {
    let w = world();
    let (addr, stop) = w.serve();
    w.sync(Some("premium"), addr);
    stop.store(true, Ordering::Relaxed);
    let st = w.store();
    write(&st.root, "objects/ff/ffff", b"stale");
    assert_eq!(st.clean().unwrap(), 5);
    assert!(!st.root.join("objects/ff/ffff").exists());
    assert!(w
        .store()
        .pack()
        .unwrap()
        .unwrap()
        .files
        .iter()
        .filter(|e| e.from != From::Base)
        .all(|e| st.has(&e.hash, e.size)));
}
