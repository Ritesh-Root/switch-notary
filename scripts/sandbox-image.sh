#!/usr/bin/env bash
# Build the test sandbox image and pin its id in infra/notary.env (NOTARY_SANDBOX_IMAGE).
# Engine is explicit: NOTARY_SANDBOX_ENGINE=podman|docker (no auto-detection).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${NOTARY_SANDBOX_ENGINE:?set NOTARY_SANDBOX_ENGINE to podman or docker}"
case "$ENGINE" in podman|docker) ;; *) echo "NOTARY_SANDBOX_ENGINE must be podman or docker" >&2; exit 1;; esac
"$ENGINE" build -q -t agora-notary-sandbox:1 -f "$ROOT/sandbox/Containerfile" "$ROOT/sandbox" >/dev/null
ID="$("$ENGINE" image inspect --format '{{.Id}}' agora-notary-sandbox:1)"
ID="${ID#sha256:}"
ENVF="$ROOT/infra/notary.env"
touch "$ENVF"
grep -v '^NOTARY_SANDBOX_IMAGE=' "$ENVF" > "$ENVF.tmp" || true
echo "NOTARY_SANDBOX_IMAGE=$ID" >> "$ENVF.tmp" && mv "$ENVF.tmp" "$ENVF"
echo "sandbox image $ID"
