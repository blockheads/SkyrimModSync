# SkyrimModSync

Play Skyrim with your friends' exact mod setup. The host shares their Mod Organizer 2 profile; friends get
the same mods with one command, and their own game is restored afterwards.

## Install

Download `sms` from the [latest release](https://github.com/blockheads/SkyrimModSync/releases/latest)
(Windows: `sms-windows-x86_64.exe`, Linux: `sms-linux-x86_64`, then `chmod +x` it) and run it from a
terminal.

## Host

```sh
sms host init --mo2 "C:\Modding\MO2" --profile "Default"
sms host publish        # prints your code: send it to your friends
sms host serve          # keep this running while friends download
```

Change mods in MO2 as usual, then publish again (and restart `serve`). To play yourself:

```sh
sms setup               # once
sms play
```

## Friend

```sh
sms setup               # once: finds Skyrim, asks where to keep the mods
sms play <code>         # downloads the mods, loads them, starts Skyrim
sms unload              # after playing: your game is back as it was
```

Next time, just `sms play`.

Optional: `sms nexus-login` with a Nexus Premium API key downloads Nexus mods from Nexus instead of from
the host.

## Good to know

- `sms status` shows what is synced and loaded; `sms clean` frees space from old versions.
- Mods to share must be installed in MO2; loose files in the game folder are not shared.
- Respect mod permissions: share with friends only.

## Build

```sh
cargo build --release   # target/release/sms
cargo test
```

GPL-3.0-or-later.
