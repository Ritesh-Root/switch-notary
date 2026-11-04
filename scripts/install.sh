#!/usr/bin/env bash
# Wire a desk into Switch and write infra/notary.env.
#
#   scripts/install.sh offline   rooms demo-desk/demo-work with scripted builder + reviewer (no model keys)
#   scripts/install.sh live      rooms acme-desk/acme-work, created by applying templates/agora-desk.template.yaml
#                                in Switch Console (Claude Code builder, OpenCode reviewer)
#
# Both: add priya and sam to the desk, give the Notary an explicit addressing policy (its owner and
# the owner's agents anywhere, any person in the desk), and pick the evidence settings.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

MODE="${1:-}"
case "$MODE" in
  offline) TEAM=demo; BUILDER=demo-builder; REVIEWER=demo-reviewer; REPO_URL="$("$ROOT/scripts/seed-demo.sh")";;
  live) TEAM=acme; BUILDER=acme-builder; REVIEWER=acme-reviewer; REPO_URL="${NOTARY_REPO_URL:-https://github.com/Ritesh-Root/acme-invoices.git}";;
  *) die "usage: scripts/install.sh offline|live";;
esac
ENGINE="${NOTARY_SANDBOX_ENGINE:?set NOTARY_SANDBOX_ENGINE to podman or docker}"
NOTARY_KEY_FILE="$STATE_DIR/agent.json"
[[ -f "$NOTARY_KEY_FILE" ]] || die "register the Notary first: scripts/register.sh"
NOTARY_ID="$(json 'd["id"]' < "$NOTARY_KEY_FILE")"
gateway_login

register_scripted() { # the keyless demo's builder and reviewer: plain HTTP agents with no model
  local file="$STATE_DIR/scripted.json"
  [[ -f "$file" ]] || echo '{}' > "$file"
  local reg_key
  reg_key="$(gw POST /api-keys '{"label":"agora-notary-scripted"}' | json 'd["key"]')"
  for name in "$BUILDER" "$REVIEWER"; do
    if python3 -I -c "import json,sys; sys.exit(0 if sys.argv[2] in json.load(open(sys.argv[1])) else 1)" "$file" "$name"; then continue; fi
    local body out
    body="$(N="$name" python3 -I -c 'import json,os; print(json.dumps({"name":os.environ["N"],"display_name":os.environ["N"],
      "description":"Scripted agent for the Agora Notary keyless demo (no model): fault injection on purpose",
      "connector_type":"scripted","integration_profile":{"connection_model":"always_on","message_exchange":True,
      "pre_invocation_mediation":[],"post_invocation_mediation":[],"event_reporting":[]},"tools":[],"models":[],"metadata":{},"overwrite":True}))')"
    out="$(curl -fsS -H "Authorization: Bearer $reg_key" -H 'Content-Type: application/json' -d "$body" "$SWITCH/agents")"
    ( umask 077; OUT="$out" N="$name" python3 -I -c 'import json,os,sys; f=sys.argv[1]; d=json.load(open(f)); d[os.environ["N"]]=json.loads(os.environ["OUT"]); json.dump(d,open(f,"w"))' "$file" )
    echo "registered scripted agent $name"
  done
}

room_id() { gw GET "/rooms?search=$1" | N="$1" python3 -I -c 'import json,os,sys; print(next((r["id"] for r in json.load(sys.stdin) if r["name"]==os.environ["N"]),""))'; }

[[ "$MODE" == offline ]] && register_scripted

HUB="$(room_id "$TEAM-desk")"
if [[ -z "$HUB" ]]; then
  [[ "$MODE" == offline ]] || die "no room $TEAM-desk: apply templates/agora-desk.template.yaml in Switch Console first"
  BODY="$(T="$TEAM" B="$BUILDER" R="$REVIEWER" python3 -I -c 'import json,os,sys; print(json.dumps({"yaml":open(sys.argv[1]).read(),
    "inputs":{"team":os.environ["T"],"builder":os.environ["B"],"reviewer":os.environ["R"],"notary":"agora-notary"},"template_name":"agora notary desk"}))' "$ROOT/templates/desk.rooms.yaml")"
  gw POST /rooms/from-yaml "$BODY" >/dev/null
  HUB="$(room_id "$TEAM-desk")"
fi
WORK="$(room_id "$TEAM-work")"
[[ -n "$HUB" && -n "$WORK" ]] || die "couldn't find $TEAM-desk and $TEAM-work"
echo "rooms: $TEAM-desk=$HUB $TEAM-work=$WORK"

gw POST "/rooms/$HUB/users" '{"user_names":["priya","sam"]}' >/dev/null && echo "priya and sam are in $TEAM-desk"

# The Notary reads the work room's tool activity as the gateway admin, who must be a chat member there.
ME="$(gw GET /auth/me | json 'd["id"]')"
gw POST "/chats/$WORK/members" "{\"userId\":\"$ME\"}" >/dev/null && echo "gateway admin can read $TEAM-work activity"

POLICY="$(HUB="$HUB" python3 -I -c 'import json,os; print(json.dumps({"policy":{"rules":[
  {"rooms":"*","room_groups":"*","users":[],"agents":[],"owner":True,"owner_agents":True,"platform":False},
  {"rooms":[os.environ["HUB"]],"room_groups":"*","users":"*","agents":[],"owner":False,"owner_agents":False,"platform":False}]}}))')"
gw PUT "/agents/$NOTARY_ID/addressing-policy" "$POLICY" >/dev/null && echo "notary addressing policy set (owner + owner's agents; anyone in $TEAM-desk)"

ENVF="$ROOT/infra/notary.env"
IMAGE="$(grep -E '^NOTARY_SANDBOX_IMAGE=' "$ENVF" 2>/dev/null | cut -d= -f2- || true)"
[[ -n "$IMAGE" ]] || die "build the sandbox image first: make sandbox-image"
cat > "$ENVF" <<EOF
# Written by scripts/install.sh $MODE
NOTARY_HUB_ROOM=$HUB
NOTARY_WORK_ROOM=$WORK
NOTARY_BUILDER=$BUILDER
NOTARY_REVIEWER=$REVIEWER
NOTARY_REPO_URL=$REPO_URL
NOTARY_SANDBOX_ENGINE=$ENGINE
NOTARY_SANDBOX_IMAGE=$IMAGE
NOTARY_APPROVAL=${NOTARY_APPROVAL:-card}
NOTARY_ACTIVITY=${NOTARY_ACTIVITY:-on}
NOTARY_DATA_DIR=${NOTARY_DATA_DIR:-$HOME/.local/share/agora-notary/$MODE}
EOF
echo "wrote infra/notary.env ($MODE)"
