"""Discover installed Steam tools from manifests, without account data or launches."""
from __future__ import annotations

import os
import re
from pathlib import Path

from .paths import SyncError


def parse_vdf(text):
    tokens = re.findall(r'//[^\n]*|"(?:\\.|[^"\\])*"|[{}]|[^\s{}"]+', text)
    tokens = [t for t in tokens if not t.startswith("//")]
    offset = 0
    def item():
        nonlocal offset
        token = tokens[offset]
        offset += 1
        if token.startswith('"'):
            return token[1:-1].replace('\\"', '"').replace('\\\\', '\\')
        return token
    def object_(nested=False):
        nonlocal offset
        data = {}
        while offset < len(tokens):
            key = item()
            if key == "}":
                if not nested:
                    raise ValueError("Unexpected closing brace")
                return data
            if key == "{" or offset == len(tokens):
                raise ValueError("Invalid VDF key")
            value = item()
            data[key] = object_(True) if value == "{" else value
        if nested:
            raise ValueError("Missing closing brace")
        return data
    return object_()


def discover(home: Path | None = None, steam: Path | None = None, base: Path | None = None):
    home = home or Path.home()
    roots = [steam] if steam else [home / ".steam/steam", home / ".steam/debian-installation",
                                  home / ".local/share/Steam", home / '.var/app/com.valvesoftware.Steam/.local/share/Steam']
    roots = list(dict.fromkeys(p.resolve() for p in roots if p and (p / "steamapps").is_dir()))
    candidates = []
    for root in roots:
        libraries = [root]
        vdf = root / "steamapps/libraryfolders.vdf"
        if vdf.is_file():
            try:
                entries = parse_vdf(vdf.read_text()).get("libraryfolders", {})
                libraries += [Path(e["path"]) for e in entries.values() if isinstance(e, dict) and "path" in e]
            except (OSError, ValueError, TypeError):
                pass  # The primary library remains usable even when secondary metadata is broken.
        libraries = list(dict.fromkeys(p.resolve() for p in libraries))
        games, protons, runtimes = [], [], []
        for library in libraries:
            apps = library / "steamapps"
            manifest = apps / "appmanifest_489830.acf"
            if manifest.is_file():
                try:
                    folder = parse_vdf(manifest.read_text())["AppState"]["installdir"]
                    if not isinstance(folder, str) or "/" in folder or "\\" in folder or folder in {".", ".."}:
                        raise ValueError("Invalid game directory")
                    path = apps / "common" / folder
                    if (path / "SkyrimSE.exe").is_file():
                        games.append(path.resolve())
                except (OSError, KeyError, ValueError, TypeError):
                    pass
            proton = apps / "common/Proton 10.0"
            runtime = apps / "common/SteamLinuxRuntime_sniper"
            if (proton / "proton").is_file():
                protons.append(proton.resolve())
            if (runtime / "run").is_file():
                runtimes.append(runtime.resolve())
        for game in dict.fromkeys(games):
            if base and game != base.resolve():
                continue
            candidates.append({"steam": root, "base": game,
                               "proton": protons[0] if protons else None,
                               "runtime": runtimes[0] if runtimes else None})
    # Multiple installations must be an explicit local choice.
    candidates = list({str(c["base"]): c for c in candidates}.values())
    if len(candidates) != 1:
        raise SyncError("game_discovery", "Cannot locate a Steam Skyrim install. Supply explicit paths with --base and --steam." if not candidates
                        else "Multiple Skyrim installations found; select the intended one with --base PATH.")
    return candidates[0]


def desktop_environment():
    result = {}
    allowed = {"DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"}
    if os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"):
        return result
    for process in Path("/proc").glob("[0-9]*"):
        try:
            if (process / "comm").read_text().strip() != "steam":
                continue
            values = dict(v.split(b"=", 1) for v in (process / "environ").read_bytes().split(b"\0") if b"=" in v)
            result = {key: values[key.encode()].decode() for key in allowed if key.encode() in values}
            break
        except (OSError, UnicodeError, ValueError):
            pass
    return result
