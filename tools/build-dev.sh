#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo build --workspace --manifest-path "$ROOT_DIR/Cargo.toml"
"$ROOT_DIR/tools/build-codex-hosted-helper.sh" >/dev/null

echo "$ROOT_DIR/target/debug/agentview"
echo "$ROOT_DIR/target/debug/agentview-codex-hosted"
