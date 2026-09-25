---
kind: reference
status: active
authority: normative
domain: ci
canonical_for: local-readiness
open_items: false
last_verified: 2026-09-18
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

On Linux, `just ci` also runs and ratchets CRAP directly. The write adds passing new functions and
saves improvements. If an existing function has worse CRAP or cyclomatic complexity, the write
keeps its old complete entry. Thus, an ignored score increase below the CRAP floor cannot raise the
baseline. A regression that the check rejects cannot write. On other hosts, `just ci` defers CRAP
to final readiness. This avoids treating host-sensitive coverage as a proxy for the authoritative
Linux result and avoids running the coverage suite twice before the WSL check.

The same Linux-only write applies to `code-metrics/module-health-baseline.json` and
`code-metrics/unwrap-classification-baseline.json`. Every host still *compares* them during `just ci`. Only
Linux `just ci` rewrites them. A Windows rewrite dirties the tree, and `just ready` then cannot
run Debian CRAP, which validates a clean committed `HEAD`. GitHub only reads both files. To record
a new best off Linux, run `just module-health-ratchet` or `just unwrap-ratchet` on purpose and
inspect the diff before you commit it. The Linux write uses the same compile-time
`cfg!(target_os = "linux")` gate as host CRAP, so `run_quality_ratchets` does not grow a
runtime branch.

The host-stable function ratchet is configured in `code-metrics/function-complexity.toml` and committed in
`code-metrics/function-complexity-baseline.json`. Existing functions may not gain cyclomatic complexity. New
functions must stay under the configured ceiling. A persistent exception must name one exact file
and function and include an explicit ceiling, reason, and review date. The check fails if its
generated report or committed baseline cannot be read and decoded.

The cross-provider fallback feature carries three narrow function-complexity waivers — one per call
site that gained an irreducible +1 cyclomatic branch for the fallback decision:
`Config::validate_providers` (two inline error checks: self-reference guard + undeclared-name guard),
`OpenAiCompatibleProvider::complete` (the single `if let Some(fb) = self.fallback_for_status(status)` branch),
and `OpenAiCompatibleProvider::complete_stream` (the same shape on the streaming path). Each waiver
carries a hard ceiling and review date; any further growth in these functions is not covered and must be
split, not waived. The `Config::validate_providers` waiver is for the inline shape specifically — a
prior split attempt regressed the metrics more than the inline because a new helper function adds to the
`functions` count and its own cyclomatic.

The mechanical-jobs feature (ADR-0020) carries three narrow function-complexity waivers — one
per call site that gained an irreducible +1 cyclomatic branch for the new dispatch shape.
`Config::validate_schedules` (ceiling 5): the four per-job-kind `if`/`&&` checks were extracted
into a module-scope `validate_schedule_job` helper, but the call site uses `?` (one extra branch
in the ratcher's count, matching the inline shape the existing `if let Err(e) = ...` already
adds). Without the helper, the inline form would push cyclomatic past 10; the helper holds the
per-kind checks below the new-function ceiling (20). `build_event` on `crates/cron/src/lib.rs`
(ceiling 6): the new `if let Some(job) = &schedule.job { insert_job(&mut map, job); }` branch —
the body is the dedicated `insert_job` helper, so the inline is the ratchet-friendly shape.
`Daemon::react` on `crates/daemon/src/react.rs` (ceiling 5): the new
`if let Some(outcome) = self.handle_mechanical(event) { return outcome; }` early-return mirrors
the existing `handle_proposal_event` early-return directly above it — the handler itself is a
sibling method, so the inline shape matches the function's existing pattern. Each waiver carries
a hard ceiling and review date; any further growth in these functions is not covered and must
be split, not waived.

The coverage-sensitive CRAP ceiling is 29.9. New functions must remain below 30. Existing
functions may sit above 30; the per-function Linux baseline prevents those scores from rising.
cargo-crap `--fail-above` is not applied to the whole report, because that would fail the
known tail. `liberado ci crap` applies the ceiling only to entries that cargo-crap's
move-aware baseline matcher classifies as new. This preserves distinct same-name functions.

The unwraps classifier and ratchet are configured in `code-metrics/unwrap-classification.toml` and committed
in `code-metrics/unwrap-classification-baseline.json`. The AST classifier walks production `.unwrap()` and
`.expect()` calls, categorizing them into proven invariants, local failures, and process-fatal unwraps.
New process-fatal unwraps are blocked by CI without a narrow, reviewed waiver.

Unwrap classification is behind the `liberado-cli` feature `ci-unwraps` (tree-sitter + tree-sitter-rust)
so the default product graph stays slim. Operator recipes pass the feature explicitly:

- `just ci` runs `cargo run -p liberado-cli --features ci-unwraps -- ci`
- `just unwrap-classification` runs `… --features ci-unwraps -- ci unwraps`
- `just unwrap-ratchet` runs `… --features ci-unwraps -- ci unwraps-ratchet`

Invoking `liberado ci unwraps` / `unwraps-ratchet` without `--features ci-unwraps` returns an error that
points at those just recipes. See also [slim-build.md](slim-build.md) for the feature fence.

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

## Build profiles

Two build profiles share the same lockfile. CI and mutation work use the full
workspace; day-to-day slim builds fence the sidecars that are not part of the
Liberado product root.

- `just build` — full native workspace (CI).
- `just build-slim` — the slim profile: native workspace minus the sysmap
  tool, the free-proxy provider, and the WASM-only WebUI. Use this for
  product-root compiles; see [slim-build.md](slim-build.md) for the gated
  list and rationale.
- `just build-release` — release binary of `-p liberado-cli` (the `liberado`
  binary); already slim.
- `just build-slim-release` — release variant of the slim profile.

## Mutation-testing recipes

The `just` file carries the mutation campaign entry points backing
[`skills/mutants-campaign.md`](../../../skills/mutants-campaign.md):

- `just mutants <crate-dir>` — run cargo-mutants for one crate and append a ledger row to
  `code-metrics/mutants-ledger.json` (append-only; a row is recorded only when outcomes are complete and
  viable).
- `just mutants-agent` — coder-agent only (`--lib-only`; its e2e test hangs under mutants).
- `just mutants-record <crate-dir>` — ingest an existing `mutants.out/` without re-running.
- `just mutants-report` / `just mutants-next` — workspace health and the next crate to
  campaign.

These recipes build through `CARGO_TARGET_DIR=target/liberado-invoke` so the invoke binary never
collides with `target/debug`. Per-crate baseline timeouts live in the CLI's
`build_mutants_command`; add an entry there when a crate's unmutated baseline exceeds the 3s
floor on a cold cache.

Coverage, mutation, and comparison jobs stay on isolated targets. Ordinary coding worktrees
may share a managed cache when `[coder.workspace]` names one. See
[`cargo-targets.md`](cargo-targets.md).

## Memory on a 16G host

Workspace Clippy (`cargo clippy --workspace --all-targets`) and `cargo llvm-cov` start several
`rustc` / `clippy-driver` processes. Each often needs several GiB. That is the usual pin on a
16G N150, not the later test run. `just ci` is `cargo run … -- ci`, so those children inherit a
jobserver sized to the CPU count unless Liberado drops it.

`liberado ci` removes the inherited jobserver and sets `CARGO_BUILD_JOBS` from free RAM: it
keeps 4 GiB for the OS, desktop, daemon, and Cargo, and counts one rustc job per 4 GiB after
that. A host with about 6 GiB free therefore gets one job. Set `LIBERADO_CI_JOBS` to force a
count. The console prints `cargo jobs=N (…)` at the start of the run.
