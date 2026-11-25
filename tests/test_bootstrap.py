import hashlib
import io
import tarfile
from pathlib import Path

import pytest

from skyrim_mod_sync.bootstrap import EngineRelease, ensure_engine
from skyrim_mod_sync.paths import SyncError


@pytest.fixture
def engine_archive(tmp_path, monkeypatch):
    artifact = tmp_path / "engine.tar.gz"
    with tarfile.open(artifact, "w:gz") as archive:
        info = tarfile.TarInfo("./jackify-engine")
        data = b"synthetic executable, never run"
        info.size, info.mode = len(data), 0o755
        archive.addfile(info, io.BytesIO(data))
    release = EngineRelease("fixture", "https://example.invalid/engine", artifact.stat().st_size,
                            hashlib.sha256(artifact.read_bytes()).hexdigest())
    monkeypatch.setattr("skyrim_mod_sync.bootstrap.JACKIFY_LINUX", release)
    monkeypatch.setattr("skyrim_mod_sync.bootstrap.platform.system", lambda: "Linux")
    monkeypatch.setattr("skyrim_mod_sync.bootstrap.platform.machine", lambda: "x86_64")
    return artifact, release


def put_cached(root, artifact, release):
    path = root / "tools/downloads" / (release.sha256 + ".tar.gz")
    path.parent.mkdir(parents=True)
    path.write_bytes(artifact.read_bytes())


def test_bootstrap_reuses_verified_engine_and_rejects_changes(engine_archive, tmp_path, monkeypatch):
    artifact, release = engine_archive
    root = tmp_path / "managed"
    put_cached(root, artifact, release)
    monkeypatch.setattr("skyrim_mod_sync.bootstrap._download", lambda *_: pytest.fail("cached engine must be reused"))
    engine = ensure_engine(root)
    assert ensure_engine(root) == engine
    engine.write_bytes(b"changed engine")
    with pytest.raises(SyncError, match="changed"):
        ensure_engine(root)
    assert engine.read_bytes() == b"changed engine"


def test_bad_archive_download_is_rejected_before_extraction(engine_archive, tmp_path, monkeypatch):
    artifact, release = engine_archive
    class Response(io.BytesIO):
        def geturl(self):
            return release.url
    monkeypatch.setattr("skyrim_mod_sync.bootstrap.urlopen", lambda *a, **kw: Response(b"X" * release.size))
    root = tmp_path / "managed"
    with pytest.raises(SyncError, match="SHA-256"):
        ensure_engine(root)
    assert not (root / "tools/jackify-engine-fixture").exists()
    assert not list(root.rglob("*.partial"))


@pytest.mark.parametrize("name,link", [("../escape", False), ("proxy", True)])
def test_engine_archive_rejects_traversal_and_links(engine_archive, tmp_path, monkeypatch, name, link):
    artifact, release = engine_archive
    with tarfile.open(artifact, "w:gz") as archive:
        entry = tarfile.TarInfo(name)
        if link:
            entry.type, entry.linkname = tarfile.SYMTYPE, "../../escape"
            archive.addfile(entry)
        else:
            entry.size = 1
            archive.addfile(entry, io.BytesIO(b"x"))
    release = EngineRelease(release.version, release.url, artifact.stat().st_size,
                            hashlib.sha256(artifact.read_bytes()).hexdigest())
    monkeypatch.setattr("skyrim_mod_sync.bootstrap.JACKIFY_LINUX", release)
    root = tmp_path / "managed"
    put_cached(root, artifact, release)
    with pytest.raises(SyncError):
        ensure_engine(root)
    assert not (root / "tools/jackify-engine-fixture").exists()
    assert not (tmp_path / "escape").exists()


def test_cancelled_bootstrap_publishes_no_partial_engine(engine_archive, tmp_path):
    artifact, release = engine_archive
    root = tmp_path / "managed"
    put_cached(root, artifact, release)
    cancelled = False
    def event(_):
        nonlocal cancelled
        cancelled = True
    def check_cancel():
        if cancelled:
            raise SyncError("cancelled", "cancelled")
    with pytest.raises(SyncError, match="cancelled"):
        ensure_engine(root, event, check_cancel)
    assert not (root / "tools/jackify-engine-fixture").exists()
    assert ensure_engine(root).exists()
