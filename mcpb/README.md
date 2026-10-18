# MCPB bundle

`manifest.json` here is a **template** for the [MCP Bundle](https://github.com/modelcontextprotocol/mcpb)
(`.mcpb`) extensions that ship with each release. It is not directly valid on its own: the entry
point it names (`server/moneylover-mcp`) is a release binary that is not in the repository, so
`mcpb validate mcpb/` fails until a binary is put there.

[`../scripts/build-mcpb.sh`](../scripts/build-mcpb.sh) fills in the parts that differ per platform
— `version`, `compatibility.platforms`, `server.entry_point` and `server.mcp_config.command` — and
then runs the pinned `@anthropic-ai/mcpb` CLI, which validates the manifest and packs the bundle.
That script is what the release workflow calls, so the published bundles are built from this file.

To try it by hand, download the archives of a release into `dist/` (the script needs the
per-target archives, not a bare binary, and it uses `lipo`, so it runs on macOS):

```sh
gh release download v0.3.1 --pattern 'moneylover-mcp-v0.3.1-*' --dir dist
DIST_DIR=dist OUT_DIR=/tmp/out scripts/build-mcpb.sh v0.3.1
```

Publishing the results to the MCP Registry is a separate, manual step; see
[`../scripts/publish-registry.sh`](../scripts/publish-registry.sh).
