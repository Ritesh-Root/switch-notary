#!/usr/bin/env bash
# Download Switch's published standalone compose file, pinned by digest.
# We fetch it instead of vendoring it (Switch is Apache-2.0 + Commons Clause).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/infra/compose.yml"
# standalone-compose:0.31.0, layer digest (matches switch-v0.31.0 deploy/local/standalone-docker-compose.yml)
DIGEST="sha256:cba238878078cf3d77fd9e136ef432f1ce5fd1bfe597acd172b9faa6720f0121"

if [[ -f "$OUT" ]] && [[ "$(sha256sum "$OUT.orig" 2>/dev/null | cut -d' ' -f1)" == "${DIGEST#sha256:}" ]]; then
  echo "compose.yml already fetched"; exit 0
fi

mkdir -p "$ROOT/infra"
TOKEN="$(curl -fsS "https://ghcr.io/token?scope=repository:sandbox-quantum/standalone-compose:pull" | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')"
curl -fsSL -H "Authorization: Bearer $TOKEN" -o "$OUT.orig" \
  "https://ghcr.io/v2/sandbox-quantum/standalone-compose/blobs/$DIGEST"

GOT="$(sha256sum "$OUT.orig" | cut -d' ' -f1)"
if [[ "$GOT" != "${DIGEST#sha256:}" ]]; then
  echo "digest mismatch: got sha256:$GOT, want $DIGEST" >&2
  rm -f "$OUT.orig"; exit 1
fi

# Fully qualify Docker Hub short names so podman's enforcing short-name mode can't block a pull.
sed -e 's|image: postgres:16-alpine|image: docker.io/library/postgres:16-alpine|' \
    -e 's|image: mattermost/|image: docker.io/mattermost/|' "$OUT.orig" > "$OUT"
echo "fetched Switch standalone compose ($DIGEST) -> infra/compose.yml"
