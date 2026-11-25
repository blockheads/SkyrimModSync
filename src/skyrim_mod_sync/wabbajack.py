from __future__ import annotations

import json
from collections import Counter
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit
from zipfile import BadZipFile, ZipFile

from .paths import SyncError
from .recipe import MAX_MANIFEST


def inspect_list(path: Path):
    """Read metadata only. Never extract or execute list contents."""
    try:
        with ZipFile(path) as archive:
            entry = archive.getinfo("modlist")
            if entry.file_size > MAX_MANIFEST:
                raise SyncError("invalid_wabbajack", "The Wabbajack manifest exceeds the supported size.")
            data = json.loads(archive.read(entry))
    except (BadZipFile, KeyError, UnicodeError, json.JSONDecodeError) as exc:
        raise SyncError("invalid_wabbajack", "This file is not a readable Wabbajack list.") from exc
    if not isinstance(data, dict) or not isinstance(data.get("Archives"), list):
        raise SyncError("invalid_wabbajack", "Unsupported Wabbajack modlist schema.")
    sources = []
    kinds = Counter()
    for item in data["Archives"]:
        if not isinstance(item, dict) or not isinstance(item.get("State", {}), dict):
            raise SyncError("invalid_wabbajack", "Invalid archive source metadata.")
        state = item.get("State", {})
        kind = str(state.get("$type", "unknown"))
        kinds[kind] += 1
        url = state.get("Url") or state.get("URL") or state.get("ManualURL") or ""
        if "Nexus" in kind and state.get("GameName") == "SkyrimSpecialEdition" and state.get("ModID"):
            url = f"https://www.nexusmods.com/skyrimspecialedition/mods/{state['ModID']}"
        parts = urlsplit(str(url))
        # Strip account tokens and signed URL query strings from displayed/shared hints.
        url = urlunsplit((parts.scheme, parts.hostname or "", parts.path, "", "")) if parts.scheme == "https" else ""
        sources.append({"name": str(item.get("Name", "Unnamed archive")), "kind": kind, "url": url})
    directives = data.get("Directives", [])
    if not isinstance(directives, list) or any(not isinstance(d, dict) for d in directives):
        raise SyncError("invalid_wabbajack", "Invalid installation directives.")
    profile_paths = [str(d.get("To", "")) for d in directives if str(d.get("To", "")).startswith("profiles\\")]
    roots = [str(d.get("To", "")) for d in directives if "\\root\\" in str(d.get("To", "")).lower()]
    return {"name": data.get("Name", "Unnamed list"), "game": data.get("GameType"),
            "archives": len(sources), "archive_bytes": sum(int(a.get("Size", 0)) for a in data["Archives"]),
            "directives": len(directives), "source_kinds": dict(kinds), "sources": sources,
            "profiles": sorted({p.split("\\")[1] for p in profile_paths if len(p.split("\\")) > 2}),
            "root_builder_files": len(roots)}


def inspect_package_sources(package: Path):
    with ZipFile(package) as archive, archive.open("recipe.wabbajack") as embedded:
        return inspect_list(embedded)
