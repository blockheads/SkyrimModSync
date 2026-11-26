# SkyrimModSync

Play Skyrim with your friends' exact mod setup. The host shares a pack code; friends paste it, play, and
turn it off afterwards to get their own game back.

## Play

1. Download `sms` from the [latest release](https://github.com/blockheads/SkyrimModSync/releases/latest)
   (Windows: `sms-windows-x86_64.exe`; Linux: `sms-linux-x86_64`, then `chmod +x` it) and put it in your
   Skyrim folder (the one with `SkyrimSE.exe`).
2. Run it (double-click on Windows). The first time it asks where to keep mods and for your Nexus API key
   (Enter skips it), then for the pack code your host gave you.
3. Start Skyrim from Steam.

When you're done: `sms disable` puts your game back as it was; `sms enable` turns the pack on again. Run
`sms` again to get the host's newest pack (Enter reuses the last code).

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

Each mod comes from `nexus`, `url`, `github` (newest release) or `local` (a folder next to the list). Its
files go into `Data/`; `folder` picks a folder inside the archive, `root` lists files that go next to
`SkyrimSE.exe`. Later mods win conflicts. `sms host add modlist.json <nexus file link>` adds a Nexus mod.

Then, in the list's folder:

```sh
sms host
```

It builds the pack, uploads it to a secret gist (using your `gh` login, or asking once for a GitHub token
with the gist scope), turns the pack on in your own game, prints the pack code and sends friends the files
Nexus can't, until Ctrl-C. The code stays the same when you change the list and run it again.

Without Nexus Premium, `sms host` lists the archives to save by hand into `downloads/` next to the list.

## Good to know

- `sms status` shows what is set up and enabled; `sms clean` frees space from old versions.
- Everyone needs the same Skyrim version and edition (Anniversary content included or not).
- Respect mod permissions: share with friends only.

## Build

```sh
cargo build --release   # target/release/sms
cargo test
```

GPL-3.0-or-later.
