# Codex Patch Queue

AgentView vendors Codex as a git submodule and keeps local Codex changes as
patches in this directory.

Patch rules:

- Keep patches small and product-neutral.
- Patch Codex only for hosted-session extension points needed by AgentView.
- Do not patch Codex model, tool, approval, or transcript semantics.
- Generate patches from `third_party/codex` and store them here.
- `tools/check-codex-patches.sh` must pass before committing.
- `tools/update-codex.sh <ref>` is the supported way to move Codex to a new tag
  or commit and re-check the patch queue.

Expected initial patches:

- `0001-expose-hosted-session-view.patch`
- `0002-add-host-detach-event.patch`

