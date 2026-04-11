#!/usr/bin/env bash
#
# scripts/smoke.sh — end-to-end local smoke test for overacp-server.
#
# Starts the release binary on localhost:8080, mints an admin JWT
# (self-signed with PyJWT), uses it to mint an agent JWT via
# POST /tokens, then drives every REST endpoint and every major
# tunnel dispatch path. Each request is printed with its response
# so the output is easy to read in sequence.
#
# Dependencies on $PATH:
#   - cargo (builds the binary on first run)
#   - curl, jq, uuidgen, openssl
#   - python3 with PyJWT (`pip install PyJWT`)
#   - websocat  (`cargo install websocat`)
#
# Exit 0 on full success, non-zero on any failure.

set -euo pipefail

BASE_URL=${BASE_URL:-http://localhost:8080}
WS_URL=${WS_URL:-ws://localhost:8080}
LOG=${LOG:-/tmp/overacp-smoke.log}

# ── plumbing ─────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null || { echo "missing dep: $1"; exit 1; }; }
for dep in curl jq uuidgen openssl python3 websocat; do need "$dep"; done
python3 -c 'import jwt' 2>/dev/null || {
  echo "missing dep: python3 -m pip install PyJWT"
  exit 1
}

hr() { printf '\n── %s ' "$1"; printf '%.0s─' $(seq 1 $((60 - ${#1}))); echo; }

SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── 0. start server ──────────────────────────────────────────────

hr "0. build + start overacp-server"
cargo build -p overacp-server --release 2>&1 | tail -2

export OVERACP_JWT_SIGNING_KEY="$(openssl rand -hex 32)"
: > "$LOG"
./target/release/overacp-server > "$LOG" 2>&1 &
SERVER_PID=$!
echo "server pid=$SERVER_PID, log=$LOG"

for _ in $(seq 1 20); do
  if curl -fs -o /dev/null "$BASE_URL/healthz" 2>/dev/null; then break; fi
  sleep 0.2
done
curl -fs "$BASE_URL/healthz" >/dev/null || { echo "server didn't come up"; exit 1; }
echo "healthz=ok"

# ── 1. mint an admin JWT ─────────────────────────────────────────

hr "1. self-mint admin JWT"
ADMIN_JWT=$(python3 - <<'PY'
import os, jwt, time, uuid
print(jwt.encode({
    "sub": str(uuid.uuid4()),
    "role": "admin",
    "exp": int(time.time()) + 3600,
    "iss": "overacp",
}, os.environ["OVERACP_JWT_SIGNING_KEY"], algorithm="HS256"))
PY
)
echo "ADMIN_JWT (first 40 chars): ${ADMIN_JWT:0:40}..."

# ── 2. mint an agent JWT via POST /tokens ────────────────────────

hr "2. POST /tokens (admin mints agent JWT)"
AGENT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "request: agent_id=$AGENT_ID"
MINT_RESP=$(curl -fsS -X POST "$BASE_URL/tokens" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"agent_id\": \"$AGENT_ID\", \"ttl_secs\": 300}")
echo "response:"
echo "$MINT_RESP" | jq '{token: (.token[0:40] + "..."), claims}'
AGENT_JWT=$(echo "$MINT_RESP" | jq -r .token)

# ── 3. empty /agents list ────────────────────────────────────────

hr "3. GET /agents (empty — no tunnels yet)"
curl -fsS -H "Authorization: Bearer $ADMIN_JWT" "$BASE_URL/agents" | jq

# ── 4. drive the tunnel dispatch table ───────────────────────────

hr "4. drive tunnel dispatch table"
# Synchronous pipeline: feed five JSON-RPC frames to websocat and
# read the response lines. The trailing `sleep 0.5` keeps stdin
# open long enough to catch the heartbeat's (non-)response before
# websocat closes.
(
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  echo '{"jsonrpc":"2.0","id":3,"method":"quota/check"}'
  echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}'
  echo '{"jsonrpc":"2.0","method":"heartbeat"}'
  sleep 0.5
) | websocat -tE\
    -H="Authorization: Bearer $AGENT_JWT" \
    "$WS_URL/tunnel/$AGENT_ID" 2>/dev/null \
  | while IFS= read -r line; do echo "<- $(echo "$line" | jq -c .)"; done

# ── 5. push while offline → queued → drain on reconnect ─────────

hr "5. REST push while agent is offline → queued → drained on reconnect"
# This is the headline "REST meets tunnel" flow: it exercises the
# MessageQueue + drain path end-to-end over a real WebSocket. The
# "live delivery" path is covered by the Rust test
# `rest_push_delivers_session_message_inline_to_live_tunnel`.

for n in first second; do
  resp=$(curl -fsS -X POST "$BASE_URL/agents/$AGENT_ID/messages" \
    -H "Authorization: Bearer $ADMIN_JWT" \
    -H "Content-Type: application/json" \
    -d "{\"role\":\"user\",\"content\":\"queued-$n\"}")
  echo "push '$n' → $resp"
done

echo "agent state while offline:"
curl -fsS -H "Authorization: Bearer $ADMIN_JWT" \
  "$BASE_URL/agents/$AGENT_ID" | jq '{agent_id, connected, idle_secs}'

echo "reconnecting, reading two drained frames:"
# `sleep 0.5` as stdin keeps the WS alive; `head -n 2` closes the
# pipe after two frames which terminates websocat cleanly.
sleep 0.5 | websocat -tE\
    -H="Authorization: Bearer $AGENT_JWT" \
    "$WS_URL/tunnel/$AGENT_ID" 2>/dev/null \
  | head -n 2 \
  | while IFS= read -r line; do echo "<- $(echo "$line" | jq -c .)"; done

# ── 6. admin registry ops ────────────────────────────────────────

hr "6. admin registry ops (GET /agents, GET /agents/{id}, DELETE)"

echo "GET /agents (agent is recently-disconnected from section 5):"
curl -fsS -H "Authorization: Bearer $ADMIN_JWT" "$BASE_URL/agents" \
  | jq '[.agents[] | {agent_id, connected, idle_secs}]'

echo "GET /agents/{id}:"
curl -fsS -H "Authorization: Bearer $ADMIN_JWT" \
  "$BASE_URL/agents/$AGENT_ID" | jq

echo "DELETE /agents/{id} (no live tunnel, so 404):"
curl -sS -X DELETE -H "Authorization: Bearer $ADMIN_JWT" \
  "$BASE_URL/agents/$AGENT_ID" -o /dev/null -w "http %{http_code}\n"

# ── 7. POST /agents/{id}/cancel ─────────────────────────────────

hr "7. POST /agents/{id}/cancel (202 — best-effort even when offline)"
curl -fsS -X POST -H "Authorization: Bearer $ADMIN_JWT" \
  "$BASE_URL/agents/$AGENT_ID/cancel" -o /dev/null -w "http %{http_code}\n"

# ── 8. auth matrix ──────────────────────────────────────────────

hr "8. auth matrix (status codes only)"
status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

echo "GET  /agents             (no bearer):                 $(status "$BASE_URL/agents")"
echo "GET  /agents             (agent bearer, not admin):   $(status -H "Authorization: Bearer $AGENT_JWT" "$BASE_URL/agents")"
echo "POST /tokens             (agent bearer, not admin):   $(status -X POST -H "Authorization: Bearer $AGENT_JWT" -H 'Content-Type: application/json' -d '{"agent_id":"'"$(uuidgen)"'"}' "$BASE_URL/tokens")"
echo "GET  /agents/<id>        (no bearer):                 $(status "$BASE_URL/agents/$AGENT_ID")"
echo "POST /agents/<id>/cancel (agent bearer, matching id): $(status -X POST -H "Authorization: Bearer $AGENT_JWT" "$BASE_URL/agents/$AGENT_ID/cancel")"

# ── done ─────────────────────────────────────────────────────────

hr "done"
echo "smoke test complete"
