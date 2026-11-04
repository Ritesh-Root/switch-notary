#!/usr/bin/env bash
# Run the notary binary with infra/.env and infra/notary.env loaded: scripts/notary.sh run|doctor
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ -f "$ROOT/infra/.env" && -f "$ROOT/infra/notary.env" ]] || { echo "run make up and make install-offline (or install-live) first" >&2; exit 1; }
set -a; . "$ROOT/infra/.env"; . "$ROOT/infra/notary.env"; set +a
BIN="$ROOT/target/release/notary"
[[ -x "$BIN" ]] || cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml" -p notary
exec "$BIN" "$@"
