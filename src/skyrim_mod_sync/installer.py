from __future__ import annotations

import json
import hashlib
import os
import shutil
import signal
import subprocess
import tempfile
import re
from contextlib import contextmanager
from pathlib import Path

from .paths import SyncError, inventory, locate, sha256, under
from .recipe import Recipe, canonical


def atomic_json(path: Path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".partial")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(data, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def verify(recipe: Recipe, directory: Path, check_cancel=None):
    expected = {f["path"]: f for f in recipe.data["files"]}
    actual = dict(inventory(directory))
    differences = []
    from .runtime_files import runtime_logs
    logs = runtime_logs(expected)
    for name in sorted(expected.keys() - actual.keys()):
        differences.append({"kind": "missing", "path": name})
    for name in sorted(actual.keys() - expected.keys()):
        if name.casefold() not in logs:
            differences.append({"kind": "extra", "path": name})
    for name in sorted(expected.keys() & actual.keys()):
        f = expected[name]
        if actual[name].stat().st_size != f["size"] or sha256(actual[name], check_cancel) != f["sha256"]:
            differences.append({"kind": "changed", "path": name})
    return differences


def plan(recipe: Recipe, base: Path, root: Path, check_cancel=None):
    _separate(root, base)
    blockers = []
    for f in recipe.data["files"]:
        if f["source"]["kind"] == "base":
            try:
                source = locate(base, f["source"]["path"])
                if source.stat().st_size != f["size"]:
                    blockers.append({"kind": "base_changed", "path": f["source"]["path"]})
            except SyncError as exc:
                if exc.code == "cancelled":
                    raise
                blockers.append({"kind": exc.code, "path": f["source"]["path"]})
    native = [f["path"] for f in recipe.data["files"] if Path(f["path"]).suffix.casefold() in {".exe", ".dll", ".bat", ".cmd", ".ps1"}]
    destination = root.resolve() / "installs" / recipe.setup_id
    reuse = destination.is_dir() and not verify(recipe, destination, check_cancel)
    return {"setup_id": recipe.setup_id, "name": recipe.data["name"], "destination": str(destination),
            "files": len(recipe.data["files"]), "installed_bytes": sum(f["size"] for f in recipe.data["files"]),
            "recipe_bytes": recipe.data["artifact"]["size"], "native_files": native,
            "blockers": blockers, "reusable": reuse,
            "download_bytes": None, "note": "Mod download size and authentication are resolved by the selected Wabbajack engine."}


def _separate(root: Path, source: Path):
    root, source = root.resolve(), source.resolve()
    if root.is_relative_to(source) or source.is_relative_to(root):
        raise SyncError("unsafe_destination", "Managed storage must be separate from the source game/setup.")


@contextmanager
def job_lock(path: Path):
    # Advisory OS locks are released if the process dies; the lock file can remain.
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a+b") as stream:
        if os.name == "nt":
            import msvcrt
            stream.seek(0)
            stream.write(b"\0")
            stream.flush()
            stream.seek(0)
            try:
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            except OSError as exc:
                raise SyncError("job_busy", "This setup is already being installed.") from exc
        else:
            import fcntl
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise SyncError("job_busy", "This setup is already being installed.") from exc
        yield


def run_engine(engine: Path, artifact: Path, output: Path, downloads: Path, log: Path, check_cancel=lambda: None,
               engine_kind="wabbajack", base: Path | None = None, command_prefix=(), environment=None,
               emit=lambda _: None):
    from .launch import external_environment
    engine = engine.resolve()
    if not engine.is_file():
        raise SyncError("engine_missing", "Choose a local Wabbajack-compatible CLI executable.")
    command = [str(engine)]
    if engine.suffix.casefold() == ".dll":
        dotnet = shutil.which("dotnet")
        if not dotnet:
            raise SyncError("engine_missing", "This engine needs a locally installed .NET runtime.")
        command = [dotnet, str(engine)]
    if engine_kind == "jackify":
        command += ["install", "-w", str(artifact), "-o", str(output), "-d", str(downloads)]
        if command_prefix and base:
            # These options are hidden from help; source inspection and the real
            # release invocation confirm support. Generic fixture/override stays minimal.
            command += ["-g", str(base.resolve()), "--show-file-progress"]
    elif engine_kind == "wabbajack":
        command += ["install", "--wabbajack", str(artifact), "--output", str(output), "--downloads", str(downloads)]
    else:
        raise SyncError("engine_unsupported", "Unknown installer engine adapter.")
    with log.open("ab") as stream:
        process = None
        offset = log.stat().st_size
        last_progress = None
        try:
            process = subprocess.Popen([*command_prefix, *command], cwd=output.parent, stdout=stream, stderr=subprocess.STDOUT,
                                       env=environment or external_environment(), start_new_session=os.name != "nt")
            while True:
                check_cancel()
                with log.open('rb') as progress_log:
                    progress_log.seek(offset)
                    chunk = progress_log.read(256 * 1024)
                    offset += len(chunk)
                counters = re.findall(r'Installing files (\d+)/(\d+)', chunk.decode(errors='replace'))
                if counters and counters[-1] != last_progress:
                    last_progress = counters[-1]
                    emit({'state': 'reconstructing', 'completed': int(last_progress[0]), 'total': int(last_progress[1])})
                try:
                    code = process.wait(timeout=0.25)
                    break
                except subprocess.TimeoutExpired:
                    continue
        except BaseException:
            if process is not None and process.poll() is None:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                                   stdout=stream, stderr=subprocess.STDOUT, check=False)
                else:
                    os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    if os.name != "nt":
                        os.killpg(process.pid, signal.SIGKILL)
                    else:
                        process.kill()
                    process.wait()
            raise
    if code:
        raise SyncError("engine_failed", f"The installer stopped (exit {code}). See {log}; authentication or manual downloads may be needed.")


def install(recipe: Recipe, *, base: Path, root: Path, engine: Path | None = None,
            reconstructed: Path | None = None, accepted: bool = False, emit=lambda _: None,
            engine_kind="wabbajack", managed_source=False):
    if not accepted:
        raise SyncError("consent_required", "Review the setup and explicitly approve installation first.")
    _separate(root, base)
    if reconstructed:
        if managed_source:
            try:
                parts = reconstructed.resolve().relative_to(root.resolve() / 'code-jobs').parts
            except ValueError:
                parts = ()
            if len(parts) != 2 or parts[1] != 'reconstructed' or not re.fullmatch('[0-9a-f]{64}', parts[0]):
                raise SyncError('unsafe_destination', 'The internal source must be an owned code-job reconstruction.')
        else:
            _separate(root, reconstructed)
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    # Both recipes with the same final setup and differing recipes share this lock.
    with job_lock(root / "locks" / (recipe.setup_id + ".lock")):
        job = root / "jobs" / recipe.job_id
        job.mkdir(parents=True, exist_ok=True)
        cancel = job / "cancel"
        cancel.unlink(missing_ok=True)
        def check_cancel():
            if cancel.exists():
                raise SyncError("cancelled", "Installation cancelled; retry to reuse completed work.")
        def event(state, **extra):
            data = {"state": state, "setup_id": recipe.setup_id, **extra}
            atomic_json(job / "status.json", data)
            with (job / "events.jsonl").open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(data) + "\n")
            emit(data)
        try:
            event("checking_base")
            report = plan(recipe, base, root, check_cancel)
            if report["blockers"]:
                raise SyncError("base_mismatch", "The local base game differs from the host. Inspect the plan before downloading mods.")
            destination = Path(report["destination"])
            if destination.exists():
                if not report["reusable"]:
                    raise SyncError("installed_setup_changed", "The existing managed setup changed. It will not be overwritten; inspect its differences.")
                event("files_verified", directory=str(destination), reused=True)
                return destination
            if shutil.disk_usage(root).free < report["installed_bytes"]:
                raise SyncError("disk_full", "Insufficient free space for the isolated setup, before downloads and reconstruction.")
            if reconstructed is None:
                if engine is None:
                    from .bootstrap import ensure_engine
                    engine = ensure_engine(root, lambda data: event(data["state"], **{k: v for k, v in data.items() if k != "state"}), check_cancel)
                    engine_kind = "jackify"
                event("preparing_recipe")
                recipe.extract_artifact(job / "recipe.wabbajack",
                                        lambda data: event(data["state"], **{k: v for k, v in data.items() if k != "state"}), check_cancel)
                reconstructed = job / "reconstructed"
                reconstructed.mkdir(exist_ok=True)
                downloads = root / "downloads"
                downloads.mkdir(exist_ok=True)
                event("reconstructing", log=str(job / "engine.log"))
                run_engine(engine, job / "recipe.wabbajack", reconstructed, downloads, job / "engine.log", check_cancel,
                           engine_kind=engine_kind, base=base)
            generated_profiles = {f['source']['path'].split('/')[2] for f in recipe.data['files']
                                  if f['source']['path'].startswith('.skyrimsync-generated/profiles/')}
            if generated_profiles:
                from .profiles import profile_inis
                for profile in generated_profiles:
                    profile_inis(reconstructed, profile)
            from .runtime_files import generate_runtime_file
            for f in recipe.data['files']:
                if f['source']['path'].startswith('.skyrimsync-generated/runtime/'):
                    generate_runtime_file(reconstructed, f['source']['path'])
            event("staging")
            stage = job / "staging"
            stage.mkdir(exist_ok=True)
            for index, f in enumerate(recipe.data["files"], 1):
                check_cancel()
                target = under(stage, f["path"])
                if target.is_file() and target.stat().st_size == f["size"] and sha256(target, check_cancel) == f["sha256"]:
                    continue
                source_root = base if f["source"]["kind"] == "base" else reconstructed
                source = locate(source_root, f["source"]["path"])
                if source.stat().st_size != f["size"] or sha256(source, check_cancel) != f["sha256"]:
                    raise SyncError("source_mismatch", f"Reconstructed file differs from the host: {f['path']}")
                target.parent.mkdir(parents=True, exist_ok=True)
                with tempfile.NamedTemporaryFile(dir=job, prefix="copy-", delete=False) as stream:
                    temporary = Path(stream.name)
                try:
                    with source.open("rb") as src, temporary.open("wb") as dst:
                        copied = 0
                        digest = hashlib.sha256()
                        for block in iter(lambda: src.read(1024 * 1024), b""):
                            check_cancel()
                            copied += len(block)
                            if copied > f["size"]:
                                raise SyncError("source_changed", f"File grew while being copied: {f['path']}")
                            dst.write(block)
                            digest.update(block)
                    if temporary.stat().st_size != f["size"] or digest.hexdigest() != f["sha256"]:
                        raise SyncError("source_mismatch", f"Reconstructed file differs from the host: {f['path']}")
                    temporary.replace(target)
                finally:
                    temporary.unlink(missing_ok=True)
                if index == 1 or index % 25 == 0 or index == report['files']:
                    event("staging", file=f["path"], completed=index, total=report["files"])
            event("verifying")
            differences = verify(recipe, stage, check_cancel)
            if differences:
                raise SyncError("verification_failed", f"Staged installation differs: {differences[:8]}")
            destination.parent.mkdir(parents=True, exist_ok=True)
            stage.rename(destination)
            atomic_json(root / "receipts" / (recipe.setup_id + ".json"),
                        {"schema": 1, "setup_id": recipe.setup_id, "manifest": recipe.data,
                         "directory": str(destination), "state": "files_verified"})
            event("files_verified", directory=str(destination), reused=False)
            return destination
        except BaseException as exc:
            code = exc.code if isinstance(exc, SyncError) else "cancelled" if isinstance(exc, KeyboardInterrupt) else "install_failed"
            event("cancelled" if code == "cancelled" else "failed", reason=code, message=str(exc))
            raise
