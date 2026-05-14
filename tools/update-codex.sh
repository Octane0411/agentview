#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_DIR="$ROOT_DIR/third_party/codex"
PATCH_DIR="$ROOT_DIR/patches/codex"
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

PATCHES=()
while IFS= read -r patch; do
  PATCHES+=("$patch")
done < <(find "$PATCH_DIR" -maxdepth 1 -type f -name '*.patch' | sort)
if [[ "${#PATCHES[@]}" -gt 0 ]]; then
  for patch in "${PATCHES[@]}"; do
    git -C "$CODEX_DIR" apply "$patch"
  done
fi

"$ROOT_DIR/tools/check-codex-patches.sh"

echo "codex ref: $(git -C "$CODEX_DIR" rev-parse HEAD)"
