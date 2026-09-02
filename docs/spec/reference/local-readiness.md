---
kind: reference
status: active
authority: normative
domain: ci
canonical_for: local-readiness
open_items: false
last_verified: 2026-09-02
---

# Local readiness

`just push` is the canonical ship command on Windows and Debian. It runs full local CI, then
`just ready`, verifies the final receipt, and pushes. `just ready` requires a full-CI receipt for
the same commit and tree. It checks formatting, locked metadata, workspace Clippy, changed-package
tests, architecture and workflow rules, module health, host-stable per-function complexity,
documentation contracts, and the exact Linux CRAP gate. Success writes `.liberado/ready.json`.

Full CI writes `.liberado/ci-ready.json`. The exact Linux CRAP check writes
`.liberado/crap-linux-ready.json`. The final receipt accepts both results only when all three
receipts bind the current commit, tracked changes, and untracked files. A commit, amend, merge,
rebase, conflict resolution, or content change makes them stale.

`just ready` installs the committed pre-push hook automatically. `just verify-ready`, the hook,
and `just push` reject a stale or old-contract receipt. Manual installation remains available:

```console check=false
just setup-hooks
```

Both `just ci` and `just ready` run the base-aware documentation-impact audit used by GitHub.
They compare `HEAD` with its merge base on `origin/main`; an isolated repository falls back to
`HEAD^`. A contract-bearing source change must update the document named by `docs-audit.toml` or
carry a narrow, reviewed waiver.

Coverage-sensitive CRAP remains a Debian authority and is part of every final readiness run.
`just crap-linux` remains available for a focused check. It runs natively on Debian/Linux. On
Windows it maps the checkout into the Debian WSL
distribution, bundles the clean committed `HEAD` into a managed Linux-native workspace, and runs
the same Rust CLI command there. The native workspace prevents Windows worktree metadata and
coverage objects from contaminating Linux tests or reports. Driver and coverage artifacts use
separate Linux-only target directories under the selected user's cache.
The default distribution name is `Debian`; set `LIBERADO_DEBIAN_WSL_DISTRO` when the installed
Debian-compatible distribution uses another name. The runner selects the first non-root login so
permission-sensitive tests retain their meaning; set `LIBERADO_DEBIAN_WSL_USER` to choose another
login. Windows checkout paths are changed to forward-slash form before `wslpath` maps the bundle,
so a worktree such as `C:\tmp\review` keeps each path component intact.

On Linux, `just ci` also runs and ratchets CRAP directly. On other hosts it defers CRAP to final
readiness. This avoids treating host-sensitive coverage as a proxy for the authoritative Linux
result and avoids running the coverage suite twice before the WSL check.

The host-stable function ratchet is configured in `function-complexity.toml` and committed in
`function-complexity-baseline.json`. Existing functions may not gain cyclomatic complexity. New
functions must stay under the configured ceiling. A persistent exception must name one exact file
and function and include an explicit ceiling, reason, and review date. The check fails if its
generated report or committed baseline cannot be read and decoded.

The coverage-sensitive CRAP ceiling is 29.9. New functions must remain below 30. Existing
functions may sit above 30; the per-function Linux baseline prevents those scores from rising.
cargo-crap `--fail-above` is not applied to the whole report, because that would fail the
known tail. `liberado ci crap` applies the ceiling only to functions that are not in
`crap-baseline.json`.

The unwraps classifier and ratchet are configured in `unwrap-classification.toml` and committed
in `unwrap-classification-baseline.json`. The AST classifier walks production `.unwrap()` and
`.expect()` calls, categorizing them into proven invariants, local failures, and process-fatal unwraps.
New process-fatal unwraps are blocked by CI without a narrow, reviewed waiver. Operator recipes include
`just unwrap-classification` (or `cargo liberado ci unwraps`) and `just unwrap-ratchet`.

## Operator recipes

The cross-platform operator recipes call the Rust CLI and read host-specific values from
`ops.toml`. Copy `config.example/ops.toml` to an untracked location, then pass it with
`--config` or `LIBERADO_OPS_CONFIG`. The main entry points are:

- `just ops-config-check --config <path>` — validate operations configuration.
- `just dev-start`, `just dev-status`, and `just stop-daemon` — manage a local daemon.
- `just deploy-homelab`, `just deploy-webui-homelab`, `just smoke-homelab`, and
  `just latency-homelab` — run configured remote operations.
- `just paseo-install` — install and register the configured ACP bridge.
- `just branches-clean` — audit merged branches with the standalone Python tool; deletion still
  requires its explicit `--apply` flag.

The justfile is only the convenience surface. Rust owns operator business logic, and the Python
branch cleaner remains independent so repository cleanup never depends on the binary built from
the repository being cleaned.

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
