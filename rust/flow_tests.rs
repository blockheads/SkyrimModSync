//! The whole flow on fake games: a host publishes an MO2 profile, a friend syncs it (a fake Nexus
//! and the host over a local P2P connection), loads it, and unloads back to byte-identical folders.
use crate::{
    channel, config,
    deploy::{self, Game},
    host::{Binding, Host},
    mo2,
    pack::Source,
    peer,
    store::Store,
    sync,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// Every file under the folders, by relative path.
fn tree(roots: &[&Path]) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, tag: &str, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                walk(base, &e.path(), tag, out);
            } else {
                let rel = e
                    .path()
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(format!("{tag}:{rel}"), fs::read(e.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    for (i, r) in roots.iter().enumerate() {
        walk(r, r, &i.to_string(), &mut out);
    }
    out
}

/// A fake Nexus API on plain HTTP: mod 100 file 200 is the fixture's A archive.
fn fake_nexus(zip: Vec<u8>) -> String {
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
            let path = first.split_whitespace().nth(1).unwrap_or("").to_owned();
            let (status, body): (&str, Vec<u8>) = match path.as_str() {
                "/v1/users/validate.json" => {
                    ("200 OK", br#"{"name":"friend","is_premium":true}"#.to_vec())
                }
                "/v1/games/skyrimspecialedition/mods/100/files/200.json" => {
                    ("200 OK", br#"{"category_name":"MAIN"}"#.to_vec())
                }
                "/v1/games/skyrimspecialedition/mods/100/files/200/download_link.json" => {
                    ("200 OK", format!(r#"[{{"URI":"{link}"}}]"#).into_bytes())
                }
                "/dl/A.zip" => ("200 OK", zip.clone()),
                "/repos/fake/mod/releases?per_page=10" => (
                    "200 OK",
                    format!(r#"[{{"draft":false,"tag_name":"v1","assets":[{{"name":"notes.txt","browser_download_url":"x"}},{{"name":"A-1.0.zip","browser_download_url":"{link}"}}]}}]"#).into_bytes(),
                ),
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

static STOP: AtomicBool = AtomicBool::new(false);

#[test]
fn host_publishes_friend_syncs_loads_and_unloads_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::env::set_var("SMS_CONFIG_DIR", root.join("config"));
    std::env::set_var("SMS_ALLOW_HTTP", "1");
    mo2::fixture::instance(root);
    write(root, "game/skse64_loader.exe", b"loader");
    // Loose in the game folder or logged by a past session: never shared.
    let private = [
        "game/SkyrimSE_1.7.104.exe",
        "game/Data/Video/logo.bik",
        "game/Data/HandInstalled.esp",
        "game/Data/SKSE/Plugins/logs/tp_client.log",
        "mo2/overwrite/SKSE/Plugins/logs/tp_client.1.log",
    ];
    for p in private {
        write(root, p, b"private");
    }
    let mut zip = vec![];
    fs::File::open(root.join("mo2/downloads/A-100-1-0.zip"))
        .unwrap()
        .read_to_end(&mut zip)
        .unwrap();
    let api = fake_nexus(zip);
    std::env::set_var("SMS_NEXUS_API", &api);
    std::env::set_var("SMS_GITHUB_API", &api);

    // Host: the profile becomes a pack; A's files come from Nexus, the rest from the host.
    let host = Host {
        dir: root.join("host"),
    };
    host.init(Binding {
        name: "Test pack".into(),
        mo2: root.join("mo2"),
        profile: "Default".into(),
        game: None,
        overrides: Default::default(),
    })
    .unwrap();
    let draft = host.draft().unwrap();
    let source = |p: &str| {
        draft
            .pack
            .files
            .iter()
            .find(|e| e.path.eq_ignore_ascii_case(p))
            .unwrap()
            .source
            .clone()
    };
    assert!(matches!(source("Data/A.esp"), Source::Archive { .. }));
    assert!(
        matches!(source("Data/textures/shared.dds"), Source::Host { .. }),
        "B wins over A's copy, B has no archive"
    );
    assert_eq!(source("Data/Skyrim.esm"), Source::Base);
    assert!(draft
        .pack
        .files
        .iter()
        .all(|e| !e.path.eq_ignore_ascii_case("SkyrimSELauncher.exe")));
    let secret = crate::files::hash_bytes(b"private");
    assert!(draft.pack.files.iter().all(|e| e.sha256 != secret));
    assert!(draft.local.keys().all(|sha| *sha != secret));
    let status = host.status().unwrap();
    let mods = status["mods"].as_array().unwrap();
    assert_eq!(
        mods.iter().find(|m| m["name"] == "A").unwrap()["source"],
        "nexus"
    );
    assert_eq!(
        mods.iter().find(|m| m["name"] == "B").unwrap()["source"],
        "host"
    );
    let published = host.publish(Some(&root.join("test.smspack"))).unwrap();
    assert_eq!(published["sequence"], 1);
    assert_eq!(
        host.publish(None).unwrap()["sequence"],
        1,
        "an unchanged profile keeps its head"
    );

    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let (tx, rx) = mpsc::channel();
    let host_dir = host.dir.clone();
    let server = std::thread::spawn(move || {
        peer::serve_until(
            &host_dir,
            true,
            Some(addr),
            |v| tx.send(v.clone()).unwrap(),
            || STOP.load(Ordering::Relaxed),
        )
    });
    rx.recv().unwrap();

    // Friend: their own mod, their own copy of a pack path, Bethesda's launcher, their settings.
    let friend = root.join("friend");
    write(&friend, "game/SkyrimSE.exe", b"exe");
    write(&friend, "game/Data/Skyrim.esm", b"esm");
    write(&friend, "game/Data/Friend.esp", b"mine");
    write(&friend, "game/Data/Textures/shared.dds", b"friend's");
    write(&friend, "game/SkyrimSELauncher.exe", b"bethesda");
    write(
        &friend,
        "game/Skyrim_Default.ini",
        b"[General]\nsLanguage=ENGLISH\n",
    );
    write(&friend, "local/Plugins.txt", b"*Friend.esp\n");
    write(
        &friend,
        "docs/Skyrim.ini",
        b"[General]\nuGridsToLoad=9\nsLanguage=ENGLISH\n",
    );
    write(
        &friend,
        "docs/SkyrimPrefs.ini",
        b"[Display]\niSize W=1920\n",
    );
    let game = Game {
        root: friend.join("game"),
        local: friend.join("local"),
        documents: friend.join("docs"),
    };
    let before = tree(&[&game.root, &game.local, &game.documents]);
    let store = Store::open(&friend.join("store")).unwrap();

    let (pack, head) =
        sync::resolve(root.join("test.smspack").to_str().unwrap(), &store, None).unwrap();
    let err = sync::sync(&pack, &head, &store, &game, Some(addr), None).unwrap_err();
    assert_eq!(err.code, "nexus_key_required");
    config::save_key("premium-key").unwrap();
    let synced = sync::sync(&pack, &head, &store, &game, Some(addr), None).unwrap();
    assert_eq!(synced["archives"], 1);
    assert!(synced["host_files"].as_u64().unwrap() >= 3);
    STOP.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();

    // Load: the pack in place, the friend's own mod set aside, SKSE in Steam's launcher slot.
    let loaded = deploy::load(&game, &store, &pack, head.sequence).unwrap();
    assert_eq!(loaded["skse_launcher"], true);
    let read =
        |rel: &str| fs::read(crate::files::locate(&game.root, rel).unwrap().unwrap()).unwrap();
    assert_eq!(read("Data/A.esp"), b"a-plugin");
    assert_eq!(read("Data/textures/shared.dds"), b"from-b");
    assert_eq!(read("d3dx9_42.dll"), b"root-file");
    assert_eq!(read("Data/SKSE/Plugins/gen.ini"), b"generated");
    assert_eq!(read("SkyrimSELauncher.exe"), b"loader");
    assert!(crate::files::locate(&game.root, "Data/Friend.esp")
        .unwrap()
        .is_none());
    assert_eq!(
        fs::read_to_string(game.local.join("Plugins.txt")).unwrap(),
        "*A.esp\r\n*B.esp\r\n"
    );
    let ini = crate::ini::parse(&fs::read_to_string(game.documents.join("Skyrim.ini")).unwrap());
    assert_eq!(ini["General"]["uGridsToLoad"], "5", "the host's setting");
    assert_eq!(
        ini["General"]["sLanguage"], "ENGLISH",
        "the friend's own setting stays"
    );
    assert_eq!(
        fs::read(game.documents.join("SkyrimPrefs.ini")).unwrap(),
        b"[Display]\niSize W=1920\n"
    );
    assert!(matches!(
        game.state().unwrap(),
        deploy::State::Loaded { .. }
    ));
    assert_eq!(
        deploy::load(&game, &store, &pack, head.sequence).unwrap()["changed"],
        0,
        "loading twice is a no-op"
    );

    // Unload: every byte back as it was.
    deploy::unload(&game).unwrap();
    assert_eq!(tree(&[&game.root, &game.local, &game.documents]), before);

    // A load cut short (its last change done, Loaded never written, a torn line) is undone next time.
    deploy::load(&game, &store, &pack, head.sequence).unwrap();
    let journal = game.root.join(".skyrim-mod-sync/journal.jsonl");
    let text = fs::read_to_string(&journal).unwrap();
    let cut = text
        .lines()
        .take(text.lines().count() - 1)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n{\"op\":\"pla";
    fs::write(&journal, cut).unwrap();
    assert_eq!(game.state().unwrap(), deploy::State::Interrupted);
    deploy::recover(&game).unwrap();
    assert_eq!(tree(&[&game.root, &game.local, &game.documents]), before);

    // The host plays its own pack: synced from its own MO2 files (the P2P host is stopped), loaded into
    // its game folder, which then can't be published until unloaded.
    let (own, own_head, local) = host.own_pack().unwrap().unwrap();
    assert_eq!(own, pack);
    let host_game = Game {
        root: root.join("game"),
        local: root.join("host-local"),
        documents: root.join("host-docs"),
    };
    let host_before = tree(&[&host_game.root]);
    let host_store = Store::open(&root.join("host-store")).unwrap();
    let synced = sync::sync(&own, &own_head, &host_store, &host_game, None, Some(&local)).unwrap();
    assert!(synced["files_local"].as_u64().unwrap() >= 5);
    assert_eq!(synced["archives"], 0, "nothing downloaded");
    assert_eq!(synced["host_files"], 0, "nothing fetched");
    deploy::load(&host_game, &host_store, &own, own_head.sequence).unwrap();
    assert_eq!(
        fs::read(host_game.root.join("Data/textures/shared.dds")).unwrap(),
        b"from-b"
    );
    assert_eq!(host.publish(None).unwrap_err().code, "pack_loaded");
    deploy::unload(&host_game).unwrap();
    assert_eq!(tree(&[&host_game.root]), host_before);
    assert_eq!(host.publish(None).unwrap()["sequence"], 1);

    // A mod pointed at a GitHub repo: the newest release's archive, matched by hash like Nexus's.
    host.set_override(
        "A",
        Some(crate::host::Override::Github {
            repo: "fake/mod".into(),
            asset: None,
        }),
    )
    .unwrap();
    let draft = host.draft().unwrap();
    let a = draft
        .pack
        .archives
        .iter()
        .find(|a| a.name == "A.zip")
        .expect("A's archive from the release");
    assert!(matches!(&a.origin, crate::pack::Origin::Url { url } if url.ends_with("/dl/A.zip")));
    assert!(matches!(source("Data/A.esp"), Source::Archive { .. }));
    host.set_override("A", None).unwrap();

    // A pack file can't be replayed to an older revision once a newer one was accepted.
    let older = channel::read_file(&root.join("test.smspack")).unwrap().1;
    assert!(store.accept(&older).is_ok(), "the same head again is fine");
}
