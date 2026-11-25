# Environment audit and validation plan — 2026-10-13

The current SME run proves one real machine's download/install flow and main-menu
launch. It does not establish a portable, ready-to-play product. The in-game memory
failure remains unresolved. This audit identifies concrete setup assumptions and
adds inexpensive reproductions without launching Steam or Skyrim.

## How discovery and destination selection work today

Discovery tries `.steam/steam`, `.steam/debian-installation`, and `.local/share/Steam`
under the current user home. It resolves aliases, reads `steamapps/libraryfolders.vdf`,
then checks each library for `appmanifest_489830.acf` and `SkyrimSE.exe`. It looks for
exact `Proton 10.0` and `SteamLinuxRuntime_sniper` directories across those libraries.
Multiple distinct Skyrim candidates are refused; missing tools are returned as missing.

The default managed root is the XDG user data directory plus `skyrim-mod-sync`, or
`~/.local/share/skyrim-mod-sync`. On Windows, the intended root is under LOCALAPPDATA.
`--root` and the frontend's Storage option select another volume. An installation
lives beneath `root/installs/<final-fingerprint>`. The default does not choose a
large disk automatically; the real SME test explicitly selected a suitable volume.

The discovery function accepts an explicit Steam root, but `sync` and `play` currently
do not expose that override. The selected Steam/game/runtime paths are not a complete
persisted local configuration. Error text suggesting selection in Settings is ahead
of the simplified frontend's actual capabilities.

## What the new environment fixtures establish

`tests/test_environment_layouts.py` builds temporary machine layouts and calls the
same discovery code used by the application. Its namespace case invokes real
bubblewrap against a temporary fake home, without changing the HOME environment.
No real account, Steam client, mod download or game process is involved.

| Scenario | Observed result |
| --- | --- |
| Each of the three native Steam locations | Detected |
| Several symlinks to the same Steam/game installation | Deduplicated on Linux |
| External game library with spaces, Unicode or shell-like literal text | Detected |
| Explicit custom Steam root passed to discovery | Detected; CLI override still missing |
| Two distinct Skyrim installations | Refused instead of guessing |
| Disconnected secondary drive plus valid primary game | Primary game detected |
| Executable without Steam install manifest | Refused |
| Missing Proton/runtime | Reported missing |
| Missing bubblewrap | Refused before engine state is written |
| Flatpak-only Steam root | Known failure: not discovered |
| Broken libraryfolders.vdf plus valid primary install | Known failure: primary candidate discarded |
| Game library under home, outside Steam's own directory | Known failure: discovered game hidden by engine namespace |
| Empty or relative XDG_DATA_HOME | Known failure: relative managed root rather than standard fallback |

Known failures are marked `xfail(strict=True)`. They document failing desired
behaviors, not supported environments. Fixing one must remove its marker; an
unexpected pass fails CI so support labels cannot silently become stale. A runner
unable to create a Linux namespace skips that integration probe explicitly.

## Research findings that affect the design

Flatpak Steam has a separate filesystem view and requires access to external library
mounts. Finding its directory alone cannot establish that Steam, the installer and
the game can see the same target. The package documentation also describes incorrect
free-space reports when access is granted at the wrong directory level.
[Flatpak Steam documentation](https://github.com/flathub/com.valvesoftware.Steam/wiki#i-want-to-add-external-disk-for-steam-libraries).

Proton/runtime selection is a compatibility pair. Valve documents sniper for official
Proton 8–10, and Steam Runtime 4 for Proton 11 or newer. We should initially support
a named tested pair and report its absence, then add verified pairs. Picking the
highest directory name would not be a compatibility policy.
[Valve runtime documentation](https://github.com/ValveSoftware/steam-runtime/blob/master/doc/reporting-steamlinuxruntime-bugs.md).

The XDG specification requires absolute paths and falls back when variables are
empty. Current data/config helpers do not fully enforce that. Large managed game
storage should be a deliberate choice when the default volume lacks capacity.
[XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/).

Windows paths need platform discovery rather than Linux home guesses. Its known-folder
API provides the actual configured Documents and app-data locations. Discovering those
locations does not solve isolating Skyrim's saves, INIs and plugin activation: that
is a separate launch-adapter requirement which must be proven before Windows Play.
[Microsoft known-folder API](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderpath).

## Keep the implementation small

Separate the portable target from local machine choices. A target identifies game
content, mods, load order and shared gameplay settings. Steam location, storage,
launch tools and hardware-specific settings are local. The current manifest includes
all selected INIs, so separation of hardware settings is still design work.

A small resolved local setup record should contain Steam kind/root, game path, managed
storage and the supported launch-tool pair. Each detected value should retain its
origin so diagnostics can explain it. Explicit flags override saved choices; saved
choices override autodetection. Ambiguity requires selection. Validate saved paths
again when running, since drives can disappear or installations can move.

Proposed next interface: a `doctor` command explains local candidates and blockers;
a plan reports target-specific base-game requirements and total working space before
multi-gigabyte downloads. Existing `sync` and `play` consume that same resolution.
These are recommendations, not implemented commands. The frontend presents the result
and only asks for a choice when detection is ambiguous or no viable storage exists.

Checks should include write/rename/lock capability in owned scratch storage, execution
permission, prefix filesystem requirements, source/destination separation, required
content/version, engine namespace visibility and tools actually runnable. Clean up
scratch probes. Do not infer compatibility solely from the existence of a directory.

## Simulate progressively more real environments

1. **Temporary layouts and injected failures, every test run.** Expand these fixtures
   with removed mounts, read-only targets, exhausted capacity, long paths, missing
   tools, duplicate installs and malformed metadata. Run portable tests on Windows
   and Linux. Fake Windows directory names on Linux do not substitute for Windows.
2. **OS integration, in controlled users/VMs.** Test actual Windows registry/known
   folders, NTFS and user permissions; Linux namespaces, separate home libraries,
   supported native Steam packaging, and Flatpak access boundaries. Use a tiny
   harmless probe executable to check launching and path visibility before Skyrim.
   Containers help with packaging/dependency checks but share the host kernel and
   do not establish Windows or GPU behavior.
3. **Network/storage fault injection.** A local test server can truncate downloads,
   reject requests, delay responses and interrupt connections. A bounded temporary
   filesystem can exercise disk exhaustion and rename failures. Most existing
   download tests mock the read function; actual HTTP recovery remains to add.
4. **Real acceptance, on a short support matrix.** Reproduce the same pinned target
   on controlled Windows and Linux game machines, compare files and effective
   configuration, load into gameplay, exit, and repeat. Test return to personal play
   and changed-target refusal. A real launch uses a process-tree memory limit,
   timeout and resource monitoring; stop only the owned process tree. Never repeat
   an unbounded load test on the user's desktop.

Start with Steam Skyrim SE on native Linux x86-64 and Windows x86-64. Flatpak is a
separate integration target; Deck, alternative editions, custom Proton builds and
other operating systems need their own validation. Windows installation/launch is
not implemented today. The existing GitHub workflow describes Windows/Linux fixture
jobs but has not run remotely: this repo has no remote or CI history yet.

The next implementation work is to fix the demonstrated discovery/storage/namespace
failures, expose explicit choices through the CLI, and add diagnostics. Reliable
SME gameplay and the Windows launch adapter remain acceptance blockers. Avoid adding
more pack features until those foundations have evidence.
