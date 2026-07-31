# liberado-coder-core — coding backend contracts

`liberado-coder-core` is the **coding domain pack's** contract layer — not Liberado's agentic
kernel. It deliberately owns no model loop, file mutation, sandbox lifecycle, forge API, queue, or
PR creation logic.

Canonical architecture:
[`docs/spec/architecture/agentic-loops.md`](../../docs/spec/architecture/agentic-loops.md).
These types specialize the logical Goal / Session / Event / Terminal vocabulary. Promote into
`liberado-common` or a session crate when a second domain would otherwise depend on this crate for
non-coding work (see modularity extraction trigger).

It exists so several surfaces can share one vocabulary:

- the PR factory (`liberado-pr-dispatch-mcp` today, likely collapsed/renamed later);
- the first-party Liberado loop backend (`coder-agent`);
- future TUI/CLI/WebUI coding sessions;
- sandbox implementations;
- eval and `heuristics-tuner` scenarios;
- migration backends such as the existing vtcode wrapper.

## Surface

- `CoderBackend` — the async trait a PR factory or UI client calls. It receives a prepared workspace
  and returns a `CoderRunResult`; it does not commit, push, or open PRs.
- `CoderRunRequest` — `CoderTask` + `WorkspaceRef` + resolved `CoderRunConfig`.
- `CoderRunConfig` — backend name, role configs, sandbox spec, optional validation command, and
  command/path/progress policies.
- `trace_dir` — optional run-config path where backends can write durable `CoderTrace` artifacts.
- `CoderRunResult` — backend outcome, summary, files changed, validation notes, critic verdict,
  diagnostics, and optional trace path.
- `CoderEvent` / `CoderTrace` — stable replay/render vocabulary for logs and future UI clients.
- `SandboxSpec`, `CoderCommandConfig`, `CommandPolicy`, `PathPolicy`, `ProgressPolicy` —
  config-shaped policy structs. `ProgressPolicy` owns loop/watchdog thresholds and trace shaping
  caps such as event-preview length.

## Design Rules

- **No hardcoded prompts or model assumptions.** `CoderRoleConfig` carries model and prompt path/body;
  config loading/validation lives in a higher crate.
- **Backend-neutral.** `LIBERADO_LOOP_BACKEND` is a stable name only. This crate knows no Liberado
  executor wiring.
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
