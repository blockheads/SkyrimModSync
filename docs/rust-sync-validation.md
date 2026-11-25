# Rust synchronization validation — 2026-10-14

Work is confined to the standalone SkyrimModSync repository and disposable managed
folders. The original SkyrimCoop checkout and personal game installation are not
modified. The tested binary is native Rust, with no Python process in its core.

## Recipe-only milestone regression checks (v2)

- 39 ordinary Rust regression tests passed (32 integration/CLI tests and seven unit
  tests); the explicitly enabled real bubblewrap namespace test also passed, for 40 total.
- The namespace check covers a base/cache inside the real home directory, Unicode and
  spaces, preserved HOME, read-only base files and writable explicit cache, with the
  personal credential file hidden from the installer filesystem.
- The preserved Python reference suite passes all 90 tests.
- Capture/compare tests cover executable/preloader/co-op hashes, plugins and light flags,
  order, BSAs, scripts, loose assets and selected configuration. UI/runtime output
  exclusions do not hide meaningful mod INIs or unknown extra DLLs.
- Repair tests cover reuse, missing game binaries, removal of extras, saves, backups,
  rollback, recovery, writer locks, running targets, cancellation and disk preflight.
  Deleted game/config directories are repairable; recognized co-op loader copies are
  excluded while the source client and unknown DLLs remain authoritative.
- Modified reconstruction and untracked host files block publication. Duplicate nested
  recipe keys, unsafe paths, symlinks, inconsistent packages and invalid light types fail.

## Recipe-only milestone real acceptance (v2)

SME 2.7.1 is the baseline, with a licensed Skyrim 1.6.1170 source. A fresh physical
copy of the previously reconstructed SME game has been captured and reproduced by its
original recipe using the Rust CLI. Its final shared tree declares 1,551 files.

This test found and fixed two real configuration cases: OAR's ImGui window layout is
runtime UI state, while its main INI remains shared configuration. Crash Logger's
shipped `C:\symcache` hint contains no machine identity and is retained byte-for-byte;
other absolute machine paths are refused. Upstream code identifies the respective
[ImGui settings destination](https://raw.githubusercontent.com/ersh1/OpenAnimationReplacer/main/src/UI/UIManager.cpp)
and [debug symbol-cache use](https://github.com/alandtse/CrashLoggerSSE/blob/main/src/Crash/PDB/PdbHandler.cpp).

The two-copy desynchronization/repair acceptance passed. Two independent physical game
and configuration trees have target identity `1bf3b46227525d5da7b4f83501721fbca2c4ac8cf31d9da31b40e3d257e858e3`. Their Skyrim executables
have distinct inodes. Initial equality, deliberate desynchronization, transactional
repair, fresh equality and subsequent verification all passed. The host's final capture
still matched its original capture. A manually added real SkyrimCoop client DLL caused
publication to fail as `unreproducible_target`, with no share package emitted.

Detected real differences were a missing `po3_Tweaks.dll`, changed `Skyrim.ini`, reordered
`loadorder.txt`/plugins, and a same-size/same-timestamp edit to `ActorUtil.pex`. The old
friend tree was retained in a backup after the original repair. That earlier implementation
took 351 seconds because it staged a complete physical tree. It has been replaced by
changed-file repair, measured below.

The original full-tree repair measured CLI RSS of 87,252 KiB (about 85.2 MiB). The separate fresh
Rust-driven Jackify bootstrap/reconstruction completed and its deterministic outputs
were verified, using the pre-existing verified archive cache. This scoped experiment
used a process-tree cgroup cap of 4 GiB, with swap disabled. The cap is not a measurement
of aggregate peak memory. No Skyrim game process was launched.

Linux tests, rustfmt and Clippy pass. The Windows GNU target passes all-target compilation
and Clippy checks; Windows tests/gameplay have not been executed. Source and build outputs
remain in the standalone repository. The native executable is `dist/rust/skyrim-mod-sync`.

## Incremental repair acceptance

Repeated the same four real changes on the existing disposable friend instance;
no new game copy was made. Changed-file repair passed in **31.462 seconds**, compared
with 351.092 seconds for the earlier full-tree repair. It staged **1,019,750 bytes**
(four files) instead of copying the 17,832,234,947-byte instance. All **1,547 unchanged
files retained their inode, size and nanosecond modification time**. Fresh equality
passed, and the host remained unchanged.

The backup contains the three altered files, the previous receipt and delta metadata;
the previously missing DLL has no old file to retain. Rollback took **0.153 seconds**,
restored the original desynchronization, and preserved unchanged identities. A second
repair succeeded in 33.348 seconds.

With no full reconstruction in the friend's storage, deleting `po3_Tweaks.dll` again
recovered exactly **one output from one verified cached archive**. The internally
filtered recipe excluded every unrelated directive/archive. This repair completed in
**37.154 seconds**, including first engine bootstrap for this storage, and staged
1,013,248 bytes. The engine job contains only that requested DLL and installer metadata.
No mod archive download was needed. Whole source archives may still be needed when the
cache is absent; this does not promise downloading only bytes within a compressed archive.

Warm repair's measured maximum RSS was 75,736 KiB (about 74 MiB); the timed archive
recovery reported 101,984 KiB (about 100 MiB). The complete acceptance driver ran under
a 4 GiB process-tree memory cap with swap disabled. No game process was launched.
File hashing still accounts for most of the warm repair time: approximately 15 seconds
for each shared-tree scan on this machine. These measurements depend on storage/cache
and are not cross-platform performance promises.

New regressions verify unchanged identities, missing directories, directory casing,
file/directory conflicts, offline inline settings, unrelated corrupted sources,
reuse of targeted reconstruction outputs and filtered archive/patch dependencies.
Recovery is exercised before/after individual renames, including interruption during
undo and after commit. A failed final verification restores changed files and ownership.
Unchanged files are never included in an incremental stage or backup. The first install
and host publication remain full operations; old directory-swap journals/backups remain
compatible. Windows execution and power-loss/filesystem fault injection remain untested.

The supplementary real source acceptance also passed: deleted `Skyrim.ini` was
regenerated from the pinned inline profile and archive-invalidation tweaks; deleted
`CrashLogger.ini` was reconstructed using its patch payload and one cached archive;
and the previous targeted job supplied `po3_Tweaks.dll` directly. Only the patched
output invoked the installer. The three-file repair staged 1,018,272 bytes, completed
in 33.396 seconds, preserved 1,548 unchanged identities and passed final fresh checks.

This uncovered and fixed a repeated-installer issue: NLog creates logs beside its
executable. Integrity checks now accept only the exact current/numeric rotation log
names while still verifying every pinned engine file and rejecting unrelated extras.
The namespace mounts the engine read-only and redirects that logs directory to the
current job. Both the real installer retry and a namespace test proving executable
storage is read-only and job logs are writable passed. The failed pre-commit trial
left its deliberate missing files intact; the successful retry repaired them.

## Limits

File equality does not prove runtime loading or gameplay. The SKU's SKSE loader PE
resource reports `0.2.2.6`; this is preserved as raw file-version metadata, not presented
as an observed runtime version. Runtime SKSE 2.2.6 was observed in the earlier Python
launch experiment, which later failed an in-game memory test.

SME has no SkyrimCoop binary. Full co-op recipe acquisition, two-machine gameplay,
Windows/macOS runtime support, Flatpak installer integration, a frontend and an
in-game repair handoff remain outside this milestone. The HTTPS share workflow requires
the user to upload a package to their own existing hosting; no new hosting service is
provided. Remote CI has been configured but has not executed.


## Local artifacts

- `build/rust-validation/real-acceptance-summary.json`: real comparisons, timings and RSS.
- `build/rust-validation/compare_changed.json` and `compare_repaired.json`: exact differences.
- `build/rust-validation/native-engine.json` and its progress/metrics: native reconstruction.
- `build/rust-validation/real-acceptance-evidence-path.txt`: full disposable-run evidence.
- `build/rust-validation/incremental-acceptance-summary.json`: changed-file timing, payloads and identity checks.
- `build/rust-validation/incremental-sources-summary.json`: real inline generation, patching and repaired-output reuse.
- `build/rust-validation/incremental-sources-evidence-path.txt`: repeat-installer/final verification evidence.
- `build/rust-validation/incremental-acceptance-evidence-path.txt`: repair, rollback and archive recovery evidence.
- `build/rust-validation/latest-package-path.txt`: the verified recipe-backed target package.
- `build/rust-validation/final-host-verify.json` and `final-friend-verify.json`: final binary checks.

The original archive cache was reused; this is not a fresh Nexus download acceptance for
Rust. Code encoding, URL validation, pinning, streaming, corrupt/truncated/oversized
responses and offline reuse have regression coverage. A hosted SMS2 package exchange
between two machines has not been tested; uploading the package remains the host's step.

## Editable revisions milestone (v3)

The CLI now imports an editable MO2/Jackify profile read-only, freezes unmatched
local files, publishes Ed25519-signed revisions and serves them through Iroh. One
SMS3 code follows explicit revisions; clients pin the head once and refuse replay.
The original recipe remains a source for unchanged reconstructed outputs. SHA-256
pins every shared file; BLAKE3 verifies peer objects and resumable blob chunks.

New tests cover edited imports without recompiling Wabbajack, frozen-source
immutability, signature tampering, local DLL transfer, same-code updates, unchanged
inodes, rollback, intentional historical fetch, replay refusal, server restart,
corrupt local blob recovery and cancellation/retry of a 64 MiB peer transfer.
Kernel namespace probes verify game, Proton and Steam runtime writes stay in owned
layers. A capped shell fixture verifies that Proton and its Wine-session wait both
run inside the runtime container, and that prefix/saves persist across revisions.
The latter is a lifecycle regression check, not a gameplay simulation.

The real edited SME driver imported 1,553 files (17,852,302,275 bytes), including
the actual co-op loader/client DLLs. Its cold friend install passed in 147.87s,
measured maximum CLI RSS 122,996 KiB, under a 4 GiB process-tree cap with zero swap.
This caught and fixed case-inconsistent staging of mixed inline/archive sources.
Evidence is under the disposable `Rust-Editable-SME-20261014` folder on the NVMe.

Initial capped launch diagnostics caught real platform integration issues: Steam
runtime `.ref`/cache writes, Proton distribution locks, native Steam SDK visibility
in a private home, and waiting outside the runtime's namespace. Writes now use
private tool overlays/cache; SDK mounts are selected explicitly. Proton loads its
native SDK from HOME, as shown in Valve's
[bridge implementation](https://raw.githubusercontent.com/ValveSoftware/Proton/proton_10.0/lsteamclient/unixlib.cpp).
The temporary Xvfb display was replaced with a separate 60 Hz Xorg dummy display.
No existing game/display process was terminated.

**Gameplay acceptance remains pending.** Loading SKSE/the co-op DLL or observing
an early successful process exit is insufficient. Load/save/exit and repeat/update
launches must be observed under the cap. Memory metrics include anonymous/shared
memory separately from total cgroup charges (which also include reclaimable file
cache). WAN and Windows launch remain unvalidated. The current dependencies require
Rust 1.91; earlier Windows cross-compilation evidence above applies to the v2 core.

The real same-code add/change/delete cycle and both final verifiers passed. Additional
co-op UI transfer installed 8 files / 182,444 bytes, preserving 1,553 existing inodes
and mtimes. It completed in 33.973s after eliminating two redundant repair scans.
Local validation artifacts are `build/rust-validation/editable-revisions-summary.json`
and `build/rust-validation/coop-assets-summary.json`. One publisher operation's outer
wall time includes an intentional test-driver pause; its subprocess metrics report
the actual 118.45s rather than the paused 1,301s outer duration.

The full-pack launch subsequently hit the 4 GiB cap (kernel confirmed a cgroup OOM,
not a global OOM). The CLI now treats any observed cgroup OOM as a failure even if
the launcher exits zero; a controlled 128 MiB child-OOM test reproduces and verifies
this. Shader worker limiting alone did not fix the full pack. The minimal SKSE
diagnostic and proper loader wait are being evaluated; full gameplay is pending.
The SKSE loader's documented
[`-waitforclose` option](https://github.com/ianpatt/skse64/blob/master/skse64_loader/Options.cpp)
is now used in addition to the private Wine-session wait.

## Final result for this implementation

53 Rust checks pass: 7 unit tests, 41 ordinary integration/CLI tests, and all 5
explicit Linux environment tests. Clippy and rustfmt pass. The environment tests
include bounded child-OOM and timeout cases; the latter found and fixed an older
systemd option incompatibility. Empty owned scopes now stop after the session.

The minimal SKSE diagnostic reached Riverwood and wrote a real 2,346,355-byte save
inside the instance's persistent private prefix (`__MO_Saves` is the imported
profile's save path). Anonymous/shared usage stayed near 2 GiB. This diagnostic
eventually timed out; clean exit and full co-op gameplay are not claimed.

The full SME pack exceeded the 4 GiB cap even with the co-op DLL removed. Disabling
Engine Fixes alone and limiting shader workers did not resolve it. The cap contained
the failures; no uncapped or larger-memory game was launched. Those observations
narrow the remaining investigation but do not identify a proven root cause.

After diagnostics, the friend was restored to the published full co-op revision 5
from verified cache, replacing only the 10 absent co-op DLL/UI files. User saves and
diagnostic layers remain private. The original SkyrimCoop checkout, personal game
files, and existing user game/display sessions were not modified or terminated.
See `build/rust-validation/gameplay-acceptance-summary.json` for results and paths.
The full modpack's load/save/clean-exit acceptance remains a release blocker.

The final bounded checks also exercise very short-lived child OOMs. A fast-exit
accounting race was fixed by persisting the actual helper's cgroup counters before
exit, matched to its unique scope. Missing final accounting fails closed. Helpers
verify a finite kernel memory limit and zero swap before executing a game. The
5 explicit environment tests and repeated child-OOM regression pass after this fix.
