# SkyrimModSync

Play Skyrim with your friends' exact mod setup. The host shares a pack code; friends paste it, play, and
turn it off afterwards to get their own game back.

## Play

1. Download `sms` from the [latest release](https://github.com/blockheads/SkyrimModSync/releases/latest)
   (Windows: `sms-windows-x86_64.exe`; Linux: `sms-linux-x86_64`, then `chmod +x` it) and put it in your
   Skyrim folder (the one with `SkyrimSE.exe`).
2. Double-click it: a window opens. Under **Setup**, check the folders and add your Nexus API key if you
   have one. Paste the pack code your host gave you and press **Check**.
3. It lists what your game lacks (mods, game files from the host, your own mods it sets aside while the
   pack is on). **Download and enable** gets it and turns the pack on, saying each step as it goes
   (downloads show their speed and time left; **Cancel** stops, keeping what finished).
4. Start Skyrim from Steam.

When you're done, **Disable** (or `sms disable`) puts your game back as it was. Open it again and press
Check to get the host's newest pack.

From a terminal, `sms` asks the same questions as text and prints the same lines as the window: it lists
what is out of sync and asks before downloading (`sms enable <code>`; `--yes` skips the question). `sms gui` opens the window on any system.

A Premium Nexus key downloads Nexus mods from Nexus; without one, mods come from the host, who must have
`sms host` running while you sync.

## Host

Write a mod list, `modlist.json`:

```json
{
  "name": "Our pack",
  "mods": [
    { "name": "SKSE", "nexus": { "mod": 30379, "file": 795992 },
      "folder": "skse64_2_03_01", "root": ["skse64_loader.exe", "skse64_1_7_104.dll"] },
    { "name": "USSEP", "nexus": { "mod": 266, "file": 800977 } },
    { "name": "Our mod", "github": { "repo": "OWNER/REPO" } },
    { "name": "Tweaks", "local": "tweaks" }
  ],
  "plugins": ["unofficial skyrim special edition patch.esp"]
}
```

Each mod comes from `nexus`, `url`, `github` (newest release) or `local`: a folder or archive, next to the
list or absolute, sent by the host over P2P. A `*` in a local name picks the newest match, so
`"local": "/path/to/build/package/MyMod-*.zip"` shares your latest build with no release. Its
files go into `Data/`; `folder` picks a folder inside the archive, `root` lists files that go next to
`SkyrimSE.exe`. Later mods win conflicts. Archives can be zip, 7z or rar.

To change the list without editing it (the list in this folder, or `--list FILE`):

```sh
sms host add "https://www.nexusmods.com/skyrimspecialedition/mods/266?tab=files&file_id=800977"
sms host add ~/mods/MyMod.zip        # or a folder; the host sends its files
sms host remove "unofficial"         # by name (or part of it), Nexus link or path
```

`add` puts the mod's plugins at the end of `plugins` (the same mod again replaces its entry, for a new
version); `remove` takes them out. Run `sms host` again to share the change.

Then, in the list's folder:

```sh
sms host
```

It builds the pack, uploads it to a secret gist (using your `gh` login, or asking once for a GitHub token
with the gist scope), turns the pack on in your own game, prints the pack code and sends friends the files
Nexus can't, until Ctrl-C. The code stays the same when you change the list and run it again.

Without Nexus Premium, `sms host` lists the archives to save by hand into `downloads/` next to the list.

## Good to know

- `sms check [code]` lists what is out of sync without downloading.
- `sms status` shows what is set up and enabled; `sms clean` frees space from old versions.
- Everyone needs the same Skyrim version and edition (Anniversary content included or not). Game `.bsa`
  files and Creation Club plugins (`cc*.esm`/`.esl`, `_ResourcePack.esl`, and `Skyrim.ccc`, which lists them) are the
  exception: a friend missing
  one, or with another version, gets the host's while the pack is on (`sms host` running), and their own back
  on `sms disable`. Each player keeps their own language: the game's text is in every language
  already, and the voice BSA (`Skyrim - Voices_en0.bsa`, `_de0`, …) stays the player's.
- Respect mod permissions: share with friends only.

## Build

```sh
cargo build --release   # target/release/sms
cargo test
```

GPL-3.0-or-later.
