# liberado-coder-sandbox - workspace and command boundaries

`liberado-coder-sandbox` owns the deterministic boundary between a coding agent and a prepared
workspace. It does not know about providers, PRs, prompts, or the model loop.

## Responsibilities

- Resolve model-supplied paths under a workspace root and reject escapes.
- Provide a host-local sandbox for unit tests and development.
- Provide a Docker command runner scaffold for production isolation.
- Define command request/output contracts shared by host-local and Docker runners.
- Enforce command policy before subprocess execution.
- Cap command output before it is returned to the model.

## Non-Responsibilities

- No forge operations, commits, pushes, or PR creation.
- No prompt construction or model calls.
- No Docker image building, long-lived container lifecycle, or remote runner orchestration yet.
- No file editing tools; `liberado-coder-tools` builds those on top of this boundary.

## Backends

- `HostWorkspace`: local filesystem implementation for tests and trusted development runs.
- `DockerWorkspace`: first Docker backend. It maps a prepared host workspace into `/workspace` with
  `docker run --rm -i`, applies configured volumes, network mode, user, env allowlist, and request
  env, then executes the policy-checked command inside the container. Unit tests assert the generated
  Docker argv without requiring a live Docker daemon.
- Remote runners can implement the same traits later without changing the agent loop.
