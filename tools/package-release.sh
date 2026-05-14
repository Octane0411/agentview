#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(
  awk -F'"' '/^version = / { print $2; exit }' "$ROOT_DIR/Cargo.toml"
)"
TARGET_NAME="${AGENTVIEW_PACKAGE_TARGET:-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)}"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/target/dist}"
PACKAGE_ROOT="$ROOT_DIR/target/package"
PACKAGE_NAME="agentview-$VERSION-$TARGET_NAME"
PACKAGE_DIR="$PACKAGE_ROOT/$PACKAGE_NAME"

cargo build --release --workspace --manifest-path "$ROOT_DIR/Cargo.toml"
"$ROOT_DIR/tools/build-codex-hosted-helper.sh" --release >/dev/null

rm -rf "$PACKAGE_DIR"
install -d "$PACKAGE_DIR/bin" "$PACKAGE_DIR/docs"
install -m 0755 "$ROOT_DIR/target/release/agentview" "$PACKAGE_DIR/bin/agentview"
install -m 0755 \
  "$ROOT_DIR/target/release/agentview-codex-hosted" \
  "$PACKAGE_DIR/bin/agentview-codex-hosted"
install -m 0644 "$ROOT_DIR/README.md" "$PACKAGE_DIR/README.md"
install -m 0644 "$ROOT_DIR/AGENTS.md" "$PACKAGE_DIR/AGENTS.md"
install -m 0644 \
  "$ROOT_DIR/docs/codex-agent-view-spec.md" \
  "$PACKAGE_DIR/docs/codex-agent-view-spec.md"
install -m 0644 \
  "$ROOT_DIR/docs/agentview-codex-mainline-plan.md" \
  "$PACKAGE_DIR/docs/agentview-codex-mainline-plan.md"

install -d "$DIST_DIR"
tar -C "$PACKAGE_ROOT" -czf "$DIST_DIR/$PACKAGE_NAME.tar.gz" "$PACKAGE_NAME"

echo "$DIST_DIR/$PACKAGE_NAME.tar.gz"
