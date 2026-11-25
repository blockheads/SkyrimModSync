# SkyrimModSync

Share a Skyrim mod setup with friends. The host keeps editing mods in Mod Organizer 2 and publishes the
profile; a friend runs `sms play <code>`, which syncs the mods, loads them into their game and starts
Skyrim through Steam. `sms unload` puts their game back exactly as it was.

A CLI (`--json` for scripts). Windows and Linux (Steam/Proton). macOS is not supported yet.

**Get it:** the [latest release](https://github.com/blockheads/SkyrimModSync/releases/latest) has
`sms-windows-x86_64.exe` and `sms-linux-x86_64` (`chmod +x` it). Run it from a terminal.

```
host                                      friend
  MO2: mods, priorities, load order         sms setup              find Skyrim, pick a store folder
  sms host init --mo2 DIR --profile P       sms play <code>        sync + load + start Skyrim
  sms host status                           ... play ...
  sms host publish   → the code             sms unload             restore the game
  sms host serve     (while friends sync)   sms play               later: same host, no code needed
  sms play           (the host too, from its own files)
```

## What a pack is

One JSON document: every file the profile deploys (enabled mods by priority, MO2's overwrite folder,
mods' `Root/` files; from the game folder itself only SKSE's root files), each with its size and BLAKE3
hash, plus the load order and the shared INI settings (screen and controls stay personal). Each file comes
from one of:

- **base:** licensed game content (`Skyrim.esm`, the BSAs, Creation Club files). The friend's own copy
  must match; it is never sent or downloaded.
- **archive:** a member of the archive its mod was installed from, found by hash. MO2 records each mod's
  Nexus ids; `sms host source MOD url URL` or `sms host source MOD github OWNER/REPO [ASSET]` (the newest
  release, picked at each publish) point a mod elsewhere.
- **host:** everything else (generated output, edited files, hand installs, RAR archives).

The code is the host's network id. Friends connect to the host directly over
[iroh](https://iroh.computer) (it crosses home routers and relays when it must, and proves the host is who
the code says). The host sends files straight from its MO2 folders: nothing is copied for sharing.

## Nexus

Optional. With a Premium account's API key (`sms nexus-login`), Nexus mods download from Nexus through
its documented API, which spares the host's upload. Without one, or when a download fails, the host sends
those files. The tool never automates the website.

## Load and unload

The store is a folder the friend picks. `sms load` plans the whole change first (files to place, the
friend's own `Data/` files to set aside, `plugins.txt`, `loadorder.txt`, INI settings, SKSE's loader in
Steam's `SkyrimSELauncher.exe` slot), writes the plan to `<game>/.skyrim-mod-sync/journal.json`, then
carries it out with hardlinks from the store (copies across drives, and for config files a mod may
rewrite). `sms unload` walks the plan backwards; a load cut short is undone by the next command. The host
plays its own pack the same way, hardlinked from its MO2 files. `sms clean` deletes store files the
current pack no longer uses.

## Sharing responsibly

The tool shares whatever the host has; it ships no mod content and has no public catalogue. Codes travel
between friends. Respect the permissions of the mods you share: prefer Nexus sources (the default when MO2
knows them) and send only what has no public source.

## Build and test

```sh
cargo build --release --locked      # target/release/sms
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

The tests run whole flows on fake games: a fake Nexus and GitHub, a real local iroh host, byte-identical
unloads, a cut transfer resuming, a crash mid-load undone, host play. Releases: push a `v*` tag;
`.github/workflows/release.yml` builds Linux and Windows binaries and publishes them with `SHA256SUMS`.

## Layout

```
rust/
  main.rs     the CLI              host.rs    MO2 profile → pack, publish
  mo2.rs      reading MO2          pack.rs    the format; rules.rs what may travel
  hash.rs     BLAKE3 + cache       archive.rs zip/7z
  net.rs      HTTPS, Nexus, GitHub peer.rs    the host's service and fetching from it
  store.rs    the friend's store   sync.rs    filling it
  deploy.rs   plan, load, unload   ini.rs, steam.rs, launch.rs, config.rs, files.rs
```

Licensed GPL-3.0-or-later.
