# Harness comparison runs

**Status**: current

`liberado coder compare` owns the durable infrastructure for a Liberado/Pi comparison. Do not
assemble long-lived run policy in PowerShell. A wrapper can supply arguments, but worktree setup,
build-cache isolation, process order, Git preservation, and artifact collection are compiled Rust
code in `crates/cli/src/compare_cmd.rs`.

## Prepare

```text
liberado coder compare prepare <run-dir> --commit main
```

`prepare` resolves the revision to one commit and creates two detached Git worktrees:

```text
<run-dir>/
  manifest.json
  worktrees/liberado/
  worktrees/pi/
  targets/liberado/
  targets/pi/
  artifacts/liberado/{git,sessions,traces}/
  artifacts/pi/{git,sessions,traces}/
```

The command copies the required `turbovault/` and `turbomcp/` path dependencies into each
worktree. It rejects symlinks and Windows reparse points. It never links a worktree to a sibling
checkout.

The Cargo target directories are separate. Never share one target directory across comparison
worktrees. Cargo can otherwise reuse freshness state or a same-named workspace binary from the
wrong checkout.

The default compile and command timeout is 1,800 seconds. Use
`--compile-timeout-secs <n>` when a colder machine needs more time.

## Run

```text
liberado coder compare run <run-dir> --task <task-file> \
  --model deepseek/deepseek-v4-flash --thinking high --task-aware-context \
  --acceptance-overlay <hidden-test-dir>
```

`run` copies the task to `<run-dir>/task.txt`, writes the exact pins, and prewarms both isolated
caches with `cargo check --workspace --locked`. Both warm-ups must pass before a model call.
Liberado runs first. Pi runs second. After each harness, the runner applies the same independent
`cargo test --workspace --no-fail-fast` gate. A harness exit of zero does not hide a red common
gate. Pi receives the captured task through its supported `@file` input, which avoids unsafe
Windows batch-file quoting.

`--acceptance-overlay <dir>` captures an independent test overlay before either model runs. The
runner installs the same files only while it verifies each result, then removes them before it
saves or commits the harness worktree. An overlay cannot replace a model-visible file. The
captured oracle and its SHA-256 fingerprint stay under the run directory, so the acceptance rule
is reviewable without becoming model context. Use an overlay for behavior that the normal
workspace suite does not test; omit it when the task already has an adequate independent gate.

`--task-aware-context` changes one Liberado model-visible variable and records
`task_aware_context=true` in `pins.txt`. It writes `[coder.repo_map] task_aware = true` to the run's
captured `tuning.toml`; omit the flag for the default-off control. Pi stays on its native dynamic
search path. Use this flag only when context routing is the declared experiment.

Two other hypotheses remain parked and documented, not mixed into this run:

- An external-contract prompt overlay could require the model to inspect a named protocol, wire
  type, generated type, or framework API before editing. It is not implemented because it would
  change the system prompt and overlap with the routing experiment.
- A fresh critic pass is already configurable and default off. It costs another model call and has
  not shown that it repairs missing authoritative context, so it stays off for this comparison.

Each harness writes to its own stable artifact directory. Pi's durable session directory is inside
that directory. Liberado trace files that start with the run session ID are copied there after the
run.

## Save, including failure

```text
liberado coder compare save <run-dir> <liberado|pi> \
  --session-id <id> --exit-code <code>
```

`run` calls `save` logic after each harness, including a nonzero harness exit. `save` commits dirty
tracked work with a local comparison identity, creates an archive branch, and records:

- the status before and after save;
- the saved HEAD and short log;
- a binary-capable patch and diff stat against the pinned base;
- the process exit code and session ID;
- the independent verifier exit code and logs;
- matching Liberado traces or Pi sessions;
- stdout, stderr, warm-up output, and timestamps.

The archive ref is `archive/harness-compare/<run-name>/<harness>`. Thus, useful work remains
reachable even if a harness reports failure or the comparison directory is moved later.
