import json
from pathlib import Path
from zipfile import ZipFile

import pytest

from skyrim_mod_sync.exporter import export_setup
from skyrim_mod_sync.recipe import load


def put(root: Path, name: str, data: bytes):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


@pytest.fixture
def setup(tmp_path):
    base = tmp_path / "base"
    installed = tmp_path / "host-list"
    put(base, "SkyrimSE.exe", b"synthetic game executable")
    put(base, "Data/Skyrim.esm", b"synthetic licensed base master")
    put(installed, "Game Folder Files/skse64_loader.exe", b"synthetic SKSE loader")
    put(installed, "Game Folder Files/skse64_1_6_1170.dll", b"synthetic SKSE library")
    put(installed, "mods/Low/meta.ini", b"manager metadata")
    put(installed, "mods/Low/textures/example.dds", b"lower-priority texture")
    put(installed, "mods/High/textures/example.dds", b"winning texture")
    put(installed, "mods/High/Example.esp", b"synthetic plugin")
    put(installed, "mods/High/hidden.dds.mohidden", b"hidden file")
    put(installed, "mods/Disabled/Unused.esp", b"disabled plugin")
    put(installed, "profiles/Coop/modlist.txt", b"# MO2 highest priority first\n+High\n+Low\n-Disabled\n")
    put(installed, "profiles/Coop/plugins.txt", b"*Example.esp\n")
    put(installed, "profiles/Coop/loadorder.txt", b"Skyrim.esm\nExample.esp\n")
    put(installed, "profiles/Coop/Skyrim.ini", b"[General]\nsLanguage=ENGLISH\n")
    artifact = tmp_path / "toy.wabbajack"
    with ZipFile(artifact, "w") as archive:
        archive.writestr("modlist", json.dumps({"Name": "Synthetic offline fixture", "GameType": "SkyrimSpecialEdition", "Archives": [], "Directives": []}))
        for path in installed.rglob("*"):
            if path.is_file():
                archive.write(path, "fixture/" + path.relative_to(installed).as_posix())
    package = tmp_path / "friends.skyrimsync"
    export_setup(base=base, installed=installed, artifact=artifact, output=package,
                 name="Friends setup", profile="Coop")
    return {"base": base, "installed": installed, "artifact": artifact, "package": package,
            "recipe": load(package), "root": tmp_path / "managed"}
