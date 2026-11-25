//! Whole flows on fake games: a host publishes a mod list (a fake Nexus and GitHub serve its archives),
//! a friend syncs it (from those, or from the host over a local iroh connection), loads, unloads.
use crate::{
    deploy::{self, Game},
    host::Host,
    net::Net,
    pack::{From, Origin},
    peer,
    store::Store,
    sync,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
};

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
    for (n, b) in entries {
        z.start_file(*n, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b).unwrap();
    }
    z.finish().unwrap().into_inner()
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

/// A fake Nexus and GitHub on plain HTTP. Nexus mod 100 file 200 is A.zip; fake/skse's newest release
/// has skse.zip; fake/pack's has pack.json (whatever the test puts in `pack`).
fn fake_api(pack: Arc<Mutex<Vec<u8>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let a = zip(&[
        ("A.esp", b"a-plugin"),
        ("textures/shared.dds", b"from-a"),
        ("fomod/info.xml", b"x"),
    ]);
    let skse = zip(&[
        ("skse64_2/skse64_loader.exe", b"loader"),
        ("skse64_2/Data/Scripts/a.pex", b"script"),
        ("skse64_2/src/x.cpp", b"source"),
    ]);
    let b = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let (mut line, mut free) = (String::new(), false);
            while reader.read_line(&mut line).unwrap() > 2 {
                free |= line.to_lowercase().starts_with("apikey: free");
                line.clear();
            }
            let release = |name: &str| {
                format!(r#"[{{"draft":false,"assets":[{{"name":"notes.txt"}},{{"name":"{name}","browser_download_url":"{b}/dl/{name}"}}]}}]"#).into_bytes()
            };
            let (status, body): (&str, Vec<u8>) =
                match first.split_whitespace().nth(1).unwrap_or("") {
                    "/v1/games/skyrimspecialedition/mods/100/files/200/download_link.json"
                        if free =>
                    {
                        ("403 Forbidden", b"{}".to_vec())
                    }
                    "/v1/games/skyrimspecialedition/mods/100/files/200.json" => {
                        ("200 OK", br#"{"file_name":"A-100-1-0.zip"}"#.to_vec())
                    }
                    "/v1/games/skyrimspecialedition/mods/100/files/200/download_link.json" => (
                        "200 OK",
                        format!(r#"[{{"URI":"{b}/dl/A.zip"}}]"#).into_bytes(),
                    ),
                    "/v1/games/skyrimspecialedition/mods/100.json" => {
                        ("200 OK", br#"{"name":"Mod A"}"#.to_vec())
                    }
                    "/repos/fake/skse/releases?per_page=10" => ("200 OK", release("skse.zip")),
                    "/repos/fake/pack/releases?per_page=10" => ("200 OK", release("pack.json")),
                    "/dl/A.zip" => ("200 OK", a.clone()),
                    "/dl/skse.zip" => ("200 OK", skse.clone()),
                    "/dl/pack.json" => ("200 OK", pack.lock().unwrap().clone()),
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

const LIST: &str = r#"{
  "name": "Test",
  "mods": [
    { "name": "SKSE", "github": { "repo": "fake/skse" }, "folder": "skse64_2", "root": ["skse64_loader.exe"] },
    { "name": "A", "nexus": { "mod": 100, "file": 200 } },
    { "name": "B", "local": "B", "root": ["d3dx9_42.dll"] }
  ],
  "plugins": ["A.esp", "B.esp"],
  "ini": { "Skyrim.ini": { "General": { "uGridsToLoad": "5" } } }
}"#;

/// A host (its game with private files that must never be shared, a mod list) and a friend's game.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    host: Host,
    api: String,
    pack: Arc<Mutex<Vec<u8>>>,
    friend: Game,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    write(&root, "game/SkyrimSE.exe", b"exe");
    write(&root, "game/Data/Skyrim.esm", b"esm");
    for p in [
        "game/SkyrimSE_1.7.104.exe",
        "game/Data/HandInstalled.esp",
        "game/Data/SKSE/Plugins/logs/x.log",
    ] {
        write(&root, p, b"private");
    }
    write(&root, "list/modlist.json", LIST.as_bytes());
    write(&root, "list/B/B.esp", b"b-plugin");
    write(&root, "list/B/textures/shared.dds", b"from-b");
    write(&root, "list/B/d3dx9_42.dll", b"root-file");
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
    let pack = Arc::new(Mutex::new(vec![]));
    let api = fake_api(pack.clone());
    World {
        _dir: dir,
        host: Host {
            dir: root.join("host"),
        },
        root,
        api,
        pack,
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
    /// Publishes (the host has a key) and puts the pack on the fake GitHub.
    fn publish(&self) -> serde_json::Value {
        let out = self.root.join("pack.json");
        let r = self
            .host
            .publish(
                &self.root.join("list/modlist.json"),
                &self.root.join("game"),
                &self.net(Some("host")),
                Some(&out),
            )
            .unwrap();
        *self.pack.lock().unwrap() = fs::read(out).unwrap();
        r
    }
    /// Serves on a local port until the returned flag is set.
    fn serve(&self) -> (SocketAddr, Arc<AtomicBool>) {
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
    fn sync(&self, source: &str, key: Option<&str>, addr: Option<SocketAddr>) -> serde_json::Value {
        let net = self.net(key);
        let pack = sync::fetch_pack(source, &net, addr).unwrap();
        sync::sync(&pack, &self.friend, &self.store(), &net, addr, None).unwrap()
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
fn a_list_resolves_by_its_install_rules_and_never_shares_private_files() {
    let w = world();
    w.publish();
    let pack = w.host.published().unwrap().unwrap().pack;
    let from = |p: &str| {
        pack.files
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(p))
            .map(|e| e.from.clone())
    };
    assert!(matches!(from("Data/A.esp"), Some(From::Archive { .. })));
    assert_eq!(
        from("Data/textures/shared.dds"),
        Some(From::Host),
        "B comes later and wins"
    );
    assert!(
        matches!(from("skse64_loader.exe"), Some(From::Archive { .. })),
        "root file"
    );
    assert!(
        matches!(from("Data/Scripts/a.pex"), Some(From::Archive { .. })),
        "SKSE's Data/"
    );
    assert_eq!(from("d3dx9_42.dll"), Some(From::Host));
    assert_eq!(from("Data/Skyrim.esm"), Some(From::Base));
    for skipped in [
        "Data/src/x.cpp",
        "src/x.cpp",
        "Data/fomod/info.xml",
        "Data/HandInstalled.esp",
        "SkyrimSE_1.7.104.exe",
    ] {
        assert_eq!(from(skipped), None, "{skipped}");
    }
    assert_eq!(pack.plugins, ["*A.esp", "*B.esp"]);
    assert_eq!(pack.loadorder, ["Skyrim.esm", "A.esp", "B.esp"]);
    assert!(pack
        .archives
        .iter()
        .any(|a| matches!(&a.origin, Origin::Url { url } if url.ends_with("/dl/skse.zip"))));
}

#[test]
fn a_friend_with_a_key_plays_from_a_github_link_with_no_host_and_unloads_to_the_same_bytes() {
    let w = world();
    let before = w.friend_tree();
    w.publish();
    // Only B's files come from the host: give them its own copy as if served, then the rest by link.
    let (addr, stop) = w.serve();
    let synced = w.sync("github:fake/pack", Some("friend"), Some(addr));
    stop.store(true, Ordering::Relaxed);
    assert_eq!(synced["archives"], 2, "SKSE from GitHub, A from Nexus");
    assert_eq!(synced["host_files"], 3, "B's files only");

    let loaded = w.load();
    assert!(loaded["placed"].as_u64().unwrap() >= 6);
    assert_eq!(w.read("Data/A.esp"), b"a-plugin");
    assert_eq!(w.read("Data/textures/shared.dds"), b"from-b");
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

    deploy::unload(&w.friend).unwrap();
    assert_eq!(w.friend_tree(), before);
}

#[test]
fn a_pack_without_host_files_needs_no_host() {
    let w = world();
    let list = LIST
        .replace(
            r#",
    { "name": "B", "local": "B", "root": ["d3dx9_42.dll"] }"#,
            "",
        )
        .replace(r#", "B.esp""#, "");
    write(&w.root, "list/modlist.json", list.as_bytes());
    w.publish();
    let synced = w.sync("github:fake/pack", Some("friend"), None);
    assert_eq!(synced["host_files"], 0);
    w.load();
    assert_eq!(w.read("Data/A.esp"), b"a-plugin");
}

#[test]
fn without_a_key_nexus_mods_come_from_the_host() {
    let w = world();
    w.publish();
    let (addr, stop) = w.serve();
    let code = w.host.key().unwrap().public().to_string();
    let synced = w.sync(&code, None, Some(addr));
    stop.store(true, Ordering::Relaxed);
    assert_eq!(synced["archives"], 1, "GitHub needs no key");
    assert_eq!(
        synced["archives_from_host"].as_array().unwrap().len(),
        1,
        "the Nexus one"
    );
    w.load();
    assert_eq!(
        w.read("Data/A.esp"),
        b"a-plugin",
        "extracted by the host from its archive"
    );
    assert_eq!(w.read("Data/Scripts/a.pex"), b"script");
}

#[test]
fn a_load_cut_short_is_undone() {
    let w = world();
    let before = w.friend_tree();
    w.publish();
    let (addr, stop) = w.serve();
    w.sync("github:fake/pack", Some("friend"), Some(addr));
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
    w.publish();
    let (addr, stop) = w.serve();
    let pack = w.host.published().unwrap().unwrap().pack;
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
    w.publish();
    let own = w.host.published().unwrap().unwrap();
    let game = Game {
        root: w.root.join("game"),
        local: w.root.join("host-local"),
        documents: w.root.join("host-docs"),
    };
    let before = tree(&[&game.root]);
    let st = Store::open(&w.root.join("host-store")).unwrap();
    let synced = sync::sync(&own.pack, &game, &st, &w.net(None), None, Some(&own)).unwrap();
    assert!(synced["files_local"].as_u64().unwrap() >= 6);
    let plan = deploy::plan(&own.pack, &game, &mut st.hashes(), |e| {
        st.has(&e.hash, e.size).then(|| st.object(&e.hash))
    })
    .unwrap();
    deploy::load(&game, &plan, "Test").unwrap();
    assert_eq!(fs::read(game.root.join("Data/A.esp")).unwrap(), b"a-plugin");
    assert_eq!(
        w.host
            .publish(
                &w.root.join("list/modlist.json"),
                &game.root,
                &w.net(Some("k")),
                None
            )
            .unwrap_err()
            .code,
        "pack_loaded"
    );
    deploy::unload(&game).unwrap();
    assert_eq!(tree(&[&game.root]), before);
}

#[test]
fn without_premium_the_host_downloads_nexus_archives_by_hand() {
    let w = world();
    let list = w.root.join("list/modlist.json");
    let game = w.root.join("game");
    let err = w
        .host
        .publish(&list, &game, &w.net(Some("free")), None)
        .unwrap_err();
    assert_eq!(err.code, "manual_download");
    assert!(
        err.message.contains("A-100-1-0.zip") && err.message.contains("file_id=200"),
        "{}",
        err.message
    );
    let a = zip(&[
        ("A.esp", b"a-plugin"),
        ("textures/shared.dds", b"from-a"),
        ("fomod/info.xml", b"x"),
    ]);
    write(&w.root, "list/downloads/A-100-1-0.zip", &a);
    w.host
        .publish(&list, &game, &w.net(Some("free")), None)
        .unwrap();
    let pack = w.host.published().unwrap().unwrap().pack;
    let a = pack
        .archives
        .iter()
        .find(|a| matches!(a.origin, Origin::Nexus { r#mod: 100, .. }))
        .unwrap();
    assert_eq!(a.name, "A-100-1-0.zip");
}

#[test]
fn add_appends_a_nexus_mod_from_its_link() {
    let w = world();
    let list = w.root.join("new.json");
    let r = w
        .host
        .add(
            &list,
            "https://www.nexusmods.com/skyrimspecialedition/mods/100?tab=files&file_id=200",
            &w.net(Some("k")),
        )
        .unwrap();
    assert_eq!(r["mod"], "Mod A");
    let read = crate::list::ModList::read(&list).unwrap();
    assert_eq!(
        read.mods[0].nexus.as_ref().map(|n| (n.r#mod, n.file)),
        Some((100, 200))
    );
}

#[test]
fn clean_keeps_only_what_the_pack_uses() {
    let w = world();
    w.publish();
    let (addr, stop) = w.serve();
    w.sync("github:fake/pack", Some("friend"), Some(addr));
    stop.store(true, Ordering::Relaxed);
    let st = w.store();
    write(&st.root, "objects/ff/ffff", b"stale");
    assert_eq!(st.clean().unwrap(), 5);
    let pack = st.pack().unwrap().unwrap();
    assert!(pack
        .files
        .iter()
        .filter(|e| e.from != From::Base)
        .all(|e| st.has(&e.hash, e.size)));
}
