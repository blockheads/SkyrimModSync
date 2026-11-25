import json
import subprocess
import sys

import pytest

from skyrim_mod_sync.bridge import capabilities, status
from skyrim_mod_sync.installer import install
from skyrim_mod_sync.paths import SyncError


def test_status_is_optional_readonly_and_distinguishes_fresh_checks(setup):
    root, identity = setup["root"], setup["recipe"].setup_id
    assert capabilities()["operations"] == ["status"]
    assert not capabilities()["requires_chat"]
    assert status(root, identity)["state"] == "missing"
    assert not root.exists()
    directory = install(setup["recipe"], base=setup["base"], root=root,
                        reconstructed=setup["installed"], accepted=True)
    assert status(root, identity)["state"] == "installed_unchecked"
    report = status(root, identity, fresh=True)
    assert report["state"] == "files_verified"
    assert report["verified_now"]
    assert "directory" not in report
    (directory / "game/unexpected.dll").write_bytes(b"unexpected")
    report = status(root, identity, fresh=True)
    assert report["state"] == "different"
    assert {"kind": "extra", "path": "game/unexpected.dll"} in report["differences"]


def test_status_uses_owned_paths_and_validates_receipt_identity(setup):
    identity, root = setup["recipe"].setup_id, setup["root"]
    install(setup["recipe"], base=setup["base"], root=root,
            reconstructed=setup["installed"], accepted=True)
    path = root / "receipts" / (identity + ".json")
    data = json.loads(path.read_text())
    data["directory"] = "../unrelated"
    path.write_text(json.dumps(data))
    assert status(root, identity, fresh=True)["state"] == "files_verified"
    data["setup_id"] = "a" * 64
    path.write_text(json.dumps(data))
    with pytest.raises(SyncError, match="receipt"):
        status(root, identity)


def test_status_cli_validates_identifiers_without_writing(setup):
    result = subprocess.run([sys.executable, "-m", "skyrim_mod_sync", "status",
                             "--setup-id", "../../escape", "--root", str(setup["root"])],
                            capture_output=True, text=True)
    assert result.returncode == 2
    assert json.loads(result.stdout)["reason"] == "invalid_setup_id"
    assert not setup["root"].exists()
