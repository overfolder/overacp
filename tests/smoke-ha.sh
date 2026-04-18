#!/usr/bin/env bash
#
# tests/smoke-ha.sh — two-instance HA smoke test for overacp-server.
#
# Starts a dockerized Valkey instance, then two overacp-server binaries
# on different ports sharing the same Valkey. Proves:
#
#   1. Cross-instance push: POST to server B, agent on server A receives.
#   2. Cross-instance SSE: SSE subscriber on B sees frames from A.
#   3. Failover: kill server A, reconnect agent to B, messages route.
#
# Dependencies on $PATH:
#   - docker, curl, jq, uuidgen, openssl
#   - python3 with PyJWT (`pip install PyJWT`)
#   - websocat  (`cargo install websocat`)
#
# Environment:
#   - TARGET_DIR (optional, default ./target/debug)
#
# Exit 0 on full success, non-zero on any failure.

set -euo pipefail

TARGET_DIR=${TARGET_DIR:-./target/debug}
VALKEY_PORT=6399
ALPHA_PORT=8181
BETA_PORT=8182
LOG_ALPHA=/tmp/overacp-ha-alpha.log
LOG_BETA=/tmp/overacp-ha-beta.log

# ── plumbing ─────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null || { echo "missing dep: $1"; exit 1; }; }
for dep in docker curl jq uuidgen openssl python3 websocat; do need "$dep"; done
python3 -c 'import jwt' 2>/dev/null || {
  echo "missing dep: python3 -m pip install PyJWT"
  exit 1
}

hr() { printf '\n── %s ' "$1"; printf '%.0s─' $(seq 1 $((60 - ${#1}))); echo; }
fail() { echo "FAIL: $1"; exit 1; }

ALPHA_PID="" BETA_PID="" VALKEY_CID=""
cleanup() {
  [ -n "$ALPHA_PID" ] && kill "$ALPHA_PID" 2>/dev/null || true
  [ -n "$BETA_PID" ]  && kill "$BETA_PID" 2>/dev/null  || true
  [ -n "$VALKEY_CID" ] && docker rm -f "$VALKEY_CID" >/dev/null 2>&1 || true
  # Kill any leftover background jobs (websocat, curl)
  jobs -p 2>/dev/null | xargs -r kill 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── 0. start Valkey ─────────────────────────────────────────────

hr "0. start Valkey"
VALKEY_CID=$(docker run -d --rm -p "$VALKEY_PORT:6379" valkey/valkey:8)
echo "valkey container=$VALKEY_CID port=$VALKEY_PORT"

# Wait for Valkey to be ready.
for _ in $(seq 1 30); do
  docker exec "$VALKEY_CID" valkey-cli ping 2>/dev/null | grep -q PONG && break
  sleep 0.2
done
docker exec "$VALKEY_CID" valkey-cli ping | grep -q PONG || fail "valkey didn't come up"
echo "valkey=PONG"

# ── 1. start two overacp-server instances ────────────────────────

hr "1. start alpha + beta"
export OVERACP_JWT_SIGNING_KEY="$(openssl rand -hex 32)"
REDIS_URL="redis://localhost:$VALKEY_PORT"

OVERACP_REDIS_URL="$REDIS_URL" \
OVERACP_INSTANCE_ID=alpha \
PORT="$ALPHA_PORT" \
  "$TARGET_DIR/overacp-server" > "$LOG_ALPHA" 2>&1 &
ALPHA_PID=$!

OVERACP_REDIS_URL="$REDIS_URL" \
OVERACP_INSTANCE_ID=beta \
PORT="$BETA_PORT" \
  "$TARGET_DIR/overacp-server" > "$LOG_BETA" 2>&1 &
BETA_PID=$!

echo "alpha pid=$ALPHA_PID port=$ALPHA_PORT"
echo "beta  pid=$BETA_PID  port=$BETA_PORT"

for port in $ALPHA_PORT $BETA_PORT; do
  for _ in $(seq 1 40); do
    curl -fs "http://localhost:$port/healthz" >/dev/null 2>&1 && break
    sleep 0.2
  done
  curl -fs "http://localhost:$port/healthz" >/dev/null \
    || fail "server on port $port didn't come up"
  echo "  port=$port healthz=ok"
done

# ── 2. mint JWTs ────────────────────────────────────────────────

hr "2. mint JWTs"
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

AGENT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
MINT_RESP=$(curl -fsS -X POST "http://localhost:$ALPHA_PORT/tokens" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"agent_id\": \"$AGENT_ID\", \"ttl_secs\": 300}")
AGENT_JWT=$(echo "$MINT_RESP" | jq -r .token)
echo "agent_id=$AGENT_ID"

# ── 3. connect agent to ALPHA ──────────────────────────────────

hr "3. connect agent to ALPHA"
# Agent echoes any session/message back as stream/textDelta, then
# logs the raw frame to a file for assertion.
AGENT_OUT=/tmp/overacp-ha-agent.out
: > "$AGENT_OUT"

(
  websocat -tE \
    -H="Authorization: Bearer $AGENT_JWT" \
    "ws://localhost:$ALPHA_PORT/tunnel/$AGENT_ID" 2>/dev/null \
  | while IFS= read -r line; do
      echo "$line" >> "$AGENT_OUT"
      # Echo stream/textDelta for any session/message received.
      if echo "$line" | jq -e '.method == "session/message"' >/dev/null 2>&1; then
        echo '{"jsonrpc":"2.0","method":"stream/textDelta","params":{"delta":"echo"}}'
      fi
    done
) &
AGENT_WS_PID=$!
sleep 0.5

# Verify ALPHA sees the agent.
curl -fsS -H "Authorization: Bearer $ADMIN_JWT" \
  "http://localhost:$ALPHA_PORT/agents" | jq -e ".agents | length == 1" >/dev/null \
  || fail "agent not registered on alpha"
echo "agent registered on alpha"

# ── 4. CROSS-INSTANCE PUSH ─────────────────────────────────────

hr "4. cross-instance push: POST to BETA, agent on ALPHA"
curl -fsS -X POST "http://localhost:$BETA_PORT/agents/$AGENT_ID/messages" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d '{"role":"user","content":{"text":"cross-instance hello"}}' | jq -c .

sleep 1
grep -q "cross-instance hello" "$AGENT_OUT" \
  || fail "cross-instance message did not reach agent on alpha"
echo "cross-instance push: OK"

# ── 5. CROSS-INSTANCE SSE ──────────────────────────────────────

hr "5. cross-instance SSE: subscribe via BETA"
SSE_OUT=/tmp/overacp-ha-sse.out
: > "$SSE_OUT"

timeout 4 curl -sNf \
  -H "Authorization: Bearer $ADMIN_JWT" \
  "http://localhost:$BETA_PORT/agents/$AGENT_ID/stream" > "$SSE_OUT" 2>/dev/null &
SSE_PID=$!
sleep 0.5

# Trigger a stream/textDelta by pushing a message via ALPHA.
curl -fsS -X POST "http://localhost:$ALPHA_PORT/agents/$AGENT_ID/messages" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d '{"role":"user","content":{"text":"trigger echo"}}' >/dev/null

sleep 1
wait $SSE_PID 2>/dev/null || true

grep -q "textDelta" "$SSE_OUT" \
  || fail "SSE on BETA did not receive ALPHA's stream fan-out"
echo "cross-instance SSE: OK"

# ── 6. FAILOVER ────────────────────────────────────────────────

hr "6. failover: kill ALPHA, reconnect to BETA"
kill "$ALPHA_PID" && wait "$ALPHA_PID" 2>/dev/null || true
ALPHA_PID=""
kill "$AGENT_WS_PID" 2>/dev/null || true
echo "alpha killed"

# Wait for lease to expire or be released.
sleep 2

# Reconnect agent to BETA.
AGENT_OUT2=/tmp/overacp-ha-agent2.out
: > "$AGENT_OUT2"

(
  websocat -tE \
    -H="Authorization: Bearer $AGENT_JWT" \
    "ws://localhost:$BETA_PORT/tunnel/$AGENT_ID" 2>/dev/null \
  | while IFS= read -r line; do
      echo "$line" >> "$AGENT_OUT2"
    done
) &
sleep 0.5

# Push via BETA (which is now the only instance).
curl -fsS -X POST "http://localhost:$BETA_PORT/agents/$AGENT_ID/messages" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d '{"role":"user","content":{"text":"failed over"}}' | jq -c .

sleep 1
grep -q "failed over" "$AGENT_OUT2" \
  || fail "failover to BETA did not route new message"
echo "failover: OK"

# ── done ────────────────────────────────────────────────────────

hr "RESULT"
echo "OK: cross-instance push, SSE fan-out, and failover all work."
