---
kind: reference
status: active
authority: normative
domain: ci
canonical_for: local-readiness
open_items: false
last_verified: 2026-10-29
---

# Local readiness

`just push` is the canonical ship command on Windows and Debian. It runs full local CI, then
`just ready`, verifies the final receipt, and pushes. `just ready` requires a full-CI receipt for
the same commit and tree. It checks formatting, locked metadata, workspace Clippy, changed-package
tests, architecture and workflow rules, module health, the cyclomatic diff gate,
documentation contracts, and the exact Linux CRAP diff gate. Success writes `.liberado/ready.json`.

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

CRAP and cyclomatic complexity share one diff gate. The gate scores a function only when that
function's line span intersects a line `git diff` added or modified since the merge-base. The
base defaults to `origin/main`. `--base <rev>` overrides it. A push to `main` uses the first
parent. Untouched functions are not scored. There is no committed per-function baseline and no
waiver.

A new function must meet both limits: CRAP at or under 29.9 (`.cargo-crap.toml` `threshold`) and
cyclomatic complexity at or under 20 (`code-metrics/function-complexity.toml`
`new_function_ceiling`). A function that already exists on the merge-base may stay over a limit.
If it is over a limit after the change, that score must not be strictly greater than the score
measured on the merge-base. CRAP and cyclomatic are compared separately. A CRAP rise counts when
the new score exceeds the base by more than 0.001. A cyclomatic rise counts when the new score
exceeds the base by more than 0.01. Scores at or under the limit pass.

A rename or a move is the same function. The gate pairs it with the merge-base function that no
longer exists at its old identity: first by the same qualified name, then by a normalized body
similarity of at least 0.8. The pair is a touched legacy function, and its cap is the matched
base score. A copy of an over-limit function, while the original still exists, is a new function
and must meet the limits. A split keeps the original identity as legacy. Each extracted function
is new.

`just ci` runs the CRAP diff gate on every host. It measures HEAD with cargo-llvm-cov and
cargo-crap. It measures merge-base coverage only when a touched legacy function is over 29.9.
That report is cached at `.liberado/crap-base-<sha>.json`. GitHub Actions restores the same
report from the `crap-report-<sha>` cache when a push to `main` saved it, and measures on a miss.
The CRAP job inherits `contents: read`. It does not grant `actions: write`. `actions/cache`
uses the runner cache token.

The base lcov file is an absolute path under the HEAD checkout, `.liberado/diff-gate-base.lcov`.
The merge-base worktree does not contain `.liberado`, so a relative `.liberado/crap.lcov` path
cannot be created there. The temporary worktree is `.liberado/diff-gate-worktree`. It is removed
with `git worktree remove --force` and `git worktree prune` when the measurement returns and
when it returns an error.

Before that coverage run, the console and `.liberado/diff-gate-report.json` name each touched
legacy function that is over the CRAP limit. Each line has `file:line`, the function name, and
the current CRAP score. The report field is `base_crap_triggers`.

The base coverage target is `.liberado/diff-gate-cov-target`. It is separate from the HEAD
llvm-cov target. Registry artifacts are hardlinked into it. Workspace binaries, profraw, and
profdata are not copied, so the two coverage reports do not mix. A missing HEAD target leaves
the base target cold. The gate still measures the merge-base.
`just function-complexity` and the early-lint job run the cyclomatic half. They do not run
coverage. Both halves write `.liberado/diff-gate-report.json`.

`just ready` runs the cyclomatic diff gate, then the exact Linux CRAP diff gate.
`just crap-linux` remains available for a focused check. It runs natively on Debian/Linux. On
Windows it maps the checkout into the Debian WSL
distribution, bundles the clean committed `HEAD` into a managed Linux-native workspace, and runs
`liberado ci crap --base <merge-base-sha>` there. The native workspace prevents Windows worktree
metadata and coverage objects from contaminating Linux tests or reports. Driver and coverage
artifacts use separate Linux-only target directories under the selected user's cache.
The default distribution name is `Debian`; set `LIBERADO_DEBIAN_WSL_DISTRO` when the installed
Debian-compatible distribution uses another name. The runner selects the first non-root login so
permission-sensitive tests retain their meaning; set `LIBERADO_DEBIAN_WSL_USER` to choose another
login. Windows checkout paths are changed to forward-slash form before `wslpath` maps the bundle,
so a worktree such as `C:\tmp\review` keeps each path component intact.

On every host, `just ci` compares `code-metrics/module-health-baseline.json` and
`code-metrics/unwrap-classification-baseline.json`. Only
Linux `just ci` rewrites them. A Windows rewrite dirties the tree, and `just ready` then cannot
run Debian CRAP, which validates a clean committed `HEAD`. GitHub only reads both files. To record
a new best off Linux, run `just module-health-ratchet` or `just unwrap-ratchet` on purpose and
inspect the diff before you commit it. The Linux write uses
`cfg!(target_os = "linux")` inside `run_quality_ratchets`. The CRAP diff gate is not behind that
compile-time gate. It runs on every host.

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
- `just webui-smoke` — headless Chrome smoke against the release WebUI bundle (also runs in CI
  after the WASM build).
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
