#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_DIR="$ROOT_DIR/third_party/codex"
PATCH_DIR="$ROOT_DIR/patches/codex"

if [[ ! -d "$CODEX_DIR/.git" && ! -f "$CODEX_DIR/.git" ]]; then
  echo "error: Codex submodule is not initialized at $CODEX_DIR" >&2
  echo "hint: git submodule update --init --recursive third_party/codex" >&2
  exit 1
fi

PATCHES=()
while IFS= read -r patch; do
  PATCHES+=("$patch")
done < <(find "$PATCH_DIR" -maxdepth 1 -type f -name '*.patch' | sort)
if [[ "${#PATCHES[@]}" -eq 0 ]]; then
  echo "codex patches: none"
  exit 0
fi

tracked_status="$(git -C "$CODEX_DIR" status --porcelain --untracked-files=no)"
if [[ -n "$tracked_status" ]]; then
  all_applied=true
  for patch in "${PATCHES[@]}"; do
    if ! git -C "$CODEX_DIR" apply --reverse --check "$patch" >/dev/null 2>&1; then
      all_applied=false
      break
    fi
  done

  if [[ "$all_applied" == true ]]; then
    echo "codex patches: already applied (${#PATCHES[@]})"
    exit 0
  fi

  echo "error: Codex submodule has tracked changes that do not match patches/codex" >&2
  echo "$tracked_status" >&2
  exit 1
fi

for patch in "${PATCHES[@]}"; do
  git -C "$CODEX_DIR" apply --check "$patch"
done

echo "codex patches: apply cleanly (${#PATCHES[@]})"
