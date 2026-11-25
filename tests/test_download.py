import base64
import gzip
import json
from zipfile import ZipFile

import pytest
import xxhash

from skyrim_mod_sync.download import download_list
from skyrim_mod_sync.paths import SyncError


def digest(data):
    return base64.b64encode(xxhash.xxh64(data).intdigest().to_bytes(8, "little")).decode()


@pytest.fixture
def cdn(tmp_path):
    artifact = tmp_path / "original.wabbajack"
    with ZipFile(artifact, "w") as z:
        z.writestr("modlist", json.dumps({"Name": "CDN test", "Archives": [], "Directives": []}))
    data = artifact.read_bytes()
    chunks = [data[:80], data[80:]]
    definition = {"Size": len(data), "Hash": digest(data), "Parts": [
        {"Index": 0, "Offset": 0, "Size": len(chunks[0]), "Hash": digest(chunks[0])},
        {"Index": 1, "Offset": len(chunks[0]), "Size": len(chunks[1]), "Hash": digest(chunks[1])}]}
    return definition, chunks


def test_cdn_download_reuses_verified_parts(cdn, tmp_path, monkeypatch):
    definition, chunks = cdn
    calls = []
    def read(url, limit, check_cancel=lambda: None):
        calls.append(url)
        return gzip.compress(json.dumps(definition).encode()) if url.endswith('.gz') else chunks[int(url.rsplit('/', 1)[1])]
    monkeypatch.setattr("skyrim_mod_sync.download._read", read)
    url = "https://authored-files.wabbajack.org/test.wabbajack_id"
    cache = tmp_path / "cache"
    one = tmp_path / "one.wabbajack"
    download_list(url, one, cache)
    assert one.read_bytes() == b"".join(chunks)
    calls.clear()
    download_list(url, tmp_path / "two.wabbajack", cache)
    assert calls == [url + "/definition.json.gz"]


def test_wrong_part_never_publishes_a_recipe(cdn, tmp_path, monkeypatch):
    definition, chunks = cdn
    monkeypatch.setattr("skyrim_mod_sync.download._read", lambda u, n, check_cancel=lambda: None: gzip.compress(json.dumps(definition).encode()) if u.endswith('.gz') else b"HTML error page")
    output = tmp_path / "bad.wabbajack"
    with pytest.raises(SyncError, match="failed verification"):
        download_list("https://authored-files.wabbajack.org/test", output, tmp_path / "cache")
    assert not output.exists()


def test_overlapping_cdn_definition_rejected(cdn, tmp_path, monkeypatch):
    definition, _ = cdn
    definition["Parts"][1]["Offset"] = 0
    monkeypatch.setattr("skyrim_mod_sync.download._read", lambda u, n, check_cancel=lambda: None: gzip.compress(json.dumps(definition).encode()))
    with pytest.raises(SyncError, match="overlap"):
        download_list("https://authored-files.wabbajack.org/test", tmp_path / "bad", tmp_path / "cache")


def test_cancelled_download_keeps_verified_parts_for_retry(cdn, tmp_path, monkeypatch):
    definition, chunks = cdn
    cancelled = False
    def read(url, limit, check_cancel=lambda: None):
        check_cancel()
        return gzip.compress(json.dumps(definition).encode()) if url.endswith('.gz') else chunks[int(url.rsplit('/', 1)[1])]
    monkeypatch.setattr("skyrim_mod_sync.download._read", read)
    def check_cancel():
        if cancelled:
            raise SyncError("cancelled", "cancelled")
    def event(data):
        nonlocal cancelled
        if data["state"] == "downloading_recipe":
            cancelled = True
    output = tmp_path / "nested/recipe.wabbajack"
    cache = tmp_path / "cache"
    with pytest.raises(SyncError, match="cancelled"):
        download_list("https://authored-files.wabbajack.org/test", output, cache, event, check_cancel)
    assert not output.exists()
    assert any(p.name.isdigit() for p in cache.rglob("*"))
    assert not list(tmp_path.rglob("*.partial"))
    download_list("https://authored-files.wabbajack.org/test", output, cache)
    assert output.read_bytes() == b"".join(chunks)
