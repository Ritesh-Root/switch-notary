#!/usr/bin/env bash
# Register the Notary as an always-on Switch agent, owned by the Switch admin.
# It uses the admin's personal registration key, not AGENT_REGISTRATION_TOKEN: agents registered
# with the bootstrap token belong to a synthetic account, so people can't address them and the
# builder's owner-agents policy refuses them.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

NAME="${NOTARY_AGENT_NAME:-agora-notary}"
KEY_FILE="$STATE_DIR/agent.json"

if [[ -f "$KEY_FILE" ]]; then
  ID="$(json 'd["id"]' < "$KEY_FILE")"
  KEY="$(json 'd["api_key"]' < "$KEY_FILE")"
  if curl -fsS -o /dev/null -H "Authorization: Bearer $KEY" "$SWITCH/agents/$ID/ops"; then
    echo "$NAME already registered ($ID)"; exit 0
  fi
  echo "saved key for $NAME no longer works; registering again" >&2
fi

gateway_login
REG_KEY="$(gw POST /api-keys '{"label":"agora-notary"}' | json 'd["key"]')"

BODY="$(NAME="$NAME" python3 -I -c 'import json,os; print(json.dumps({
  "name": os.environ["NAME"], "display_name": "Notary",
  "description": "Checks every claim agents make against git, its own test run and Switch tool activity before people see it",
  "connector_type": "agora-notary",
  "integration_profile": {"connection_model": "always_on", "message_exchange": True,
    "pre_invocation_mediation": [], "post_invocation_mediation": [], "event_reporting": []},
  "tools": [], "models": [], "metadata": {}, "overwrite": True}))')"

umask 077
curl -fsS -H "Authorization: Bearer $REG_KEY" -H 'Content-Type: application/json' -d "$BODY" "$SWITCH/agents" > "$KEY_FILE.tmp"
mv "$KEY_FILE.tmp" "$KEY_FILE"
echo "registered $NAME ($(json 'd["id"]' < "$KEY_FILE")); key saved to $KEY_FILE"
