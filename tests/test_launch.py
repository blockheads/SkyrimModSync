import json
import os
from pathlib import Path

import pytest

from skyrim_mod_sync.installer import install
from skyrim_mod_sync.launch import launch
from skyrim_mod_sync.paths import SyncError

pytestmark = pytest.mark.skipif(os.name == "nt", reason="Linux/Proton adapter")


@pytest.fixture
def launch_setup(setup, tmp_path, monkeypatch):
    install(setup["recipe"], base=setup["base"], root=setup["root"],
            reconstructed=setup["installed"], accepted=True)
    tools = {name: tmp_path / name for name in ("proton", "steam", "runtime")}
    for path in tools.values():
        path.mkdir()
    (tools["steam"] / "steamapps").mkdir()
    for name, exe in (("proton", "proton"), ("runtime", "run")):
        p = tools[name] / exe
        p.write_text("#!/bin/sh\nexit 0\n")
        p.chmod(0o755)
    monkeypatch.setattr("skyrim_mod_sync.launch.sys.platform", "linux")
    monkeypatch.setattr("skyrim_mod_sync.launch.process_running", lambda n, prefix=None: n == "steam")
    return tools


def test_isolated_prefix_configuration_and_exact_launch_command(setup, launch_setup, tmp_path, monkeypatch):
    calls = []
    personal = tmp_path / "personal-documents"
    personal.mkdir()
    (personal / "untouched.ini").write_text("personal")
    def initialize(command, **kwargs):
        calls.append((command, kwargs))
        user = Path(kwargs["env"]["WINEPREFIX"]) / "drive_c/users/steamuser"
        user.mkdir(parents=True)
        (user / "Documents").symlink_to(personal, target_is_directory=True)
        (user / "My Documents").symlink_to(personal, target_is_directory=True)
        return type("Result", (), {"returncode": 0})()
    def start(command, **kwargs):
        calls.append((command, kwargs))
        return type("Process", (), {"pid": 1234})()
    monkeypatch.setattr("skyrim_mod_sync.launch.subprocess.run", initialize)
    monkeypatch.setattr("skyrim_mod_sync.launch.subprocess.Popen", start)
    result = launch(setup["recipe"], root=setup["root"], **launch_setup)
    prefix = setup["root"] / "prefixes" / setup["recipe"].setup_id
    assert result["state"] == "launch_started"
    docs = prefix / "pfx/drive_c/users/steamuser/Documents"
    assert not docs.is_symlink()
    assert (docs / "My Games/Skyrim Special Edition/Skyrim.ini").read_bytes() == b"[General]\nsLanguage=ENGLISH\n"
    assert (prefix / "pfx/drive_c/users/steamuser/AppData/Local/Skyrim Special Edition/plugins.txt").read_bytes() == b"*Example.esp\n"
    assert list(personal.iterdir()) == [personal / "untouched.ini"]
    assert calls[-1][0][-1] == "G:\\skse64_loader.exe"
    assert (prefix / "pfx/dosdevices/g:").resolve() == setup["root"] / "installs" / setup["recipe"].setup_id / "game"
    assert calls[-1][1]["env"]["STEAM_COMPAT_DATA_PATH"] == str(prefix)
    assert calls[-1][1]["env"]["SteamAppId"] == "489830"


def test_refuses_unowned_prefix(setup, launch_setup):
    prefix = setup["root"] / "prefixes" / setup["recipe"].setup_id
    prefix.mkdir(parents=True)
    with pytest.raises(SyncError, match="not owned"):
        launch(setup["recipe"], root=setup["root"], **launch_setup)


def test_wrong_bytes_cannot_be_launched(setup, launch_setup, monkeypatch):
    target = setup["root"] / "installs" / setup["recipe"].setup_id / "game/SkyrimSE.exe"
    target.write_bytes(b"wrong")
    monkeypatch.setattr("skyrim_mod_sync.launch.subprocess.Popen", lambda *_args, **_kwargs: pytest.fail("must not launch"))
    with pytest.raises(SyncError, match="pass verification"):
        launch(setup["recipe"], root=setup["root"], **launch_setup)
