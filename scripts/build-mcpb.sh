#!/usr/bin/env bash
#
# Assembles the .mcpb bundles that ship with a release.
#
# The bundles are per platform, because a bundle carries one binary and MCPB has
# no way to select between binaries by CPU architecture. macOS gets a single
# bundle because the two macOS binaries are merged into one universal one.
#
# Input   dist/                 the archives built by the release workflow
# Output  mcpb/out/*.mcpb       one bundle per platform
#         mcpb/out/SHA256SUMS.mcpb
#         mcpb/out/server.json  the MCP Registry metadata, hashes included
#
# Usage: scripts/build-mcpb.sh v0.2.0

set -euo pipefail

tag="${1:?usage: build-mcpb.sh <tag, e.g. v0.2.0>}"
version="${tag#v}"
bin="moneylover-mcp"
name="com.iyongbudiarso/moneylover-mcp"
repo="Iyongbudiarso/moneylover-mcp"
# GitHub's numeric repository id. It is stable across renames, so the registry
# can tell this repository apart from a deleted-and-recreated one.
repo_id="1417139962"
# Pinned: this tool decides the bundle layout, so the version is part of the build.
mcpb_cli=(npx --yes @anthropic-ai/mcpb@2.1.2)

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="${DIST_DIR:-$root/dist}"
out="${OUT_DIR:-$root/mcpb/out}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$out"

# Extracts the release archive for one target and prints the path of the binary.
unpack() {
  local target="$1" dir="$work/unpacked/$1" binary
  mkdir -p "$dir"
  if [[ -f "$dist/${bin}-${tag}-${target}.zip" ]]; then
    unzip -q -o "$dist/${bin}-${tag}-${target}.zip" -d "$dir"
  elif [[ -f "$dist/${bin}-${tag}-${target}.tar.gz" ]]; then
    tar -xzf "$dist/${bin}-${tag}-${target}.tar.gz" -C "$dir"
  else
    echo "no archive for target $target in $dist" >&2
    return 1
  fi
  binary="$dir/${bin}-${tag}-${target}/${bin}"
  if [[ -f "$binary.exe" ]]; then
    binary="$binary.exe"
  fi
  if [[ ! -f "$binary" ]]; then
    echo "$target: no $bin in the archive" >&2
    return 1
  fi
  echo "$binary"
}

darwin_arm="$(unpack aarch64-apple-darwin)"
darwin_intel="$(unpack x86_64-apple-darwin)"
linux_arm="$(unpack aarch64-unknown-linux-gnu)"
linux_intel="$(unpack x86_64-unknown-linux-gnu)"
windows="$(unpack x86_64-pc-windows-msvc)"

# One bundle for both Macs: lipo merges the two slices into a universal binary.
# (MCPB cannot pick a binary by architecture, so two darwin bundles would be
# indistinguishable to the client.)
universal="$work/${bin}-darwin"
lipo -create "$darwin_arm" "$darwin_intel" -output "$universal"

# bundle <suffix> <platforms-json> <binary> <entry-file-name>
bundle() {
  local suffix="$1" platforms="$2" binary="$3" entry="$4"
  local dir="$work/bundle/$suffix"
  mkdir -p "$dir/server"
  cp "$binary" "$dir/server/$entry"
  chmod +x "$dir/server/$entry"
  cp "$root/LICENSE" "$dir/LICENSE"

  # The committed manifest is a template: version, platform list, entry point
  # and command are filled in here so the repository copy cannot go stale.
  jq --arg version "$version" \
     --argjson platforms "$platforms" \
     --arg entry "server/$entry" \
     --arg dirname '${__dirname}' \
     '.version = $version
      | .compatibility.platforms = $platforms
      | .server.entry_point = $entry
      | .server.mcp_config.command = ($dirname + "/" + $entry)' \
     "$root/mcpb/manifest.json" > "$dir/manifest.json"

  echo "==> $suffix"
  "${mcpb_cli[@]}" pack "$dir" "$out/${bin}-${suffix}.mcpb"
}

bundle darwin '["darwin"]' "$universal" "$bin"
bundle linux-x86_64 '["linux"]' "$linux_intel" "$bin"
bundle linux-aarch64 '["linux"]' "$linux_arm" "$bin"
bundle windows-x86_64 '["win32"]' "$windows" "$bin.exe"

(
  cd "$out"
  shasum -a 256 ./*.mcpb > SHA256SUMS.mcpb
  cat SHA256SUMS.mcpb
)

python3 - "$out" "$version" "$name" "$repo" "$repo_id" "$tag" <<'PY'
import hashlib
import json
import os
import sys

out, version, name, repo, repo_id, tag = sys.argv[1:7]

packages = []
for suffix in ("darwin", "linux-x86_64", "linux-aarch64", "windows-x86_64"):
    filename = f"moneylover-mcp-{suffix}.mcpb"
    with open(os.path.join(out, filename), "rb") as artifact:
        digest = hashlib.sha256(artifact.read()).hexdigest()
    packages.append(
        {
            "registryType": "mcpb",
            "identifier": f"https://github.com/{repo}/releases/download/{tag}/{filename}",
            "fileSha256": digest,
            "transport": {"type": "stdio"},
        }
    )

server = {
    "$schema": "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
    "name": name,
    "title": "Money Lover",
    "description": "Unofficial Money Lover MCP server: read wallets, categories and transactions, opt-in writes.",
    "version": version,
    "websiteUrl": f"https://github.com/{repo}",
    "repository": {"url": f"https://github.com/{repo}", "source": "github", "id": repo_id},
    "packages": packages,
}

with open(os.path.join(out, "server.json"), "w") as fh:
    json.dump(server, fh, indent=2)
    fh.write("\n")
PY

echo "==> $out/server.json"
