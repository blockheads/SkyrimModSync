from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

from .exporter import export_setup
from .installer import install, plan, verify
from .paths import SyncError
from .recipe import load
from .wabbajack import inspect_list, inspect_package_sources


def default_storage():
    from .settings import default_storage as storage
    return storage()


def emit(data):
    print(json.dumps(data, ensure_ascii=False), flush=True)


def parser():
    result = argparse.ArgumentParser(description="Share and verify isolated Skyrim setups.")
    sub = result.add_subparsers(dest="command", required=True)
    gui = sub.add_parser("gui", help="Open the native desktop application")
    gui.add_argument("package", type=Path, nargs="?")
    for name, alias in (('sync', 'sync-code'), ('play', 'play-code')):
        operation = sub.add_parser(name, aliases=[alias],
                                   help='Download and prepare a pinned setup' if name == 'sync' else 'Verify and launch a synced setup')
        operation.add_argument('code')
        operation.add_argument('--root', type=Path, default=default_storage())
        if name == 'sync':
            operation.add_argument('--accept', action='store_true')
    share = sub.add_parser('share', help='Verify a synced target and print its exact setup code')
    share.add_argument('code')
    share.add_argument('--root', type=Path, default=default_storage())
    share.add_argument('--raw', action='store_true', help='Print just the code for copying or piping')
    sub.add_parser('discover', help='Detect the local Steam game and launch tools')
    login = sub.add_parser('nexus-login', help='Save a personal API key through a masked prompt')
    login.add_argument('--stdin', action='store_true', help='Read a key from a private local pipe')
    sub.add_parser('nexus-status', help='Validate the locally supplied Nexus key')
    sub.add_parser("capabilities", help="Describe the versioned local frontend protocol")
    status = sub.add_parser("status", help="Inspect a managed setup by identity without its original share file")
    status.add_argument("--setup-id", required=True)
    status.add_argument("--root", type=Path, default=default_storage())
    status.add_argument("--verify", action="store_true", help="Freshly hash files; can take time for a large setup")
    bootstrap = sub.add_parser("bootstrap-engine", help="Obtain the pinned Linux installer engine")
    bootstrap.add_argument("--root", type=Path, default=default_storage())
    wj = sub.add_parser("inspect-list", help="Read a Wabbajack list without installing it")
    wj.add_argument("artifact", type=Path)
    download = sub.add_parser("download-list", help="Download a public chunked Wabbajack recipe")
    download.add_argument("url")
    download.add_argument("--output", type=Path, required=True)
    download.add_argument("--cache", type=Path, default=default_storage() / "recipe-downloads")
    export = sub.add_parser("export", help="Share an installed Wabbajack setup")
    for flag in ("base", "installed", "artifact", "output"):
        export.add_argument("--" + flag, type=Path, required=True)
    export.add_argument("--name", required=True)
    export.add_argument("--profile")
    export.add_argument("--recipe-url", help="Use a pinned public CDN recipe to produce a small shareable file")
    export.add_argument("--layout", choices=("mo2", "stock-mo2", "native"), default="mo2")
    export.add_argument("--standard-root-builder", action="store_true",
                        help="Explicitly materialize enabled mod/root files into the game root; custom Root Builder rules are unsupported")
    for name in ("inspect", "plan", "install", "verify", "launch", "cancel"):
        command = sub.add_parser(name)
        command.add_argument("package", type=Path)
        if name in {"plan", "install"}:
            command.add_argument("--base", type=Path, required=True)
            command.add_argument("--root", type=Path, default=default_storage())
        if name == "install":
            command.add_argument("--engine", type=Path)
            command.add_argument("--engine-kind", choices=("wabbajack", "jackify"), default="wabbajack",
                                 help="Adapter for an explicit engine; automatic setup selects jackify")
            command.add_argument("--reconstructed", type=Path,
                                 help="Use a local list already reconstructed by another installer")
            command.add_argument("--accept", action="store_true",
                                 help="Approve the reviewed recipe and its native code")
        if name == "verify":
            command.add_argument("directory", type=Path)
        if name == "launch":
            command.add_argument("--root", type=Path, default=default_storage())
            for flag in ("proton", "steam", "runtime"):
                command.add_argument("--" + flag, type=Path, required=True)
        if name == "cancel":
            command.add_argument("--root", type=Path, default=default_storage())
    return result


def summary(recipe):
    return {"name": recipe.data["name"], "setup_id": recipe.setup_id,
            "files": len(recipe.data["files"]), "installed_bytes": sum(f["size"] for f in recipe.data["files"]),
            "native_files": [f["path"] for f in recipe.data["files"]
                             if Path(f["path"]).suffix.casefold() in {".dll", ".exe", ".bat", ".cmd", ".ps1"}],
            "recipe_bytes": recipe.data["artifact"]["size"]}


def main(argv=None):
    args = parser().parse_args(argv)
    try:
        if args.command == 'share':
            from .workflow import share_code
            code = share_code(args.code, args.root)
            if args.raw:
                print(code)
            else:
                emit({'state': 'files_verified', 'code': code})
            return 0
        if args.command in {'sync', 'play', 'sync-code', 'play-code'}:
            from .workflow import sync_code, play_code
            if args.command in {'sync', 'sync-code'}:
                sync_code(args.code, args.root, args.accept, emit)
            else:
                play_code(args.code, args.root, emit)
            return 0
        if args.command == 'discover':
            from .discovery import discover
            emit({k: str(v) if v is not None else None for k, v in discover().items()})
            return 0
        if args.command in {'nexus-login', 'nexus-status'}:
            from .auth import load_key, save_key, validate_key
            if args.command == 'nexus-login':
                import getpass
                key = sys.stdin.read(4097).strip() if args.stdin else getpass.getpass('Nexus personal API key (hidden): ')
                report = validate_key(key)
                save_key(key)
            else:
                report = validate_key(load_key(required=True))
            emit(report)
            return 0
        if args.command in {"capabilities", "status"}:
            from .bridge import capabilities, status
            emit(capabilities() if args.command == "capabilities" else status(args.root, args.setup_id, args.verify))
            return 0
        if args.command == "gui":
            from .gui import main as gui_main
            return gui_main([str(args.package)] if args.package else [])
        if args.command == "bootstrap-engine":
            from .bootstrap import ensure_engine
            engine = ensure_engine(args.root.resolve(), emit)
            emit({"state": "engine_ready", "path": str(engine), "engine_kind": "jackify"})
            return 0
        if args.command == "inspect-list":
            emit(inspect_list(args.artifact))
            return 0
        if args.command == "download-list":
            from .download import download_list
            args.output.parent.mkdir(parents=True, exist_ok=True)
            download_list(args.url, args.output, args.cache, emit)
            return 0
        if args.command == "export":
            # Confirm the payload is a list before spending time hashing a game tree.
            inspect_list(args.artifact)
            output = export_setup(base=args.base, installed=args.installed, artifact=args.artifact,
                                  output=args.output, name=args.name, profile=args.profile,
                                  layout=args.layout, standard_root_builder=args.standard_root_builder,
                                  recipe_url=args.recipe_url, emit=emit)
            emit({"state": "exported", "package": str(output)})
            return 0
        recipe = load(args.package)
        if args.command == "inspect":
            emit({**summary(recipe), "sources": recipe.data.get("sources", []), "artifact": recipe.data["artifact"]})
        elif args.command == "plan":
            report = plan(recipe, args.base, args.root)
            emit(report)
            return 2 if report["blockers"] else 0
        elif args.command == "install":
            install(recipe, base=args.base, root=args.root, engine=args.engine,
                    reconstructed=args.reconstructed, accepted=args.accept, emit=emit, engine_kind=args.engine_kind)
        elif args.command == "verify":
            differences = verify(recipe, args.directory)
            emit({"setup_id": recipe.setup_id, "state": "different" if differences else "files_verified",
                  "differences": differences})
            return 2 if differences else 0
        elif args.command == "launch":
            from .launch import launch
            launch(recipe, root=args.root, proton=args.proton, steam=args.steam,
                   runtime=args.runtime, emit=emit)
        elif args.command == "cancel":
            request = args.root.resolve() / "jobs" / recipe.job_id / "cancel"
            request.parent.mkdir(parents=True, exist_ok=True)
            request.write_text("cancel\n", encoding="utf-8")
            emit({"state": "cancel_requested", "setup_id": recipe.setup_id})
        return 0
    except SyncError as exc:
        emit({"state": "failed", "reason": exc.code, "message": str(exc)})
        print(str(exc), file=sys.stderr)
        return 130 if exc.code == "cancelled" else 2
    except (OSError, ValueError) as exc:
        emit({"state": "failed", "reason": "io_failed", "message": str(exc)})
        print(str(exc), file=sys.stderr)
        return 3
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
