# Skyrim Mod Sync

A CLI for reproducing an exact Skyrim setup, with a small optional native frontend.
A friend shares a code; you sync that target into its own directory and launch it.
The GUI calls the same CLI and reads its JSON progress. Qt is optional for CLI users.

The first supported workflow uses a pinned Wabbajack recipe and the native Jackify
engine for acquisition. The resulting game runs through SKSE/Proton without MO2.
SkyrimCoop remains independent and owns multiplayer compatibility checks. Chat is deferred.

**Current status:** real SME 2.7.1 archives were downloaded, reconstructed and verified.
The managed installation reached the main menu. An in-game load exhausted memory;
reliable gameplay remains unresolved. See [the measured validation](docs/sme-validation.md).
The desktop distribution has been rebuilt with the CLI-first entry point and small frontend.

## CLI first

```sh
python3 -m venv .venv
.venv/bin/pip install -e .
.venv/bin/skyrim-mod-sync nexus-login
.venv/bin/skyrim-mod-sync sync SME-2.7.1 --root '/path/to/storage' --accept
.venv/bin/skyrim-mod-sync share SME-2.7.1 --root '/path/to/storage' --raw
.venv/bin/skyrim-mod-sync play SME-2.7.1 --root '/path/to/storage'
```

`nexus-login` reads the personal API key through a masked prompt. The tested automatic
acquisition path requires Nexus Premium. Keys stay in the user's private configuration;
they are never included in shared codes or command arguments. `--stdin` supports a
private local input pipe for frontends.

`sync` downloads the pinned recipe, checks the licensed base game, obtains the pinned
engine, reconstructs the list, materializes the effective mod/configuration files,
and verifies them before publication. `--accept` approves installation of that target.

`share --raw` freshly verifies the installation and prints its exact setup code.
Pass that code to a friend, who runs the same `sync` command with it. The code includes
the recipe identity, profile and final fingerprint. It needs no hosted lookup service.
`SME-2.7.1` is a built-in starting alias; use the generated code to bind a friend's setup
to your exact result. Codes are opaque strings rather than short hosted invitation codes.

`play` verifies the selected installation again and launches SKSE in its own Proton
prefix. Steam must be running; Proton 10 and Steam Linux Runtime sniper must be installed.
The private prefix maps the game to G: to avoid long Windows paths.

Commands normally emit JSON objects, one per line. `share --raw` prints only the code.
Exit 0 means the operation succeeded, 2 means a setup was refused, 3 means an I/O
failure, and 130 means cancellation. `files_verified` proves file contents;
`launch_started` reports process creation, not successful gameplay.
The previous `sync-code` and `play-code` names remain accepted aliases.

## Small optional frontend

```sh
.venv/bin/pip install -e '.[gui]'
.venv/bin/skyrim-mod-sync gui
```

![Actual native frontend](docs/images/frontend.png)

The main window has a setup code, Sync, Play, Copy code and Cancel. Options contains
storage, Nexus connection and opening a setup file. Details shows CLI progress events.
Game and launch-tool discovery happens in the CLI. Export/import tooling and engine
overrides stay available through CLI commands instead of filling the main window.
Saved exact codes can be reopened; Play still checks the files afresh. Editing the
code or storage clears readiness.

## Other CLI operations

`discover` reports the detected Steam game and tools. `inspect-list` reads a Wabbajack
recipe without executing it. `download-list` acquires public Wabbajack CDN recipes with
verified, reusable parts. `bootstrap-engine` obtains the pinned Linux installer.
`inspect`, `plan`, `install`, `verify`, `status` and `cancel` operate on setup packages
or managed installations. Use `--help` on each command for its parameters.

An existing list can be exported to a `.skyrimsync` package:

```sh
skyrim-mod-sync export \
  --base '/path/to/licensed/game' \
  --installed '/path/to/reconstructed/list' \
  --artifact '/path/to/list.wabbajack' \
  --profile 'Chosen profile' \
  --name 'Our setup' \
  --output 'Our-setup.skyrimsync'
```

Use `--layout stock-mo2` for lists with a reconstructed Stock Game directory;
`--layout native` imports an already materialized game folder. Standard Root Builder
root files require `--standard-root-builder`. `--recipe-url` creates a small URL-backed
package instead of embedding the recipe. Unsupported custom deployment rules,
untracked overwrite output and unresolved INI tweaks are refused. Custom files need
acquisition instructions in the original recipe to reproduce successfully.

## Isolation and recovery

Managed storage contains archive downloads, reconstruction jobs, verified installations,
receipts and private Proton prefixes. Each installation is identified by final paths,
sizes and SHA-256 hashes. Sources are copied, not hard-linked. The automatic code
workflow isolates engine configuration from personal Jackify configuration and exposes
the licensed game read-only during reconstruction.

Interrupted work retains verified downloads and staged files. A changed published
installation is refused rather than silently overwritten. Known plugin logs are
modeled explicitly; plugin settings and extra executables remain verified content.
Choose a storage volume with enough room for downloads, reconstruction and the final
installation at once. The default is the user's application data directory;
`--root` chooses another volume.

Current limits: Linux x86-64, the tested SME recipe/profile conventions, Nexus Premium
for unattended Nexus downloads, and new isolated installs. General updates to existing
modded game folders, manual-download orchestration, arbitrary-list compatibility,
Windows launch and live multiplayer acceptance remain outstanding.

## Tests and packaging

```sh
.venv/bin/pip install -e '.[gui,dev]'
.venv/bin/pytest -q
```

The suite checks small synthetic modlists, failure/recovery behavior, exact-byte
verification, path validation, source isolation, acquisition adapters and the GUI's
actual CLI worker. It does not download a real list or launch Skyrim during pytest.
See [what the tests cover](docs/tests.md) and the separate real SME validation record.
The [environment audit](docs/environment-audit.md) records 13 passing layout/probe
cases and five explicit expected failures; environmental portability remains unfinished.

To create the combined CLI/frontend distribution:

```sh
.venv/bin/pip install -e '.[gui,bundle]'
.venv/bin/pyinstaller --noconfirm --clean --onedir \
  --name SkyrimModSync --paths src app.py
./dist/SkyrimModSync/SkyrimModSync --help
./dist/SkyrimModSync/SkyrimModSync gui
```

The packaged executable defaults to the CLI. The optional desktop association invokes
its `gui` command; `tools/install_desktop.py` installs that association per user.
