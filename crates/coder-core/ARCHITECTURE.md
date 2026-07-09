# liberado-coder-core — coding backend contracts

`liberado-coder-core` is the provider-agnostic contract layer for Liberado's Rust-native coding
backend. It deliberately owns no model loop, file mutation, sandbox lifecycle, forge API, queue, or
PR creation logic.

It exists so several surfaces can share one vocabulary:

- the PR factory (`liberado-pr-dispatch-mcp` today, likely collapsed/renamed later);
- the planned first-party Liberado loop backend;
- future TUI/CLI coding sessions;
- sandbox implementations;
- eval and `heuristics-tuner` scenarios;
- migration backends such as the existing vtcode wrapper.

## Surface

- `CoderBackend` — the async trait a PR factory or UI client calls. It receives a prepared workspace
  and returns a `CoderRunResult`; it does not commit, push, or open PRs.
- `CoderRunRequest` — `CoderTask` + `WorkspaceRef` + resolved `CoderRunConfig`.
- `CoderRunConfig` — backend name, role configs, sandbox spec, command/path/progress policies.
- `trace_dir` — optional run-config path where backends can write durable `CoderTrace` artifacts.
- `CoderRunResult` — backend outcome, summary, files changed, validation notes, critic verdict,
  diagnostics, and optional trace path.
- `CoderEvent` / `CoderTrace` — stable replay/render vocabulary for logs and future UI clients.
- `SandboxSpec`, `CommandPolicy`, `PathPolicy`, `ProgressPolicy` — config-shaped policy structs.

## Design Rules

- **No hardcoded prompts or model assumptions.** `CoderRoleConfig` carries model and prompt path/body;
  config loading/validation lives in a higher crate.
- **Backend-neutral.** `VTCODE_BACKEND` and `LIBERADO_LOOP_BACKEND` are stable names only. This crate
  knows no vtcode subprocess details and no Liberado executor wiring.
- **PR-factory-neutral.** Branch creation, commits, pushes, draft PRs, approvals, and revisions are
  outside this crate.
- **Sandbox-neutral.** Docker and host-local are typed specs here; lifecycle and command execution
  belong in `coder-sandbox`/`coder-tools`.
- **Trace-first.** Every meaningful loop action should eventually become a `CoderEvent`, so a future
  TUI/CLI can render a live session and an eval harness can replay what happened. Backends should
  return `trace_path` when they persist a trace artifact.

## Dependencies

- Depends on: `liberado-common` for `Outcome`/`Report`, plus serialization/error traits.
- Depended on by: planned `coder-tools`, `coder-agent`, `coder-sandbox`, PR factory integration, and
  tuning/eval crates.

## Tests

The crate currently tests JSON round-tripping of `CoderRunRequest` and conversion of
`CoderRunResult` into the generic `Report` that the rest of Liberado already understands.
