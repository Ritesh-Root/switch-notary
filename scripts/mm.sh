#!/usr/bin/env bash
# mm.sh USER CHANNEL MESSAGE: post to a Mattermost channel as a demo user (for scripted runs and spikes).
# Passwords: `user` from infra/.env; priya/sam from scripts/add-users.sh (stored in infra/.env too).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
U="$1"; CH="$2"; MSG="$3"
case "$U" in
  user) PASS="$(env_get MATTERMOST_USER_PASSWORD)";;
  *) PASS="$(env_get "DEMO_PASSWORD_${U^^}")";;
esac
BODY="$(U="$U" P="$PASS" python3 -I -c 'import json,os; print(json.dumps({"login_id":os.environ["U"],"password":os.environ["P"]}))')"
TOKEN="$(curl -fsS -D - -o /dev/null -H 'Content-Type: application/json' -d "$BODY" "$MM/api/v4/users/login" | awk 'tolower($1)=="token:"{print $2}' | tr -d '\r')"
[[ -n "$TOKEN" ]] || die "mattermost login failed for $U"
TEAM="$(curl -fsS -H "Authorization: Bearer $TOKEN" "$MM/api/v4/teams/name/$(env_get MATTERMOST_TEAM_NAME)" | json 'd["id"]')"
CID="$(curl -fsS -H "Authorization: Bearer $TOKEN" "$MM/api/v4/teams/$TEAM/channels/name/$CH" | json 'd["id"]')"
POST="$(CID="$CID" M="$MSG" python3 -I -c 'import json,os; print(json.dumps({"channel_id":os.environ["CID"],"message":os.environ["M"]}))')"
curl -fsS -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$POST" "$MM/api/v4/posts" | json 'd["id"]'
