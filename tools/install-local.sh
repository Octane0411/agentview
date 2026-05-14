#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-${CARGO_HOME:-$HOME/.cargo}/bin}"

"$ROOT_DIR/tools/build-dev.sh" >/dev/null

install -d "$BIN_DIR"
install -m 0755 "$ROOT_DIR/target/debug/agentview" "$BIN_DIR/agentview"
install -m 0755 \
  "$ROOT_DIR/target/debug/agentview-codex-hosted" \
  "$BIN_DIR/agentview-codex-hosted"

echo "$BIN_DIR/agentview"
echo "$BIN_DIR/agentview-codex-hosted"
