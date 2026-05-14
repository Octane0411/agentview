#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_DIR="$ROOT_DIR/third_party/codex"
PATCH_DIR="$ROOT_DIR/patches/codex"
PATCHED_DIR="$ROOT_DIR/target/agentview-codex-patched"
PATCHED_CODEX_DIR="$PATCHED_DIR/codex"
TARGET_DIR="$ROOT_DIR/target/codex-hosted"

if [[ ! -d "$CODEX_DIR/.git" && ! -f "$CODEX_DIR/.git" ]]; then
  echo "error: Codex submodule is not initialized at $CODEX_DIR" >&2
  echo "hint: git submodule update --init --recursive third_party/codex" >&2
  exit 1
fi

rm -rf "$PATCHED_DIR"
mkdir -p "$PATCHED_CODEX_DIR"

git -C "$CODEX_DIR" archive HEAD | tar -x -C "$PATCHED_CODEX_DIR"
git -C "$PATCHED_CODEX_DIR" init -q

PATCHES=()
while IFS= read -r patch; do
  PATCHES+=("$patch")
done < <(find "$PATCH_DIR" -maxdepth 1 -type f -name '*.patch' | sort)

for patch in "${PATCHES[@]}"; do
  git -C "$PATCHED_CODEX_DIR" apply "$patch"
done

CARGO_TARGET_DIR="$TARGET_DIR" cargo build \
  --manifest-path "$PATCHED_CODEX_DIR/codex-rs/Cargo.toml" \
  -p codex-tui \
  --bin agentview-codex-hosted

mkdir -p "$ROOT_DIR/target/debug"
cp "$TARGET_DIR/debug/agentview-codex-hosted" "$ROOT_DIR/target/debug/agentview-codex-hosted"

echo "$ROOT_DIR/target/debug/agentview-codex-hosted"
