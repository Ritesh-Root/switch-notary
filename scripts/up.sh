#!/usr/bin/env bash
# Bring up the pinned Switch stack (switch-core 0.31.0 + bundled Mattermost), register the Notary
# and create the demo people. Safe to run again.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"$ROOT/scripts/fetch-switch.sh"
"$ROOT/scripts/gen-env.sh"
cd "$ROOT/infra"
docker compose --env-file .env -f compose.yml --profile collab up -d 2>&1 | grep -v -i "external compose provider" || true
echo "waiting for Switch's setup container (Mattermost seeding, bridge registration)..."
for _ in $(seq 1 120); do
  if docker compose --env-file .env -f compose.yml --profile collab logs setup 2>/dev/null | grep -q "Setup complete!"; then break; fi
  sleep 2
done
curl -fsS http://127.0.0.1:8000/health >/dev/null || { echo "Switch isn't healthy on :8000" >&2; exit 1; }
echo "Switch is up: http://localhost:8000 (dashboard + API), Mattermost http://localhost:8065"
"$ROOT/scripts/register.sh"
"$ROOT/scripts/add-users.sh"
