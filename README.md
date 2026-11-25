# SkyrimModSync

Share a Skyrim mod setup with friends. The host keeps editing mods in Mod Organizer 2 as usual and
publishes the profile; a friend pastes the code, and `sms play` syncs the mods, loads them into
their game, and starts Skyrim through Steam. `sms unload` puts their game back exactly as it was.

A CLI (`--json` for scripts and a future GUI). Windows and Linux (Steam/Proton); macOS builds exist, playing
there (CrossOver) is untested.

**Get it:** the [latest release](https://github.com/blockheads/SkyrimModSync/releases/latest) has `sms` for
Windows (`sms-windows-x86_64.exe`), Linux and macOS. Put it anywhere and run it from a terminal (rename it `sms` if
you like); on Linux and macOS `chmod +x` it first.

```
host                                     friend
  MO2: mods, priorities, load order        sms setup            find Skyrim, pick a store folder
  sms host init --mo2 DIR --profile P      sms nexus-login      Nexus API key (Premium)
  sms host status                          sms play <code>      sync + load + start Skyrim
  sms host publish     → code              ... play ...
  sms host serve       (host-only files)   sms unload           restore the game
  sms play             (the host too: its own pack, from its own files)
```

The host plays the same way: `sms setup` once, then `sms play` with no code syncs its latest published pack from
its own MO2 files (hardlinked, nothing downloaded), loads it and starts Skyrim. Publishing waits for `sms unload`
(the loaded copies would otherwise be read as game files).

## What a pack is

The host's installed files (what the profile deploys: the game folder, enabled mods by priority,
MO2's overwrite folder, mods' `Root/` files), the load order (`plugins.txt`, `loadorder.txt`) and the
shared INI settings (screen and controls stay personal). Each file is pinned by SHA-256 and says
where its bytes come from:

- **base:** licensed game content (`Skyrim.esm`, the BSAs, Creation Club files). The friend's own
  copy must match; it is never sent or downloaded.
- **archive:** a member of the archive the mod was installed from, found by hash. MO2 keeps each
  mod's Nexus ids (`meta.ini`, the download's `.meta`); the friend downloads that archive from Nexus
  with their own API key and keeps only the needed files. `sms host source MOD url URL` points a mod
  at an HTTPS archive instead (SKSE's), and `sms host source MOD github OWNER/REPO [ASSET]` at the newest GitHub
  release's archive, picked again at every publish (a mod you release on GitHub stays current).
- **host:** everything else: generated output (Nemesis, BodySlide, LOD), edited or patched files,
  mods installed by hand, RAR archives. The host sends these over P2P (iroh) while `sms host serve`
  runs.

`sms host status` lists every mod, its source and how much the host would send. A pack with no host
files can be shared as a file (`sms host publish --output pack.smspack`) and synced without the host
online.

## Nexus

Only through Nexus's documented API and the friend's own API key. Downloading through the API needs a
Premium account; the tool does not automate the website for free accounts. Without a Premium key,
ask the host to send those mods (`sms host source MOD host`).

## Load and unload

The store is a folder the friend picks. Synced files live there by hash; `sms load` hardlinks them
into the game folder (copies across drives, and always for config files a mod may rewrite), moves
the friend's own `Data/` files that aren't in the pack out of the way, writes the pack's
`plugins.txt`, `loadorder.txt` and INI settings, and puts SKSE's loader in Steam's
`SkyrimSELauncher.exe` slot so Steam's Play starts SKSE. Every change is journaled before it is made
(`<game>/.skyrim-mod-sync/`); `sms unload` replays the journal backwards, and an interrupted load is
undone by the next command.

## Sharing responsibly

The tool shares whatever the host has; it ships no mod content and has no public catalogue. Packs
travel as codes between friends. Respect the permissions of the mods you share: prefer Nexus sources
(the default when MO2 knows them) and send only what has no public source.

## Build and test

```sh
cargo build --release --locked      # target/release/sms
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Releases: push a `v*` tag; `.github/workflows/release.yml` builds `sms` for Linux, Windows and macOS (Apple
Silicon and Intel) and publishes them with `SHA256SUMS`.

Rust 1.91 (`rust-toolchain.toml`). The tests run a whole flow on fake games: an MO2 profile
published, a friend synced through a fake Nexus and a local P2P host, loaded, unloaded back to
byte-identical folders, and a crash mid-load recovered.

## Layout

```
rust/
  mo2.rs       reading an MO2 instance: effective files, mod origins
  host.rs      draft (sources by hash), status, publish
  pack.rs      the pack format; channel.rs signed heads and codes, pack files
  archive.rs   zip/7z members; nexus.rs, github.rs, net.rs downloads
  peer.rs      the host's P2P service and fetching from it
  store.rs     the friend's store; sync.rs filling it
  deploy.rs    load/unload with the journal; ini.rs INI merging
  steam.rs     finding Steam, Skyrim, the Proton prefix; launch.rs starting the game
```

Licensed GPL-3.0-or-later.
