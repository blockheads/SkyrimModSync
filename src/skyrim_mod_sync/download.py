from __future__ import annotations

import base64
import gzip
import hashlib
import json
import os
import shutil
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

import xxhash

from .paths import SyncError
from .recipe import MAX_RECIPE
from .wabbajack import inspect_list


def wj_hash(path: Path, check_cancel=lambda: None):
    digest = xxhash.xxh64()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            check_cancel()
            digest.update(block)
    return base64.b64encode(digest.intdigest().to_bytes(8, "little")).decode()


def _read(url, limit, check_cancel=lambda: None):
    check_cancel()
    request = Request(url, headers={"User-Agent": "SkyrimModSync/0.1 public-recipe-download"})
    with urlopen(request, timeout=30) as response:
        if urlsplit(response.geturl()).scheme != "https":
            raise SyncError("download_failed", "An HTTPS download redirected to an insecure URL.")
        data = bytearray()
        while len(data) <= limit:
            check_cancel()
            block = response.read(min(256 * 1024, limit + 1 - len(data)))
            if not block:
                return bytes(data)
            data.extend(block)
        raise SyncError("download_failed", "The server returned more data than declared.")


def download_list(url: str, output: Path, cache: Path, emit=lambda _: None, check_cancel=lambda: None):
    check_cancel()
    parts = urlsplit(url)
    if parts.scheme != "https" or parts.hostname != "authored-files.wabbajack.org" or parts.query or parts.fragment or parts.username:
        raise SyncError("unsupported_source", "This downloader accepts public authored-files.wabbajack.org recipe URLs.")
    if output.exists():
        raise SyncError("destination_exists", "Choose a new recipe filename.")
    try:
        # These URLs identify chunked files, not directly downloadable ZIPs.
        compressed = _read(url.rstrip("/") + "/definition.json.gz", 2 * 1024 * 1024, check_cancel)
        import io
        with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as stream:
            raw = stream.read(4 * 1024 * 1024 + 1)
        if len(raw) > 4 * 1024 * 1024:
            raise SyncError("invalid_download", "The CDN definition is too large.")
        definition = json.loads(raw)
        size = definition["Size"]
        blocks = definition["Parts"]
        if type(size) is not int or not 0 < size <= MAX_RECIPE or not isinstance(blocks, list) or not 1 <= len(blocks) <= 10000:
            raise SyncError("invalid_download", "Invalid CDN file size or part count.")
        offset = 0
        for index, block in enumerate(blocks):
            if block["Index"] != index or block["Offset"] != offset or type(block["Size"]) is not int or not 0 < block["Size"] <= 16 * 1024 * 1024:
                raise SyncError("invalid_download", "CDN parts overlap or have invalid bounds.")
            if len(base64.b64decode(block["Hash"], validate=True)) != 8:
                raise SyncError("invalid_download", "Invalid CDN part hash.")
            offset += block["Size"]
        if offset != size or len(base64.b64decode(definition["Hash"], validate=True)) != 8:
            raise SyncError("invalid_download", "CDN definition does not describe the whole file.")
    except (KeyError, TypeError, ValueError, OSError, EOFError) as exc:
        raise SyncError("download_failed", f"Cannot read the public recipe definition: {exc}") from exc
    folder = cache / hashlib.sha256(raw).hexdigest()
    folder.mkdir(parents=True, exist_ok=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(folder).free < size or shutil.disk_usage(output.parent).free < size:
        raise SyncError("disk_full", "Insufficient space for the recipe and verified download parts.")
    def fetch(block):
        check_cancel()
        target = folder / str(block["Index"])
        if target.is_file() and target.stat().st_size == block["Size"] and wj_hash(target, check_cancel) == block["Hash"]:
            return block["Size"]
        data = _read(url.rstrip("/") + f"/parts/{block['Index']}", block["Size"], check_cancel)
        temporary = target.with_suffix(".partial")
        temporary.write_bytes(data)
        try:
            if len(data) != block["Size"] or wj_hash(temporary, check_cancel) != block["Hash"]:
                raise SyncError("download_mismatch", f"CDN part {block['Index']} failed verification.")
            temporary.replace(target)
        finally:
            temporary.unlink(missing_ok=True)
        return block["Size"]
    completed = 0
    with ThreadPoolExecutor(max_workers=4) as pool:
        futures = [pool.submit(fetch, block) for block in blocks]
        for future in as_completed(futures):
            completed += future.result()
            emit({"state": "downloading_recipe", "completed": completed, "total": size})
    temporary = output.with_name(output.name + ".partial")
    try:
        with temporary.open("wb") as stream:
            for block in blocks:
                with (folder / str(block["Index"])).open("rb") as source:
                    for data in iter(lambda: source.read(1024 * 1024), b""):
                        check_cancel()
                        stream.write(data)
        if wj_hash(temporary, check_cancel) != definition["Hash"]:
            raise SyncError("download_mismatch", "The complete recipe failed its CDN hash check.")
        info = inspect_list(temporary)
        check_cancel()
        os.link(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    emit({"state": "recipe_downloaded", "path": str(output), "name": info["name"]})
    return output
