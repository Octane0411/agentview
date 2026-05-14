#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_DIR="$ROOT_DIR/third_party/codex"

usage() {
  echo "usage: tools/update-codex.sh <tag-or-commit>" >&2
  echo "set AGENTVIEW_RUN_REAL_CODEX_E2E=1 to also run tools/e2e-hosted-detach.sh" >&2
}

if [[ "$#" -ne 1 ]]; then
  usage
  exit 2
fi

REF="$1"

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
HOSTED_HELPER="$("$ROOT_DIR/tools/build-codex-hosted-helper.sh")"

cargo test \
  --manifest-path "$ROOT_DIR/target/agentview-codex-patched/codex/codex-rs/Cargo.toml" \
  -p codex-tui \
  hosted_detach \
  --lib

cargo test --workspace --manifest-path "$ROOT_DIR/Cargo.toml"

if [[ "${AGENTVIEW_RUN_REAL_CODEX_E2E:-}" == "1" ]]; then
  "$ROOT_DIR/tools/e2e-hosted-detach.sh"
else
  echo "real Codex E2E skipped; run with AGENTVIEW_RUN_REAL_CODEX_E2E=1 to consume tokens and verify hosted detach"
fi

echo "codex ref: $(git -C "$CODEX_DIR" rev-parse HEAD)"
echo "hosted helper: $HOSTED_HELPER"
