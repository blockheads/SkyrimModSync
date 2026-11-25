# SkyrimModSync

A Rust CLI for comparing deployed Skyrim instances and reproducing a friend's exact
files and shared settings. It installs into a separate, managed co-op folder. The
licensed base game and reusable download cache stay separate.

**The core is Rust.** No Python interpreter or mod-manager process is
needed to run it. Wabbajack recipes supply acquisition instructions; the pinned
Jackify engine reconstructs them on Linux. Python remains in this repository as the
previous prototype and test reference. Its GUI and SMS1 codes belong to that prototype.

The CLI imports an editable setup, freezes signed revisions, shares a reusable code,
repairs only changed files, and provides isolated Linux play. It does not connect
multiplayer sessions. `files_match` means fresh file hashes, shared configuration
and declared plugin order match. `runtime_verified: false` remains explicit: file
equality alone does not prove the game actually loaded every plugin or played successfully.

Real SME/co-op file synchronization and same-code updates pass. A minimal SKSE
setup loads Riverwood and saves; the full SME setup still exceeds the 4 GiB startup
cap. Full modpack gameplay remains a release blocker. See the
[validation record](docs/rust-sync-validation.md) for the tested boundary.

## Build

```sh
cargo build --release --locked
./target/release/skyrim-mod-sync --help
```

Rust 1.91 (pinned by `rust-toolchain.toml`). Linux x86-64 is the supported acquisition/repair target. Capture,
comparison and package inspection are portable code. Windows CI is configured but
has not been run remotely for this revision. Windows acquisition, Mac support,
Flatpak integration and a new frontend remain deferred.

## Editable host workflow (v3)

Keep editing your original MO2/Wabbajack/Jackify installation. Import reads it;
SkyrimModSync owns a separate co-op installation and persistent personal state.
A recipe is optional, but supplying the original `.wabbajack` reuses exact archive
and reconstruction instructions. Files no longer matching those instructions are
frozen as peer objects. Friends need neither MO2 nor a new Wabbajack list.

```sh
skyrim-mod-sync import --mo2 '/lists/SME' --profile 'Skyrim Modding Essentials' \
  --recipe '/lists/SME.wabbajack' --base '/steam/Skyrim Special Edition' \
  --coop '/games/our-coop' --downloads '/archives/skyrim'
# For a Stock Game elsewhere, add --game '/lists/SME/Stock Game'.
# Or import a deployed setup: --game PATH --documents PATH --local PATH.
skyrim-mod-sync play --coop '/games/our-coop'
skyrim-mod-sync status --coop '/games/our-coop'
skyrim-mod-sync publish --coop '/games/our-coop' --output our-coop.skyrimsync
skyrim-mod-sync serve --coop '/games/our-coop'
```

`publish` prints an `SMS3.…` code containing the publisher identity, pack namespace
and transport identity. Send that same code to friends and keep `serve` running.
It is an opaque self-contained code, not a central service's short lookup number.
Each explicit sync pins the latest signed head once; a running game never switches
revision. Publishing changed authoring files advances the same channel. A portable
`.skyrimsync` revision file also pins an immutable revision, but local/modified peer
files still require that publisher online unless already cached.

```sh
# Friend: missing choices are prompted at a terminal, or supply these flags.
skyrim-mod-sync sync 'SMS3.…' --base '/their/licensed/Skyrim' \
  --coop '/their/co-op' --downloads '/their/mod-archives'
skyrim-mod-sync play --coop '/their/co-op'
# After the host publishes an update:
skyrim-mod-sync sync 'SMS3.…' --coop '/their/co-op'
skyrim-mod-sync rollback --coop '/their/co-op'
```

Credentials stay on the downloading machine. Use `nexus-login` if required recipe
archives are absent; cached verified archives need no key. Base game content must
come from each friend's licensed installation or pinned game-source instructions.
Mod byte equality in someone's personal Steam folder does not make it base-game
content. Private keys, saves and credentials are not captured as mod files. Import resolves
enabled mods; a supplied recipe is shared as provided, including its original templates
and acquisition instructions.

For a local transport test, host `serve --no-relay --bind 127.0.0.1:0` reports its
address; friend `sync CODE --peer-address 127.0.0.1:PORT` uses it directly. Normal
serving uses Iroh's relay/discovery infrastructure; `--relay URL` selects a relay.
Local tests do not establish WAN or NAT compatibility. Verified partial blob
transfers persist for retry. If the host is offline, keep the server running and retry.
Use `sync CODE --revision SHA256` to intentionally select published history; otherwise
older or conflicting signed heads are refused.

## Isolated play and runtime review

Linux play requires native Steam, Proton, Steam Linux Runtime, bubblewrap, usable
user/mount namespaces, OverlayFS, and a systemd user session. `doctor` reports tool
and kernel configuration; actual mounting is checked at launch. Select ambiguous
paths explicitly with `play --steam PATH --proton PATH --runtime PATH`.
Windows/Mac play and Flatpak integration are not supported in this milestone.

Installed game files form a read-only lower layer. Each revision has its own writable
runtime layer; each co-op instance keeps its own prefix, saves and personal settings.
The normal Saves directory maps to `personal/saves`; profile-specific relative
save directories (such as MO2’s `__MO_Saves`) persist inside the private prefix.
Proton and Steam runtime lock/cache writes also use private layers. Their installed
programs stay untouched; the selected Steam SDK and IPC support the native bridge.
The effective mounted view is verified before launch and checked after exit. Unknown
runtime overrides block the next launch until reviewed, rather than being erased.
Saves/logs remain local. Display and Controls sections of SkyrimPrefs.ini remain
personal; other shared settings are checked. A default 4 GiB process-tree memory cap
and zero swap protect the machine. `--timeout-seconds 120` bounds experiments.
Memory metrics distinguish anonymous/shared memory from total cgroup charges,
which also include reclaimable file cache. Launch logs and measured memory are under `COOP/runtime/REVISION/`.

`status` reports the runtime revision separately from the installed revision, since
older layers remain available after updates. After playtesting, `publish` asks which
shared runtime changes to promote. For scripts:

```sh
skyrim-mod-sync --json status --coop '/games/our-coop'
skyrim-mod-sync --json publish --coop '/games/our-coop' --output updated.skyrimsync \
  --include-change 'game/Data/meshes/example.nif'
# Explicitly leave unselected runtime content local:
skyrim-mod-sync publish --coop '/games/our-coop' --output updated.skyrimsync --keep-runtime
# Resolve a source/runtime conflict in favor of authoring content:
skyrim-mod-sync publish --coop '/games/our-coop' --output updated.skyrimsync \
  --use-source 'game/Data/meshes/example.nif'
```

Promoted edits and deletions persist across later draft refreshes. If the original
source changes too, publication stops with the conflicting path. Choose the source
explicitly or reconcile the edited file in the authoring profile. Revisions retain
source bindings privately; manifests contain portable logical paths and content pins.
The CLI is the backend API (`--json`); no frontend is required or implemented here.

## Compare two existing installations

Use the actual Documents and AppData/local directories used by each instance or Wine
prefix. For `capture`, game files must be physically deployed. Use `import --mo2` below
to resolve an editable mod-manager profile into its effective physical installation.

```sh
skyrim-mod-sync capture --game '/games/host/game' \
  --documents '/games/host/config/documents' \
  --local '/games/host/config/local' --output host.json

skyrim-mod-sync capture --game '/games/friend/game' \
  --documents '/games/friend/config/documents' \
  --local '/games/friend/config/local' --output friend.json

skyrim-mod-sync compare host.json friend.json
```

Capture does not need downloads, a recipe or a Nexus key. It hashes game executables,
SKSE/co-op DLLs, plugins, BSAs, loose/generated assets and shared INIs. It derives
activation, Creation/master entries, order and light flags without running Skyrim.
Missing, extra, changed and plugin-order differences are reported separately. Saves,
explicit runtime logs and OAR's ImGui window layout are excluded; OAR's mod settings
remain hashed. Binary hashes remain authoritative when displayed PE versions match.

## Legacy recipe-only targets (v1/v2)

A host must supply a recipe that reconstructs the target. SME is a test baseline and
contains no SkyrimCoop binary. A manually added co-op DLL blocks publication until it
is represented in the recipe. The legacy publisher does not transfer arbitrary loose mods. Use the v3 workflow
below for modified or local files; Nexus provenance is never inferred from a DLL.

```sh
skyrim-mod-sync configure \
  --base '/steam/Skyrim Special Edition' \
  --coop '/games/our-coop' --downloads '/archives/skyrim'
skyrim-mod-sync nexus-login

# Optional: make the recipe's deterministic target package before preparing a host.
skyrim-mod-sync prepare --recipe list.wabbajack --profile 'Selected profile' \
  --output candidate.skyrimsync
skyrim-mod-sync sync candidate.skyrimsync

# Capture that actual managed installation using its game/config folders, then:
skyrim-mod-sync publish --snapshot host.json --recipe list.wabbajack \
  --profile 'Selected profile' --output our-coop.skyrimsync
```

`prepare` and `publish` reconstruct automatically. `--reconstructed PATH` can reuse
an existing reconstruction, but every deterministic recipe directive is verified
before use. The stock-game adapter merges enabled mods, standard Root Builder files,
profile configuration and supported archive-invalidation tweaks into a physical tree.
Unsupported layouts/settings fail explicitly.

Upload the package to hosting you already use, then generate a code. `share` fetches
the uploaded object to check that it matches before printing the code.

```sh
skyrim-mod-sync share our-coop.skyrimsync \
  --url 'https://your-host.example/our-coop.skyrimsync'
```

The opaque `SMS2` code pins the public HTTPS URL, size and whole-package SHA-256. It
is not a short lookup code and needs no separate lookup service. Packages contain
reconstruction instructions and an optional original Wabbajack recipe, not copies
of licensed game assets. Direct `.skyrimsync` file import also works.

## Legacy HTTPS package workflow

```sh
skyrim-mod-sync configure --base '/their/licensed/Skyrim' \
  --coop '/their/coop-instance' --downloads '/their/archive-cache'
skyrim-mod-sync nexus-login
skyrim-mod-sync plan 'SMS2.…'
skyrim-mod-sync sync 'SMS2.…'
# Use the local package path returned by sync for a later fresh verification:
skyrim-mod-sync verify '/path/from/sync/result.skyrimsync' \
  --instance '/their/coop-instance/instance'
```

At a terminal the CLI prompts for missing path choices and saves them. Explicit flags
override saved settings. An unambiguous native Steam base can be detected automatically;
`doctor` lists alternatives, malformed metadata and missing tools. Credentials use a
separate private key file; `nexus-login` reads a masked prompt or a private `--stdin`
pipe. Credentials never enter codes, packages or command arguments.

Missing Nexus files need Premium for unattended acquisition. Without it, use the
source links in `plan`, download those archives into the cache, and retry. Cached
archives are checked by content, even if their filenames differ. Repairs acquire only
archives needed for changed files. A single output may still require downloading its
whole source archive; unrelated archives are excluded. Inline settings need no installer.

Linux reconstruction requires `bwrap` (the bubblewrap system package). The installer
gets a private home filesystem, read-only base/Steam/Proton mounts, writable managed
storage and the explicitly selected cache. HOME is preserved. It cannot replace the
user's licensed base game.

## Repair and failures

`sync` also repairs changed/missing files and removes extras from a utility-owned
instance. It hashes shared files, stages only changed/missing outputs, and leaves
unchanged files and saves in place. Each replacement/removal moves the old file into
an incremental backup before installing the new file. A fresh whole-instance check
must pass before commit. Writer locks, running-instance checks and a durable per-file
journal protect the repair: failure or interruption restores the changed files on recovery.
This is a recoverable in-place transaction; the game must remain closed during repair.

`rollback --coop PATH` restores the latest changed-file backup without rewriting
unchanged files. Initial installs use a full staged tree; older full-tree backups remain
compatible. JSON results report `files_replaced`, `files_removed`, `files_unchanged` and
`staged_bytes` (replacement payload bytes, excluding metadata/source working files).
`plan` reports changed-file space and acquisition requirements; use the same
`--reconstructed PATH` as `sync` when supplying an external reconstruction cache.
Unowned nonempty destinations are refused. Downloads and reconstructed work remain
cached after cancellation. Backups are not automatically deleted.

Add `--json` for automation: one JSON result on stdout, progress JSON lines on stderr,
no prompts. Exit codes: `0` success, `1` comparison/verification differs, `2` actionable
failure, `130` cancellation. A wrapper can invoke these same commands without implementing
a second synchronization engine.

Recipe-v1 packages remain readable/installable natively. Sync-v2 adds the explicit
snapshot/profile contract; legacy SMS1 recipe codes are refused as sync-v2 targets.

## Development and evidence

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
# Linux environment integration, requiring bubblewrap/OverlayFS user namespaces and a systemd user session:
cargo test --locked -- --ignored
```

The optional `tools/real-sync-acceptance.py` driver invokes the native binary against
SME, creates independent disposable game/config copies, deliberately desynchronizes
one, repairs it and records fresh comparisons and memory measurements. Python is
only the driver for this integration test. `tools/incremental-sync-acceptance.py` reuses
those disposable instances to measure changed-file repair, check every unchanged inode
and timestamp, exercise rollback and recover one output from a filtered recipe. Its
`--sources-only` check also exercises real profile generation, archive patching and
reuse of previously repaired outputs. Run installer acceptance under a process-tree
memory cap. These drivers never launch Skyrim.

See the maintained [implementation plan](docs/sync-implementation-plan.md) and
[Rust validation record](docs/rust-sync-validation.md). The earlier
[Python prototype documentation](docs/python-prototype.md) and
[SME gameplay experiment](docs/sme-validation.md) are historical evidence, not proof
that the Rust CLI launches a playable co-op game.

`tools/editable-revision-acceptance.py` imports a disposable edited real SME profile,
adds actual co-op DLLs, installs a friend through Iroh, publishes add/change/delete
revisions under the same code and verifies unchanged inodes and fresh equality.
It never launches Skyrim; gameplay acceptance is recorded separately.
