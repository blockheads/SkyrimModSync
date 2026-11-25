# What the automated tests prove

The current suite has 85 passing cases and five explicit expected failures from
the environment audit. Those failures are outstanding requirements, not supported
setups. See [the environment findings](environment-audit.md). Pytest counts each parameterized input as a
case; the number is not 72 different real modlists or game runs. Test fixtures contain
small fake executables, plugins, textures and recipes. They never launch Skyrim.

| Area | Concrete behavior exercised |
| --- | --- |
| Installation and verification | Resolve enabled mods in priority order; preserve load order and INIs; detect changed bytes even with unchanged size/timestamps; reject extra DLLs; copy rather than hard-link personal files |
| Downloads and installer bootstrap | Reject corrupt CDN parts or a changed engine; reuse verified downloads; refuse dangerous archive paths and symlinks; retain completed work after cancellation |
| Failure and recovery | A failed copy cannot publish a partial install; retry reuses staged work; concurrent writers are blocked; cancelling a backend stops the owned fixture subprocess |
| Sharing and discovery | Encode/decode pinned codes; reject malformed/oversized input; enforce the host fingerprint; parse synthetic Steam manifests; keep keys private and omit account data from responses |
| Launch configuration | With a mocked Proton process, copy settings into the owned prefix, preserve personal Documents, use the short G: path, refuse unowned prefixes and wrong file bytes |
| Runtime files | Allow only named diagnostic logs from declared plugins; retain detection of changed INIs and extra DLLs; recreate a supported deterministic plugin default |
| Native frontend | Click Sync and run the actual public CLI against an installed fixture; freshly verify it without Steam/network; refuse changed bytes; remember the exact code; disable Play after changing code/storage |
| Environment layouts | Native Steam roots, aliases, external and Unicode paths, missing tools and ambiguous installs; explicit failure reproductions for Flatpak, malformed library metadata, namespace visibility and invalid XDG paths |

Examples worth reading:

- `tests/test_installation.py`: same-size tampering, interrupted copy, cancellation,
  extra native plugins, and personal-source isolation.
- `tests/test_codes_workflow.py`: host fingerprint enforcement and sharing refusal
  after file changes; account response sanitization uses a fake HTTP response.
- `tests/test_gui.py`: actual Qt-to-CLI subprocess operation against cached fixtures.
- `tests/test_launch.py`: private prefix setup and launch arguments. The Proton
  process is mocked; these tests cannot establish that the game boots.

The separate [real SME validation](sme-validation.md) downloaded real archives and
launched real Skyrim. It reached the main menu, but the in-game load exhausted memory.
Passing this automated suite does not resolve that failure, establish arbitrary-list
compatibility, or prove live multiplayer synchronization.
