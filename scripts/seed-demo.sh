#!/usr/bin/env bash
# Seed a local bare copy of the demo repo (for demo-offline and CI): one commit on main, from demo/acme-invoices.
# Prints the file:// URL to use as NOTARY_REPO_URL.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="${NOTARY_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/switch-notary}"
BARE="$DATA/acme-invoices.git"
if [[ ! -d "$BARE" ]]; then
  mkdir -p "$DATA"
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  cp -r "$ROOT/demo/acme-invoices/." "$WORK/"
  git -C "$WORK" init -q -b main
  git -C "$WORK" add -A
  git -C "$WORK" -c user.name="Acme Finance" -c user.email="finance@acme.test" commit -q -m "acme-invoices 0.3.0"
  git clone -q --bare "$WORK" "$BARE"
fi
echo "file://$BARE"
