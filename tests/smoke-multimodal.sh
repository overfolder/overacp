#!/usr/bin/env bash
#
# tests/smoke-multimodal.sh — broker-level smoke test for multimodal
# content round-tripping.
#
# Verifies that multimodal content blocks (text + image_url) survive
# the broker untouched through both session/message push and turn/end
# SSE fan-out. No LLM key required — uses websocat as a fake agent.
#
# Dependencies on $PATH:
#   - curl, jq, uuidgen, openssl
#   - python3 with PyJWT (`pip install PyJWT`)
#   - websocat  (`cargo install websocat`)
#
# Environment:
#   - TARGET_DIR (optional, default ./target/debug) — directory
#     containing pre-built binaries.
#
# Exit 0 on full success, non-zero on any failure.

set -euo pipefail

TARGET_DIR=${TARGET_DIR:-./target/debug}
BASE_URL=${BASE_URL:-http://localhost:8080}
WS_URL=${WS_URL:-ws://localhost:8080}
LOG=${LOG:-/tmp/overacp-smoke-multimodal.log}
SSE_LOG=${SSE_LOG:-/tmp/overacp-smoke-multimodal-sse.log}

# ── plumbing ─────────────────────────────────────────────────────

need() { command -v "$1" >/dev/null || { echo "missing dep: $1"; exit 1; }; }
for dep in curl jq uuidgen openssl python3 websocat; do need "$dep"; done
python3 -c 'import jwt' 2>/dev/null || {
  echo "missing dep: python3 -m pip install PyJWT"
  exit 1
}

hr() { printf '\n── %s ' "$1"; printf '%.0s─' $(seq 1 $((60 - ${#1}))); echo; }

SERVER_PID=""
SSE_PID=""
cleanup() {
  [ -n "$SSE_PID" ] && kill "$SSE_PID" 2>/dev/null || true
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── 0. start server ──────────────────────────────────────────────

hr "0. start overacp-server"
export OVERACP_JWT_SIGNING_KEY="$(openssl rand -hex 32)"
: > "$LOG"
"$TARGET_DIR/overacp-server" > "$LOG" 2>&1 &
SERVER_PID=$!
echo "server pid=$SERVER_PID, log=$LOG"

for _ in $(seq 1 20); do
  if curl -fs -o /dev/null "$BASE_URL/healthz" 2>/dev/null; then break; fi
  sleep 0.2
done
curl -fs "$BASE_URL/healthz" >/dev/null || { echo "server didn't come up"; exit 1; }
echo "healthz=ok"

# ── 1. mint JWTs ────────────────────────────────────────────────

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

hr "2. POST /tokens (admin mints agent JWT)"
AGENT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "agent_id=$AGENT_ID"
MINT_RESP=$(curl -fsS -X POST "$BASE_URL/tokens" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"agent_id\": \"$AGENT_ID\", \"ttl_secs\": 300}")
AGENT_JWT=$(echo "$MINT_RESP" | jq -r .token)
echo "AGENT_JWT (first 40 chars): ${AGENT_JWT:0:40}..."

# ── 3. start SSE subscriber ─────────────────────────────────────

hr "3. start SSE subscriber in background"
: > "$SSE_LOG"
curl -sN "$BASE_URL/agents/$AGENT_ID/stream" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  > "$SSE_LOG" 2>&1 &
SSE_PID=$!
echo "sse pid=$SSE_PID, log=$SSE_LOG"
sleep 0.5

# ── 4. push multimodal message via REST ──────────────────────────

hr "4. POST multimodal session/message"

# A minimal 1x1 red PNG pixel, base64-encoded.
IMG_B64="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg=="

MULTIMODAL_CONTENT=$(cat <<EOF
[
  {"type": "text", "text": "describe this image"},
  {"type": "image_url", "image_url": {"url": "data:image/png;base64,${IMG_B64}"}}
]
EOF
)

PUSH_RESP=$(curl -fsS -X POST "$BASE_URL/agents/$AGENT_ID/messages" \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d "{\"role\":\"user\",\"content\":${MULTIMODAL_CONTENT}}")
echo "push → $PUSH_RESP"
# Message is queued because no agent tunnel is connected yet.

# ── 5. connect fake agent, drain multimodal message ──────────────

hr "5. connect fake agent and read queued multimodal message"

# Connect with websocat and read 1 line (the buffered session/message).
RECEIVED=$(sleep 0.5 | websocat -tE \
  -H="Authorization: Bearer $AGENT_JWT" \
  "$WS_URL/tunnel/$AGENT_ID" 2>/dev/null \
  | head -n 1)

echo "received frame:"
echo "$RECEIVED" | jq .

# Assert content blocks arrived intact.
BLOCK_COUNT=$(echo "$RECEIVED" | jq '.params.content | length')
FIRST_TYPE=$(echo "$RECEIVED" | jq -r '.params.content[0].type')
FIRST_TEXT=$(echo "$RECEIVED" | jq -r '.params.content[0].text')
SECOND_TYPE=$(echo "$RECEIVED" | jq -r '.params.content[1].type')
SECOND_URL=$(echo "$RECEIVED" | jq -r '.params.content[1].image_url.url')

echo "block_count=$BLOCK_COUNT first_type=$FIRST_TYPE second_type=$SECOND_TYPE"

FAIL=0
if [ "$BLOCK_COUNT" != "2" ]; then
  echo "FAIL: expected 2 content blocks, got $BLOCK_COUNT"
  FAIL=1
fi
if [ "$FIRST_TYPE" != "text" ]; then
  echo "FAIL: expected first block type 'text', got '$FIRST_TYPE'"
  FAIL=1
fi
if [ "$FIRST_TEXT" != "describe this image" ]; then
  echo "FAIL: text content mismatch: '$FIRST_TEXT'"
  FAIL=1
fi
if [ "$SECOND_TYPE" != "image_url" ]; then
  echo "FAIL: expected second block type 'image_url', got '$SECOND_TYPE'"
  FAIL=1
fi
if [[ "$SECOND_URL" != data:image/png* ]]; then
  echo "FAIL: image_url.url doesn't start with data:image/png"
  FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
  echo "multimodal session/message assertion failed"
  exit 1
fi
echo "session/message content blocks intact"

# ── 6. fake agent emits turn/end with multimodal content ─────────

hr "6. fake agent emits turn/end with multimodal messages"

TURN_END_PAYLOAD=$(cat <<ENDPAYLOAD
{"jsonrpc":"2.0","method":"turn/end","params":{"messages":[{"role":"user","content":[{"type":"text","text":"describe this image"},{"type":"image_url","image_url":{"url":"data:image/png;base64,${IMG_B64}"}}]},{"role":"assistant","content":"It is a 1x1 red pixel."}],"usage":{"input_tokens":100,"output_tokens":10}}}
ENDPAYLOAD
)

# Send the turn/end notification through the tunnel, then close.
(echo "$TURN_END_PAYLOAD"; sleep 0.3) \
  | websocat -tE \
    -H="Authorization: Bearer $AGENT_JWT" \
    "$WS_URL/tunnel/$AGENT_ID" 2>/dev/null \
  | cat > /dev/null

# ── 7. assert turn/end on SSE preserves multimodal ───────────────

hr "7. assert turn/end on SSE preserves multimodal blocks"

# Wait up to 5 seconds for the turn/end event on SSE.
DEADLINE=$(( $(date +%s) + 5 ))
TURN_END_LINE=""
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  TURN_END_LINE=$(grep -m1 '"turn/end"' "$SSE_LOG" || true)
  if [ -n "$TURN_END_LINE" ]; then break; fi
  sleep 0.2
done

if [ -z "$TURN_END_LINE" ]; then
  echo "FAIL: turn/end never arrived on SSE"
  echo "— sse log —"; cat "$SSE_LOG" || true
  exit 1
fi

TURN_JSON=$(echo "$TURN_END_LINE" | sed 's/^data: //')
echo "turn/end received:"
echo "$TURN_JSON" | jq '{method, msg_count: (.params.messages | length)}'

# The first message (user) should contain 2 multimodal blocks.
USER_CONTENT_LEN=$(echo "$TURN_JSON" | jq '.params.messages[0].content | length')
USER_BLOCK0_TYPE=$(echo "$TURN_JSON" | jq -r '.params.messages[0].content[0].type')
USER_BLOCK1_TYPE=$(echo "$TURN_JSON" | jq -r '.params.messages[0].content[1].type')
ASST_CONTENT=$(echo "$TURN_JSON" | jq -r '.params.messages[1].content')

echo "user content blocks=$USER_CONTENT_LEN types=[$USER_BLOCK0_TYPE, $USER_BLOCK1_TYPE]"
echo "assistant content=$ASST_CONTENT"

FAIL=0
if [ "$USER_CONTENT_LEN" != "2" ]; then
  echo "FAIL: expected 2 user content blocks, got $USER_CONTENT_LEN"
  FAIL=1
fi
if [ "$USER_BLOCK0_TYPE" != "text" ]; then
  echo "FAIL: expected user block 0 type 'text', got '$USER_BLOCK0_TYPE'"
  FAIL=1
fi
if [ "$USER_BLOCK1_TYPE" != "image_url" ]; then
  echo "FAIL: expected user block 1 type 'image_url', got '$USER_BLOCK1_TYPE'"
  FAIL=1
fi
if [ "$ASST_CONTENT" != "It is a 1x1 red pixel." ]; then
  echo "FAIL: assistant content mismatch: '$ASST_CONTENT'"
  FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
  echo "turn/end multimodal assertion failed"
  echo "$TURN_JSON" | jq
  exit 1
fi
echo "turn/end multimodal content intact"

# ── done ─────────────────────────────────────────────────────────

hr "done"
echo "smoke-multimodal test complete"
