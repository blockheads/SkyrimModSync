"""Obtain a launcher-selected engine. Share files cannot select executable downloads."""
from __future__ import annotations

import json
import os
import platform
import shutil
import tarfile
import tempfile
from dataclasses import dataclass
from pathlib import Path
from urllib.request import Request, urlopen
from urllib.parse import urlsplit

from .paths import SyncError, inventory, relative_path, sha256, under


@dataclass(frozen=True)
class EngineRelease:
    version: str
    url: str
    size: int
    sha256: str
    executable: str = "jackify-engine"


JACKIFY_LINUX = EngineRelease(
    "0.5.10",
    "https://github.com/Omni-guides/dev-jackify-engine/releases/download/v0.5.10/jackify-engine-0.5.10-linux-x64.tar.gz",
    58_345_382,
    "10480a3d452f6611d236219aab450e43bdcd7eab2f9c5cde96fb43e55ce5625d",
)
MAX_EXPANDED = 512 * 1024**2
MAX_MEMBERS = 2000


def _download(release, destination, emit, check_cancel):
    temporary = destination.with_suffix(".partial")
    try:
        request = Request(release.url, headers={"User-Agent": "SkyrimModSync/0.1 engine-bootstrap"})
        with urlopen(request, timeout=30) as source, temporary.open("wb") as target:
            if urlsplit(source.geturl()).scheme != "https":
                raise SyncError("engine_download_failed", "The engine download redirected away from HTTPS.")
            count = 0
            for block in iter(lambda: source.read(256 * 1024), b""):
                check_cancel()
                count += len(block)
                if count > release.size:
                    raise SyncError("engine_mismatch", "The engine download exceeds its pinned size.")
                target.write(block)
                emit({"state": "downloading_engine", "completed": count, "total": release.size})
        if count != release.size or sha256(temporary, check_cancel) != release.sha256:
            raise SyncError("engine_mismatch", "The engine download failed its pinned SHA-256 check.")
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def _extract(archive_path, stage, check_cancel):
    expected = []
    seen = set()
    total = 0
    with tarfile.open(archive_path, "r:gz") as archive:
        for index, member in enumerate(archive, 1):
            check_cancel()
            if index > MAX_MEMBERS:
                raise SyncError("invalid_engine", "The engine archive has too many entries.")
            name = member.name
            if name == "." and member.isdir():
                continue
            if name.startswith("./"):
                name = name[2:]
            name = relative_path(name)
            if name.casefold() in seen:
                raise SyncError("invalid_engine", "Duplicate engine archive entry.")
            seen.add(name.casefold())
            target = under(stage, name)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile() or member.size < 0:
                raise SyncError("invalid_engine", "Engine archives must contain ordinary files and directories.")
            total += member.size
            if total > MAX_EXPANDED:
                raise SyncError("invalid_engine", "The expanded engine exceeds its size limit.")
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source, target.open("xb") as output:
                remaining = member.size
                while remaining:
                    check_cancel()
                    block = source.read(min(1024 * 1024, remaining))
                    if not block:
                        raise SyncError("invalid_engine", "The engine archive is truncated.")
                    output.write(block)
                    remaining -= len(block)
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
            expected.append({"path": name, "size": member.size, "sha256": sha256(target, check_cancel),
                             "executable": bool(member.mode & 0o111)})
    return expected


def ensure_engine(root: Path, emit=lambda _: None, check_cancel=lambda: None):
    if platform.system() != "Linux" or platform.machine().lower() not in {"x86_64", "amd64"}:
        raise SyncError("engine_unsupported", "Automatic engine setup currently supports Linux x86-64; configure a compatible engine on this platform.")
    from .installer import atomic_json, job_lock
    release = JACKIFY_LINUX
    directory = root / "tools" / ("jackify-engine-" + release.version)
    receipt = root / "tool-receipts" / ("jackify-engine-" + release.version + ".json")
    root.mkdir(parents=True, exist_ok=True)
    with job_lock(root / "locks" / "engine-bootstrap.lock"):
        check_cancel()
        if directory.exists():
            try:
                data = json.loads(receipt.read_text())
                if data["archive_sha256"] != release.sha256:
                    raise ValueError("Unexpected engine identity")
                actual = dict(inventory(directory))
                if set(actual) != {f["path"] for f in data["files"]}:
                    raise ValueError("Unexpected engine files")
                for f in data["files"]:
                    path = actual[f["path"]]
                    if path.stat().st_size != f["size"] or sha256(path, check_cancel) != f["sha256"]:
                        raise ValueError("Changed engine files")
                    if f["executable"] and not os.access(path, os.X_OK):
                        raise ValueError("Engine executable permissions changed")
            except (OSError, ValueError, KeyError, TypeError) as exc:
                raise SyncError("engine_changed", "The managed engine changed; it will not be executed or overwritten.") from exc
            emit({"state": "engine_ready", "version": release.version, "reused": True})
            return directory / release.executable
        cache = root / "tools" / "downloads"
        cache.mkdir(parents=True, exist_ok=True)
        archive = cache / (release.sha256 + ".tar.gz")
        emit({"state": "preparing_engine", "version": release.version})
        if not archive.is_file() or archive.stat().st_size != release.size or sha256(archive, check_cancel) != release.sha256:
            if shutil.disk_usage(root).free < release.size + MAX_EXPANDED:
                raise SyncError("disk_full", "Insufficient space for the installer engine.")
            _download(release, archive, emit, check_cancel)
        if shutil.disk_usage(root).free < MAX_EXPANDED:
            raise SyncError("disk_full", "Insufficient space to unpack the installer engine.")
        directory.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="engine-", dir=directory.parent) as temporary:
            stage = Path(temporary) / "files"
            stage.mkdir()
            expected = _extract(archive, stage, check_cancel)
            executable = under(stage, release.executable)
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise SyncError("invalid_engine", "The archive has no executable installer engine.")
            check_cancel()
            # A receipt is stored outside the distribution; subsequent uses rehash it.
            atomic_json(receipt, {"version": release.version, "archive_sha256": release.sha256, "files": expected})
            stage.rename(directory)
        emit({"state": "engine_ready", "version": release.version, "reused": False})
        return directory / release.executable
