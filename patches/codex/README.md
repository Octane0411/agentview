# Codex Patch Queue

AgentView vendors Codex as a git submodule and keeps local Codex changes as
patches in this directory.

Patch rules:

- Keep patches small and product-neutral.
- Patch Codex only for hosted-session extension points needed by AgentView.
- Do not patch Codex model, tool, approval, or transcript semantics.
- Generate patches from `third_party/codex` and store them here.
- `tools/check-codex-patches.sh` must pass before committing.
- `tools/build-codex-hosted-helper.sh` must build when patches change hosted
  TUI behavior or the helper binary.
- `tools/update-codex.sh <ref>` is the supported way to move Codex to a new tag
  or commit. It re-checks the patch queue, rebuilds the hosted helper, runs the
  patched Codex hosted-detach test, and runs the AgentView workspace tests.
- Set `AGENTVIEW_RUN_REAL_CODEX_E2E=1` when the update should also run the
  real-Codex PTY hosted detach flow.

Expected initial patches:

- `0001-expose-hosted-session-view.patch`
- `0002-add-agentview-hosted-helper-bin.patch`
