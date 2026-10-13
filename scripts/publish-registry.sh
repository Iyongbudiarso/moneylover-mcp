#!/usr/bin/env bash
#
# Publishes the released .mcpb bundles to the official MCP Registry.
#
# Run this after the release workflow has finished. It downloads the bundles and
# server.json from the GitHub release, re-checks every SHA-256, and publishes.
#
# Publishing is deliberately manual: it needs a private key that must never live
# in this repository or in CI. One-time setup:
#
#   # 1. Generate a signing key and keep it outside the checkout.
#   openssl genpkey -algorithm Ed25519 -out ~/.config/mcp-registry-key.pem
#   chmod 600 ~/.config/mcp-registry-key.pem
#
#   # 2. Publish the public half as a TXT record on the apex domain
#   #    (iyongbudiarso.com, not a subdomain), next to any existing records:
#   openssl pkey -in ~/.config/mcp-registry-key.pem -pubout -outform DER | tail -c 32 | base64
#   #    iyongbudiarso.com.  IN TXT  "v=MCPv1; k=ed25519; p=<base64 output>"
#
#   # 3. Log in. This grants com.iyongbudiarso/* , the namespace used by
#   #    com.iyongbudiarso/moneylover-mcp.
#   mcp-publisher login dns --domain=iyongbudiarso.com \
#     --private-key="$(openssl pkey -in ~/.config/mcp-registry-key.pem -noout -text \
#                        | grep -A3 'priv:' | tail -n +2 | tr -d ' :\n')"
#
# Usage: scripts/publish-registry.sh v0.2.0

set -euo pipefail

tag="${1:?usage: publish-registry.sh <tag, e.g. v0.2.0>}"
repo="Iyongbudiarso/moneylover-mcp"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "==> downloading the released bundles from $tag"
gh release download "$tag" --repo "$repo" --dir "$work" \
  --pattern '*.mcpb' --pattern 'server.json'

echo "==> checking every bundle against the hash recorded in server.json"
python3 - "$work" <<'PY'
import hashlib
import json
import os
import sys

work = sys.argv[1]
with open(os.path.join(work, "server.json")) as fh:
    server = json.load(fh)

for package in server["packages"]:
    filename = os.path.basename(package["identifier"])
    with open(os.path.join(work, filename), "rb") as artifact:
        digest = hashlib.sha256(artifact.read()).hexdigest()
    if digest != package["fileSha256"]:
        raise SystemExit(
            f"{filename}: sha256 mismatch\n"
            f"  server.json: {package['fileSha256']}\n"
            f"  download:    {digest}"
        )
    print(f"  ok  {filename}")
PY

cd "$work"

echo "==> validating server.json"
mcp-publisher validate

echo "==> publishing $tag"
mcp-publisher publish
