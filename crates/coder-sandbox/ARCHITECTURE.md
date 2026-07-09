# liberado-coder-sandbox - workspace and command boundaries

`liberado-coder-sandbox` owns the deterministic boundary between a coding agent and a prepared
workspace. It does not know about providers, PRs, prompts, or the model loop.

## Responsibilities

- Resolve model-supplied paths under a workspace root and reject escapes.
- Provide a host-local sandbox for unit tests and development.
- Define command request/output contracts shared by host-local and future Docker runners.
- Enforce command policy before subprocess execution.
- Cap command output before it is returned to the model.

## Non-Responsibilities

- No forge operations, commits, pushes, or PR creation.
- No prompt construction or model calls.
- No Docker implementation yet; this crate defines the seam Docker will plug into.
- No file editing tools; `liberado-coder-tools` builds those on top of this boundary.

## Planned Backends

- `HostWorkspace`: local filesystem implementation for tests and trusted development runs.
- `DockerSandbox`: first production backend. It should map `SandboxSpec::Docker` into container
  lifecycle, volumes, network policy, user, env allowlist, and cleanup.
- Remote runners can implement the same traits later without changing the agent loop.
