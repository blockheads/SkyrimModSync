# Local validation — 2026-10-13

The table below records the earlier fixture and packaging checks. For the subsequent
real SME download, reconstruction, main-menu launch, and unresolved in-game memory
failure, see [the current SME validation](sme-validation.md). The latest automated
suite passes 72 tests; the packaged binary was rebuilt with the CLI-first entry point
and smaller frontend. Native Sync driving the packaged CLI and packaged `share --raw`
passed against cached synthetic files. Real gameplay remains unresolved.

This is a working prototype. The product's two-machine, ready-to-play acceptance
test remains outstanding.

| Check | Result | Scope |
| --- | --- | --- |
| `.venv/bin/pytest -q` | 54 passed | Linux, Python 3.10.12; synthetic files/processes |
| Native Qt Install button | Passed | Actual CLI worker, isolated fixture and fresh verification |
| Interrupted download/copy | Passed | Retained verified CDN parts/staged files, no published partial installation |
| Cancelled backend | Passed | Real POSIX fixture subprocess terminated; no real installer invoked |
| PyInstaller desktop build | Passed | Complete Linux folder under `dist/SkyrimModSync`, approximately 169 MiB |
| Packaged CLI install and verify | Passed | Seven-file synthetic fixture; differences empty |
| Packaged CLI real recipe inspection | Passed | Legends of the Frost 3.3.4, 616 archives, 168,860 directives |
| Public CDN acquisition | Passed | Every part and whole-file xxHash64 checked; SHA-256 recorded separately |
| Pinned native engine | Passed for bootstrap/version/help | Real 0.5.10 archive hash checked; native executable runs, no mod install/account use |
| Packaged automatic engine bootstrap | Passed over public HTTPS | Actual download, pinned hash, bounded extraction and published engine |
| Automatic engine adapter | Passed with fixture | No explicit engine path; exact published release arguments, synthetic reconstruction |
| Local optional frontend protocol | Passed | Read-only setup status, fresh hash evidence, receipt identity checks |
| Separate chat CLI to packaged launcher | Passed | Different virtual environment/process; fixture verified, changed and missing states |
| Linux launch adapter | Passed with fixtures | Private configuration/prefix and command/environment checks; no game boot |
| Desktop file-association installer | Passed in temporary data home | No changes to the user's actual associations |
| Qt X11 platform library dependencies | Resolved on this host | Other distributions are not validated |
| Windows CI matrix | Configured, not run | No remote repository or CI execution |

The demo under `build/demo` contains fake Skyrim executable bytes and a fake recipe.
It cannot run a game. Packaged smoke-test results are in `build/packaged-*.jsonl`.
The downloaded real recipe and its inspection are in `build/research`; see
[the public recipe probe](public-list-probe.md).

Not validated: a real native reconstruction operation, account/manual-download flows,
real mod acquisition, host effective-view parity, real SKSE/Proton boot, Steam
invites, Windows isolated launch, or a live multiplayer session. The prototype
downloads a pinned Linux engine instead of bundling it in the GUI distribution.
The expected-byte check is implemented; it is
not evidence that these untested integrations work.

Runtime-created extra files currently cause strict verification to fail. Support for
a pack needs a validated runtime-file policy and effective-view comparison before
this build can promise routine one-click play. See [the acceptance criteria](design.md).
