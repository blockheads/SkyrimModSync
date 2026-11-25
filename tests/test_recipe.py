import copy
import json
import os
from zipfile import ZipFile

import pytest

from skyrim_mod_sync.paths import SyncError, inventory, locate, relative_path
from skyrim_mod_sync.recipe import Recipe, canonical, load, validate, write


@pytest.mark.parametrize("path", ["../file", "/absolute", "C:/file", "foo\\bar", "CON.dll", "data/../file", "data//file", "file.", "file ", "data/file:stream"])
def test_windows_and_traversal_paths_are_rejected(path):
    with pytest.raises(SyncError):
        relative_path(path)


@pytest.mark.skipif(os.name == "nt", reason="Tests ambiguous names on a case-sensitive filesystem")
def test_case_collisions_and_symlink_escape(tmp_path):
    (tmp_path / "Textures").mkdir()
    (tmp_path / "textures").mkdir()
    with pytest.raises(SyncError, match="Ambiguous"):
        list(inventory(tmp_path))
    (tmp_path / "textures").rmdir()
    (tmp_path / "Textures/escape").symlink_to("/etc/passwd")
    with pytest.raises(SyncError, match="Symbolic"):
        list(inventory(tmp_path))


def test_case_insensitive_source_resolution(tmp_path):
    (tmp_path / "SkyrimSE.exe").write_bytes(b"game")
    assert locate(tmp_path, "skyrimse.EXE").name == "SkyrimSE.exe"


def test_same_result_has_same_id_with_different_installer_metadata(setup):
    data = copy.deepcopy(setup["recipe"].data)
    data["name"] = "Different display name"
    data["artifact"]["sha256"] = "a" * 64
    data["files"][0]["source"]["path"] = "another/source"
    other = Recipe(validate(data), setup["package"])
    assert other.setup_id == setup["recipe"].setup_id
    assert other.job_id != setup["recipe"].job_id


def test_duplicate_destinations_and_file_directory_overlap(setup):
    data = copy.deepcopy(setup["recipe"].data)
    duplicate = copy.deepcopy(data["files"][0])
    duplicate["path"] = duplicate["path"].upper()
    data["files"].append(duplicate)
    with pytest.raises(SyncError):
        validate(data)
    data = copy.deepcopy(setup["recipe"].data)
    duplicate["path"] = "game/Data"
    data["files"].append(duplicate)
    with pytest.raises(SyncError, match="both a file and a directory"):
        validate(data)


def test_malicious_archive_members_rejected_without_extraction(setup, tmp_path):
    package = tmp_path / "bad.skyrimsync"
    with ZipFile(package, "w") as archive:
        archive.writestr("manifest.json", canonical(setup["recipe"].data))
        archive.write(setup["artifact"], "recipe.wabbajack")
        archive.writestr("../outside", b"escape")
    with pytest.raises(SyncError):
        load(package)
    assert not (tmp_path.parent / "outside").exists()


def test_duplicate_json_keys_rejected(setup, tmp_path):
    package = tmp_path / "duplicate.skyrimsync"
    raw = canonical(setup["recipe"].data).decode().replace('"schema":1', '"schema":1,"schema":1')
    with ZipFile(package, "w") as archive:
        archive.writestr("manifest.json", raw)
        archive.write(setup["artifact"], "recipe.wabbajack")
    with pytest.raises(SyncError, match="Duplicate JSON key"):
        load(package)


def test_tampered_embedded_recipe_is_never_used(setup, tmp_path):
    package = tmp_path / "tampered.skyrimsync"
    raw = setup["artifact"].read_bytes()
    with ZipFile(package, "w") as archive:
        archive.writestr("manifest.json", canonical(setup["recipe"].data))
        archive.writestr("recipe.wabbajack", b"X" + raw[1:])
    with pytest.raises(SyncError, match="failed verification"):
        load(package).extract_artifact(tmp_path / "extracted.wabbajack")
    assert not (tmp_path / "extracted.wabbajack").exists()


def test_small_share_file_can_pin_a_remote_recipe(setup, tmp_path, monkeypatch):
    data = copy.deepcopy(setup["recipe"].data)
    data["artifact"]["url"] = "https://authored-files.wabbajack.org/fixture.wabbajack_id"
    package = tmp_path / "small.skyrimsync"
    write(package, data, setup["artifact"])
    with ZipFile(package) as archive:
        assert archive.namelist() == ["manifest.json"]
    assert package.stat().st_size < setup["package"].stat().st_size
    def download(url, output, cache, emit, check_cancel):
        assert url == data["artifact"]["url"]
        output.write_bytes(setup["artifact"].read_bytes())
    monkeypatch.setattr("skyrim_mod_sync.download.download_list", download)
    output = tmp_path / "retrieved.wabbajack"
    load(package).extract_artifact(output)
    assert output.read_bytes() == setup["artifact"].read_bytes()


def test_remote_recipe_replacement_fails_its_pinned_identity(setup, tmp_path, monkeypatch):
    data = copy.deepcopy(setup["recipe"].data)
    data["artifact"]["url"] = "https://authored-files.wabbajack.org/fixture.wabbajack_id"
    package = tmp_path / "small.skyrimsync"
    write(package, data, setup["artifact"])
    monkeypatch.setattr("skyrim_mod_sync.download.download_list", lambda u, p, c, emit, check_cancel: p.write_bytes(b"replacement"))
    output = tmp_path / "retrieved.wabbajack"
    with pytest.raises(SyncError, match="changed or failed"):
        load(package).extract_artifact(output)
    assert not output.exists()


def test_embedded_recipe_cancellation_removes_partial_file(setup, tmp_path):
    calls = 0
    def check_cancel():
        nonlocal calls
        calls += 1
        if calls == 2:
            raise SyncError("cancelled", "cancelled")
    output = tmp_path / "recipe.wabbajack"
    with pytest.raises(SyncError, match="cancelled"):
        setup["recipe"].extract_artifact(output, check_cancel=check_cancel)
    assert not output.exists()
    assert not output.with_suffix(".partial").exists()
