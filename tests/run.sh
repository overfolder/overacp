#!/usr/bin/env bash
#
# tests/run.sh — sequential test runner.
#
# Discovers tests/*.sh (excluding itself), runs each one, captures
# exit codes, and prints a PASS / FAIL / SKIP summary.
#
# Usage:
#   ./tests/run.sh              # run all tests
#   ./tests/run.sh broker       # run only tests matching "broker"
#
# Environment:
#   TARGET_DIR  — forwarded to each test script (default: ./target/debug)
#
# Tests that require LLM_API_KEY are auto-skipped when the var is unset.

set -uo pipefail

# Load .env so LLM_API_KEY (and friends) are visible to the skip-logic
# below — individual test scripts also source .env, but the runner needs
# it earlier to decide whether to skip e2e tests.
if [ -f .env ]; then
  set -a; source .env; set +a
fi

SELF="$(realpath "$0")"
DIR="$(dirname "$SELF")"
FILTER="${1:-}"

export TARGET_DIR="${TARGET_DIR:-./target/debug}"

# ── discover tests ───────────────────────────────────────────────

TESTS=()
for f in "$DIR"/*.sh; do
  [ "$(realpath "$f")" = "$SELF" ] && continue
  [ -n "$FILTER" ] && [[ "$(basename "$f")" != *"$FILTER"* ]] && continue
  TESTS+=("$f")
done

if [ ${#TESTS[@]} -eq 0 ]; then
  echo "no tests matched${FILTER:+ filter \"$FILTER\"}"
  exit 1
fi

# ── run tests ────────────────────────────────────────────────────

PASSED=0
FAILED=0
SKIPPED=0
FAILURES=()

banner() {
  local width=68
  printf '\n%s\n' "$(printf '=%.0s' $(seq 1 $width))"
  printf '  %s\n' "$1"
  printf '%s\n\n' "$(printf '=%.0s' $(seq 1 $width))"
}

for test in "${TESTS[@]}"; do
  name="$(basename "$test" .sh)"

  # Auto-skip tests that need an LLM key when it's absent.
  if [[ "$name" == *e2e* ]] && [ -z "${LLM_API_KEY:-}" ]; then
    banner "SKIP  $name  (LLM_API_KEY not set)"
    SKIPPED=$((SKIPPED + 1))
    continue
  fi

  banner "RUN   $name"

  set +e
  bash "$test"
  rc=$?
  set -e

  if [ $rc -eq 0 ]; then
    banner "PASS  $name"
    PASSED=$((PASSED + 1))
  else
    banner "FAIL  $name  (exit $rc)"
    FAILED=$((FAILED + 1))
    FAILURES+=("$name")
  fi
done

# ── summary ──────────────────────────────────────────────────────

echo ""
echo "========================================"
echo "  Results: $PASSED passed, $FAILED failed, $SKIPPED skipped"
if [ ${#FAILURES[@]} -gt 0 ]; then
  echo "  Failed:  ${FAILURES[*]}"
fi
echo "========================================"

[ "$FAILED" -eq 0 ]
