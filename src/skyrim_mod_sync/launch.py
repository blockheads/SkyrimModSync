from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

from .installer import atomic_json, job_lock, verify
from .paths import SyncError, locate, under
from .recipe import Recipe


def external_environment():
    environment = dict(os.environ)
    if getattr(sys, "frozen", False):
        old = environment.pop("LD_LIBRARY_PATH_ORIG", None)
        if old is not None:
            environment["LD_LIBRARY_PATH"] = old
        else:
            environment.pop("LD_LIBRARY_PATH", None)
        bundle = Path(sys._MEIPASS).resolve()
        for key in ("PATH", "QT_PLUGIN_PATH", "QT_QPA_PLATFORM_PLUGIN_PATH"):
            if key in environment:
                environment[key] = os.pathsep.join(p for p in environment[key].split(os.pathsep)
                                                  if p and not Path(p).resolve().is_relative_to(bundle))
    return environment


def process_running(name, prefix=None):
    for entry in Path("/proc").glob("[0-9]*/comm"):
        try:
            process = entry.parent
            matched = entry.read_text().strip().casefold() == name.casefold()
            if not matched and name.casefold().endswith('.exe'):
                arguments = (process / 'cmdline').read_bytes().decode(errors='replace').split('\0')
                matched = any(a.replace('\\', '/').rsplit('/', 1)[-1].casefold() == name.casefold() for a in arguments)
            if matched:
                if prefix is None:
                    return True
                values = dict(v.split(b'=', 1) for v in (process / 'environ').read_bytes().split(b'\0') if b'=' in v)
                configured = values.get(b'WINEPREFIX', b'').decode(errors='replace')
                if configured and Path(configured).resolve() == (prefix / 'pfx').resolve():
                    return True
        except OSError:
            pass
    return False


def _private_directory(path: Path, prefix: Path, replace_link=False):
    if not path.parent.resolve().is_relative_to(prefix.resolve()):
        raise SyncError("unsafe_prefix", "The managed prefix redirects a parent directory outside its storage.")
    if path.is_symlink():
        if not replace_link:
            raise SyncError("unsafe_prefix", "Unexpected symbolic link in the managed prefix.")
        # This prefix belongs to this utility. Replace only the known user-folder link,
        # never its destination (which Proton may point at the real user's Documents).
        path.unlink()
    path.mkdir(parents=True, exist_ok=True)
    if not path.resolve().is_relative_to(prefix.resolve()):
        raise SyncError("unsafe_prefix", "Managed settings escaped their prefix.")
    return path


def launch(recipe: Recipe, *, root: Path, proton: Path, steam: Path, runtime: Path, emit=lambda _: None):
    if sys.platform != "linux":
        raise SyncError("launch_unsupported", "Isolated Windows launch/settings control is not implemented yet.")
    root = root.resolve()
    install = root / "installs" / recipe.setup_id
    prefix = root / "prefixes" / recipe.setup_id
    # Distinct private prefixes may run independently. Prevent duplicate starts of
    # this setup, including Wine processes whose comm is merely "Main".
    if process_running('SkyrimSE.exe', prefix) or process_running('skse64_loader.exe', prefix):
        raise SyncError('game_running', 'This managed Skyrim setup is already running.')
    if not install.is_dir() or verify(recipe, install):
        raise SyncError("setup_not_verified", "The managed installation must pass verification before launch.")
    if not process_running("steam"):
        raise SyncError("steam_missing", "Start Steam before launching Skyrim.")
    proton_script = proton.resolve() / "proton"
    runtime_script = runtime.resolve() / "run"
    if not (steam.resolve() / "steamapps").is_dir():
        raise SyncError("steam_missing", "Choose the Steam directory containing steamapps.")
    for executable in (proton_script, runtime_script):
        if not executable.is_file() or not os.access(executable, os.X_OK):
            raise SyncError("launch_configuration", f"Missing runnable launch tool: {executable}")
    loader = locate(install / "game", "skse64_loader.exe")
    if prefix.is_symlink():
        raise SyncError("unsafe_prefix", "A managed prefix cannot be a symbolic link.")
    owned = prefix / "skyrim-mod-sync-owned.json"
    if prefix.exists() and not owned.is_file():
        raise SyncError("unsafe_prefix", "This existing prefix is not owned by Skyrim Mod Sync.")
    prefix.mkdir(parents=True, exist_ok=True)
    atomic_json(owned, {"owner": "SkyrimModSync", "setup_id": recipe.setup_id})
    environment = external_environment()
    from .discovery import desktop_environment
    environment.update(desktop_environment())
    environment.update({"STEAM_COMPAT_CLIENT_INSTALL_PATH": str(steam.resolve()),
                        "STEAM_COMPAT_DATA_PATH": str(prefix), "WINEPREFIX": str(prefix / "pfx"),
                        "SteamAppId": "489830", "SteamGameId": "489830", "STEAM_COMPAT_APP_ID": "489830"})
    # SME's Engine Fixes part 2 is a native game-root preloader.
    if (install / 'game/d3dx9_42.dll').is_file():
        environment['WINEDLLOVERRIDES'] = 'd3dx9_42=n,b'
    command = [str(runtime_script), "--", str(proton_script), "run"]
    state = root / "launches" / recipe.setup_id
    state.mkdir(parents=True, exist_ok=True)
    with job_lock(state / "launch.lock"):
        with (state / "launch.log").open("ab") as log:
            if not (prefix / "pfx/drive_c/users/steamuser").is_dir():
                emit({"state": "initializing_prefix"})
                initialized = subprocess.run(command + ["wineboot.exe", "-u"], cwd=install / "game",
                                             env=environment, stdout=log, stderr=subprocess.STDOUT)
                if initialized.returncode:
                    raise SyncError("prefix_failed", f"Proton prefix initialization failed. See {state / 'launch.log'}")
            user = prefix / "pfx/drive_c/users/steamuser"
            if not user.is_dir() or user.is_symlink():
                raise SyncError("unsafe_prefix", "Proton did not initialize a private steamuser directory.")
            # Bethesda and some SKSE plugins still use MAX_PATH buffers. Give this
            # prefix a short drive root, independent of the Linux storage path.
            drives = _private_directory(prefix / "pfx/dosdevices", prefix)
            game_drive = drives / "g:"
            if game_drive.exists() or game_drive.is_symlink():
                if not game_drive.is_symlink() or game_drive.resolve() != (install / "game").resolve():
                    raise SyncError("unsafe_prefix", "The managed game drive points at another directory.")
            else:
                game_drive.symlink_to(install / "game", target_is_directory=True)
            documents = _private_directory(user / "Documents", prefix, replace_link=True)
            # Some Wine builds use this alias for CSIDL_PERSONAL.
            old_documents = user / "My Documents"
            if old_documents.is_symlink():
                old_documents.unlink()
                old_documents.symlink_to("Documents", target_is_directory=True)
            appdata = _private_directory(user / "AppData", prefix)
            local = _private_directory(appdata / "Local", prefix)
            documents_target = _private_directory(documents / "My Games/Skyrim Special Edition", prefix)
            local_target = _private_directory(local / "Skyrim Special Edition", prefix)
            for f in recipe.data["files"]:
                if f["path"].startswith("config/documents/"):
                    target = under(documents_target, f["path"].removeprefix("config/documents/"))
                elif f["path"].startswith("config/local/"):
                    target = under(local_target, f["path"].removeprefix("config/local/"))
                else:
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(under(install, f["path"]), target)
            process = subprocess.Popen(command + ["G:\\" + loader.name], cwd=install / "game", env=environment,
                                       stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        record = {"state": "launch_started", "setup_id": recipe.setup_id,
                  "pid": process.pid, "prefix": str(prefix), "log": str(state / "launch.log")}
        atomic_json(state / "status.json", record)
        emit(record)
        # This confirms process creation only. The game/co-op plugin must check loading.
        return record
