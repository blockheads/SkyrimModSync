# Pinned native engine probe — 2026-10-13

The launcher obtained the public Linux x64 Jackify engine 0.5.10 without using an
account. The release metadata supplies a SHA-256 digest; the downloaded artifact
matched it before any executable was run.

- [Release](https://github.com/Omni-guides/dev-jackify-engine/releases/tag/v0.5.10).
- Archive: `jackify-engine-0.5.10-linux-x64.tar.gz`, 58,345,382 bytes.
- SHA-256: `10480a3d452f6611d236219aab450e43bdcd7eab2f9c5cde96fb43e55ce5625d`.
- Unpacked: 386 tar entries, 136,283,198 file bytes, no symbolic/hard links.
- `--version`: `0.5.10+e1981db487b06b5b798370eec6273cefffd0ef26`, exit 0.
- `--help` and `install --help`: exit 0.

The release's install help lists `-w/--wabbajack`, `-m/--machineUrl`, `-o/--output`,
`-d/--downloads` and `--debug`. It does not list the `--show-file-progress` or `-g`
options used by newer Jackify source. The release adapter therefore invokes:

```text
jackify-engine install -w <recipe> -o <reconstruction> -d <downloads>
```

The pinned release locates the game itself; an explicit base-directory override has
not been established for it. Our own base-content checks and final manifest checks
still apply. This version's game discovery and acquisition/authentication behavior
need a real supported-pack acceptance test.

Bootstrap manually extracts bounded ordinary tar entries, rejects traversal/links,
checks the pinned archive hash, publishes separately owned files and records their
hashes. Subsequent use freshly checks the distribution against that receipt and
refuses a changed distribution. The software pin is launcher-owned, never supplied
by a friend's share file. Failed/cancelled bootstrap does not publish a partial engine.

The executable ran only version/help operations. No mod installation, Nexus login,
Steam account use, or game launch occurred. XDG probe directories and process working
directory were private build directories. Reuse verification still passed after the
help commands. The rebuilt packaged launcher subsequently performed its own actual
HTTPS bootstrap into a fresh managed directory, with pinned hash verification and
successful publication. Its 226 progress/result events are recorded in
`build/bootstrap-live-events.jsonl`. Logs/artifacts are retained under Git-ignored
`build/engine-smoke`, `build/bootstrap-live` and `build/research/engine`.
