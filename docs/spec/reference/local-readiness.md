---
kind: reference
status: active
authority: normative
domain: ci
canonical_for: local-readiness
open_items: false
last_verified: 2026-08-21
---

# Local readiness

`just ready` is the fast pre-push gate on Windows and Debian. It checks formatting, locked
metadata, workspace Clippy, architecture and workflow rules, module health, host-stable
per-function complexity, and documentation contracts. Success writes `.liberado/ready.json`.

The receipt binds the current commit, tracked changes, and untracked files. A commit, amend,
merge, rebase, conflict resolution, or content change makes it stale. `just verify-ready`, the
committed pre-push hook, and `just push` reject a stale receipt. Enable the hook with:

```console check=false
just setup-hooks
```

Coverage-sensitive CRAP remains a Debian authority. Run `just crap-linux` after Rust control-flow
changes. It runs natively on Debian/Linux. On Windows it maps the checkout into the Debian WSL
distribution and runs the same Rust CLI command there.
The default distribution name is `Debian`; set `LIBERADO_DEBIAN_WSL_DISTRO` when the installed
Debian-compatible distribution uses another name.

The host-stable function ratchet is configured in `function-complexity.toml` and committed in
`function-complexity-baseline.json`. Existing functions may not gain cyclomatic complexity. New
functions must stay under the configured ceiling. A persistent exception must name one exact file
and function and include an explicit ceiling, reason, and review date.

## Mutation-testing recipes

The `just` file carries the mutation campaign entry points backing
[`Skills/mutants-campaign.md`](../../../Skills/mutants-campaign.md):

- `just mutants <crate-dir>` — run cargo-mutants for one crate and append a ledger row to
  `mutants-ledger.json` (append-only; a row is recorded only when outcomes are complete and
  viable).
- `just mutants-agent` — coder-agent only (`--lib-only`; its e2e test hangs under mutants).
- `just mutants-record <crate-dir>` — ingest an existing `mutants.out/` without re-running.
- `just mutants-report` / `just mutants-next` — workspace health and the next crate to
  campaign.

These recipes build through `CARGO_TARGET_DIR=target/liberado-invoke` so the invoke binary never
collides with `target/debug`. Per-crate baseline timeouts live in the CLI's
`build_mutants_command`; add an entry there when a crate's unmutated baseline exceeds the 3s
floor on a cold cache.
