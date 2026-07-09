# liberado-coder-agent - Executor-backed coding backend

`liberado-coder-agent` is the first Rust-native implementation of `CoderBackend`. It wires the
generic `liberado-executor` loop to the coding tool runtime and converts the resulting `Report` into
a `CoderRunResult`.

## Current MVP

- Uses one configured coder role.
- Builds `CodingToolRuntime` over the prepared workspace.
- Runs `Executor::execute` in report mode.
- Loads the coder role prompt from inline config or `prompt_path`.
- Checks `git status --porcelain` after the loop and fails `NoChanges` if the model filed a success
  report without a real workspace change. This backend invariant does not go through the
  model-visible `run_command` tool or its command policy.
- Writes a `CoderTrace` JSON replay artifact when `trace_dir` is configured. Current events include
  session/role/report/tool-start/tool-finish/file-change/guard/finish events.

## Not Done Yet

- Planner/critic/repair roles.
- Docker sandbox execution.
- Fine-grained `CoderEvent` trace emission for individual model turns. Tool calls are captured by a
  tracing runtime wrapper; model-turn events likely need executor streaming hooks.
- No-progress loop guards beyond what `liberado-executor` already provides generically.
- Config-dir-relative prompt path resolution. The current MVP reads the resolved path it is given.

Those belong here as the backend matures; PR factory and TUI clients should still consume only
`liberado-coder-core` contracts.
