# liberado-coder-agent - Executor-backed coding backend

`liberado-coder-agent` is the first Rust-native implementation of `CoderBackend`. It wires the
generic `liberado-executor` loop to the coding tool runtime and converts the resulting `Report` into
a `CoderRunResult`.

## Current MVP

- Uses one configured coder role.
- Builds `CodingToolRuntime` over the prepared workspace.
- Runs `Executor::execute` in report mode.
- Checks `git diff --name-only <base_ref>` after the loop and fails `NoChanges` if the model filed a
  success report without a real diff.

## Not Done Yet

- Planner/critic/repair roles.
- Docker sandbox execution.
- Structured `CoderEvent` trace emission.
- No-progress loop guards beyond what `liberado-executor` already provides generically.
- Prompt loading from config paths.

Those belong here as the backend matures; PR factory and TUI clients should still consume only
`liberado-coder-core` contracts.
