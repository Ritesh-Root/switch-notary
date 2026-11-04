# Shared helpers for the setup scripts. Source it; don't run it.
# Every path is quoted: this repo may live in a folder whose name has spaces.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$ROOT/infra/.env"
SWITCH="${NOTARY_SWITCH_URL:-http://127.0.0.1:8000}"
MM="${NOTARY_MATTERMOST_URL:-http://127.0.0.1:8065}"
STATE_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/switch-notary"
COOKIES="$STATE_DIR/gateway.cookies"

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null || die "$1 is required"; }
need curl; need python3

env_get() { # env_get KEY -> value from infra/.env (never echoed by callers)
  [[ -f "$ENV_FILE" ]] || die "infra/.env is missing; run make up"
  grep -E "^$1=" "$ENV_FILE" | head -1 | cut -d= -f2-
}

json() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

gateway_login() { # logs in as the Switch admin, cookie jar in $COOKIES (0600)
  mkdir -p "$STATE_DIR" && chmod 700 "$STATE_DIR"
  local body
  body="$(EMAIL="$(env_get GATEWAY_ADMIN_EMAIL)" PASS="$(env_get GATEWAY_ADMIN_PASSWORD)" \
    python3 -I -c 'import json,os; print(json.dumps({"email":os.environ["EMAIL"],"password":os.environ["PASS"]}))')"
  ( umask 077; curl -fsS -c "$COOKIES" -H 'Content-Type: application/json' -d "$body" "$SWITCH/gateway/auth/login" >/dev/null ) \
    || die "gateway login failed (is the stack up? make up)"
}

gw() { # gw METHOD PATH [JSON] -> response body; uses the admin cookie
  local method="$1" path="$2" data="${3:-}"
  if [[ -n "$data" ]]; then
    curl -fsS -b "$COOKIES" -X "$method" -H 'Content-Type: application/json' -d "$data" "$SWITCH/gateway$path"
  else
    curl -fsS -b "$COOKIES" -X "$method" "$SWITCH/gateway$path"
  fi
}
