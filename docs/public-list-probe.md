# Public recipe probe — 2026-10-13

Downloaded **Legends of the Frost 3.3.4**, tagged Lightweight in the public Wabbajack
gallery, without an account. This tests a real recipe format and CDN acquisition,
not the list's game or co-op compatibility. The published metadata dates to 2024;
do not infer current support for SkyrimCoop's runtime from its gallery presence.

- [Gallery metadata](https://github.com/wabbajack-tools/mod-lists/blob/master/modlists.json).
- [Author's instructions](https://raw.githubusercontent.com/foreverphoenix/the-phoenix-flavour/master/WABBAJACK-LOTF.md).
- Recipe URL: `https://authored-files.wabbajack.org/Legends%20of%20the%20Frost.wabbajack_0547b088-116d-441e-a2d4-46c075368e90`.
- Downloaded size: **475,344,149 bytes**.
- SHA-256: `10dd627877c067a6c916e51d4831ab4b853ec84e7b4b48e25a10dfce8c218293`.
- Wabbajack whole-file xxHash64: `Ye2P0hBmgpU=`. Every CDN part and the whole recipe matched.
- **616 archives**, including **430 Nexus**, **183 game-file sources**, 2 HTTP and 1 CDN source.
- **168,860 reconstruction directives**, 75 files under mod `root` directories.
- One profile: `Legends of the Frost`.
- Recipe-declared original archives: **46,253,661,857 bytes**. The small/lightweight
  gallery label does not mean a small first download.

The apparent download URL is a chunked CDN resource. An ordinary GET returned a
989-byte HTML page. The successful tool follows upstream's `definition.json.gz` and
`parts/<index>` protocol and verifies hashes. It retains valid parts for retry.

The attempted BASELINE 1.0.1 public recipe definition returned HTTP 404. No file was
accepted or installed from that response.

The downloaded recipe is retained locally in `build/research/` (Git-ignored). Mod
archives, a reconstructed installation, a game boot and a live co-op session were
not tested. No Nexus/Bethesda/Steam account or existing game setup was used.

Reproduce the successful probe:

```sh
skyrim-mod-sync download-list \
  'https://authored-files.wabbajack.org/Legends%20of%20the%20Frost.wabbajack_0547b088-116d-441e-a2d4-46c075368e90' \
  --output build/research/Legends-of-the-Frost-3.3.4.wabbajack \
  --cache build/research/parts
skyrim-mod-sync inspect-list build/research/Legends-of-the-Frost-3.3.4.wabbajack
```
