# SkyrimModSync: design

`sms` shares a Skyrim mod setup between friends. A mod list resolves into a pack (every file, pinned by hash, with
where its bytes come from); players sync it, enable it in their game, play, and disable it.

Decisions (2026-11-25):

- **Files, not recipes.** What players receive is the per-file pack. No Wabbajack-style patching or rebuilding:
  anything that isn't a plain archive member comes from the host.
- **Our own mod list.** A JSON list with one source per mod (nexus, url, github, local) and a simple install rule
  (`folder`, `root`). Mod organizer setups (MO2, Vortex, Wabbajack) become converters into it later; the pack is the
  stable interface.
- **A pack code is a secret gist's id** (v0.7). `sms host` uploads the pack there itself (the GitHub CLI's login, or
  a token asked once). A pack with no host-only files needs no host online. Short codes would need a lookup service
  we run; not worth it while codes are copy-pasted.
- **No mod manager for players.** Enable / disable deploy the pack into the game folder, Vortex-style, journaled;
  players start Skyrim from Steam (SKSE's loader sits in Steam's launcher slot).
- **Double-click for players** (v0.7): `sms` in the Skyrim folder, run with no command, asks the mods folder, the
  Nexus key and the pack code once; later runs take the last code with Enter. One command for hosts: `sms host`.
- **Nexus only through the user's own API key** (Premium downloads; `sms setup` asks; without Premium the host saves
  archives by hand into `downloads/`, and players get the files from the host).
- **A simple window for players** (v0.8, egui): double-clicking opens it on Windows (the console it
  started with is closed), `sms gui` anywhere; a Setup section (folders, Nexus key), pack code, Check (the
  out-of-sync list), Download and enable, Disable, Cancel while busy. It shows the same lines the terminal
  prints (each step, download progress with speed and time left, the result), one for one. Hosts and
  scripts use the CLI, `--json` everywhere. Linux and Windows releases.
- **General**: codes and links travel between friends; no catalogue, no mod content shipped.
- **Simple and fast** (v0.5): one hash (BLAKE3, parallel), no signing layer (iroh authenticates the host), the host
  serves files from where they are.

## The whole flow

```
host:    sms host [modlist.json]
         → unload own pack → publish (resolve, pin) → pack.json to the host's secret gist
         → enable here → serve files until Ctrl-C (needed for players without Premium, local files)
pack  = files {path, size, blake3, from} + plugins + loadorder + ini + host id
        from = base (player's own copy must match) | archive{hash, member} | host

players: sms (no command; setup on the first run) → pack code (gist id or link)
         → fetch pack → list what is out of sync, ask → sync, enable → Play in Steam → sms disable
         (`sms check` lists only; `--yes` skips the question; the host's own sync never asks)
```

## 1. The mod list (`list.rs`)

```
mod = name + one source: nexus{mod, file} | url | github{repo, asset?} | local (folder or archive; * = newest)
      + folder (only that folder inside the archive) + root (files beside SkyrimSE.exe)
install rule: root files → game root; Data/… → Data/; a mod with a Data/ folder installs
only that and its root files (SKSE: skips src/); otherwise everything → Data/; fomod/ never
later mods win conflicts; plugins = load order; ini = shared settings
sms host add LIST <nexus file link> appends a Nexus mod
```

## 2. Publish (`host.rs`)

```
every archive first: cached by source; Nexus link (Premium) or downloads/<name> by hand
  (missing ones listed together with their Nexus links); github → newest release asset
base files: the host's own licensed game files (version/edition check for players); the host serves its
  base BSAs and cc*.esm/esl / _ResourcePack.esl / Skyrim.ccc to players whose copy is missing or differs
members listed once per archive hash; each mod's files by its install rule
published.json = pack + local files + archive members (the host serves either,
extracting a member on first request); --output writes pack.json
```

## 3. Transfer (`peer.rs`)

```
one iroh connection, one stream per request, 8 at a time:
  "pack"                  → the pack JSON
  "file <hash> <offset>"  → '1' + bytes from the host's own file, or '0'
friend writes <hash>.part, checks size and hash, renames into the store; a cut file resumes
```

## 4. Sync (`sync.rs`)

```
plan → what the game and store lack; base BSAs / cc plugins missing or different → from the host (placed over the
       player's, restored on disable); other base files that differ → refuse (version/edition); the voice BSA is never in the pack
       and stays the player's (their language; every other game BSA holds all languages)
archives 3 at a time: Nexus link (key) or URL → hash checked → wanted members into the store
any failure / no key → those files from the host
```

## 5. Enable / disable (`deploy.rs`)

```
one walk of the game folder (folded path → real casing)
plan: Dir, Aside (friend's Data/ files not in the pack), Place (hardlink; config copied;
      SKSE's loader in SkyrimSELauncher.exe), User (Plugins.txt, loadorder.txt, INIs merged)
journal.json written once → carried out → `loaded` marker
disable: journal backwards, each step tolerant of one that never ran; interrupted = undone first
```

Measured on v0.5 (this machine, 22 GB clean game, page cache smaller than the data): a cold publish read 10-17 s
(v0.4.1: 21 s); memory-mapped hashing was slower (25-32 s) and was dropped. Binary 9.5 MB (v0.4.1: 26 MB), same
speed (size-optimized build, hashing/unpacking/crypto at full optimization).

## Later

Converters into the mod list or straight into a pack (scanning an installed game folder against the downloaded
archives, MO2, Vortex, Wabbajack), macOS (CrossOver), hosting from the window, RAR archives, peers sharing
files they already have.

## Not yet tested

Windows gameplay, Steam's Play starting SKSE through the launcher slot, a real Nexus Premium download, the window
on Windows (double-click detection, closing the console). Tested in v0.7: the double-click flow on a fake game, P2P
through iroh's public relays (same machine), the real gist upload; in v0.8 the window on Linux (X11, fake game:
check, enable, disable).
