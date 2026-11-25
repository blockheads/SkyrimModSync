# Skyrim Modding Essentials validation — 2026-10-13

The real Linux setup pipeline completed for SME 2.7.1. Reliable gameplay and the
two-machine acceptance test remain outstanding.

| Check | Result |
| --- | --- |
| Public recipe download | Passed; 30,404,713 bytes, SHA-256 pinned |
| Licensed base-game preflight | All 85 recipe game-file sources matched size and xxHash64 |
| Nexus authentication | Supplied personal key validated as Premium; stored privately outside the repo |
| Remote archives | All 216 downloaded: 208 Nexus and 8 HTTP sources; approximately 1.64 GB |
| Native installer | Pinned Jackify 0.5.10 completed reconstruction in an isolated filesystem namespace |
| Reconstruction checks | Deterministic files verified; seven machine-remapped files excluded explicitly |
| Managed game publication | 1,550 declared files, 17,832,233,836 bytes; final SHA-256 verification passed |
| Shared code | Self-contained recipe/profile code generated with final setup fingerprint |
| SKSE launch | Correct Skyrim 1.6.1170 and SKSE 2.2.6 loaded in the managed prefix |
| Main menu | Reached after mapping the managed game to the private Wine G: drive |
| In-game load | Failed; the kernel killed test PID 3155312 at 19,276,328 kB anonymous resident memory |
| Current automated tests | 85 passed, 5 explicit environment failures marked xfail; not gameplay acceptance |

The first physical-path launch crashed during loose-file traversal. Some paths were
277 characters long. Launching through G: subsequently reached the main menu. This
supports the path-length diagnosis; it does not establish the cause of the later
memory growth.

The in-game test attempted `coc qasmoke`. Successful entry was not observed. At
21:23:32 local time, the kernel reported a global out-of-memory condition and killed
the managed Skyrim process. The memory problem is unresolved. The managed game is
no longer running. The user's existing Skyrim sessions were not stopped.

Known runtime logs and a deterministic OpenAnimationReplacer 2.3.6 configuration
are now modeled in source and covered by focused regression tests. Those changes
still need republication of the real target and a repeat Play test. The existing published
target does not yet include that generated configuration. The desktop bundle has
been rebuilt with the CLI-first entry point and smaller frontend. Packaged `share`
and the native Sync button calling the packaged CLI passed against a synthetic
cached installation. Those checks did not launch Skyrim or download mods.

Next acceptance steps: diagnose the memory growth under a process memory limit;
prove stable gameplay and repeat launches; reconstruct a second isolated install
from the exact shared code and compare its fingerprint; exercise the packaged GUI
Sync/Play flow. No live SkyrimCoop session or Steam-invite test has been performed.

Development evidence is Git-ignored under `build/research`, including
`sme-code-workflow-events.jsonl`, `sme-materialization-events.jsonl`,
`sme-play-short-drive-events.jsonl`, and `sme-main-menu.png`. The automatic pipeline
ran against real archives; subsequent verifier fixes were resumed using its owned
reconstruction rather than repeating downloads.
