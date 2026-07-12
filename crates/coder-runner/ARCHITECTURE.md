# liberado-coder-runner - Process Boundary for Coder Backends

`liberado-coder-runner` exposes the Rust-native coding backend as a small executable:
`liberado-coder-run`.

The binary accepts a serialized `CoderRunRequest` from `liberado-coder-core` and writes a serialized
`CoderRunResult` to stdout. Diagnostics and tracing logs go to stderr. This keeps process callers
such as `liberado-pr-dispatch-mcp` from linking directly against the in-process loop stack, while the
future TUI/API can still use `liberado-coder-agent` as a normal library (see
[`docs/architecture/agentic-loops.md`](../../docs/architecture/agentic-loops.md)).

## Current Contract

- Input: `CoderRunRequest` JSON from `--request <path>` or stdin.
- Output: `CoderRunResult` JSON on stdout.
- Provider profile: loaded from `topology.toml` in `--config-dir <dir>` when supplied, otherwise
  `Topology::default()`.
- Provider selection: `LIBERADO_CODER_PROVIDER` overrides `topology.provider`.
- Model selection: each role's `model` in the request wins when the backend asks for that role's
  provider. This lets prompt/model tuning live in `tuning.coder` without recompiling.

## Why This Exists

`liberado-pr-dispatch-mcp` is still a nested workspace with its own provider crate. Directly linking
it to the root loop crates would create type and package-shape friction before we have learned enough
from live loop runs. A JSON subprocess boundary is more stable: dispatch can swap `vtcode exec` for
`liberado-coder-run` while the loop backend keeps maturing behind the core contracts.

## Not Done Yet

- Streaming events over stdout/stderr or a sidecar trace channel.
- Config-dir-relative prompt path resolution before the request reaches `liberado-coder-agent`.
- Executing planner/critic/repair roles. The runner can construct providers for role configs, but
  the current backend still only asks for the coder role.
- A first-class CLI UX. This binary is a machine contract, not the future human-facing coding TUI.
