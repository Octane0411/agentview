#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENTVIEW_BIN="${AGENTVIEW_BIN:-$ROOT_DIR/target/debug/agentview}"
HOSTED_BIN="${AGENTVIEW_CODEX_HOSTED:-$ROOT_DIR/target/debug/agentview-codex-hosted}"
AGENTVIEW_HOME="${AGENTVIEW_HOME:-$HOME/.agentview}"
BUILD=1
CLEAN=1
INSTALL=0
BIN_DIR="${BIN_DIR:-/opt/homebrew/bin}"
AGENTVIEW_ARGS=()

usage() {
  cat >&2 <<'EOF'
usage: tools/start-agentview.sh [options] [--] [agentview args...]

Builds local dev binaries, removes stale AgentView runtime files, runs doctor,
then starts AgentView from target/debug.

Options:
  --home PATH    Use a specific AGENTVIEW_HOME for this run.
  --install      Also install binaries to BIN_DIR, default /opt/homebrew/bin.
  --no-build     Skip building local dev binaries.
  --no-clean     Skip stale runtime cleanup.
  -h, --help     Show this help.

Examples:
  tools/start-agentview.sh
  tools/start-agentview.sh -- list --all
  AGENTVIEW_HOME=/tmp/agentview-smoke tools/start-agentview.sh
EOF
}

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --home)
      if [[ "$#" -lt 2 ]]; then
        echo "error: --home requires a path" >&2
        exit 2
      fi
      AGENTVIEW_HOME="$2"
      shift 2
      ;;
    --install)
      INSTALL=1
      shift
      ;;
    --no-build)
      BUILD=0
      shift
      ;;
    --no-clean)
      CLEAN=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      AGENTVIEW_ARGS=("$@")
      break
      ;;
    *)
      AGENTVIEW_ARGS=("$@")
      break
      ;;
  esac
done

process_alive() {
  local pid="$1"
  [[ -n "$pid" ]] || return 1
  kill -0 "$pid" >/dev/null 2>&1
}

process_command() {
  local pid="$1"
  ps -p "$pid" -o command= 2>/dev/null || true
}

cleanup_stale_runtime() {
  local jobs_dir="$AGENTVIEW_HOME/jobs"
  [[ -d "$jobs_dir" ]] || return 0

  local cleaned=0
  local active=0
  local job_dir job_id pid child_pid command
  shopt -s nullglob
  for job_dir in "$jobs_dir"/*; do
    [[ -d "$job_dir" ]] || continue
    job_id="$(basename "$job_dir")"

    if [[ -f "$job_dir/hosted-pty.pid" ]]; then
      pid="$(tr -d '[:space:]' < "$job_dir/hosted-pty.pid" || true)"
      if process_alive "$pid"; then
        command="$(process_command "$pid")"
        if [[ "$command" == *"__hosted-pty-host $job_id"* ]]; then
          active=$((active + 1))
        else
          echo "warning: leaving unexpected live pid $pid for $job_id: $command" >&2
        fi
      else
        rm -f "$job_dir/hosted-pty.pid" "$job_dir/hosted-pty.sock"
        cleaned=$((cleaned + 1))
      fi
    fi

    if [[ -f "$job_dir/hosted-pty-child.pid" ]]; then
      child_pid="$(tr -d '[:space:]' < "$job_dir/hosted-pty-child.pid" || true)"
      if ! process_alive "$child_pid"; then
        rm -f "$job_dir/hosted-pty-child.pid"
        cleaned=$((cleaned + 1))
      fi
    fi

    if [[ -S "$job_dir/hosted-pty.sock" && ! -f "$job_dir/hosted-pty.pid" ]]; then
      rm -f "$job_dir/hosted-pty.sock"
      cleaned=$((cleaned + 1))
    fi
  done
  shopt -u nullglob

  if [[ "$cleaned" -gt 0 || "$active" -gt 0 ]]; then
    echo "agentview cleanup: removed $cleaned stale runtime file set(s), kept $active active hosted PTY host(s)" >&2
  fi
}

if [[ "$BUILD" -eq 1 ]]; then
  "$ROOT_DIR/tools/build-dev.sh" >/dev/null
fi

if [[ "$INSTALL" -eq 1 ]]; then
  BIN_DIR="$BIN_DIR" "$ROOT_DIR/tools/install-local.sh"
fi

if [[ "$CLEAN" -eq 1 ]]; then
  cleanup_stale_runtime
fi

export AGENTVIEW_HOME
export AGENTVIEW_CODEX_HOSTED="$HOSTED_BIN"
export AGENTVIEW_PERSISTENT_CODEX_TUI="${AGENTVIEW_PERSISTENT_CODEX_TUI:-1}"

echo "agentview: $AGENTVIEW_BIN" >&2
echo "hosted helper: $AGENTVIEW_CODEX_HOSTED" >&2
echo "state: $AGENTVIEW_HOME" >&2
"$AGENTVIEW_BIN" doctor >&2

exec "$AGENTVIEW_BIN" "${AGENTVIEW_ARGS[@]}"
