from __future__ import annotations

import hashlib
import os
import re
from pathlib import Path, PurePosixPath


class SyncError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def relative_path(value: str) -> str:
    if not isinstance(value, str) or not value or len(value) > 2048:
        raise SyncError("invalid_path", "A recipe contains an invalid path.")
    pieces = value.split("/")
    reserved = {"con", "prn", "aux", "nul", *(f"com{i}" for i in range(1, 10)),
                *(f"lpt{i}" for i in range(1, 10))}
    if any(not p or p in {".", ".."} or p.endswith((".", " "))
           or re.search(r'[\\:<>"|?*\x00-\x1f]', p)
           or p.split(".")[0].casefold() in reserved for p in pieces):
        raise SyncError("invalid_path", f"Unsafe Windows-relative path: {value!r}")
    if PurePosixPath(value).is_absolute():
        raise SyncError("invalid_path", f"Absolute path is not allowed: {value!r}")
    return value


def under(root: Path, value: str) -> Path:
    relative_path(value)
    root = root.resolve()
    target = root.joinpath(*value.split("/"))
    if not target.resolve().is_relative_to(root):
        raise SyncError("path_escape", f"Path escapes the selected directory: {value}")
    return target


def locate(root: Path, value: str) -> Path:
    """Resolve Windows spelling on Linux without accepting ambiguous trees."""
    relative_path(value)
    base = root.resolve()
    target = base
    for part in value.split("/"):
        if not target.is_dir():
            raise SyncError("missing_file", f"Missing file: {value}")
        matches = [p for p in target.iterdir() if p.name.casefold() == part.casefold()]
        if len(matches) != 1:
            code = "case_collision" if matches else "missing_file"
            raise SyncError(code, f"Cannot resolve an unambiguous file: {value}")
        target = matches[0]
        if target.is_symlink() or not target.resolve().is_relative_to(base):
            raise SyncError("path_escape", f"Symbolic links are not accepted: {value}")
    if not target.is_file():
        raise SyncError("missing_file", f"Missing file: {value}")
    return target


def sha256(path: Path, check_cancel=None) -> str:
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            if check_cancel:
                check_cancel()
            result.update(block)
    return result.hexdigest()


def inventory(root: Path):
    root = root.resolve()
    if not root.is_dir():
        raise SyncError("missing_directory", f"Directory does not exist: {root}")
    names: dict[str, str] = {}
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in sorted(dirs + files):
            path = Path(directory) / name
            rel = path.relative_to(root).as_posix()
            relative_path(rel)
            if path.is_symlink():
                raise SyncError("path_escape", f"Symbolic links are not accepted: {rel}")
            key = rel.casefold()
            if key in names:
                raise SyncError("case_collision", f"Ambiguous paths: {names[key]} and {rel}")
            names[key] = rel
        for name in sorted(files):
            path = Path(directory) / name
            if not path.is_file():
                raise SyncError("invalid_file", f"Not an ordinary file: {path}")
            yield path.relative_to(root).as_posix(), path
