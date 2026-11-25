from __future__ import annotations

from pathlib import Path

from .paths import SyncError, inventory, relative_path, sha256
from .recipe import write
from .wabbajack import inspect_list


def export_setup(*, base: Path, installed: Path, artifact: Path, output: Path,
                 name: str, profile: str | None = None, layout: str = "mo2",
                 standard_root_builder: bool = False, recipe_url: str | None = None, emit=lambda _: None,
                 runtime_only=False, profile_ini_overrides=None):
    base, installed, artifact = base.resolve(), installed.resolve(), artifact.resolve()
    if not artifact.is_file():
        raise SyncError("missing_recipe", "Select the original .wabbajack recipe.")
    winners = {}

    def overlay(directory: Path, destination: str, kind: str, source_prefix: str = "", mod=False):
        if not directory.is_dir():
            return
        for rel, path in inventory(directory):
            if mod and (rel.casefold() == "meta.ini" or rel.casefold().endswith(".mohidden")):
                continue
            first = rel.split("/")[0].casefold()
            if mod and runtime_only and first in {'calientetools', 'nemesis_engine'}:
                # Authoring executables/configuration stay in the reconstructed workspace.
                # Their generated meshes/animations remain ordinary runtime assets.
                continue
            if mod and first == "ini tweaks":
                raise SyncError("unsupported_layout", "Mod INI tweaks need to be merged before export.")
            if mod and first == "root":
                if not standard_root_builder:
                    raise SyncError("unsupported_layout", "This list uses Root Builder. Explicitly select standard root deployment after reviewing its settings.")
                target = relative_path("game/" + rel.split("/", 1)[1])
                source = relative_path(source_prefix + rel)
                winners[target.casefold()] = (target, path, {"kind": kind, "path": source})
                continue
            target = relative_path(destination + rel)
            source = relative_path(source_prefix + rel)
            winners[target.casefold()] = (target, path, {"kind": kind, "path": source})

    if layout != "stock-mo2":
        overlay(base, "game/", "base")
    if layout == "native":
        overlay(installed, "game/", "wabbajack")
    elif layout in {"mo2", "stock-mo2"}:
        if not profile:
            raise SyncError("profile_required", "Select the MO2 profile used by the host.")
        relative_path(profile)
        if "/" in profile:
            raise SyncError("invalid_path", "Profile must be a single directory name.")
        selected = installed / "profiles" / profile
        modlist = selected / "modlist.txt"
        if not modlist.is_file():
            raise SyncError("missing_profile", f"Profile does not contain modlist.txt: {profile}")
        overwrite = installed / "overwrite"
        if overwrite.is_dir() and any(inventory(overwrite)):
            raise SyncError("untracked_output", "Move required overwrite output into an enabled recipe mod before sharing.")
        tweaks = selected / "initweaks.ini"
        if not profile_ini_overrides and tweaks.is_file() and any(line.strip() and not line.lstrip().startswith(("#", ";"))
                                    for line in tweaks.read_text(encoding="utf-8-sig").splitlines()):
            raise SyncError("unsupported_layout", "Profile INI tweaks need to be merged into the exported INIs first.")
        root_name = "Stock Game" if layout == "stock-mo2" else "Game Folder Files"
        root = installed / root_name
        if layout == "stock-mo2" and not (root / "SkyrimSE.exe").is_file():
            raise SyncError("missing_stock_game", "The reconstructed list has no complete Stock Game directory.")
        overlay(root, "game/", "wabbajack", root_name + "/")
        lines = modlist.read_text(encoding="utf-8-sig").splitlines()
        enabled = []
        for line in lines:
            line = line.strip()
            if line.startswith("+"):
                mod = relative_path(line[1:])
                if "/" in mod:
                    raise SyncError("invalid_path", "Mod names must be single directory names.")
                if mod.casefold() in {n.casefold() for n in enabled}:
                    raise SyncError("invalid_profile", f"Duplicate enabled mod: {mod}")
                enabled.append(mod)
        # MO2 serializes modlist.txt highest priority first.
        for mod in reversed(enabled):
            directory = installed / "mods" / mod
            if not directory.is_dir() or directory.is_symlink():
                raise SyncError("missing_mod", f"Enabled mod is missing: {mod}")
            overlay(directory, "game/Data/", "wabbajack", f"mods/{mod}/", mod=True)
        for filename in ("Skyrim.ini", "SkyrimPrefs.ini", "SkyrimCustom.ini"):
            file = (profile_ini_overrides or {}).get(filename)
            if file is None:
                from .paths import locate
                try:
                    file = locate(selected, filename)
                except SyncError as exc:
                    if exc.code == 'missing_file':
                        continue
                    raise
            if file.is_file():
                target = "config/documents/" + filename
                winners[target.casefold()] = (target, file, {"kind": "wabbajack", "path": file.relative_to(installed).as_posix()})
        for filename in ("plugins.txt", "loadorder.txt"):
            file = selected / filename
            if file.is_file():
                target = "config/local/" + filename
                winners[target.casefold()] = (target, file, {"kind": "wabbajack", "path": f"profiles/{profile}/{filename}"})
        if "config/local/plugins.txt" not in winners:
            raise SyncError("missing_activation", "The selected profile must contain plugins.txt.")
    else:
        raise SyncError("unsupported_layout", f"Unsupported layout: {layout}")
    if runtime_only:
        from .runtime_files import add_runtime_defaults
        add_runtime_defaults(winners, installed)
    # Windows treats differently cased directory segments as one directory. Materialize
    # that view on Linux too, without changing source lookup or final file spelling.
    directories = {}
    canonical_winners = []
    for target, path, source in sorted(winners.values()):
        parts = target.split("/")
        parent = []
        for part in parts[:-1]:
            key = "/".join(parent + [part]).casefold()
            directories.setdefault(key, part)
            parent.append(directories[key])
        canonical_winners.append(("/".join(parent + [parts[-1]]), path, source))
    files = []
    for index, (target, path, source) in enumerate(canonical_winners, 1):
        emit({"state": "exporting", "file": target, "completed": index, "total": len(winners)})
        files.append({"path": target, "size": path.stat().st_size, "sha256": sha256(path), "source": source})
    data = {"schema": 1, "name": name, "artifact": {"sha256": sha256(artifact), "size": artifact.stat().st_size}, "files": files}
    data["sources"] = inspect_list(artifact)["sources"]
    if recipe_url:
        data["artifact"]["url"] = recipe_url
    write(output, data, artifact)
    return output
