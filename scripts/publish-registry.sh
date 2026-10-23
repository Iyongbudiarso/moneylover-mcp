#!/usr/bin/env bash
#
# Publishes the released .mcpb bundles to the official MCP Registry.
#
# Run this after the release workflow has finished. It reuses cached bundles,
# re-checks every SHA-256 against server.json, logs in if the registry token is
# missing or about to expire, and finally verifies the published version from
# the consumer side.
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
# Steps 3 (login) is now done by this script; it derives the domain from the
# namespace in server.json.
#
# Usage: scripts/publish-registry.sh <tag, e.g. v0.4.0> [--dry-run] [--force]
#
#   --dry-run  download + verify only, never touch the registry
#   --force    publish even if that version is already listed
#
# Env overrides:
#   MCP_PUBLISH_REPO     owner/repo (default Iyongbudiarso/moneylover-mcp)
#   MCP_REGISTRY_KEY     signing key (default ~/.config/mcp-registry-key.pem)
#   MCP_REGISTRY_DOMAIN  namespace domain (default: parsed from server.json)
#   MCP_PUBLISH_CACHE    artifact cache (default ~/.cache/mcp-publisher)

set -euo pipefail

tag="${1:?usage: publish-registry.sh <tag, e.g. v0.4.0> [--dry-run] [--force]}"
shift || true

dry_run=0
force=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) dry_run=1 ;;
    --force) force=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

repo="${MCP_PUBLISH_REPO:-Iyongbudiarso/moneylover-mcp}"
key="${MCP_REGISTRY_KEY:-$HOME/.config/mcp-registry-key.pem}"
token_file="${XDG_CONFIG_HOME:-$HOME/.config}/mcp-publisher/token.json"
cache="${MCP_PUBLISH_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/mcp-publisher}/${repo//\//__}/$tag"
registry="https://registry.modelcontextprotocol.io/v0"

phase() { printf '\n==> %s  [%ss]\n' "$*" "$SECONDS"; }
die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "curl is required"
command -v mcp-publisher >/dev/null || die "mcp-publisher not on PATH (https://github.com/modelcontextprotocol/registry/releases)"
[[ -f $key ]] || die "signing key not found: $key (run the one-time setup above)"

mkdir -p "$cache"

# ---------------------------------------------------------------- server.json
# Always take server.json from the tag itself: main may already carry the next
# version, and the registry validates exactly what is in the working directory.
if [[ ! -s $cache/server.json ]]; then
  phase "fetching server.json for $tag"
  curl -fsSL --retry 3 --retry-all-errors --connect-timeout 10 --max-time 60 \
    -o "$cache/server.json.part" \
    "https://github.com/$repo/releases/download/$tag/server.json" \
    || die "no server.json asset on release $tag"
  mv "$cache/server.json.part" "$cache/server.json"
fi

read -r name version < <(python3 - "$cache/server.json" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    server = json.load(fh)
print(server["name"], server["version"])
PY
)
[[ -n $name && -n $version ]] || die "server.json is missing name/version"
encoded=$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1], safe=""))' "$name")
echo "    $name $version"

# ------------------------------------------------------- already published?
# Registry entries are immutable per version: publishing twice cannot succeed,
# so fail (or exit) before moving 20+ MB of bundles around.
if (( force == 0 )); then
  status=$(curl -fsS --connect-timeout 10 --max-time 30 "$registry/servers/$encoded/versions/$version" 2>/dev/null \
    | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin)["_meta"]["io.modelcontextprotocol.registry/official"]["status"])
except Exception:
    print("absent")' 2>/dev/null || echo absent)
  if [[ $status == active ]]; then
    echo "    version $version is already active in the registry — nothing to do"
    echo "    (bump the version and cut a new tag; entries are immutable)"
    exit 0
  fi
fi

# --------------------------------------------------------------- artifacts
phase "checking cached bundles in $cache"
python3 - "$cache" <<'PY' > "$cache/.queue"
import hashlib, json, os, sys

cache = sys.argv[1]
with open(os.path.join(cache, "server.json")) as fh:
    server = json.load(fh)

queued = 0
for package in server["packages"]:
    filename = os.path.basename(package["identifier"])
    path = os.path.join(cache, filename)
    if os.path.exists(path):
        with open(path, "rb") as artifact:
            if hashlib.sha256(artifact.read()).hexdigest() == package["fileSha256"]:
                print(f"  cached  {filename}", file=sys.stderr)
                continue
        print(f"  stale   {filename}", file=sys.stderr)
    else:
        print(f"  missing {filename}", file=sys.stderr)
    print(f"{package['identifier']}\t{path}")
    queued += 1
PY

if [[ -s $cache/.queue ]]; then
  phase "downloading $(wc -l < "$cache/.queue") bundle(s) in parallel"
  # Each worker downloads to .part and only then renames, so a stalled or
  # truncated transfer can never be mistaken for a cached artifact.
  while IFS=$'\t' read -r url path; do
    [[ -n $url ]] || continue
    (
      curl -fsSL --retry 3 --retry-all-errors --connect-timeout 10 --max-time 240 \
        -o "$path.part" "$url" && mv "$path.part" "$path"
    ) &
    while (( $(jobs -rp | wc -l) >= 4 )); do wait -n || true; done
  done < "$cache/.queue"
  wait
fi
rm -f "$cache/.queue"

phase "verifying every bundle against server.json"
python3 - "$cache" <<'PY'
import hashlib, json, os, sys

cache = sys.argv[1]
with open(os.path.join(cache, "server.json")) as fh:
    server = json.load(fh)

for package in server["packages"]:
    filename = os.path.basename(package["identifier"])
    path = os.path.join(cache, filename)
    if not os.path.exists(path):
        raise SystemExit(f"{filename}: not downloaded")
    with open(path, "rb") as artifact:
        digest = hashlib.sha256(artifact.read()).hexdigest()
    if digest != package["fileSha256"]:
        raise SystemExit(
            f"{filename}: sha256 mismatch\n"
            f"  server.json: {package['fileSha256']}\n"
            f"  download:    {digest}"
        )
    print(f"  ok  {filename}")
PY

if (( dry_run == 1 )); then
  echo "    --dry-run: stopping before publishing"
  exit 0
fi

# -------------------------------------------------------------------- login
# The registry JWT lives ~5 minutes, so log in right before publishing instead
# of once per session. Reusing the same key keeps the DNS TXT record valid.
phase "checking registry token"
if python3 - "$token_file" "$name" <<'PY'
import base64, json, sys, time

token_file, name = sys.argv[1], sys.argv[2]
try:
    with open(token_file) as fh:
        token = json.load(fh)["token"]
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    claims = json.loads(base64.urlsafe_b64decode(payload))
except Exception:
    sys.exit(1)

if claims.get("exp", 0) - time.time() < 120:
    sys.exit(1)
if not any(p.get("resource", "").startswith(name.split("/")[0]) for p in claims.get("permissions", [])):
    sys.exit(1)
print(f"    token ok, valid for {int(claims['exp'] - time.time())}s")
PY
then
  :
else
  base="${name%%/*}"
  case "$name" in
    com.*/*)
      # Namespace is the reversed domain: com.iyongbudiarso -> iyongbudiarso.com
      if [[ -n ${MCP_REGISTRY_DOMAIN:-} ]]; then
        domain="$MCP_REGISTRY_DOMAIN"
      else
        domain=$(python3 -c 'import sys;print(".".join(reversed(sys.argv[1].split("."))))' "$base")
      fi
      ;;
    io.github.*/*) die "io.github namespace needs 'mcp-publisher login github' — do that manually, then rerun" ;;
    *) die "unsupported namespace: $name" ;;
  esac
  keyhex=$(openssl pkey -in "$key" -outform DER | tail -c 32 | od -An -tx1 -v | tr -d ' \n')
  [[ ${#keyhex} -eq 64 ]] || die "could not derive the ed25519 hex key from $key"
  echo "    logging in as $domain (token expired or missing)"
  mcp-publisher login dns --domain="$domain" --private-key="$keyhex" >/dev/null
  echo "    logged in"
fi

# ------------------------------------------------------------------ publish
cd "$cache"
phase "validating"
mcp-publisher validate
phase "publishing $version"
mcp-publisher publish

# ------------------------------------------------- verify as a consumer would
phase "verifying from the registry API"
curl -fsS --retry 3 --connect-timeout 10 --max-time 30 \
  "$registry/servers/$encoded/versions/latest" \
  | python3 -c 'import json,sys
data = json.load(sys.stdin)
entry, meta = data["server"], data["_meta"]["io.modelcontextprotocol.registry/official"]
print("    latest: %s (%s), %d package(s)" % (entry["version"], meta["status"], len(entry.get("packages", []))))'


printf '\n✅ done in %ss\n' "$SECONDS"
