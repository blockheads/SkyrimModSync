from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit
from zipfile import BadZipFile, ZIP_DEFLATED, ZipFile

from .paths import SyncError, relative_path, sha256

MAX_MANIFEST = 64 * 1024 * 1024
MAX_RECIPE = 8 * 1024 * 1024 * 1024
MAX_FILES = 500_000


def canonical(value) -> bytes:
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


def _unique_object(pairs):
    obj = {}
    for key, value in pairs:
        if key in obj:
            raise SyncError("invalid_recipe", f"Duplicate JSON key: {key}")
        obj[key] = value
    return obj


def validate(data):
    required = {"schema", "name", "artifact", "files"}
    if not isinstance(data, dict) or not required.issubset(data) or set(data) - required - {"sources"}:
        raise SyncError("invalid_recipe", "Unsupported setup descriptor.")
    if type(data["schema"]) is not int or data["schema"] != 1 or not isinstance(data["name"], str) or not 1 <= len(data["name"]) <= 200:
        raise SyncError("invalid_recipe", "Unsupported schema or setup name.")
    artifact = data["artifact"]
    if not isinstance(artifact, dict) or set(artifact) not in ({"sha256", "size"}, {"sha256", "size", "url"}):
        raise SyncError("invalid_recipe", "Missing Wabbajack artifact identity.")
    _identity(artifact)
    if "url" in artifact:
        url = urlsplit(artifact["url"])
        if url.scheme != "https" or url.hostname != "authored-files.wabbajack.org" or url.query or url.fragment or url.username:
            raise SyncError("invalid_recipe", "Remote recipes must use a public Wabbajack CDN URL.")
    if artifact["size"] > MAX_RECIPE:
        raise SyncError("invalid_recipe", "Recipe artifact is too large.")
    sources = data.get("sources", [])
    if not isinstance(sources, list) or len(sources) > 10000:
        raise SyncError("invalid_recipe", "Invalid source list.")
    for source in sources:
        if not isinstance(source, dict) or set(source) != {"name", "kind", "url"} or any(not isinstance(v, str) or len(v) > 4096 for v in source.values()):
            raise SyncError("invalid_recipe", "Invalid source preview.")
        if source["url"]:
            url = urlsplit(source["url"])
            if url.scheme != "https" or url.username or url.query:
                raise SyncError("invalid_recipe", "Source previews cannot contain credentials or signed download queries.")
    files = data["files"]
    if not isinstance(files, list) or not 1 <= len(files) <= MAX_FILES:
        raise SyncError("invalid_recipe", "The setup must declare its expected files.")
    paths = {}
    sources = {}
    for f in files:
        if not isinstance(f, dict) or set(f) != {"path", "sha256", "size", "source"}:
            raise SyncError("invalid_recipe", "Invalid file instruction.")
        relative_path(f["path"])
        if not f["path"].startswith(("game/", "config/documents/", "config/local/")):
            raise SyncError("invalid_recipe", "Files must belong to the isolated game or configuration.")
        _identity(f)
        key = f["path"].casefold()
        if key in paths:
            raise SyncError("case_collision", f"Duplicate destination: {f['path']}")
        paths[key] = f["path"]
        source = f["source"]
        if not isinstance(source, dict) or set(source) != {"kind", "path"} or source["kind"] not in {"base", "wabbajack"}:
            raise SyncError("invalid_recipe", "Unsupported file source.")
        relative_path(source["path"])
        source_key = (source["kind"], source["path"].casefold())
        identity = (f["sha256"], f["size"])
        if source_key in sources and sources[source_key] != identity:
            raise SyncError("invalid_recipe", "One source has conflicting expected contents.")
        sources[source_key] = identity
    for key in paths:
        parent = key.rsplit("/", 1)[0]
        while "/" in parent:
            if parent in paths:
                raise SyncError("invalid_recipe", "A destination is both a file and a directory.")
            parent = parent.rsplit("/", 1)[0]
    if "game/skyrimse.exe" not in paths or "game/skse64_loader.exe" not in paths:
        raise SyncError("invalid_recipe", "A setup must contain SkyrimSE.exe and skse64_loader.exe.")
    return data


def _identity(obj):
    if not isinstance(obj["sha256"], str) or not re.fullmatch("[0-9a-f]{64}", obj["sha256"]):
        raise SyncError("invalid_recipe", "Expected a SHA-256 content identity.")
    if type(obj["size"]) is not int or not 0 <= obj["size"] <= 1024**4:
        raise SyncError("invalid_recipe", "Invalid expected file size.")


@dataclass(frozen=True)
class Recipe:
    data: dict
    package: Path

    @property
    def setup_id(self):
        # Sources and installer choices do not determine equality of the final tree.
        manifest = sorted(({k: f[k] for k in ("path", "size", "sha256")} for f in self.data["files"]),
                          key=lambda f: f["path"])
        return hashlib.sha256(canonical(manifest)).hexdigest()

    @property
    def job_id(self):
        return hashlib.sha256(canonical(self.data)).hexdigest()

    def extract_artifact(self, destination: Path, emit=lambda _: None, check_cancel=lambda: None):
        check_cancel()
        destination.parent.mkdir(parents=True, exist_ok=True)
        identity = self.data["artifact"]
        if destination.is_file() and destination.stat().st_size == identity["size"] and sha256(destination, check_cancel) == identity["sha256"]:
            return
        if "url" in identity:
            from .download import download_list
            destination.unlink(missing_ok=True)
            download_list(identity["url"], destination, destination.parent / "recipe-parts", emit, check_cancel)
            if destination.stat().st_size != identity["size"] or sha256(destination, check_cancel) != identity["sha256"]:
                destination.unlink(missing_ok=True)
                raise SyncError("artifact_mismatch", "The public recipe changed or failed its pinned SHA-256 check.")
            return
        partial = destination.with_suffix(".partial")
        try:
            with ZipFile(self.package) as archive, archive.open("recipe.wabbajack") as src, partial.open("wb") as dst:
                remaining = self.data["artifact"]["size"]
                while remaining:
                    check_cancel()
                    block = src.read(min(1024 * 1024, remaining))
                    if not block:
                        raise SyncError("artifact_mismatch", "The embedded recipe is truncated.")
                    dst.write(block)
                    remaining -= len(block)
                if src.read(1):
                    raise SyncError("artifact_mismatch", "The embedded recipe is larger than declared.")
            if sha256(partial, check_cancel) != self.data["artifact"]["sha256"]:
                raise SyncError("artifact_mismatch", "The embedded Wabbajack recipe failed verification.")
            partial.replace(destination)
        finally:
            partial.unlink(missing_ok=True)


def load(package: Path) -> Recipe:
    try:
        with ZipFile(package) as archive:
            infos = archive.infolist()
            names = {i.filename for i in infos}
            if len(infos) != len(names) or names not in ({"manifest.json"}, {"manifest.json", "recipe.wabbajack"}):
                raise SyncError("invalid_recipe", "A setup file must contain a manifest and optionally its Wabbajack recipe.")
            for info in infos:
                if stat.S_ISLNK(info.external_attr >> 16) or info.flag_bits & 1:
                    raise SyncError("invalid_recipe", "Encrypted or linked setup entries are not supported.")
            manifest = archive.getinfo("manifest.json")
            if manifest.file_size > MAX_MANIFEST:
                raise SyncError("invalid_recipe", "The manifest is too large.")
            data = validate(json.loads(archive.read(manifest), object_pairs_hook=_unique_object))
            if ("url" in data["artifact"]) != (names == {"manifest.json"}):
                raise SyncError("invalid_recipe", "The recipe must be embedded or URL-addressed, exclusively.")
            if "recipe.wabbajack" in names and archive.getinfo("recipe.wabbajack").file_size != data["artifact"]["size"]:
                raise SyncError("artifact_mismatch", "The recipe size disagrees with its manifest.")
    except (BadZipFile, UnicodeError, json.JSONDecodeError, KeyError, TypeError) as exc:
        raise SyncError("invalid_recipe", f"Cannot read this setup file: {exc}") from exc
    return Recipe(data, package.resolve())


def write(package: Path, data: dict, artifact: Path):
    validate(data)
    package = package.absolute()
    package.parent.mkdir(parents=True, exist_ok=True)
    if package.exists():
        raise SyncError("destination_exists", "Choose a new setup filename; an existing file will not be replaced.")
    temporary = package.with_name(package.name + ".partial")
    if temporary.exists():
        raise SyncError("destination_exists", "An export is already in progress at this filename.")
    try:
        with temporary.open("xb") as stream, ZipFile(stream, "w", compression=ZIP_DEFLATED) as archive:
            archive.writestr("manifest.json", canonical(data))
            if "url" not in data["artifact"]:
                # Wabbajack is already compressed; store it for fast random-access previews.
                from zipfile import ZIP_STORED
                archive.write(artifact, "recipe.wabbajack", compress_type=ZIP_STORED)
        package_data = load(temporary)
        if sha256(artifact) != package_data.data["artifact"]["sha256"]:
            raise SyncError("source_changed", "The recipe changed during export; export it again.")
        if "url" not in data["artifact"]:
            digest = hashlib.sha256()
            with ZipFile(temporary) as archive, archive.open("recipe.wabbajack") as source:
                for block in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(block)
            if digest.hexdigest() != data["artifact"]["sha256"]:
                raise SyncError("source_changed", "The recipe changed during packaging; export it again.")
        # Link publishes without overwriting another exporter racing this one.
        os.link(temporary, package)
    finally:
        temporary.unlink(missing_ok=True)
