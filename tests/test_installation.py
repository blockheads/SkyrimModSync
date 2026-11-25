import json
import os
import subprocess
import sys

import pytest

from skyrim_mod_sync.exporter import export_setup
from skyrim_mod_sync.installer import install, job_lock, plan, verify
from skyrim_mod_sync.paths import SyncError
from skyrim_mod_sync.recipe import load


def do_install(setup, **kwargs):
    return install(setup["recipe"], base=setup["base"], root=setup["root"],
                   reconstructed=setup["installed"], accepted=True, **kwargs)


def test_complete_export_install_reuse_and_no_mo2_runtime(setup):
    result = do_install(setup)
    assert (result / "game/Data/textures/example.dds").read_bytes() == b"winning texture"
    assert (result / "config/local/plugins.txt").read_bytes() == b"*Example.esp\n"
    assert not (result / "game/Data/meta.ini").exists()
    assert not (result / "game/Data/Unused.esp").exists()
    assert not list(result.rglob("*.mohidden"))
    assert not (result / "ModOrganizer.exe").exists()
    assert verify(setup["recipe"], result) == []
    events = []
    assert do_install(setup, emit=events.append) == result
    assert events[-1]["reused"]
    assert json.loads((setup["root"] / "receipts" / (setup["recipe"].setup_id + ".json")).read_text())["state"] == "files_verified"


def test_consent_before_any_write(setup):
    with pytest.raises(SyncError, match="approve"):
        install(setup["recipe"], base=setup["base"], root=setup["root"], reconstructed=setup["installed"])
    assert not setup["root"].exists()


def test_base_mismatch_stops_before_reconstruction(setup, monkeypatch):
    (setup["base"] / "SkyrimSE.exe").write_bytes(b"different version")
    monkeypatch.setattr("skyrim_mod_sync.installer.run_engine", lambda *_: pytest.fail("engine must not run"))
    assert plan(setup["recipe"], setup["base"], setup["root"])["blockers"]
    with pytest.raises(SyncError, match="base game differs"):
        install(setup["recipe"], base=setup["base"], root=setup["root"], engine=setup["artifact"], accepted=True)
    assert not (setup["root"] / "installs").exists()


def test_failed_copy_cannot_publish_and_retry_completes(setup):
    winner = setup["installed"] / "mods/High/Example.esp"
    good = winner.read_bytes()
    winner.write_bytes(b"corrupt")
    with pytest.raises(SyncError, match="differs from the host"):
        do_install(setup)
    assert not (setup["root"] / "installs" / setup["recipe"].setup_id).exists()
    job = setup["root"] / "jobs" / setup["recipe"].job_id
    assert json.loads((job / "status.json").read_text())["state"] == "failed"
    winner.write_bytes(good)
    assert not verify(setup["recipe"], do_install(setup))


def test_same_size_mtime_tampering_is_detected_and_not_overwritten(setup):
    result = do_install(setup)
    plugin = result / "game/Data/Example.esp"
    before = plugin.stat()
    plugin.write_bytes(b"X" * before.st_size)
    os.utime(plugin, ns=(before.st_atime_ns, before.st_mtime_ns))
    assert verify(setup["recipe"], result) == [{"kind": "changed", "path": "game/Data/Example.esp"}]
    with pytest.raises(SyncError, match="will not be overwritten"):
        do_install(setup)
    assert plugin.read_bytes() == b"X" * before.st_size


def test_extra_native_plugin_prevents_verified_state(setup):
    result = do_install(setup)
    path = result / "game/Data/extra.dll"
    path.write_bytes(b"unexpected native plugin")
    assert {"kind": "extra", "path": "game/Data/extra.dll"} in verify(setup["recipe"], result)


def test_files_are_copies_and_personal_setup_is_untouched(setup):
    original = (setup["base"] / "SkyrimSE.exe").read_bytes()
    result = do_install(setup)
    (result / "game/SkyrimSE.exe").write_bytes(b"modified managed copy")
    assert (setup["base"] / "SkyrimSE.exe").read_bytes() == original


def test_refuses_overlapping_storage(setup):
    with pytest.raises(SyncError, match="separate"):
        plan(setup["recipe"], setup["base"], setup["base"] / "managed")


def test_job_lock_blocks_second_writer(setup):
    with job_lock(setup["root"] / "locks" / (setup["recipe"].setup_id + ".lock")):
        with pytest.raises(SyncError, match="already being installed"):
            do_install(setup)


@pytest.mark.parametrize("relative", ["overwrite/generated.hkx", "mods/High/root/proxy.dll", "profiles/Coop/initweaks.ini"])
def test_unsupported_effective_views_refuse_export(setup, relative):
    path = setup["installed"] / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"required output")
    output = setup["package"].with_name("blocked.skyrimsync")
    with pytest.raises(SyncError):
        export_setup(base=setup["base"], installed=setup["installed"], artifact=setup["artifact"],
                     output=output, name="Unsupported", profile="Coop")
    assert not output.exists()


def test_cli_backend_contract_runs_in_staging(setup, tmp_path):
    if os.name == "nt":
        pytest.skip("POSIX fixture executable")
    engine = tmp_path / "fixture-engine"
    engine.write_text(f"#!{sys.executable}\n" + '''import argparse,pathlib,zipfile
p=argparse.ArgumentParser();p.add_argument('operation');p.add_argument('--wabbajack');p.add_argument('--output');p.add_argument('--downloads');a=p.parse_args()
assert a.operation == 'install'
with zipfile.ZipFile(a.wabbajack) as z:
 for name in z.namelist():
  if name.startswith('fixture/'):
   target=pathlib.Path(a.output)/name[len('fixture/'):];target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(z.read(name))
print('Fixture backend completed')
''')
    engine.chmod(0o755)
    result = subprocess.run([sys.executable, "-m", "skyrim_mod_sync", "install", str(setup["package"]),
                             "--base", str(setup["base"]), "--root", str(setup["root"]),
                             "--engine", str(engine), "--accept"], capture_output=True, text=True)
    assert result.returncode == 0, result.stderr + result.stdout
    events = [json.loads(line) for line in result.stdout.splitlines()]
    assert events[-1]["state"] == "files_verified"
    assert not verify(setup["recipe"], setup["root"] / "installs" / setup["recipe"].setup_id)
    assert "Fixture backend completed" in (setup["root"] / "jobs" / setup["recipe"].job_id / "engine.log").read_text()


def test_standard_root_builder_materializes_selected_root_files(setup):
    path = setup["installed"] / "mods/High/root/engine-proxy.dll"
    path.parent.mkdir(parents=True)
    path.write_bytes(b"synthetic proxy")
    package = setup["package"].with_name("root-builder.skyrimsync")
    export_setup(base=setup["base"], installed=setup["installed"], artifact=setup["artifact"],
                 output=package, name="Root Builder", profile="Coop", standard_root_builder=True)
    result = install(load(package), base=setup["base"], root=setup["root"],
                     reconstructed=setup["installed"], accepted=True)
    assert (result / "game/engine-proxy.dll").read_bytes() == b"synthetic proxy"
    assert not (result / "game/Data/root").exists()


def test_cancellation_records_status_and_retry_reuses_staged_files(setup):
    job = setup["root"] / "jobs" / setup["recipe"].job_id
    copied = []
    def cancel_after_copy(data):
        if data["state"] == "staging" and data.get("file"):
            copied.append(data["file"])
            (job / "cancel").write_text("cancel\n")
    with pytest.raises(SyncError, match="cancelled"):
        do_install(setup, emit=cancel_after_copy)
    assert json.loads((job / "status.json").read_text())["state"] == "cancelled"
    assert not (setup["root"] / "installs" / setup["recipe"].setup_id).exists()
    staged = job / "staging" / copied[0]
    before = staged.stat().st_mtime_ns
    result = do_install(setup)
    assert (result / copied[0]).stat().st_mtime_ns == before
    assert not verify(setup["recipe"], result)


@pytest.mark.skipif(os.name == "nt", reason="POSIX process-group fixture")
def test_cancelling_backend_stops_its_process(setup, tmp_path):
    from skyrim_mod_sync.installer import run_engine
    marker = tmp_path / "pid"
    engine = tmp_path / "sleeping-engine"
    engine.write_text(f"#!{sys.executable}\nimport os,pathlib,time\npathlib.Path({str(marker)!r}).write_text(str(os.getpid()))\ntime.sleep(60)\n")
    engine.chmod(0o755)
    def check_cancel():
        if marker.exists():
            raise SyncError("cancelled", "cancelled")
    with pytest.raises(SyncError, match="cancelled"):
        run_engine(engine, setup["artifact"], tmp_path, tmp_path, tmp_path / "engine.log", check_cancel)
    with pytest.raises(ProcessLookupError):
        os.kill(int(marker.read_text()), 0)


@pytest.mark.skipif(os.name == "nt", reason="POSIX fixture executable")
def test_install_without_engine_path_bootstraps_and_uses_pinned_adapter(setup, tmp_path, monkeypatch):
    engine = tmp_path / "jackify-engine"
    engine.write_text(f"#!{sys.executable}\n" + '''import argparse,pathlib,zipfile
p=argparse.ArgumentParser();p.add_argument('operation');p.add_argument('-w');p.add_argument('-o');p.add_argument('-d');a=p.parse_args()
assert a.operation == 'install'
with zipfile.ZipFile(a.w) as z:
 for name in z.namelist():
  if name.startswith('fixture/'):
   target=pathlib.Path(a.o)/name[len('fixture/'):];target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(z.read(name))
''')
    engine.chmod(0o755)
    calls = []
    def bootstrap(root, emit, check_cancel):
        calls.append(root)
        check_cancel()
        emit({"state": "engine_ready", "version": "fixture"})
        return engine
    monkeypatch.setattr("skyrim_mod_sync.bootstrap.ensure_engine", bootstrap)
    result = install(setup["recipe"], base=setup["base"], root=setup["root"], accepted=True)
    assert calls == [setup["root"].resolve()]
    assert not verify(setup["recipe"], result)
