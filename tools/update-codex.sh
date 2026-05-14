#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_DIR="$ROOT_DIR/third_party/codex"
REF="${1:-rust-v0.130.0}"

if [[ ! -d "$CODEX_DIR/.git" && ! -f "$CODEX_DIR/.git" ]]; then
  git -C "$ROOT_DIR" submodule update --init --recursive third_party/codex
fi

tracked_status="$(git -C "$CODEX_DIR" status --porcelain --untracked-files=no)"
if [[ -n "$tracked_status" ]]; then
  echo "error: Codex submodule has tracked changes. Commit, stash, or reset them before updating." >&2
  echo "$tracked_status" >&2
  exit 1
fi

git -C "$CODEX_DIR" fetch --tags origin
git -C "$CODEX_DIR" checkout "$REF"

"$ROOT_DIR/tools/check-codex-patches.sh"
"$ROOT_DIR/tools/build-codex-hosted-helper.sh" >/dev/null

echo "codex ref: $(git -C "$CODEX_DIR" rev-parse HEAD)"
echo "hosted helper: $ROOT_DIR/target/debug/agentview-codex-hosted"
