"""Private, local Nexus credentials. Never part of a setup descriptor or CLI argument."""
from __future__ import annotations

import json
import os
import stat
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from .paths import SyncError


def credential_path():
    from .settings import config_directory
    return config_directory() / 'nexus-api-key'


def save_key(value: str, path: Path | None = None):
    value = value.strip()
    if not 10 <= len(value) <= 4096 or any(ord(c) < 33 or ord(c) > 126 for c in value):
        raise SyncError("nexus_key_invalid", "Enter the personal API key from Nexus API Access.")
    path = path or credential_path()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    if os.name != "nt":
        os.fchmod(fd, 0o600)
    with os.fdopen(fd, "w", encoding="ascii") as stream:
        stream.write(value + "\n")


def load_key(required=False, path: Path | None = None):
    path = path or credential_path()
    if not path.exists():
        if required:
            raise SyncError("nexus_login_required", "Connect your Nexus account before installing this list.")
        return None
    if path.is_symlink() or path.stat().st_size > 4096:
        raise SyncError("nexus_key_invalid", "The local Nexus credential file is invalid.")
    if os.name != "nt" and stat.S_IMODE(path.stat().st_mode) & 0o077:
        raise SyncError("nexus_key_permissions", "The Nexus credential file must be readable only by your user (mode 600).")
    return path.read_text(encoding="ascii").strip()


def validate_key(key: str):
    request = Request("https://api.nexusmods.com/v1/users/validate.json", headers={
        "apikey": key, "Application-Name": "SkyrimModSync", "Application-Version": "0.1.0",
        "User-Agent": "SkyrimModSync/0.1.0 development-test"})
    try:
        with urlopen(request, timeout=30) as response:
            data = json.loads(response.read(65536))
    except HTTPError as exc:
        raise SyncError("nexus_key_rejected", f"Nexus rejected the API key (HTTP {exc.code}).") from None
    except (URLError, OSError, ValueError):
        raise SyncError("nexus_unavailable", "Could not validate the Nexus account; retry when online.") from None
    if not isinstance(data, dict) or type(data.get("is_premium")) is not bool:
        raise SyncError("nexus_unavailable", "Nexus returned an invalid account response.")
    return {"state": "nexus_authenticated", "premium": data["is_premium"]}
