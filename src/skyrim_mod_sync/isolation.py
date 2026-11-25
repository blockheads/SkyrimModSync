"""Run the stock engine with a private filesystem view; HOME is not changed."""
from __future__ import annotations

import json
import os
import shutil
from pathlib import Path

from .paths import SyncError


def engine_context(root: Path, job: Path, steam: Path | None, proton: Path | None, key: str | None,
                   *, base: Path | None = None, downloads: Path | None = None):
    from .launch import external_environment
    bwrap = shutil.which("bwrap")
    if not bwrap:
        raise SyncError("isolation_missing", "Install the bubblewrap system package to run the isolated Linux installer.")
    root, job = root.resolve(), job.resolve()
    if not job.is_relative_to(root):
        raise SyncError("unsafe_destination", "Installer state must belong to managed storage.")
    config = root / "engine-config"
    data = root / "engine-data"
    config.mkdir(parents=True, exist_ok=True, mode=0o700)
    data.mkdir(parents=True, exist_ok=True, mode=0o700)
    home = Path.home()
    steam_mount = home / ".steam"
    if steam and (not steam_mount.is_dir() or not steam.resolve().is_relative_to(steam_mount.resolve())):
        steam_mount = steam.resolve()
    settings = {"jackify_data_dir": str(data)}
    if proton:
        settings["proton_path"] = str(proton.resolve())
    (config / "config.json").write_text(json.dumps(settings))
    # The virtual user home is private. Steam's installation metadata remains read-only.
    prefix = [bwrap, "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--unshare-pid",
              "--die-with-parent", "--tmpfs", str(home), "--dir", str(home / ".config"),
              ]
    for path in dict.fromkeys(p.resolve() for p in (steam_mount if steam else None, base, proton) if p):
        prefix += ['--ro-bind', str(path), str(path)]
    prefix += ['--bind', str(root), str(root)]
    if downloads:
        prefix += ['--bind', str(downloads.resolve()), str(downloads.resolve())]
    prefix += ["--bind", str(config), str(home / ".config/jackify"),
               "--bind", str(data), str(home / "Jackify"), "--chdir", str(job), "--"]
    environment = external_environment()
    environment.pop("NEXUS_OAUTH_INFO", None)
    environment.pop("NEXUS_API_KEY", None)
    if key:
        environment["NEXUS_API_KEY"] = key
    return prefix, environment
