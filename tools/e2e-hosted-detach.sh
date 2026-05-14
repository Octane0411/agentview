#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENTVIEW_BIN="${AGENTVIEW_BIN:-$ROOT_DIR/target/debug/agentview}"
HOSTED_BIN="${AGENTVIEW_CODEX_HOSTED:-$ROOT_DIR/target/debug/agentview-codex-hosted}"
TIMEOUT_SECONDS="${AGENTVIEW_E2E_TIMEOUT:-180}"

if ! command -v expect >/dev/null 2>&1; then
  echo "error: expect is required for the hosted detach PTY E2E" >&2
  exit 1
fi

if ! command -v codex >/dev/null 2>&1; then
  echo "error: codex must be available on PATH" >&2
  exit 1
fi

if [[ ! -x "$AGENTVIEW_BIN" ]]; then
  cargo build -p agentview-cli
fi

if [[ ! -x "$HOSTED_BIN" ]]; then
  if [[ -n "${AGENTVIEW_CODEX_HOSTED:-}" ]]; then
    echo "error: AGENTVIEW_CODEX_HOSTED is set but is not executable: $HOSTED_BIN" >&2
    exit 1
  fi
  "$ROOT_DIR/tools/build-codex-hosted-helper.sh" >/dev/null
fi

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/agentview-hosted-e2e.XXXXXX")"
REPO="$TMP_ROOT/repo"
STORE="$TMP_ROOT/store"
JOB_ID=""

cleanup() {
  local status=$?
  if [[ -n "$JOB_ID" && -d "$STORE" ]]; then
    AGENTVIEW_HOME="$STORE" NO_COLOR=1 "$AGENTVIEW_BIN" __supervisor-shutdown >/dev/null 2>&1 || true
  fi
  if [[ "$status" -eq 0 && -z "${KEEP_AGENTVIEW_E2E:-}" ]]; then
    rm -rf "$TMP_ROOT"
  else
    echo "e2e temp root: $TMP_ROOT" >&2
  fi
}
trap cleanup EXIT

run_agentview() {
  AGENTVIEW_HOME="$STORE" \
    NO_COLOR=1 \
    AGENTVIEW_CODEX_HOSTED="$HOSTED_BIN" \
    "$AGENTVIEW_BIN" "$@"
}

wait_until() {
  local description="$1"
  local timeout="$2"
  shift 2
  local start
  start="$(date +%s)"
  while true; do
    if "$@"; then
      return 0
    fi
    if (( "$(date +%s)" - start >= timeout )); then
      echo "error: timed out waiting for $description" >&2
      return 1
    fi
    sleep 2
  done
}

init_repo() {
  mkdir -p "$REPO"
  git -C "$REPO" init -q
  printf '%s\n' "# AgentView hosted detach E2E" > "$REPO/README.md"
  git -C "$REPO" add README.md
  git -C "$REPO" commit -q -m "init"
}

dispatch_job() {
  local marker="$1"
  local output
  local prompt
  prompt="Use the shell tool to run exactly: sleep 60; echo $marker. Do not edit files. After the command finishes, reply exactly $marker."
  output="$(run_agentview run --cwd "$REPO" --sandbox workspace-write "$prompt" 2>&1)"
  printf '%s\n' "$output"
  JOB_ID="$(
    awk '/^backgrounded/ { print $2; exit }' <<<"$output"
  )"
  if [[ -z "$JOB_ID" ]]; then
    echo "error: could not parse job id from dispatch output" >&2
    exit 1
  fi
}

PEEK_OUTPUT=""
THREAD_ID=""
TURN_ID=""

job_has_active_turn() {
  PEEK_OUTPUT="$(run_agentview peek "$JOB_ID" 2>&1)" || return 1
  [[ "$PEEK_OUTPUT" == *"working"* && "$PEEK_OUTPUT" == *"thread: "* && "$PEEK_OUTPUT" == *"turn: "* ]]
}

remember_active_ids() {
  THREAD_ID="$(awk '/^thread:/ { print $2; exit }' <<<"$PEEK_OUTPUT")"
  TURN_ID="$(awk '/^turn:/ { print $2; exit }' <<<"$PEEK_OUTPUT")"
  if [[ -z "$THREAD_ID" || -z "$TURN_ID" ]]; then
    echo "error: missing thread or turn id in peek output" >&2
    printf '%s\n' "$PEEK_OUTPUT" >&2
    exit 1
  fi
}

job_still_running_same_turn() {
  PEEK_OUTPUT="$(run_agentview peek "$JOB_ID" 2>&1)" || return 1
  [[ "$PEEK_OUTPUT" == *"working"* && "$PEEK_OUTPUT" == *"thread: $THREAD_ID"* && "$PEEK_OUTPUT" == *"turn: $TURN_ID"* ]]
}

job_completed_with_marker() {
  local marker="$1"
  PEEK_OUTPUT="$(run_agentview peek "$JOB_ID" 2>&1)" || return 1
  [[ "$PEEK_OUTPUT" == *"completed"* && "$PEEK_OUTPUT" == *"$marker"* ]]
}

attach_hidden_and_detach() {
  local label="$1"
  local hold_seconds="$2"
  local expect_file="$TMP_ROOT/attach-$label.exp"
  local output_file="$TMP_ROOT/attach-$label.out"
  local hold_ms=$((hold_seconds * 1000))

  cat > "$expect_file" <<EOF
set timeout 45
match_max 200000
log_user 0
log_file -noappend "$output_file"
stty rows 42 columns 132
set env(AGENTVIEW_HOME) "$STORE"
set env(NO_COLOR) "1"
set env(AGENTVIEW_CODEX_HOSTED) "$HOSTED_BIN"
set env(COLUMNS) "132"
set env(LINES) "42"
spawn "$AGENTVIEW_BIN" __hosted-attach --no-alt-screen "$JOB_ID"
after $hold_ms
send "\033\[D"
expect {
  "detached $JOB_ID" { exit 0 }
  eof { exit 12 }
  timeout { exit 11 }
}
EOF

  if ! expect "$expect_file"; then
    echo "error: attach/detach PTY run failed ($label)" >&2
    cat "$output_file" >&2 || true
    exit 1
  fi
}

assert_detach_logs() {
  local logs
  logs="$(run_agentview logs "$JOB_ID" 1000)"
  local detach_count
  detach_count="$(grep -c "hosted_attach_detached" <<<"$logs" || true)"
  if (( detach_count < 2 )); then
    echo "error: expected at least two hosted_attach_detached events, got $detach_count" >&2
    printf '%s\n' "$logs" >&2
    exit 1
  fi
  if grep -Eiq "conversation interrupted|hosted_attach_quit|\"status\":\"interrupted\"" <<<"$logs"; then
    echo "error: detach emitted an interruption or quit marker" >&2
    printf '%s\n' "$logs" >&2
    exit 1
  fi
}

main() {
  local marker
  marker="AGENTVIEW_HOSTED_DETACH_E2E_$(date +%s)_OK"

  init_repo
  dispatch_job "$marker"
  wait_until "active Codex turn" 90 job_has_active_turn
  remember_active_ids

  attach_hidden_and_detach first 7
  wait_until "same turn after first detach" 20 job_still_running_same_turn

  attach_hidden_and_detach second 4
  wait_until "same turn after second detach" 20 job_still_running_same_turn

  assert_detach_logs
  wait_until "completed Codex turn with marker" "$TIMEOUT_SECONDS" job_completed_with_marker "$marker"

  printf 'ok: hosted detach E2E passed for %s\n' "$JOB_ID"
  printf 'thread: %s\nturn: %s\nmarker: %s\n' "$THREAD_ID" "$TURN_ID" "$marker"
}

main "$@"
