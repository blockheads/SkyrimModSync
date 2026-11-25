use serde_json::json;
use skyrim_mod_sync::{
    acquisition, config, files,
    package::{self, Artifact, Code},
    recipe::{self, Recipe},
    snapshot,
    target::{self, Storage},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use zip::{write::SimpleFileOptions, ZipWriter};
struct Fixture {
    dir: TempDir,
    reconstructed: PathBuf,
    artifact: PathBuf,
    base: PathBuf,
    downloads: PathBuf,
    storage: Storage,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let reconstructed = root.join("reconstructed");
        let base = root.join("licensed base");
        let downloads = root.join("archive cache");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&downloads).unwrap();
        fs::create_dir_all(&reconstructed).unwrap();
        let mut entries = vec![
            ("Stock Game/SkyrimSE.exe", b"fixture Skyrim binary".to_vec()),
            (
                "Stock Game/skse64_loader.exe",
                b"fixture SKSE binary".to_vec(),
            ),
            ("Stock Game/d3dx9_42.dll", b"preloader".to_vec()),
            ("Stock Game/Data/Skyrim.esm", plugin(false)),
            ("mods/Co-op/root/coop-native.dll", b"root binary".to_vec()),
            (
                "mods/Co-op/SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll",
                b"co-op client build A".to_vec(),
            ),
            ("mods/Low/Foo.esp", plugin(false)),
            ("mods/Low/Bar.esl", plugin(true)),
            ("mods/Low/scripts/test.pex", b"compiled script".to_vec()),
            ("mods/Low/meshes/test.nif", b"mesh-low".to_vec()),
            ("mods/High/meshes/test.nif", b"mesh-high".to_vec()),
            (
                "profiles/Test/modlist.txt",
                b"+High\n+Co-op\n+Low\n".to_vec(),
            ),
            (
                "profiles/Test/plugins.txt",
                b"*Foo.esp\n*Bar.esl\n".to_vec(),
            ),
            (
                "profiles/Test/loadorder.txt",
                b"Skyrim.esm\nFoo.esp\nBar.esl\n".to_vec(),
            ),
            (
                "profiles/Test/Skyrim.ini",
                b"[General]\nuGridsToLoad=5\n\n".to_vec(),
            ),
            (
                "profiles/Test/SkyrimPrefs.ini",
                b"[Display]\niSize W=1920\n".to_vec(),
            ),
            (
                "profiles/Test/SkyrimCustom.ini",
                b"[General]\nbTest=1\n".to_vec(),
            ),
        ];
        entries.push(("Stock Game/Data/voice.bsa", b"archive content".to_vec()));
        let artifact = root.join("fixture.wabbajack");
        let mut zip = ZipWriter::new(fs::File::create(&artifact).unwrap());
        let mut directives = vec![];
        for (i, (name, bytes)) in entries.iter().enumerate() {
            write(&reconstructed.join(name), bytes);
            let id = format!("source-{i}");
            zip.start_file(&id, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
            directives.push(json!({"$type":"InlineFile","To":name.replace('/',"\\"),"Size":bytes.len(),"Hash":acquisition::wj_hash(&reconstructed.join(name)).unwrap(),"SourceDataID":id}));
        }
        zip.start_file("modlist", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            &serde_json::to_vec(&json!({"Name":"Test list","Archives":[],"Directives":directives}))
                .unwrap(),
        )
        .unwrap();
        zip.finish().unwrap();
        let storage = Storage::open(&root.join("managed co-op")).unwrap();
        Self {
            dir,
            reconstructed,
            artifact,
            base,
            downloads,
            storage,
        }
    }
    fn manifest(&self) -> package::Manifest {
        let candidate = self.dir.path().join("candidate");
        fs::create_dir(&candidate).unwrap();
        recipe::materialize(
            &Recipe::load(&self.artifact).unwrap(),
            &self.reconstructed,
            "Test",
            &candidate,
        )
        .unwrap()
    }
    fn package(&self) -> package::Package {
        let manifest = self.manifest();
        let path = self.dir.path().join("target.skyrimsync");
        package::write(&path, &manifest, &self.artifact).unwrap();
        package::load(&path).unwrap()
    }
    #[cfg(target_os = "linux")]
    fn sync(&self, p: &package::Package) -> serde_json::Value {
        target::sync(
            p,
            &self.storage,
            &self.base,
            &self.downloads,
            None,
            None,
            Some(&self.reconstructed),
        )
        .unwrap()
    }
}
fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn plugin(light: bool) -> Vec<u8> {
    let mut h = b"TES4\0\0\0\0\0\0\0\0payload".to_vec();
    h[8..12].copy_from_slice(&(if light { 0x200u32 } else { 0u32 }).to_le_bytes());
    h
}
#[test]
fn paths_reject_escape_reserved_and_ambiguous_trees() {
    for p in [
        "../x",
        "/Data/x",
        "Data/CON.txt",
        "Data/a:b",
        "a\\b",
        "a./x",
        "a /x",
        "a//b",
        "a/../b",
    ] {
        assert!(files::relative(p).is_err(), "{p}");
    }
    assert!(files::relative("Data/élève/file.esp").is_ok());
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("Data/a"), b"a");
    write(&d.path().join("data/b"), b"b");
    #[cfg(unix)]
    assert_eq!(
        files::inventory(d.path()).unwrap_err().code,
        "case_collision"
    );
}
#[cfg(unix)]
#[test]
fn symbolic_links_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("file"), b"abc");
    std::os::unix::fs::symlink(d.path().join("file"), d.path().join("link")).unwrap();
    assert_eq!(files::inventory(d.path()).unwrap_err().code, "path_escape");
}
#[test]
fn fixture_matches_and_all_file_categories_are_authoritative() {
    let f = Fixture::new();
    let manifest = f.manifest();
    let want = manifest.snapshot.as_ref().unwrap();
    let candidate = f.dir.path().join("candidate");
    assert_eq!(want.plugins.len(), 3);
    assert_eq!(want.plugins[2].light, Some(true));
    assert_eq!(
        want.versions.coop_sha256,
        Some(files::hash_bytes(b"co-op client build A"))
    );
    assert_eq!(
        fs::read(candidate.join("game/Data/meshes/test.nif")).unwrap(),
        b"mesh-high"
    );
    assert!(
        snapshot::compare(want, &snapshot::capture_install(&candidate).unwrap())
            .unwrap()
            .files_match
    );
    for path in [
        "game/SkyrimSE.exe",
        "game/skse64_loader.exe",
        "game/d3dx9_42.dll",
        "game/coop-native.dll",
        "game/Data/Skyrim.esm",
        "game/Data/Foo.esp",
        "game/Data/Bar.esl",
        "game/Data/voice.bsa",
        "game/Data/scripts/test.pex",
        "game/Data/meshes/test.nif",
        "game/Data/SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll",
        "config/documents/Skyrim.ini",
        "config/documents/SkyrimPrefs.ini",
        "config/documents/SkyrimCustom.ini",
        "config/local/plugins.txt",
        "config/local/loadorder.txt",
    ] {
        let file = candidate.join(path);
        let original = fs::read(&file).unwrap();
        let mut altered = original.clone();
        let i = altered.len() - 1;
        altered[i] ^= 1;
        fs::write(&file, &altered).unwrap();
        let actual = snapshot::capture_install(&candidate);
        if let Ok(actual) = actual {
            let comparison = snapshot::compare(want, &actual).unwrap();
            assert!(!comparison.files_match, "{path}");
            assert!(
                comparison
                    .differences
                    .iter()
                    .any(|d| d["kind"] == "changed" && d["path"] == path),
                "{path}"
            );
        }
        fs::write(file, original).unwrap();
    }
}
#[test]
fn logs_and_saves_are_excluded_but_extra_native_dlls_are_detected() {
    let f = Fixture::new();
    let manifest = f.manifest();
    let root = f.dir.path().join("candidate");
    write(&root.join("game/d3dx9_42.log"), b"runtime");
    write(&root.join("game/Saves/save.ess"), b"private");
    assert!(
        snapshot::compare(
            manifest.snapshot.as_ref().unwrap(),
            &snapshot::capture_install(&root).unwrap()
        )
        .unwrap()
        .files_match
    );
    write(
        &root.join("game/Data/SKSE/Plugins/unrecognized.dll"),
        b"new binary",
    );
    write(
        &root.join("game/Logs/unrecognized.dll"),
        b"extra native binary",
    );

    let diff = snapshot::compare(
        manifest.snapshot.as_ref().unwrap(),
        &snapshot::capture_install(&root).unwrap(),
    )
    .unwrap();
    assert!(diff.differences.iter().any(|d| d["kind"] == "extra"));
    assert!(diff
        .differences
        .iter()
        .any(|d| d["kind"] == "extra" && d["path"] == "game/Logs/unrecognized.dll"));
}
#[test]
fn declared_plugin_order_light_and_missing_are_reported() {
    let f = Fixture::new();
    let m = f.manifest();
    let root = f.dir.path().join("candidate");
    let want = m.snapshot.unwrap();
    write(
        &root.join("config/local/loadorder.txt"),
        b"Skyrim.esm\nBar.esl\nFoo.esp\n",
    );
    write(&root.join("game/Data/Bar.esl"), &plugin(false));
    let actual = snapshot::capture_install(&root).unwrap();
    let diff = snapshot::compare(&want, &actual).unwrap();
    assert!(diff
        .differences
        .iter()
        .any(|d| d["kind"] == "plugin_order" && d["after"] == "Skyrim.esm"));
    assert!(diff.differences.iter().any(|d| d["kind"] == "plugin_light"));
    write(&root.join("config/local/plugins.txt"), b"*Bar.esl\n");
    let actual = snapshot::capture_install(&root).unwrap();
    assert!(snapshot::compare(&want, &actual)
        .unwrap()
        .differences
        .iter()
        .any(|d| d["kind"] == "plugin_missing"));
}
#[cfg(target_os = "linux")]
#[test]
fn install_repair_backup_rollback_and_offline_reuse() {
    let f = Fixture::new();
    let p = f.package();
    assert_eq!(f.sync(&p)["state"], "files_verified");
    let root = f.storage.instance();
    let initial = snapshot::capture_install(&root).unwrap();
    write(&root.join("game/Data/Foo.esp"), &plugin(true));
    write(&root.join("game/Data/extra.dll"), b"extra");
    write(
        &root.join("config/documents/Saves/save.ess"),
        b"private save",
    );
    fs::remove_file(root.join("game/SkyrimSE.exe")).unwrap();
    let result = f.sync(&p);
    assert_eq!(result["state"], "files_verified");
    assert!(
        snapshot::compare(&initial, &snapshot::capture_install(&root).unwrap())
            .unwrap()
            .files_match
    );
    assert_eq!(
        fs::read(root.join("config/documents/Saves/save.ess")).unwrap(),
        b"private save"
    );
    let backups = fs::read_dir(f.storage.root.join("backups"))
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path().join("game/Data/extra.dll").exists());
    fs::remove_dir_all(&f.reconstructed).unwrap();
    assert_eq!(f.sync(&p)["reused"], true);
    f.storage.rollback().unwrap();
    assert!(root.join("game/Data/extra.dll").exists());
    assert!(!root.join("game/SkyrimSE.exe").exists());
}
#[cfg(target_os = "linux")]
#[test]
fn unknown_mod_or_changed_reconstruction_blocks_publication_and_repair() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    let root = f.storage.instance();
    let before = fs::read(root.join("game/Data/Foo.esp")).unwrap();
    write(&root.join("game/Data/Foo.esp"), &plugin(true));
    write(
        &f.reconstructed.join("mods/Low/Foo.esp"),
        b"changed archive output",
    );
    let error = target::sync(
        &p,
        &f.storage,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(&f.reconstructed),
    )
    .unwrap_err();
    assert_eq!(error.code, "reconstruction_mismatch");
    assert_ne!(fs::read(root.join("game/Data/Foo.esp")).unwrap(), before);
    assert!(!f.storage.root.join("transaction.json").exists());
}
#[test]
fn manual_coop_changes_fail_host_provenance_check() {
    let f = Fixture::new();
    let m = f.manifest();
    let root = f.dir.path().join("candidate");
    write(
        &root.join("game/Data/SKSE/Plugins/manual.dll"),
        b"manual binary",
    );
    let host = snapshot::capture_install(&root).unwrap();
    let error = target::publish(
        &host,
        &Recipe::load(&f.artifact).unwrap(),
        &f.reconstructed,
        "Test",
        &f.dir.path().join("bad.skyrimsync"),
        &f.storage,
    )
    .unwrap_err();
    assert_eq!(error.code, "unreproducible_target");
    assert!(!f.dir.path().join("bad.skyrimsync").exists());
    assert!(m.snapshot.is_some());
}
#[test]
fn packages_codes_pin_exact_bytes_and_reject_conflicting_metadata() {
    let f = Fixture::new();
    let p = f.package();
    let code = Code {
        schema: 2,
        url: "https://example.org/co-op.skyrimsync".into(),
        sha256: files::sha(&p.path).unwrap(),
        size: fs::metadata(&p.path).unwrap().len(),
    };
    let encoded = code.encode().unwrap();
    assert_eq!(Code::decode(&encoded).unwrap().sha256, code.sha256);
    assert!(Code::decode("SMS1.other").is_err());
    let mut m = p.manifest.clone();
    m.files[0].sha256 = "0".repeat(64);
    assert!(m.validate().is_err());
    let bad = Code {
        schema: 2,
        url: "https://user:secret@example.org/x".into(),
        sha256: code.sha256,
        size: code.size,
    };
    assert!(bad.encode().is_err());
    let artifact = Artifact {
        sha256: "0".repeat(64),
        size: 1,
        url: None,
    };
    assert_eq!(artifact.size, 1);
}
#[test]
fn refuses_unowned_nonempty_folders_and_concurrent_writers() {
    let d = tempfile::tempdir().unwrap();
    write(&d.path().join("personal-game"), b"precious");
    assert_eq!(
        Storage::open(d.path()).unwrap_err().code,
        "unowned_destination"
    );
    let lock = d.path().join("locks/test");
    let _a = files::Lock::take(&lock).unwrap();
    assert_eq!(files::Lock::take(&lock).err().unwrap().code, "busy");
}
#[test]
fn rejects_machine_specific_shared_ini_paths() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("Skyrim.ini");
    for value in [
        "C:\\Users\\name\\secret",
        "/home/name/mods",
        "\\\\server\\share",
    ] {
        write(&p, format!("[General]\nsPath={value}\n").as_bytes());
        assert_eq!(
            snapshot::portable_configuration("Skyrim.ini", &p)
                .unwrap_err()
                .code,
            "unportable_configuration"
        );
    }
    write(&p, b"[Archive]\nsResourceDataDirsFinal=Data\\meshes\n");
    assert!(snapshot::portable_configuration("Skyrim.ini", &p).is_ok());
}
#[test]
fn corrupted_archive_hash_is_not_reused() {
    let f = Fixture::new();
    let mut recipe = Recipe::load(&f.artifact).unwrap();
    write(&f.downloads.join("cache-name.zip"), b"good");
    let hash = acquisition::wj_hash(&f.downloads.join("cache-name.zip")).unwrap();
    recipe.data["Archives"] = json!([{"Name":"expected.zip","Size":4,"Hash":hash,"State":{"$type":"HttpDownloader+State","Url":"https://example.org/a.zip?private=token"}}]);
    assert_eq!(
        recipe.archives(&f.base, &f.downloads).unwrap()["verified_cached"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    write(&f.downloads.join("cache-name.zip"), b"evil");
    let plan = recipe.archives(&f.base, &f.downloads).unwrap();
    assert_eq!(plan["missing_bytes"], 4);
    assert_eq!(plan["missing"][0]["url"], "https://example.org/a.zip");
}
#[test]
fn discovery_primary_survives_malformed_metadata_and_lists_flatpak() {
    let home = tempfile::tempdir().unwrap();
    let steam = home.path().join(".local/share/Steam");
    write(&steam.join("steamapps/libraryfolders.vdf"), b"\"broken\" {");
    write(
        &steam.join("steamapps/appmanifest_489830.acf"),
        b"\"AppState\" { \"installdir\" \"Skyrim test\" }",
    );
    write(
        &steam.join("steamapps/common/Skyrim test/SkyrimSE.exe"),
        b"exe",
    );
    let flatpak = home
        .path()
        .join(".var/app/com.valvesoftware.Steam/.local/share/Steam");
    write(
        &flatpak.join("steamapps/appmanifest_489830.acf"),
        b"\"AppState\" { \"installdir\" \"Skyrim test\" }",
    );
    write(
        &flatpak.join("steamapps/common/Skyrim test/SkyrimSE.exe"),
        b"exe",
    );
    let result = config::discover(home.path(), None).unwrap();
    assert_eq!(result["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(result["ambiguous"], true);
    assert!(result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["reason"] == "malformed_library_metadata"));
    assert!(result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["flatpak"] == true && v["integration_validated"] == false));
}
#[cfg(target_os = "linux")]
#[test]
fn legacy_v1_package_has_native_inspection_and_install() {
    let f = Fixture::new();
    let mut m = f.manifest();
    m.schema = 1;
    m.snapshot = None;
    m.profile = None;
    m.layout = None;
    let path = f.dir.path().join("legacy.skyrimsync");
    package::write(&path, &m, &f.artifact).unwrap();
    let p = package::load(&path).unwrap();
    f.sync(&p);
    assert_eq!(
        target::verify(&p.manifest, &f.storage.instance()).unwrap()["files_match"],
        true
    );
}
#[cfg(target_os = "linux")]
#[test]
fn interrupted_publication_restores_the_previous_install() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    fs::create_dir_all(f.storage.root.join("backups")).unwrap();
    let backup = f.storage.root.join("backups/0001");
    fs::rename(f.storage.instance(), &backup).unwrap();
    let stage = f.storage.root.join("stages/interrupted");
    fs::create_dir_all(&stage).unwrap();
    files::atomic_json(&f.storage.root.join("transaction.json"),&json!({"schema":1,"stage":"stages/interrupted","backup":"backups/0001","target_id":p.manifest.id().unwrap()})).unwrap();
    f.storage.recover().unwrap();
    assert!(f.storage.instance().join("game/SkyrimSE.exe").exists());
    assert!(!f.storage.root.join("transaction.json").exists());
    assert_eq!(
        target::verify(&p.manifest, &f.storage.instance()).unwrap()["files_match"],
        true
    );
}
#[test]
fn json_cli_differences_and_configuration_errors_have_stable_exit_codes() {
    let binary = env!("CARGO_BIN_EXE_skyrim-mod-sync");
    let d = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(binary)
        .args(["--json", "capture", "--output"])
        .arg(d.path().join("missing.json"))
        .env("XDG_CONFIG_HOME", d.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["reason"], "configuration_required");
    let f = Fixture::new();
    let m = f.manifest();
    let a = f.dir.path().join("a.json");
    let b = f.dir.path().join("b.json");
    files::atomic_json(&a, &m.snapshot).unwrap();
    let mut changed = m.snapshot.unwrap();
    changed.files[0].sha256 = "0".repeat(64);
    files::atomic_json(&b, &changed).unwrap();
    let output = std::process::Command::new(binary)
        .args(["--json", "compare"])
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["files_match"], false);
    assert_eq!(value["runtime_verified"], false);
}
#[test]
fn duplicate_nested_recipe_keys_and_invalid_light_types_fail() {
    assert!(skyrim_mod_sync::json::value(br#"{"outer":{"Hash":"one","Hash":"two"}}"#).is_err());
    let f = Fixture::new();
    let m = f.manifest();
    let mut value = serde_json::to_value(m.snapshot.unwrap()).unwrap();
    value["plugins"][0]["light"] = json!(0);
    assert!(serde_json::from_value::<snapshot::Snapshot>(value).is_err());
}
#[test]
fn disk_preflight_reports_an_actionable_blocker() {
    let d = tempfile::tempdir().unwrap();
    let error = files::space(d.path(), u64::MAX).unwrap_err();
    assert_eq!(error.code, "disk_full");
    assert!(error.message.contains("Choose another folder"));
}
#[test]
fn machine_remapped_runtime_sources_and_untracked_ini_tweaks_are_refused() {
    let f = Fixture::new();
    let mut recipe = Recipe::load(&f.artifact).unwrap();
    recipe.paths.insert(
        "stock game/skyrimse.exe".into(),
        "RemappedInlineFile".into(),
    );
    let candidate = f.dir.path().join("bad");
    fs::create_dir(&candidate).unwrap();
    assert_eq!(
        recipe::materialize(&recipe, &f.reconstructed, "Test", &candidate)
            .unwrap_err()
            .code,
        "untracked_output"
    );
    recipe
        .paths
        .insert("stock game/skyrimse.exe".into(), "InlineFile".into());
    write(
        &f.reconstructed.join("profiles/Test/initweaks.ini"),
        b"[Archive]\nbInvalidateOlderFiles=1\n",
    );
    assert_eq!(
        recipe::materialize(&recipe, &f.reconstructed, "Test", &candidate)
            .unwrap_err()
            .code,
        "untracked_output"
    );
}
#[test]
fn known_ui_layout_output_and_shipped_cache_hint_do_not_hide_mod_settings() {
    let f = Fixture::new();
    let m = f.manifest();
    let root = f.dir.path().join("candidate");
    write(
        &root.join("game/Data/SKSE/Plugins/OpenAnimationReplacer.dll"),
        b"supported dll fixture",
    );
    let want = snapshot::capture_install(&root).unwrap();
    write(
        &root.join("game/Data/SKSE/Plugins/OpenAnimationReplacer_imgui.ini"),
        b"[Window][Debug]\nPos=1,2\n",
    );
    assert!(
        snapshot::compare(&want, &snapshot::capture_install(&root).unwrap())
            .unwrap()
            .files_match
    );
    write(
        &root.join("game/Data/SKSE/Plugins/OpenAnimationReplacer.ini"),
        b"[General]\nuAnimationLimit=5\n",
    );
    assert!(
        !snapshot::compare(&want, &snapshot::capture_install(&root).unwrap())
            .unwrap()
            .files_match
    );
    let ini = root.join("game/Data/SKSE/Plugins/CrashLogger.ini");
    write(&ini, b"[Debug]\nSymcache Directory = c:\\symcache\n");
    assert!(
        snapshot::portable_configuration("game/Data/SKSE/Plugins/CrashLogger.ini", &ini).is_ok()
    );
    write(
        &ini,
        b"[Debug]\nSymcache Directory = c:\\Users\\name\\symcache\n",
    );
    assert!(
        snapshot::portable_configuration("game/Data/SKSE/Plugins/CrashLogger.ini", &ini).is_err()
    );
    assert!(m.snapshot.is_some());
}
#[cfg(target_os = "linux")]
#[test]
fn running_target_is_refused_before_any_repair() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    let root = f.storage.instance();
    let before = files::sha(&root.join("game/SkyrimSE.exe")).unwrap();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .current_dir(&root)
        .spawn()
        .unwrap();
    let result = target::sync(
        &p,
        &f.storage,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(&f.reconstructed),
    );
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(result.unwrap_err().code, "target_running");
    assert_eq!(files::sha(&root.join("game/SkyrimSE.exe")).unwrap(), before);
}
#[cfg(target_os = "linux")]
#[test]
fn cancellation_returns_130_without_creating_a_snapshot() {
    use std::io::{BufRead, BufReader};
    let f = Fixture::new();
    let m = f.manifest();
    let root = f.dir.path().join("candidate");
    // A sparse 512 MiB asset makes cancellation deterministic without allocating RAM.
    let huge = fs::File::create(root.join("game/Data/large.bsa")).unwrap();
    huge.set_len(512 * 1024 * 1024).unwrap();
    let out = f.dir.path().join("cancelled.json");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "capture", "--game"])
        .arg(root.join("game"))
        .arg("--documents")
        .arg(root.join("config/documents"))
        .arg("--local")
        .arg(root.join("config/local"))
        .arg("--output")
        .arg(&out)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut first = String::new();
    BufReader::new(stderr).read_line(&mut first).unwrap();
    assert!(first.contains("capturing"));
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(130));
    assert!(!out.exists());
    assert!(m.snapshot.is_some());
}
#[test]
fn xdg_empty_and_relative_values_use_absolute_fallbacks() {
    let binary = env!("CARGO_BIN_EXE_skyrim-mod-sync");
    let home = config::home().unwrap();
    for value in ["", "relative/settings"] {
        let out = std::process::Command::new(binary)
            .args(["--json", "doctor"])
            .env("XDG_CONFIG_HOME", value)
            .env("XDG_DATA_HOME", value)
            .output()
            .unwrap();
        assert!(out.status.success());
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["state"], "diagnostics");
        #[cfg(not(windows))]
        {
            assert_eq!(
                v["config_directory"],
                json!(home.join(".config/skyrim-mod-sync"))
            );
            assert_eq!(
                v["default_storage"],
                json!(home.join(".local/share/skyrim-mod-sync"))
            );
        }
        let _ = home;
    }
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "Requires working Linux bubblewrap user namespaces; run explicitly"]
fn real_namespace_preserves_home_and_binds_explicit_sources_and_cache() {
    let home = config::home().unwrap();
    let temp = tempfile::tempdir_in(&home).unwrap();
    let base = temp.path().join("licensed base");
    let cache = temp.path().join("Unicode archive cache é");
    fs::create_dir_all(&base).unwrap();
    fs::create_dir_all(&cache).unwrap();
    write(&base.join("test"), b"licensed bytes");
    write(&cache.join("test"), b"archive bytes");
    let owned = tempfile::tempdir().unwrap();
    let storage = Storage::open(owned.path()).unwrap();
    let job = storage.root.join("jobs/test");
    fs::create_dir_all(&job).unwrap();
    let engine = storage.root.join("tools/jackify-engine-0.5.10");
    write(&engine.join("jackify-engine"), b"pinned fixture");
    let mut command =
        skyrim_mod_sync::engine::namespace(&storage.root, &job, &base, &cache, None, None).unwrap();
    command.arg("/bin/sh").args(["-c","test \"$HOME\" = \"$1\" && cat \"$2/test\" && cat \"$3/test\" && touch \"$3/new\" && ! touch \"$2/new\" && test ! -e \"$HOME/.config/skyrim-mod-sync/nexus-api-key\" && touch \"$4/logs/wabbajack-cli.current.log\" && ! touch \"$4/injected.dll\"","namespace-check"]).arg(home).arg(base).arg(&cache).arg(&engine);
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(cache.join("new").exists());
    assert!(job.join("engine-logs/wabbajack-cli.current.log").exists());
    assert!(!engine.join("logs/wabbajack-cli.current.log").exists());
    assert!(!engine.join("injected.dll").exists());
}
#[test]
fn new_destination_paths_are_normalized_before_overlap_checks() {
    let d = tempfile::tempdir().unwrap();
    let base = d.path().join("licensed");
    fs::create_dir(&base).unwrap();
    let misleading = d.path().join("unused/../licensed/new-instance");
    let resolved = config::absolute(&misleading).unwrap();
    assert_eq!(resolved, base.join("new-instance"));
    assert_eq!(
        files::separate(&base, &resolved).unwrap_err().code,
        "overlapping_paths"
    );
    assert!(!base.join("new-instance").exists());
}
#[cfg(target_os = "linux")]
#[test]
fn missing_configuration_and_game_directories_can_be_repaired() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    let root = f.storage.instance();
    fs::remove_dir_all(root.join("config")).unwrap();
    assert_eq!(
        target::verify(&p.manifest, &root).unwrap()["files_match"],
        false
    );
    f.sync(&p);
    assert_eq!(
        target::verify(&p.manifest, &root).unwrap()["files_match"],
        true
    );
    fs::remove_dir_all(root.join("game")).unwrap();
    let diff = target::verify(&p.manifest, &root).unwrap();
    assert_eq!(diff["differences"][0]["kind"], "missing_directory");
    f.sync(&p);
    assert_eq!(
        target::verify(&p.manifest, &root).unwrap()["files_match"],
        true
    );
}
#[test]
fn coop_generated_loader_copies_are_separate_from_the_source_binary() {
    let f = Fixture::new();
    let _m = f.manifest();
    let root = f.dir.path().join("candidate");
    write(
        &root.join("game/Data/SKSE/Plugins/SkyrimCoopLoader.dll"),
        b"loader",
    );
    let want = snapshot::capture_install(&root).unwrap();
    write(
        &root.join("game/Data/SKSE/Plugins/SkyrimCoop/loaded/SkyrimTogetherClient-host.123.0.dll"),
        b"runtime copy",
    );
    assert!(
        snapshot::compare(&want, &snapshot::capture_install(&root).unwrap())
            .unwrap()
            .files_match
    );
    write(
        &root.join("game/Data/SKSE/Plugins/SkyrimCoop/loaded/other.dll"),
        b"unknown native file",
    );
    assert!(
        !snapshot::compare(&want, &snapshot::capture_install(&root).unwrap())
            .unwrap()
            .files_match
    );
    write(
        &root.join("game/Data/SKSE/Plugins/SkyrimCoop/SkyrimTogetherClient.dll"),
        b"changed source",
    );
    let diff = snapshot::compare(&want, &snapshot::capture_install(&root).unwrap()).unwrap();
    assert!(diff
        .differences
        .iter()
        .any(|d| d["kind"] == "version" && d["component"] == "coop_sha256"));
}

#[cfg(target_os = "linux")]
#[test]
fn incremental_repair_preserves_unchanged_inodes_and_stages_only_changed_bytes() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    let root = f.storage.instance();
    let unchanged = root.join("game/Data/voice.bsa");
    let before = fs::metadata(&unchanged).unwrap();
    // Existing directory spelling must survive a repair with Windows path casing.
    fs::rename(
        root.join("game/Data/scripts"),
        root.join("game/Data/SCRIPTS"),
    )
    .unwrap();
    let path = "game/Data/scripts/test.pex";
    fs::remove_file(root.join("game/Data/SCRIPTS/test.pex")).unwrap();
    write(&root.join("game/extra.dll"), b"extra");
    let expected = p
        .manifest
        .files
        .iter()
        .find(|f| f.path == path)
        .unwrap()
        .size;
    let result = f.sync(&p);
    assert_eq!(result["files_replaced"], 1);
    assert_eq!(result["files_removed"], 1);
    assert_eq!(result["staged_bytes"], expected);
    let after = fs::metadata(&unchanged).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.mtime_nsec()),
        (after.ino(), after.mtime(), after.mtime_nsec())
    );
    assert!(root.join("game/Data/SCRIPTS/test.pex").is_file());
    assert!(!root.join("game/Data/scripts").exists());
    let backup = Path::new(result["backup"].as_str().unwrap());
    assert!(!backup.join("game/Data/voice.bsa").exists());
    assert!(!backup.join("game/Data/SCRIPTS/test.pex").exists());
    f.storage.rollback().unwrap();
    assert!(!root.join("game/Data/SCRIPTS/test.pex").exists());
    assert!(root.join("game/extra.dll").exists());
    assert_eq!(before.ino(), fs::metadata(unchanged).unwrap().ino());
}

#[cfg(target_os = "linux")]
#[test]
fn repair_inline_configuration_offline_without_engine_or_reconstructed_tree() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    fs::remove_dir_all(&f.reconstructed).unwrap();
    write(
        &f.storage.instance().join("config/documents/Skyrim.ini"),
        b"changed",
    );
    let plan = target::plan(&p, &f.base, &f.downloads, &f.storage.root).unwrap();
    assert_eq!(plan["files_replaced"], 1);
    assert_eq!(plan["acquisition"]["missing_bytes"], 0);
    assert_eq!(plan["acquisition"]["reconstruction_bytes"], 0);
    let result = target::sync(&p, &f.storage, &f.base, &f.downloads, None, None, None).unwrap();
    assert_eq!(result["files_replaced"], 1);
    assert!(!f.storage.root.join("tools").exists());
    assert!(!f.storage.root.join("jobs").exists());
    assert_eq!(
        target::verify(&p.manifest, &f.storage.instance()).unwrap()["files_match"],
        true
    );
    write(&f.storage.instance().join("game/extra.dll"), b"extra");
    let result = target::sync(&p, &f.storage, &f.base, &f.downloads, None, None, None).unwrap();
    assert_eq!(result["staged_bytes"], 0);
    assert_eq!(result["files_removed"], 1);
}

#[cfg(target_os = "linux")]
#[test]
fn repair_ignores_corruption_in_unrequested_cached_outputs() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    write(
        &f.reconstructed.join("Stock Game/Data/voice.bsa"),
        b"unrelated corruption",
    );
    fs::remove_file(f.storage.instance().join("game/Data/Foo.esp")).unwrap();
    assert_eq!(f.sync(&p)["files_replaced"], 1);
}

#[cfg(target_os = "linux")]
#[test]
fn repair_path_conflict_has_actionable_error_and_preserves_current_files() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    let root = f.storage.instance();
    fs::remove_file(root.join("game/Data/Foo.esp")).unwrap();
    fs::create_dir(root.join("game/Data/Foo.esp")).unwrap();
    let error = target::sync(
        &p,
        &f.storage,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(&f.reconstructed),
    )
    .unwrap_err();
    assert_eq!(error.code, "path_conflict");
    assert!(root.join("game/Data/Foo.esp").is_dir());
    assert!(!f.storage.root.join("transaction.json").exists());
}

#[test]
fn selective_recipe_keeps_only_needed_parent_archives_and_patch_payloads() {
    let f = Fixture::new();
    let mut recipe = Recipe::load(&f.artifact).unwrap();
    recipe.data["Archives"] = json!([
        {"Hash":"parent", "Name":"needed.zip", "Size":12,"State":{"$type":"HttpDownloader","Url":"https://example.org/needed.zip"}},
        {"Hash":"unrelated", "Name":"other.zip", "Size":999,"State":{"$type":"NexusDownloader"}}
    ]);
    recipe.data["Directives"] = json!([
        {"$type":"PatchedFromArchive","To":"mods\\A\\file.dll","ArchiveHashPath":["parent","nested.7z","file.dll"],"PatchID":"source-0","Hash":"output","Size":42},
        {"$type":"FromArchive","To":"mods/B/other.dll","ArchiveHashPath":["unrelated","other.dll"],"Hash":"other","Size":999}
    ]);
    // Write the altered recipe metadata into a fixture archive retaining its payloads.
    let output = f.dir.path().join("subset.wabbajack");
    let needed = std::collections::BTreeSet::from(["mods/a/file.dll".to_owned()]);
    let subset = recipe.subset(&needed, &output).unwrap();
    assert_eq!(subset.data["Archives"].as_array().unwrap().len(), 1);
    assert_eq!(subset.data["Archives"][0]["Hash"], "parent");
    assert_eq!(subset.data["Directives"].as_array().unwrap().len(), 1);
    let mut zip = zip::ZipArchive::new(fs::File::open(output).unwrap()).unwrap();
    assert_eq!(zip.len(), 2);
    assert!(zip.by_name("source-0").is_ok());
    assert!(zip.by_name("source-1").is_err());
    assert!(recipe
        .select(&std::collections::BTreeSet::from(["missing".to_owned()]))
        .is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn repaired_output_cache_is_reused_without_its_archive_or_an_installer() {
    let f = Fixture::new();
    let p = f.package();
    f.sync(&p);
    let instruction = p
        .manifest
        .files
        .iter()
        .find(|f| f.path == "game/Data/Foo.esp")
        .unwrap();
    let job = f
        .storage
        .root
        .join("jobs")
        .join("c".repeat(64))
        .join("reconstructed");
    write(
        &job.join(&instruction.source.path),
        &fs::read(f.storage.instance().join(&instruction.path)).unwrap(),
    );
    fs::remove_dir_all(&f.reconstructed).unwrap();
    fs::remove_file(f.storage.instance().join(&instruction.path)).unwrap();
    let result = target::sync(&p, &f.storage, &f.base, &f.downloads, None, None, None).unwrap();
    assert_eq!(result["staged_bytes"], instruction.size);
    assert!(!f.storage.root.join("tools").exists());
}

#[cfg(target_os = "linux")]
fn editable_binding(f: &Fixture) -> skyrim_mod_sync::authoring::Binding {
    skyrim_mod_sync::authoring::Binding {
        schema: 1,
        mo2: Some(f.reconstructed.clone()),
        profile: Some("Test".into()),
        game: None,
        documents: None,
        local: None,
        recipe: Some(f.artifact.clone()),
        base: f.base.clone(),
        downloads: f.downloads.clone(),
    }
}
#[cfg(target_os = "linux")]
#[test]
fn edited_profile_exports_without_rebuilding_recipe_and_freezes_local_bytes() {
    use skyrim_mod_sync::{authoring, revision};
    let f = Fixture::new();
    write(
        &f.reconstructed.join("mods/High/meshes/test.nif"),
        b"edited mesh",
    );
    write(
        &f.reconstructed.join("mods/Co-op/SKSE/Plugins/local.dll"),
        b"local DLL",
    );
    let binding = editable_binding(&f);
    let original_recipe = files::sha(&f.artifact).unwrap();
    authoring::import(&f.storage, &binding).unwrap();
    let out = f.dir.path().join("revision.skyrimsync");
    let result = authoring::publish(&f.storage, &out, &[], false).unwrap();
    assert_eq!(result["sequence"], 1);
    let r = revision::installed(&f.storage).unwrap();
    let entry = r
        .files
        .iter()
        .find(|e| e.path.to_lowercase().ends_with("/local.dll"))
        .unwrap();
    let revision::Source::Blob { hash } = &entry.source else {
        panic!("Local DLL must be a blob")
    };
    write(
        &f.reconstructed.join("mods/Co-op/SKSE/Plugins/local.dll"),
        b"new local DLL",
    );
    assert_eq!(
        fs::read(revision::object_path(&f.storage.root, hash).unwrap()).unwrap(),
        b"local DLL"
    );
    assert_eq!(files::sha(&f.artifact).unwrap(), original_recipe);
    let imported = revision::read_package(&out, &f.storage.root).unwrap();
    assert_eq!(imported.0.id().unwrap(), r.id().unwrap());
    assert!(result["code"].as_str().unwrap().starts_with("SMS3."));
}
#[cfg(target_os = "linux")]
#[test]
fn revision_heads_reject_tampering_and_unknown_base_content() {
    use skyrim_mod_sync::{authoring, revision};
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let private = authoring::private_channel(&f.storage).unwrap();
    let channel = authoring::channel(&private).unwrap();
    let key =
        ed25519_dalek::SigningKey::from_bytes(&revision::decode32(&private.signing_key).unwrap());
    let r = revision::installed(&f.storage).unwrap();
    let mut head = revision::Head::sign(channel.clone(), 1, r.id().unwrap(), None, &key).unwrap();
    head.sequence = 2;
    assert_eq!(head.validate().unwrap_err().code, "invalid_signature");
    assert_eq!(
        revision::Channel::parse(&channel.code().unwrap())
            .unwrap()
            .pack,
        channel.pack
    );
    write(
        &f.reconstructed.join("Stock Game/SkyrimSE.exe"),
        b"unknown Skyrim version",
    );
    assert_eq!(
        authoring::capture(&editable_binding(&f), &f.storage)
            .unwrap_err()
            .code,
        "base_provenance_required"
    );
}
#[cfg(target_os = "linux")]
#[test]
fn same_code_peer_sync_installs_local_mod_and_updates_only_changed_files() {
    use skyrim_mod_sync::{authoring, peer, revision};
    use std::{
        io::BufRead,
        os::unix::fs::MetadataExt,
        process::{Command, Stdio},
    };
    let f = Fixture::new();
    write(
        &f.reconstructed.join("mods/Co-op/SKSE/Plugins/local.dll"),
        b"local build one",
    );
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let first =
        authoring::publish(&f.storage, &f.dir.path().join("one.skyrimsync"), &[], false).unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "serve", "--coop"])
        .arg(&f.storage.root)
        .args(["--no-relay", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Stop<'a>(&'a mut std::process::Child);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut reader = std::io::BufReader::new(server.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let address: iroh::EndpointAddr = serde_json::from_value(ready["addresses"].clone()).unwrap();
    let ip = *address.ip_addrs().find(|a| a.is_ipv4()).unwrap();
    let _stop = Stop(&mut server);
    let channel = revision::Channel::parse(first["code"].as_str().unwrap()).unwrap();
    let friend = Storage::open(&f.dir.path().join("friend")).unwrap();
    let one = peer::fetch(&channel, &friend, None, Some(ip)).unwrap();
    peer::sync(
        &one.revision,
        &one.head,
        &friend,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(ip),
    )
    .unwrap();
    let unchanged = friend.instance().join("game/Data/voice.bsa");
    let before = fs::metadata(&unchanged).unwrap();
    write(&friend.root.join("personal/saves/private.ess"), b"save");
    write(
        &f.reconstructed.join("mods/Co-op/SKSE/Plugins/local.dll"),
        b"local build two",
    );
    let second =
        authoring::publish(&f.storage, &f.dir.path().join("two.skyrimsync"), &[], false).unwrap();
    assert_eq!(first["code"], second["code"]);
    assert_eq!(second["sequence"], 2);
    let two = peer::fetch(&channel, &friend, None, Some(ip)).unwrap();
    let result = peer::sync(
        &two.revision,
        &two.head,
        &friend,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(ip),
    )
    .unwrap();
    assert_eq!(result["files_replaced"], 1);
    assert_eq!(result["staged_bytes"], 15);
    assert_eq!(before.ino(), fs::metadata(unchanged).unwrap().ino());
    assert_eq!(
        fs::read(friend.root.join("personal/saves/private.ess")).unwrap(),
        b"save"
    );
    friend.rollback().unwrap();
    assert_eq!(
        revision::installed(&friend).unwrap().id().unwrap(),
        one.revision.id().unwrap()
    );
    let historical = peer::fetch(&channel, &friend, Some(&one.head.revision), Some(ip)).unwrap();
    assert_eq!(historical.head.sequence, 1);
    // Replaying an old latest head fails; the explicit pinned request remains valid.
    files::atomic_json(&f.storage.root.join("channel-head.json"), &one.head).unwrap();
    assert_eq!(
        peer::fetch(&channel, &friend, None, Some(ip))
            .unwrap_err()
            .code,
        "revision_replay"
    );
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "Requires working Linux user/mount namespaces and OverlayFS; run explicitly"]
fn linux_overlay_probe_preserves_installed_pack_and_records_writes() {
    use skyrim_mod_sync::authoring;
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let proton = f.dir.path().join("read-only Proton");
    let runtime = f.dir.path().join("read-only runtime");
    write(&proton.join("proton"), b"fixture entrypoint");
    write(&runtime.join("run"), b"fixture entrypoint");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "__launch-helper", "--coop"])
        .arg(&f.storage.root)
        .arg("--proton")
        .arg(&proton)
        .arg("--runtime")
        .arg(&runtime)
        .arg("--probe")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["baseline_untouched"], true);
    assert_eq!(result["runtime_written"], true);
    assert_eq!(result["tools_isolated"], true);
    assert!(!proton.join(".skyrimsync-probe").exists());
    assert!(!runtime.join(".skyrimsync-probe").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn promoted_runtime_content_survives_refresh_and_source_conflicts_are_explicit() {
    use skyrim_mod_sync::{authoring, revision};
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let before = revision::installed(&f.storage).unwrap();
    let root = f.storage.root.join("runtime").join(before.id().unwrap());
    write(
        &root.join("upper/Data/meshes/test.nif"),
        b"runtime generated mesh",
    );
    files::atomic_json(
        &f.storage.root.join("last-launch.json"),
        &json!({"revision":before.id().unwrap(),"exit_success":true}),
    )
    .unwrap();
    assert_eq!(
        authoring::publish(
            &f.storage,
            &f.dir.path().join("blocked.skyrimsync"),
            &[],
            false
        )
        .unwrap_err()
        .code,
        "runtime_review_required"
    );
    authoring::publish(
        &f.storage,
        &f.dir.path().join("promoted.skyrimsync"),
        &["game/Data/meshes/test.nif".into()],
        false,
    )
    .unwrap();
    authoring::refresh(&f.storage).unwrap();
    assert_eq!(
        fs::read(f.storage.instance().join("game/Data/meshes/test.nif")).unwrap(),
        b"runtime generated mesh"
    );
    // Republishing does not ask to promote the same historical write again.
    authoring::publish(
        &f.storage,
        &f.dir.path().join("again.skyrimsync"),
        &[],
        false,
    )
    .unwrap();
    write(
        &f.reconstructed.join("mods/High/meshes/test.nif"),
        b"new manager mesh",
    );
    assert_eq!(
        authoring::refresh(&f.storage).unwrap_err().code,
        "authoring_override_conflict"
    );
    authoring::publish_with_choices(
        &f.storage,
        &f.dir.path().join("source.skyrimsync"),
        &[],
        &["game/Data/meshes/test.nif".into()],
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        fs::read(f.storage.instance().join("game/Data/meshes/test.nif")).unwrap(),
        b"new manager mesh"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn personal_preferences_do_not_become_shared_runtime_changes() {
    use skyrim_mod_sync::{authoring, play, revision};
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let r = revision::installed(&f.storage).unwrap();
    let root = f.storage.root.join("runtime").join(r.id().unwrap());
    write(
        &root.join("seed/documents/SkyrimPrefs.ini"),
        b"[Display]\niSize W=1920\n[General]\nbShared=1\n",
    );
    write(
        &root.join("documents/SkyrimPrefs.ini"),
        b"[Display]\niSize W=1280\n[General]\nbShared=1\n",
    );
    files::atomic_json(
        &f.storage.root.join("last-launch.json"),
        &json!({"revision":r.id().unwrap()}),
    )
    .unwrap();
    assert!(play::changes(&f.storage).unwrap()[0].personal);
    write(
        &root.join("documents/SkyrimPrefs.ini"),
        b"[Display]\niSize W=1280\n[General]\nbShared=0\n",
    );
    assert!(!play::changes(&f.storage).unwrap()[0].personal);
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_whiteouts_preserve_file_deletions_and_opaque_directories() {
    use skyrim_mod_sync::{authoring, play, revision};
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    fn attr(path: &Path, name: &std::ffi::CStr, value: &[u8]) {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::lsetxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0
        );
    }
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let rev = revision::installed(&f.storage).unwrap();
    let root = f.storage.root.join("runtime").join(rev.id().unwrap());
    let opaque = root.join("upper/Data/meshes");
    fs::create_dir_all(&opaque).unwrap();
    attr(&opaque, c"user.overlay.opaque", b"y");
    let whiteout = root.join("upper/Data/scripts/test.pex");
    write(&whiteout, b"");
    attr(&whiteout, c"user.overlay.whiteout", b"");
    files::atomic_json(
        &f.storage.root.join("last-launch.json"),
        &json!({"revision":rev.id().unwrap()}),
    )
    .unwrap();
    let changes = play::changes(&f.storage).unwrap();
    assert!(changes
        .iter()
        .any(|c| c.path == "game/Data/meshes/test.nif" && c.kind == "deleted"));
    assert!(changes
        .iter()
        .any(|c| c.path == "game/Data/scripts/test.pex" && c.kind == "deleted"));
    authoring::publish(
        &f.storage,
        &f.dir.path().join("deleted.skyrimsync"),
        &[
            "game/Data/meshes/test.nif".into(),
            "game/Data/scripts/test.pex".into(),
        ],
        false,
    )
    .unwrap();
    assert!(!f
        .storage
        .instance()
        .join("game/Data/meshes/test.nif")
        .exists());
    assert!(!f
        .storage
        .instance()
        .join("game/Data/scripts/test.pex")
        .exists());
}

#[cfg(target_os = "linux")]
#[test]
fn peer_restart_keeps_code_and_repairs_corrupt_cached_content() {
    use skyrim_mod_sync::{authoring, peer, revision};
    use std::{
        io::BufRead,
        process::{Child, Command, Stdio},
    };
    struct Server(Child, std::net::SocketAddr);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn serve(storage: &Storage) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
            .args(["--json", "serve", "--coop"])
            .arg(&storage.root)
            .args(["--no-relay", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        let address: iroh::EndpointAddr =
            serde_json::from_value(ready["addresses"].clone()).unwrap();
        let ip = *address.ip_addrs().find(|ip| ip.is_ipv4()).unwrap();
        Server(child, ip)
    }
    let f = Fixture::new();
    write(
        &f.reconstructed.join("mods/Co-op/local.bin"),
        b"shared peer content",
    );
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let first = authoring::publish(
        &f.storage,
        &f.dir.path().join("first.skyrimsync"),
        &[],
        false,
    )
    .unwrap();
    let channel = revision::Channel::parse(first["code"].as_str().unwrap()).unwrap();
    let friend = Storage::open(&f.dir.path().join("friend")).unwrap();
    let server = serve(&f.storage);
    let reply = peer::fetch(&channel, &friend, None, Some(server.1)).unwrap();
    peer::sync(
        &reply.revision,
        &reply.head,
        &friend,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(server.1),
    )
    .unwrap();
    drop(server);
    let server = serve(&f.storage);
    let restarted = peer::fetch(&channel, &friend, None, Some(server.1)).unwrap();
    assert_eq!(restarted.head.revision, reply.head.revision);
    let blob = reply
        .revision
        .files
        .iter()
        .find(|e| e.path.ends_with("local.bin"))
        .unwrap();
    let revision::Source::Blob { hash } = &blob.source else {
        panic!("expected peer object")
    };
    write(
        &revision::object_path(&friend.root, hash).unwrap(),
        b"corrupt cache",
    );
    fs::remove_file(friend.instance().join(&blob.path)).unwrap();
    let result = peer::sync(
        &restarted.revision,
        &restarted.head,
        &friend,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(server.1),
    )
    .unwrap();
    assert_eq!(result["files_replaced"], 1);
    assert_eq!(
        fs::read(friend.instance().join(&blob.path)).unwrap(),
        b"shared peer content"
    );
}

#[test]
fn mixed_inline_and_archive_staging_uses_one_case_insensitive_tree() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("Stock Game/CreationKitPrefs.ini"),
        b"inline",
    );
    let destination = files::destination(dir.path(), "stock game/data/archive.bsa").unwrap();
    write(&destination, b"archive");
    assert!(dir.path().join("Stock Game/data/archive.bsa").is_file());
    assert!(!dir.path().join("stock game").exists());
    assert!(files::require(dir.path(), "stock game/creationkitprefs.ini").is_ok());
    files::inventory(dir.path()).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn cancelled_peer_sync_retains_owned_cache_and_retries_to_verified_install() {
    use skyrim_mod_sync::{authoring, peer, revision};
    use std::{
        io::BufRead,
        process::{Command, Stdio},
    };
    let f = Fixture::new();
    let source = f.reconstructed.join("mods/Co-op/large-local.bin");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let mut out = fs::File::create(&source).unwrap();
    for n in 0..1024_u32 {
        let mut chunk = [0u8; 65536];
        chunk[..4].copy_from_slice(&n.to_le_bytes());
        out.write_all(&chunk).unwrap();
    }
    out.sync_all().unwrap();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let publication = authoring::publish(
        &f.storage,
        &f.dir.path().join("large.skyrimsync"),
        &[],
        false,
    )
    .unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "serve", "--coop"])
        .arg(&f.storage.root)
        .args(["--no-relay", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Stop(std::process::Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut reader = std::io::BufReader::new(server.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let addr: iroh::EndpointAddr = serde_json::from_value(ready["addresses"].clone()).unwrap();
    let ip = *addr.ip_addrs().find(|a| a.is_ipv4()).unwrap();
    let _server = Stop(server);
    let friend = Storage::open(&f.dir.path().join("cancelled friend")).unwrap();
    let code = publication["code"].as_str().unwrap();
    let mut client = Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "sync", code, "--coop"])
        .arg(&friend.root)
        .arg("--base")
        .arg(&f.base)
        .arg("--downloads")
        .arg(&f.downloads)
        .arg("--peer-address")
        .arg(ip.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut progress = std::io::BufReader::new(client.stderr.take().unwrap());
    loop {
        line.clear();
        assert!(
            progress.read_line(&mut line).unwrap() > 0,
            "client exited before peer acquisition"
        );
        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
        if event["state"] == "peer_downloading"
            && event["detail"]["size"].as_u64().unwrap_or(0) >= 64 * 1024 * 1024
        {
            break;
        }
    }
    assert_eq!(unsafe { libc::kill(client.id() as i32, libc::SIGINT) }, 0);
    let status = client.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert!(friend.root.join("peer/client").is_dir());
    let channel = revision::Channel::parse(code).unwrap();
    let reply = peer::fetch(&channel, &friend, None, Some(ip)).unwrap();
    peer::sync(
        &reply.revision,
        &reply.head,
        &friend,
        &f.base,
        &f.downloads,
        None,
        None,
        Some(ip),
    )
    .unwrap();
    assert_eq!(
        files::sha(&friend.instance().join("game/Data/large-local.bin")).unwrap(),
        files::sha(&source).unwrap()
    );
    assert_eq!(
        target::verify(&reply.revision.legacy(), &friend.instance()).unwrap()["files_match"],
        true
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Requires Linux namespaces and a systemd user cgroup session; run explicitly"]
fn capped_launcher_waits_inside_runtime_and_keeps_prefix_across_revisions() {
    use skyrim_mod_sync::authoring;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let steam = f.dir.path().join("native Steam");
    let proton = f.dir.path().join("private Proton source");
    let runtime = f.dir.path().join("private runtime source");
    write(&steam.join("linux64/steamclient.so"), b"fixture SDK");
    fn executable(path: &Path, text: &[u8]) {
        write(path, text);
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    executable(&runtime.join("run"), b"#!/bin/sh\nexport SMS_TEST_RUNTIME_CONTEXT=inner\n[ \"$1\" = -- ] && shift\nexec \"$@\"\n");
    executable(&proton.join("proton"), b"#!/bin/sh\n[ \"$SMS_TEST_RUNTIME_CONTEXT\" = inner ] || exit 20\nmkdir -p \"$WINEPREFIX/drive_c/users/steamuser\"\nprintf 'proton inside\\n' >> \"$WINEPREFIX/session.log\"\nexit 0\n");
    executable(&proton.join("files/bin/wineserver"), b"#!/bin/sh\n[ \"$SMS_TEST_RUNTIME_CONTEXT\" = inner ] || exit 21\nsleep 0.3\nprintf 'wait inside\\n' >> \"$WINEPREFIX/session.log\"\nexit 0\n");
    // Real Linux namespaces and cgroup; the only programs run are tiny shell fixtures.
    let launch = || {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
            .args(["--json", "play", "--coop"])
            .arg(&f.storage.root)
            .arg("--steam")
            .arg(&steam)
            .arg("--proton")
            .arg(&proton)
            .arg("--runtime")
            .arg(&runtime)
            .args(["--memory-limit-mib", "128", "--timeout-seconds", "15"])
            .output()
            .unwrap();
        if !out.status.success() {
            for r in fs::read_dir(f.storage.root.join("runtime")).unwrap() {
                eprintln!(
                    "{}",
                    fs::read_to_string(r.unwrap().path().join("launch.log")).unwrap_or_default()
                );
            }
        }
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let first = launch();
    assert_eq!(first["state"], "game_exited");
    let session = f.storage.root.join("personal/prefix/pfx/session.log");
    let history = fs::read_to_string(&session).unwrap();
    assert_eq!(
        history,
        "proton inside\nwait inside\nproton inside\nwait inside\n"
    );
    write(
        &f.storage.root.join("personal/saves/retained.ess"),
        b"private save",
    );
    write(
        &f.reconstructed.join("mods/Co-op/next.bin"),
        b"updated content",
    );
    let second = launch();
    assert_ne!(first["revision"], second["revision"]);
    assert_eq!(
        fs::read(f.storage.root.join("personal/saves/retained.ess")).unwrap(),
        b"private save"
    );
    assert_eq!(
        fs::read_to_string(session).unwrap(),
        format!("{history}proton inside\nwait inside\n")
    );
    assert!(!proton.join("dist.lock").exists());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Requires a systemd user cgroup; deliberately OOM-kills a shell fixture child within 128 MiB"]
fn capped_launcher_reports_child_oom_even_when_its_loader_returns_zero() {
    use skyrim_mod_sync::authoring;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let steam = f.dir.path().join("Steam");
    let proton = f.dir.path().join("Proton");
    let runtime = f.dir.path().join("runtime");
    write(&steam.join("linux64/steamclient.so"), b"fixture");
    for (path, text) in [
        (runtime.join("run"), "#!/bin/sh\n[ \"$1\" = -- ] && shift\nexec \"$@\"\n"),
        (proton.join("proton"), "#!/bin/sh\nmkdir -p \"$WINEPREFIX/drive_c/users/steamuser\"\nif [ \"$2\" = cmd.exe ]; then /usr/bin/python3 -c 'a=bytearray(512*1024*1024)' || true; fi\nexit 0\n"),
        (proton.join("files/bin/wineserver"), "#!/bin/sh\nexit 0\n"),
    ] { write(&path,text.as_bytes());fs::set_permissions(path,fs::Permissions::from_mode(0o755)).unwrap(); }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "play", "--coop"])
        .arg(&f.storage.root)
        .arg("--steam")
        .arg(&steam)
        .arg("--proton")
        .arg(&proton)
        .arg("--runtime")
        .arg(&runtime)
        .args(["--memory-limit-mib", "128", "--timeout-seconds", "15"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(2), "{value}");
    assert_eq!(value["reason"], "memory_limit_exceeded", "{value}");
    let current = skyrim_mod_sync::revision::installed(&f.storage).unwrap();
    let metrics: serde_json::Value = files::read_json(
        &f.storage
            .root
            .join("runtime")
            .join(current.id().unwrap())
            .join("launch-metrics.json"),
    )
    .unwrap();
    assert_eq!(metrics["exit_success"], false);
    assert!(metrics["oom_kills"].as_u64().unwrap() > 0);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Requires a systemd user cgroup session; validates timeout kills the owned shell tree"]
fn capped_launcher_timeout_stops_its_own_process_tree() {
    use skyrim_mod_sync::authoring;
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    authoring::import(&f.storage, &editable_binding(&f)).unwrap();
    let steam = f.dir.path().join("Steam");
    let proton = f.dir.path().join("Proton");
    let runtime = f.dir.path().join("runtime");
    write(&steam.join("linux64/steamclient.so"), b"fixture");
    for (path,text) in [
        (runtime.join("run"),"#!/bin/sh\n[ \"$1\" = -- ] && shift\nexec \"$@\"\n"),
        (proton.join("proton"),"#!/bin/sh\nmkdir -p \"$WINEPREFIX/drive_c/users/steamuser\"\nif [ \"$2\" = cmd.exe ]; then sleep 60; fi\nexit 0\n"),
        (proton.join("files/bin/wineserver"),"#!/bin/sh\nexit 0\n"),
    ] {write(&path,text.as_bytes());fs::set_permissions(path,fs::Permissions::from_mode(0o755)).unwrap();}
    let started = std::time::Instant::now();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_skyrim-mod-sync"))
        .args(["--json", "play", "--coop"])
        .arg(&f.storage.root)
        .arg("--steam")
        .arg(&steam)
        .arg("--proton")
        .arg(&proton)
        .arg("--runtime")
        .arg(&runtime)
        .args(["--memory-limit-mib", "128", "--timeout-seconds", "1"])
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(2), "{value}");
    assert_eq!(value["reason"], "launch_timeout", "{value}");
    assert!(started.elapsed().as_secs() < 10);
    let marker: serde_json::Value =
        files::read_json(&f.storage.root.join("last-launch.json")).unwrap();
    assert_eq!(marker["state"], "interrupted");
}
