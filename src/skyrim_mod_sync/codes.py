"""Portable pinned recipe/profile codes; no service, account or executable in a code."""
from __future__ import annotations

import base64
import hashlib
import json
import re
import zlib
from urllib.parse import urlsplit

from .paths import SyncError, relative_path
from .recipe import MAX_RECIPE, canonical

SME = {
    "schema": 1, "name": "Skyrim Modding Essentials 2.7.1", "profile": "Skyrim Modding Essentials",
    "url": "https://authored-files.wabbajack.org/Skyrim%20Modding%20Essentials.wabbajack_7c422547-564e-47ad-b165-59be964a3abc",
    "sha256": "7dda0f7211df4184db1e2069052787d3dc3741084e1ea486cb54a3558e492216",
    "size": 30404713, "layout": "stock-mo2", "setup_id": None,
}


def validate(data):
    required = {"schema", "name", "profile", "url", "sha256", "size", "layout", "setup_id"}
    if not isinstance(data, dict) or set(data) != required or type(data["schema"]) is not int or data["schema"] != 1:
        raise SyncError("invalid_code", "Unsupported setup code.")
    if any(not isinstance(data[k], str) or not 1 <= len(data[k]) <= 200 for k in ("name", "profile")):
        raise SyncError("invalid_code", "Invalid setup name/profile.")
    relative_path(data["profile"])
    if "/" in data["profile"] or data["layout"] != "stock-mo2":
        raise SyncError("invalid_code", "This code version supports a selected stock-game MO2 profile.")
    if not isinstance(data["url"], str) or len(data["url"]) > 4096:
        raise SyncError("invalid_code", "Invalid recipe URL.")
    parts = urlsplit(data["url"])
    if parts.scheme != "https" or parts.hostname != "authored-files.wabbajack.org" or parts.query or parts.fragment or parts.username or ' ' in data["url"]:
        raise SyncError("invalid_code", "Setup codes must reference a public Wabbajack recipe.")
    if not isinstance(data["sha256"], str) or not re.fullmatch('[0-9a-f]{64}', data["sha256"]):
        raise SyncError("invalid_code", "The recipe must be pinned to SHA-256.")
    if type(data["size"]) is not int or not 0 < data["size"] <= MAX_RECIPE:
        raise SyncError("invalid_code", "Invalid recipe size.")
    if data["setup_id"] is not None and (not isinstance(data["setup_id"], str) or not re.fullmatch('[0-9a-f]{64}', data["setup_id"])):
        raise SyncError("invalid_code", "Invalid host setup identity.")
    return data


def encode(data):
    validate(data)
    return 'SMS1-' + base64.urlsafe_b64encode(zlib.compress(canonical(data), 9)).decode().rstrip('=')


def decode(code):
    if code.strip().upper() in {'SME', 'SME-2.7.1', 'SME-271'}:
        return dict(SME)
    code = code.strip()
    if not code.startswith('SMS1-') or len(code) > 32768:
        raise SyncError("invalid_code", "Enter SME-2.7.1 or a shared SMS1 setup code.")
    try:
        value = code[5:]
        compressed = base64.b64decode(value + '=' * (-len(value) % 4), altchars=b'-_', validate=True)
        stream = zlib.decompressobj()
        raw = stream.decompress(compressed, 16385)
        if len(raw) > 16384 or not stream.eof or stream.unused_data or stream.unconsumed_tail:
            raise ValueError('Invalid compressed code')
        return validate(json.loads(raw))
    except (ValueError, TypeError, zlib.error):
        raise SyncError("invalid_code", "This setup code is damaged or unsupported.") from None


def job_id(data):
    return hashlib.sha256(canonical({k: data[k] for k in ('schema', 'profile', 'sha256', 'layout')})).hexdigest()
