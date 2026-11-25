"""Small local process protocol for optional frontends; no social dependencies."""
from __future__ import annotations

import json
import re
from pathlib import Path

from .installer import verify
from .paths import SyncError
from .recipe import MAX_MANIFEST, Recipe, validate

PROTOCOL = "skyrim-coop-launcher"
VERSION = 1


def capabilities():
    return {"protocol": PROTOCOL, "version": VERSION, "operations": ["status"],
            "planned_operations": ["prepare", "play", "join"],
            "requires_chat": False, "requires_chat_account": False}


def status(root: Path, setup_id: str, fresh=False):
    if not isinstance(setup_id, str) or not re.fullmatch(r"[0-9a-f]{64}", setup_id):
        raise SyncError("invalid_setup_id", "Expected a lowercase SHA-256 setup identity.")
    root = root.resolve()
    directory = root / "installs" / setup_id
    receipt = root / "receipts" / (setup_id + ".json")
    response = {"protocol": PROTOCOL, "version": VERSION, "setup_id": setup_id,
                "state": "missing", "verified_now": False, "differences": []}
    if directory.is_symlink() or receipt.is_symlink():
        raise SyncError("unsafe_destination", "Managed installations and receipts cannot be symbolic links.")
    if not directory.exists():
        return response
    if not receipt.is_file():
        return {**response, "state": "untracked"}
    try:
        if receipt.stat().st_size > 2 * MAX_MANIFEST:
            raise ValueError("Oversized receipt")
        data = json.loads(receipt.read_text(encoding="utf-8"))
        recipe = Recipe(validate(data["manifest"]), Path("unused-local-status-package"))
        if data["setup_id"] != setup_id or recipe.setup_id != setup_id:
            raise ValueError("Receipt identity mismatch")
    except (ValueError, KeyError, TypeError, SyncError) as exc:
        raise SyncError("invalid_receipt", "The managed installation receipt is invalid.") from exc
    response.update({"state": "installed_unchecked", "name": recipe.data["name"],
                     "files": len(recipe.data["files"])})
    if fresh:
        differences = verify(recipe, directory)
        response.update({"state": "different" if differences else "files_verified",
                         "verified_now": True, "differences": differences})
    # No local paths, Steam IDs, account details or game-host connection decisions.
    return response
