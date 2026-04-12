#!/usr/bin/env bash
#
# tests/smoke-tools.sh — smoke test for the tools surface.
#
# Two sections:
#
# Part A (broker-level, no LLM needed):
#   Drives `tools/list` and `tools/call` through the tunnel via
#   websocat. The default broker has no operator ToolHost, so
#   `tools/list` returns an empty catalogue and `tools/call` returns
#   error code 1404 (not found). This proves the remote/operator
#   tools dispatch path is wired correctly.
#
# Part B (full-stack, needs LLM_API_KEY):
#   Starts the whole pipeline (broker + supervisor + overloop + real
#   LLM), writes a marker file to the workspace, and asks the LLM
#   to read that file using the built-in `read` tool. Asserts that
#   the marker content appears in the `turn/end` messages. This
#   proves built-in tool execution works end-to-end.
#
# Dependencies on $PATH:
#   - curl, jq, uuidgen, openssl
#   - python3 with PyJWT (`pip install PyJWT`)
#   - websocat  (`cargo install websocat`)
#
# Environment:
#   - TARGET_DIR  (optional, default ./target/debug)
#   - LLM_API_KEY (optional) — if unset, Part B is skipped
#   - LLM_API_URL (optional, default https://openrouter.ai/api/v1)
#   - OVERFOLDER_MODEL (optional, default anthropic/claude-sonnet-4-20250514)
#
# Exit 0 on full success (Part B skip counts as success), non-zero
# on any failure.

set -euo pipefail

TARGET_DIR=${TARGET_DIR:-./target/debug}
BASE_URL=${BASE_URL:-http://localhost:8080}
WS_URL=${WS_URL:-ws://localhost:8080}
LOG=${LOG:-/tmp/overacp-smoke-tools.log}
AGENT_LOG=${AGENT_LOG:-/tmp/overacp-smoke-tools-agent.log}
SSE_LOG=${SSE_LOG:-/tmp/overacp-smoke-tools-sse.log}

# ── plumbing ─────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null || { echo "missing dep: $1"; exit 1; }; }
for dep in curl jq uuidgen openssl python3 websocat; do need "$dep"; done
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

# ── 0. load .env ────────────────────────────────────────────────

if [ -f .env ]; then
  set -a; source .env; set +a
fi

# ── 1. start overacp-server ─────────────────────────────────────

hr "1. start overacp-server"
export OVERACP_JWT_SIGNING_KEY="$(openssl rand -hex 32)"
: > "$LOG"
"$TARGET_DIR/overacp-server" > "$LOG" 2>&1 &
SERVER_PID=$!
echo "server pid=$SERVER_PID, log=$LOG"

for _ in $(seq 1 40); do
  if curl -fs -o /dev/null "$BASE_URL/healthz" 2>/dev/null; then break; fi
  sleep 0.2
done
curl -fs "$BASE_URL/healthz" >/dev/null || { echo "server didn't come up"; exit 1; }
echo "healthz=ok"

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
echo "ADMIN_JWT (first 40 chars): ${ADMIN_JWT:0:40}..."

AGENT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "agent_id=$AGENT_ID"
MINT_RESP=$(curl -fsS -X POST "$BASE_URL/tokens" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"agent_id\": \"$AGENT_ID\", \"ttl_secs\": 600}")
AGENT_JWT=$(echo "$MINT_RESP" | jq -r .token)
echo "AGENT_JWT (first 40 chars): ${AGENT_JWT:0:40}..."

# ════════════════════════════════════════════════════════════════
# Part A — broker-level operator tools dispatch (no LLM needed)
# ════════════════════════════════════════════════════════════════

hr "A1. tools/list via tunnel (default empty ToolHost)"

TOOLS_LIST_RESP=$( (
  echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
  sleep 0.3
) | websocat -tE \
    -H="Authorization: Bearer $AGENT_JWT" \
    "$WS_URL/tunnel/$AGENT_ID" 2>/dev/null \
  | head -n 1)

echo "response: $(echo "$TOOLS_LIST_RESP" | jq -c .)"

TOOLS_ARRAY=$(echo "$TOOLS_LIST_RESP" | jq -c '.result.tools')
if [ "$TOOLS_ARRAY" != "[]" ]; then
  echo "FAIL: expected empty tools array, got $TOOLS_ARRAY"
  exit 1
fi
echo "tools/list returned empty catalogue — dispatch path wired"

hr "A2. tools/call via tunnel (default → 1404 not found)"

TOOLS_CALL_RESP=$( (
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nonexistent","arguments":{}}}'
  sleep 0.3
) | websocat -tE \
    -H="Authorization: Bearer $AGENT_JWT" \
    "$WS_URL/tunnel/$AGENT_ID" 2>/dev/null \
  | head -n 1)

echo "response: $(echo "$TOOLS_CALL_RESP" | jq -c .)"

ERROR_CODE=$(echo "$TOOLS_CALL_RESP" | jq '.error.code')
if [ "$ERROR_CODE" != "1404" ]; then
  echo "FAIL: expected error code 1404, got $ERROR_CODE"
  exit 1
fi
echo "tools/call returned 1404 (NotFound) — dispatch + error mapping correct"

hr "A. Part A complete — operator tools dispatch verified"

# ════════════════════════════════════════════════════════════════
# Part B — full-stack built-in tool round-trip (needs LLM_API_KEY)
# ════════════════════════════════════════════════════════════════

if [ -z "${LLM_API_KEY:-}" ]; then
  hr "B. SKIP — LLM_API_KEY not set, skipping built-in tool e2e"
  hr "done"
  echo "smoke-tools test complete (Part A passed, Part B skipped)"
  exit 0
fi

: "${LLM_API_URL:=https://openrouter.ai/api/v1}"
: "${OVERFOLDER_MODEL:=anthropic/claude-sonnet-4-20250514}"

hr "B1. prepare workspace with marker file"

WORKSPACE_DIR=$(mktemp -d)
MARKER_PATH="$WORKSPACE_DIR/marker.txt"
echo "SMOKE_TOOL_CANARY_12345" > "$MARKER_PATH"
echo "marker file: $MARKER_PATH"
echo "content: $(cat "$MARKER_PATH")"

hr "B2. mint a fresh agent JWT for Part B"

AGENT_ID_B=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "agent_id=$AGENT_ID_B"
MINT_RESP_B=$(curl -fsS -X POST "$BASE_URL/tokens" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"agent_id\": \"$AGENT_ID_B\", \"ttl_secs\": 600}")
AGENT_JWT_B=$(echo "$MINT_RESP_B" | jq -r .token)

hr "B3. start SSE subscriber"

: > "$SSE_LOG"
curl -sN "$BASE_URL/agents/$AGENT_ID_B/stream" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  > "$SSE_LOG" 2>&1 &
SSE_PID=$!
echo "sse pid=$SSE_PID, log=$SSE_LOG"
sleep 0.5

hr "B4. start overacp-agent supervisor"

export RUST_LOG="${RUST_LOG:-overacp_agent=info,overloop=info}"
: > "$AGENT_LOG"
OVERACP_TOKEN="$AGENT_JWT_B" \
OVERACP_SERVER_URL="$BASE_URL" \
OVERACP_WORKSPACE="$WORKSPACE_DIR" \
OVERACP_AGENT_BINARY="$TARGET_DIR/overloop" \
LLM_API_KEY="$LLM_API_KEY" \
LLM_API_URL="$LLM_API_URL" \
OVERFOLDER_MODEL="$OVERFOLDER_MODEL" \
  "$TARGET_DIR/overacp-agent" > "$AGENT_LOG" 2>&1 &
AGENT_PID=$!
echo "agent pid=$AGENT_PID, workspace=$WORKSPACE_DIR, log=$AGENT_LOG"

hr "B5. wait for tunnel connected"
CONNECTED=false
for _ in $(seq 1 50); do
  state=$(curl -fsS -H "Authorization: Bearer $ADMIN_JWT" \
    "$BASE_URL/agents/$AGENT_ID_B" 2>/dev/null | jq -r '.connected // false')
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

hr "B6. push message asking LLM to use the read tool"

PUSH_RESP=$(curl -fsS -X POST "$BASE_URL/agents/$AGENT_ID_B/messages" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"role\":\"user\",\"content\":\"Use the read tool to read the file at $MARKER_PATH and tell me exactly what it says. Do not guess — you must use the read tool.\"}")
echo "push → $PUSH_RESP"

hr "B7. wait up to 120s for turn/end on SSE"

DEADLINE=$(( $(date +%s) + 120 ))
TURN_END_LINE=""
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  TURN_END_LINE=$(grep -m1 '"turn/end"' "$SSE_LOG" || true)
  if [ -n "$TURN_END_LINE" ]; then break; fi
  sleep 0.5
done

if [ -z "$TURN_END_LINE" ]; then
  echo "timeout waiting for turn/end"
  echo "— server log (last 20) —"; tail -20 "$LOG" || true
  echo "— agent log (last 30) —"; tail -30 "$AGENT_LOG" || true
  echo "— sse log (all) —"; cat "$SSE_LOG" || true
  exit 1
fi

TURN_END_JSON=$(echo "$TURN_END_LINE" | sed 's/^data: //')
echo "turn/end received:"
echo "$TURN_END_JSON" | jq '{method, message_count: (.params.messages | length)}'

hr "B8. assert marker content appears in turn/end messages"

# The marker string should appear somewhere in the messages — either
# in a tool-result message or echoed back by the assistant.
MESSAGES_STR=$(echo "$TURN_END_JSON" | jq -c '.params.messages')

if echo "$MESSAGES_STR" | grep -q "SMOKE_TOOL_CANARY_12345"; then
  echo "marker content found in turn/end messages — built-in read tool works"
else
  echo "FAIL: marker content 'SMOKE_TOOL_CANARY_12345' not found in turn/end messages"
  echo "— messages —"
  echo "$TURN_END_JSON" | jq '.params.messages'
  exit 1
fi

# Also verify there was a tool role message (proves a tool was actually
# called, not that the LLM hallucinated the content).
TOOL_MSG_COUNT=$(echo "$TURN_END_JSON" | jq '[.params.messages[] | select(.role == "tool")] | length')
if [ "$TOOL_MSG_COUNT" -gt 0 ]; then
  echo "tool-role messages present ($TOOL_MSG_COUNT) — tool was invoked"
else
  echo "FAIL: no tool-role messages found — LLM may not have called the read tool"
  echo "$TURN_END_JSON" | jq '.params.messages[] | {role, content}'
  exit 1
fi

hr "B. Part B complete — built-in tool e2e verified"

# ── done ─────────────────────────────────────────────────────────

hr "done"
echo "smoke-tools test complete (Part A + Part B passed)"
