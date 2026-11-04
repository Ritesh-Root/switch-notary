#!/usr/bin/env bash
# Create the demo people in the bundled Mattermost: priya (the approver) and sam (a requester who
# can't approve). Their passwords go into infra/.env as DEMO_PASSWORD_PRIYA / DEMO_PASSWORD_SAM.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

BODY="$(P="$(env_get MATTERMOST_ADMIN_PASSWORD)" U="$(env_get MATTERMOST_ADMIN_USER)" python3 -I -c 'import json,os; print(json.dumps({"login_id":os.environ["U"],"password":os.environ["P"]}))')"
TOKEN="$(curl -fsS -D - -o /dev/null -H 'Content-Type: application/json' -d "$BODY" "$MM/api/v4/users/login" | awk 'tolower($1)=="token:"{print $2}' | tr -d '\r')"
[[ -n "$TOKEN" ]] || die "mattermost admin login failed"
TEAM="$(curl -fsS -H "Authorization: Bearer $TOKEN" "$MM/api/v4/teams/name/$(env_get MATTERMOST_TEAM_NAME)" | json 'd["id"]')"

for u in priya sam; do
  KEY="DEMO_PASSWORD_${u^^}"
  if ! grep -q "^$KEY=" "$ENV_FILE"; then
    echo "$KEY=Demo-$(openssl rand -hex 8)" >> "$ENV_FILE"
  fi
  PASS="$(env_get "$KEY")"
  ID="$(curl -fsS -H "Authorization: Bearer $TOKEN" "$MM/api/v4/users/username/$u" 2>/dev/null | json 'd["id"]' 2>/dev/null || true)"
  if [[ -z "$ID" ]]; then
    NEW="$(U="$u" P="$PASS" python3 -I -c 'import json,os; u=os.environ["U"]; print(json.dumps({"username":u,"email":u+"@acme.test","password":os.environ["P"],"first_name":u.capitalize()}))')"
    ID="$(curl -fsS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$NEW" "$MM/api/v4/users" | json 'd["id"]')"
  fi
  curl -fsS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"team_id\":\"$TEAM\",\"user_id\":\"$ID\"}" "$MM/api/v4/teams/$TEAM/members" >/dev/null
  echo "mattermost user $u ready"
done
