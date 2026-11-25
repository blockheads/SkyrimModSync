# CLI-first Skyrim instance synchronization

Agreed 2026-10-14. This document is the implementation plan and progress ledger.

## Progress

- [x] Capture and compare ordinary deployed installations and explicit configuration.
- [x] Publish a captured target only when a pinned Wabbajack recipe reproduces it.
- [x] Share packages through existing hosting using a URL-and-hash code.
- [x] Plan downloads, reuse, differences, disk requirements, and actionable blockers.
- [x] Sync/repair utility-owned co-op installations transactionally; preserve rollback data.
- [x] Persist explicit paths, prompt for missing values, and provide diagnostics.
- [x] Fix demonstrated discovery, XDG, and installer-namespace failures.
- [x] Complete regression tests and acceptance with two physically distinct real installations.

## Implementation language

The production CLI and core are Rust (decision 2026-10-14). Python is the retained
prototype and regression reference, not a runtime dependency of the Rust binary.
Cargo.lock pins dependency resolution; the first native target is Linux x86-64.

## Summary and boundaries

Linux first, ordinary physically deployed Skyrim folders, exact shared settings, and
Wabbajack recipes for acquisition. No MO2/VFS integration. Base Skyrim and the co-op
installation are separate folders. Mod downloads are a reusable user-selected cache.
SkyrimCoop's current gate checks its build, Skyrim/SKSE versions, loaded plugin order
and light flags; full file hashing is still future work in that mod.

Report file/configuration equality independently from successful game loading. No GUI,
Windows acquisition, Mac compatibility, automatic joining, or in-game repair handoff
in this milestone. Do not modify the original SkyrimCoop checkout or its game processes.

## Operations and comparison contract

Implement capture, compare, publish, share, plan, sync, and verify. Snapshot the deployed
game tree and selected Documents/local configuration using relative paths, sizes and
fresh SHA-256 hashes, binary version information, activation/order and plugin light
flags. Compare native preloaders, SKSE and SkyrimCoop DLLs, plugins, BSAs, loose assets,
scripts, generated assets and shared configuration. Binary hashes are authoritative
when displayed versions match. Follow SkyrimCoop's load-order comparison semantics.

Exclude saves, account credentials, utility state and explicitly recognized runtime
outputs. Reject ambiguous paths, unsupported virtual deployments and unportable shared
configuration; do not silently omit meaningful files. Absolute machine paths stay local.

A host supplies the original recipe and selected profile. Publication reconstructs a
candidate and checks it against the captured target. Added/modified files not represented
by the recipe block publication, including a manually added SkyrimCoop DLL. Existing
SME does not contain that DLL; arbitrary file transfer is deferred.

Share a portable versioned package, uploaded using existing hosting. The code pins its
HTTPS URL and whole-package hash; direct-file import also works. Preserve recipe-v1
inspection/install compatibility and distinguish new codes/packages explicitly.

## Configuration, acquisition and repair

Require a licensed base game, dedicated co-op storage, reusable downloads, and Nexus
authentication when acquisition needs it. Capture needs the active INI and activation
locations; publishing needs a recipe/profile. Detect unambiguous values, otherwise
prompt and save choices. Explicit flags override saved configuration. Noninteractive
runs return actionable blockers. Keys never enter packages, codes or argv.

Use the pinned Linux Jackify engine. Reading its reconstructed profile metadata is
internal processing, not MO2 integration. Verify cache archives and list missing recipe
dependencies; a repair may need more reconstruction than its immediate differences.
Unattended missing Nexus downloads require Premium. Explain manual acquisition and retry.

Modify only utility-owned co-op targets. Refuse unowned nonempty destinations and repair
of running games. Stage and verify before publication, retain replaced content for
rollback, recover interrupted publication, lock writers, and retain resumable work.
Use bounded-memory streaming. The base game and saves remain untouched.

Fix native/Flatpak discovery reporting, malformed library metadata fallback, invalid
XDG fallback and visibility of explicitly selected sources/cache inside the real engine
namespace. Flatpak launch/integration remains unvalidated and must be reported as such.

## Verification and acceptance

Test equality and changes in every file/config/version/plugin category, including
same-size/same-timestamp edits and same-name plugins with different bytes. Cover known
runtime outputs versus unknown extra DLLs, provenance failures, altered packages/recipes,
archive corruption, cancellation, disk exhaustion, concurrent writers, rollback and retry.

Test alternate drives, Unicode/spaces, symlinks, ambiguity, missing tools and actual
namespace visibility. Preserve existing tests. Test JSON/noninteractive CLI results as
well as guided input.

Use two physically distinct game/configuration directories for real acceptance: the
existing co-op harness shares one game directory between its processes. SME is the
initial reconstruction/synchronization baseline; a full co-op target needs a recipe
containing its exact co-op binary. Gameplay remains a separate acceptance milestone.
Any future launch must use process-tree memory limits, monitoring and a timeout.

## Implementation notes and evidence

Completed the Linux synchronization milestone in Rust on 2026-10-14. Capture, compare,
prepare, publish, share, plan, sync/repair, verify, configure, Nexus login, diagnostics,
reconstruction and rollback are available. Recipe-v1 inspection/install stays native;
SMS2/sync-v2 is explicit. The prior Python implementation remains a reference.

- 39 ordinary Rust tests plus the explicitly enabled real namespace test pass (40 total).
- Linux and Windows GNU all-target compilation/Clippy checks pass; rustfmt passes.
- The Python reference suite passes all 90 tests.
- SME real acceptance passed with two physical copies: 1,551 shared files, 42 declared
  plugins, fresh initial equality, deliberate changes, repair, backup retention and
  fresh final equality. The host remained unchanged. The actual manually added co-op
  DLL was refused by recipe provenance checks.
- Fresh Rust-driven pinned-engine bootstrap/reconstruction passed under a 4 GiB
  process-tree cap, using the existing verified archive cache.
- The original native CLI maximum RSS was about 85.2 MiB. Before incremental repair,
  real repair took 351 seconds:
  the former conservative staging implementation reconstructed/copied much more than the
  immediately changed files.
- SMS2 code/pin and download behavior have regression coverage. The host still needs
  to upload a package to existing hosting; hosted two-machine exchange was not exercised.

All progress checkboxes refer to implemented Linux synchronization work. Gameplay,
a full recipe containing the co-op DLL, remote CI/Windows execution, Mac/Flatpak runtime
integration, a new frontend and in-game repair handoff remain deferred. The native
CLI never reports successful runtime loading based on file equality.

See [the validation record](rust-sync-validation.md) for artifacts, timing and limits.

## Incremental repair (2026-10-14)

- [x] Diff the installed files and stage only missing/changed outputs; preserve unchanged files in place.
- [x] Resolve only needed sources, including generated settings and a filtered Wabbajack recipe for archive recovery.
- [x] Journal per-file replacements/removals with crash recovery and changed-file rollback.
- [x] Report repair file/byte counts and plan only relevant acquisition/working space.
- [x] Test unchanged identity, selective acquisition, failures, recovery and rollback.
- [x] Repeat the real SME changes on the existing disposable friend instance and record timing/bytes.

Fresh verification still hashes shared files. Archive recovery can require a whole source archive,
even for one changed output. Initial installation and host publication remain full operations.

Incremental acceptance passed: the original four real SME differences now repair in
31.462 seconds, staging 1,019,750 bytes and preserving all 1,547 unchanged file identities.
Changed-file rollback took 0.153 seconds. Recovery of a deleted DLL from one cached
archive passed in 37.154 seconds, including engine bootstrap; no full reconstruction
or game-tree copy was made. Fresh host/friend equality passed under a 4 GiB process-tree
cap. Evidence is in `build/rust-validation/incremental-acceptance-summary.json`.

- [x] Verify real inline/profile generation, patched archive recovery and prior-output reuse; fix and test repeated engine logging.

The supplementary real source check passed in 33.396 seconds: regenerated Skyrim.ini,
patched CrashLogger.ini and reused the already repaired po3_Tweaks.dll. Only the patched
output required the installer (one output/one archive). All 1,548 unchanged identities
were preserved and final host/friend verification passed. The engine is now mounted
read-only with per-job writable logs; known historical NLog files do not block reuse.
Evidence is in `build/rust-validation/incremental-sources-summary.json`.

## Editable revisions, P2P and isolated play — implementation ledger

Accepted workflow: keep editing an existing MO2/Wabbajack/Jackify setup; import it
read-only, test an isolated draft, explicitly publish frozen revisions, and share one
reusable code. The host keeps a CLI server online. Known recipes remain acquisition
sources; unmatched mod files use P2P. Review in-game changes at publish. Linux first;
GUI/chat, Windows launch, friend seeding and automatic game joining remain deferred.

- [x] Add revision-v3 manifests and private channel/source records; preserve v1/v2.
- [x] Import effective editable profiles and freeze source alternatives/content.
- [x] Publish immutable signed revisions; export portable revision files.
- [x] Add persistent Iroh server, reusable codes and resumable peer acquisition.
- [x] Extend changed-file sync, verification and rollback to revisions.
- [x] Add isolated Linux overlay/Proton play and persistent local state (gameplay acceptance remains below).
- [x] Review/promote runtime changes and detect source/runtime conflicts.
- [x] Add regression, local peer interruption/retry/restart and real namespace acceptance.
- [x] Run modified real SME/co-op import, same-code updates and fresh equality acceptance.
- [ ] Prove full SME/co-op load/save/clean-exit under the 4 GiB cap — blocked by startup memory growth.
- [x] Update CLI documentation, diagnostics and validation evidence.

The new backend uses Iroh 1.3.0 / iroh-blobs 0.103.1 and Rust 1.91. It retains SHA-256
file pins, with BLAKE3 for peer objects. Peers verify publisher identity and signed
revision sequence; an update pins its head once. Files freeze before head publication.
Base game bytes come from licensed local files or pinned reconstruction instructions.
Personal Display/Controls preferences, saves, credentials and logs stay local.
Runtime layers are revision-specific; personal state/prefix persists per instance.
Unknown runtime output is retained and diagnosed, never silently discarded. All real
gameplay tests require a 4 GiB process-tree cap, monitoring and timeout; inability to
load/save/exit blocks gameplay acceptance. Original SkyrimCoop and personal games
remain untouched. WAN/Windows support is not claimed by local tests.

Initial implementation evidence: edited-profile export, frozen local file transfer,
same-code changed-file update, signed-head replay refusal, historical revision fetch,
rollback, kernel overlay probe and persistent runtime-promotion/conflict tests pass.
Gameplay acceptance is still pending.

Real v3 SME import/publication and a fresh friend install passed: 1,553 files,
17,852,302,275 bytes, including the actual loader/client DLLs as frozen peer objects.
The cold friend install completed in 147.87 seconds, measured CLI RSS 122,996 KiB.
Its first attempt exposed mixed `Stock Game`/`stock game` staging directories;
canonical output-directory resolution now fixes this for v3 and legacy repair, with
a regression test. Runtime tool-layer probes prove original game, Proton and Steam
runtime files remain untouched. Initial capped launches found writable runtime
`.ref`/cache and Proton distribution-lock requirements; both now use private layers.
Proton's native Steam bridge also requires SDK mappings inside the private home.
Gameplay acceptance is still pending; initialization failures are not counted as play.

The complete four-revision real loop passed: the same code installed the co-op DLLs,
added/changed/deleted a local asset, preserved all unchanged file identities, and
finished with fresh host/friend equality. Eight actual co-op UI assets then synced
as 182,444 replacement bytes while preserving all 1,553 existing identities. The
optimized v3 path performs one comparison under a writer lock, reuses it for peer
and recipe acquisition, then verifies the committed result; this real update took
33.973s. Its lock also covers installed-head/machine-path metadata publication.
Evidence: `build/rust-validation/editable-revisions-summary.json` and
`build/rust-validation/coop-assets-summary.json`.

Real gameplay remains under diagnosis. The full pack hit its 4 GiB cgroup cap; no
global OOM was triggered. Loader exit zero no longer hides a game child's OOM: an
explicit 128 MiB controlled regression test passes. Empty scopes are stopped after
launch. SKSE `-waitforclose` now keeps the loader tied to the actual game lifetime.
A temporary SKSE-only diagnostic profile reuses the friend instance and its rollback
backup; the published host remains unchanged. No gameplay claim is made yet.

Final validation: 48 ordinary Rust tests plus all 5 explicitly enabled Linux
environment tests pass (53 total); formatting and Clippy pass. The extra tests cover
the real namespace, private game/tool overlays, capped launcher lifetime/persistent
state, a child OOM hidden by loader exit zero, and timeout cleanup of the owned tree.

The SKSE-only diagnostic reached Riverwood and wrote a 2,346,355-byte save in the
instance's private `__MO_Saves` directory. Its anonymous/shared peak remained around
2 GiB; it ultimately met the diagnostic timeout, so clean exit is not claimed.
Full SME still exceeded 4 GiB with co-op removed; disabling Engine Fixes alone also
did not solve it. This is an outstanding pack/runtime compatibility investigation,
not file-sync acceptance. No larger uncapped launch was attempted. The friend was
restored to published revision 5 (co-op DLLs + UI) from verified cache; diagnostics
and saves are retained privately. All owned test games/displays/servers are stopped.
`build/rust-validation/gameplay-acceptance-summary.json` records the exact boundary.
The full gameplay checkbox intentionally remains open. WAN/Windows are unvalidated.

The final bounded checks also exercise very short-lived child OOMs. A fast-exit
accounting race was fixed by persisting the actual helper's cgroup counters before
exit, matched to its unique scope. Missing final accounting fails closed. Helpers
verify a finite kernel memory limit and zero swap before executing a game. The
5 explicit environment tests and repeated child-OOM regression pass after this fix.
