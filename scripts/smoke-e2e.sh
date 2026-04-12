#!/usr/bin/env bash
#
# scripts/smoke-e2e.sh — full-stack end-to-end smoke test.
#
# Drives the real pipeline: overacp-server (broker) + overacp-agent
# (supervisor) + overloop (child agent) + a real OpenAI-compatible
# LLM endpoint. POSTs a REST message, subscribes to the SSE stream,
# and asserts that a `turn/end` notification fans out with nonzero
# usage. This is the "is the whole stack alive" smoke test.
#
# The broker-only regression path (with a websocat fake agent)
# remains at scripts/smoke.sh.
#
# Dependencies on $PATH:
#   - cargo (builds the binaries on first run)
#   - curl, jq, uuidgen, openssl
#   - python3 with PyJWT (`pip install PyJWT`)
#
# Environment:
#   - LLM_API_KEY (required) — OpenAI-compatible API key, loaded
#     from .env if present. The script fails loudly if unset.
#   - LLM_API_URL (optional, default https://openrouter.ai/api/v1)
#   - OVERFOLDER_MODEL (optional, default anthropic/claude-sonnet-4-20250514)
#
# Exit 0 on full success, non-zero on any failure.

set -euo pipefail

BASE_URL=${BASE_URL:-http://localhost:8080}
SERVER_LOG=${SERVER_LOG:-/tmp/overacp-smoke-e2e-server.log}
AGENT_LOG=${AGENT_LOG:-/tmp/overacp-smoke-e2e-agent.log}
SSE_LOG=${SSE_LOG:-/tmp/overacp-smoke-e2e-sse.log}

# Logging. Set RUST_LOG to override. The default enables trace-level
# output so full JSON-RPC message payloads are printed to the server
# and agent logs.
export RUST_LOG="${RUST_LOG:-overacp_server::tunnel=trace,overacp_server=info,overacp_agent::bridge=trace,overacp_agent=info,overloop=info}"

# ── plumbing ─────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null || { echo "missing dep: $1"; exit 1; }; }
for dep in curl jq uuidgen openssl python3 cargo; do need "$dep"; done
python3 -c 'import jwt' 2>/dev/null || {
  echo "missing dep: python3 -m pip install PyJWT"
  exit 1
}

hr() { printf '\n── %s ' "$1"; printf '%.0s─' $(seq 1 $((60 - ${#1}))); echo; }

SERVER_PID=""
AGENT_PID=""
SSE_PID=""
WORKSPACE_DIR=""
cleanup() {
  [ -n "$SSE_PID" ] && kill "$SSE_PID" 2>/dev/null || true
  [ -n "$AGENT_PID" ] && kill "$AGENT_PID" 2>/dev/null || true
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  [ -n "$WORKSPACE_DIR" ] && rm -rf "$WORKSPACE_DIR" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── 0. load .env + verify LLM credentials ───────────────────────

hr "0. load .env + verify LLM_API_KEY"
if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
  echo ".env sourced"
else
  echo "no .env in CWD — relying on inherited environment"
fi

if [ -z "${LLM_API_KEY:-}" ]; then
  echo "error: LLM_API_KEY is required (set it in .env or export it)"
  exit 1
fi

: "${LLM_API_URL:=https://openrouter.ai/api/v1}"
: "${OVERFOLDER_MODEL:=anthropic/claude-sonnet-4-20250514}"
echo "LLM_API_URL=$LLM_API_URL"
echo "OVERFOLDER_MODEL=$OVERFOLDER_MODEL"

# ── 1. build release binaries ────────────────────────────────────

hr "1. build release binaries"
cargo build --release -p overacp-server -p overacp-agent -p overloop 2>&1 | tail -3

# ── 2. start overacp-server ─────────────────────────────────────

hr "2. start overacp-server"
export OVERACP_JWT_SIGNING_KEY="$(openssl rand -hex 32)"
: > "$SERVER_LOG"
./target/release/overacp-server > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!
echo "server pid=$SERVER_PID, log=$SERVER_LOG"

for _ in $(seq 1 40); do
  if curl -fs -o /dev/null "$BASE_URL/healthz" 2>/dev/null; then break; fi
  sleep 0.2
done
curl -fs "$BASE_URL/healthz" >/dev/null || { echo "server didn't come up"; exit 1; }
echo "healthz=ok"

# ── 3. mint admin JWT ────────────────────────────────────────────

hr "3. self-mint admin JWT"
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

# ── 4. mint agent JWT via POST /tokens ──────────────────────────

hr "4. POST /tokens (admin mints agent JWT)"
AGENT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "agent_id=$AGENT_ID"
MINT_RESP=$(curl -fsS -X POST "$BASE_URL/tokens" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"agent_id\": \"$AGENT_ID\", \"ttl_secs\": 600}")
AGENT_JWT=$(echo "$MINT_RESP" | jq -r .token)
echo "AGENT_JWT (first 40 chars): ${AGENT_JWT:0:40}..."

# ── 5. start SSE subscriber ─────────────────────────────────────

hr "5. start SSE subscriber in background"
: > "$SSE_LOG"
curl -sN "$BASE_URL/agents/$AGENT_ID/stream" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  > "$SSE_LOG" 2>&1 &
SSE_PID=$!
echo "sse pid=$SSE_PID, log=$SSE_LOG"
# Give SSE a moment to register as a subscriber before the agent
# starts emitting frames.
sleep 0.5

# ── 6. start overacp-agent supervisor ───────────────────────────

hr "6. start overacp-agent supervisor"
WORKSPACE_DIR=$(mktemp -d)
: > "$AGENT_LOG"
OVERACP_TOKEN="$AGENT_JWT" \
OVERACP_SERVER_URL="$BASE_URL" \
OVERACP_WORKSPACE="$WORKSPACE_DIR" \
OVERACP_AGENT_BINARY="$(pwd)/target/release/overloop" \
LLM_API_KEY="$LLM_API_KEY" \
LLM_API_URL="$LLM_API_URL" \
OVERFOLDER_MODEL="$OVERFOLDER_MODEL" \
  ./target/release/overacp-agent > "$AGENT_LOG" 2>&1 &
AGENT_PID=$!
echo "agent pid=$AGENT_PID, workspace=$WORKSPACE_DIR, log=$AGENT_LOG"

# Poll GET /agents/{id} until the tunnel is connected.
hr "7. wait for tunnel connected"
CONNECTED=false
for _ in $(seq 1 50); do
  state=$(curl -fsS -H "Authorization: Bearer $ADMIN_JWT" \
    "$BASE_URL/agents/$AGENT_ID" 2>/dev/null | jq -r '.connected // false')
  if [ "$state" = "true" ]; then
    CONNECTED=true
    break
  fi
  sleep 0.2
done
if [ "$CONNECTED" != "true" ]; then
  echo "tunnel never came up; agent log:"
  tail -20 "$AGENT_LOG" || true
  exit 1
fi
echo "tunnel connected"

# ── 8. push a message via REST ──────────────────────────────────

hr "8. POST /agents/{id}/messages"
PUSH_RESP=$(curl -fsS -X POST "$BASE_URL/agents/$AGENT_ID/messages" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d '{"role":"user","content":"Reply with exactly the single word HELLO and nothing else."}')
echo "push → $PUSH_RESP"

# ── 9. wait for turn/end on SSE ─────────────────────────────────

hr "9. wait up to 120s for turn/end on SSE"
DEADLINE=$(( $(date +%s) + 120 ))
TURN_END_LINE=""
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  TURN_END_LINE=$(grep -m1 '"turn/end"' "$SSE_LOG" || true)
  if [ -n "$TURN_END_LINE" ]; then break; fi
  sleep 0.5
done

if [ -z "$TURN_END_LINE" ]; then
  echo "timeout waiting for turn/end"
  echo "— server log (last 20) —"; tail -20 "$SERVER_LOG" || true
  echo "— agent log (last 30) —"; tail -30 "$AGENT_LOG" || true
  echo "— sse log (all) —"; cat "$SSE_LOG" || true
  exit 1
fi

echo "turn/end received:"
# SSE lines are prefixed with "data: " — strip it for jq.
TURN_END_JSON=$(echo "$TURN_END_LINE" | sed 's/^data: //')
echo "$TURN_END_JSON" | jq '{method, input_tokens: .params.usage.input_tokens, output_tokens: .params.usage.output_tokens, message_count: (.params.messages | length)}'

# ── 10. assert usage was reported ───────────────────────────────

hr "10. assert params.usage.input_tokens > 0"
INPUT_TOKENS=$(echo "$TURN_END_JSON" | jq -r '.params.usage.input_tokens // 0')
if [ "$INPUT_TOKENS" -gt 0 ]; then
  echo "usage reported ($INPUT_TOKENS input tokens)"
else
  echo "FAIL: input_tokens was $INPUT_TOKENS — the LLM did not report usage"
  echo "— turn/end frame —"; echo "$TURN_END_JSON" | jq
  exit 1
fi

# ── done ─────────────────────────────────────────────────────────

hr "done"
echo "smoke-e2e test complete"
