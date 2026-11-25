# SkyrimModSync

Play Skyrim with your friends' exact mod setup. The host writes a mod list; friends get the same mods
with one command, and their own game is restored afterwards.

## Install

Download `sms` from the [latest release](https://github.com/blockheads/SkyrimModSync/releases/latest)
(Windows: `sms-windows-x86_64.exe`, Linux: `sms-linux-x86_64`, then `chmod +x` it) and run it from a
terminal. First:

```sh
sms setup      # finds Skyrim, asks where to keep mods and for your Nexus API key
```

A Premium Nexus key downloads Nexus mods from Nexus. Without one, mods come from the host, who must be
running `sms host serve` while you sync.

## Play

```sh
sms play gist:ID              # or github:OWNER/REPO, a pack link or file, or a host's code
sms unload                    # after playing: your game is back as it was
```

Next time, just `sms play`.

## Host

Write a mod list (`modlist.json`):

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

```sh
sms host publish modlist.json --output pack.json
```

Share `pack.json`. A gist is the lightest way:

```sh
gh gist create modlist.json pack.json           # once; friends run `sms play gist:<id>`
gh gist edit <id> -f pack.json pack.json        # after publishing again
```

Or attach it to a GitHub release (`github:OWNER/REPO`), or share the code `publish` prints and keep
`sms host serve` running. Without Nexus Premium, `publish` lists the archives to save by
hand into `downloads/` next to the list.

## Good to know

- `sms status` shows what is set up, synced and loaded; `sms clean` frees space from old versions.
- Everyone needs the same Skyrim version and edition (Anniversary content included or not).
- Respect mod permissions: share with friends only.

## Build

```sh
cargo build --release   # target/release/sms
cargo test
```

GPL-3.0-or-later.
