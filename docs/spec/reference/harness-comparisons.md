# Harness comparison runs

**Status**: current

This page is the runner contract: how `liberado coder compare` pins a job, launches harnesses, and
preserves artifacts. The C3 experiment that uses this runner — four harnesses, repeats, published
score — is [`cross-harness-baseline.md`](../../future-work/cross-harness-baseline.md).

`liberado coder compare` owns the durable infrastructure for a Liberado/Pi comparison. Do not
assemble long-lived run policy in PowerShell. A wrapper can supply arguments, but worktree setup,
build-cache isolation, process order, Git preservation, and artifact collection are compiled Rust
code in `crates/harness-eval/`. The CLI is a thin argument surface over that crate.

Common verifier failures can receive bounded repair, but this assistance is disabled by default.
Benchmark comparisons must measure native first-pass harness behavior; otherwise the score includes
the coordinator's recovery policy. Opt in per dispatch with `--verifier-repair-attempts N` (for
example, `2` for production recovery). The coordinator writes verifier diagnostics into the next
session prompt, re-runs the verifier, and preserves every attempt in the normal logs. Host failures,
scope violations, and verifier timeouts remain terminal and are not sent back to the model.

## Durable jobs

The normal automation path does not require Paseo changes. It has two boundaries:

- `liberado-harness-eval` owns the versioned job contract, worktrees, verifier, adapters, journal,
  result classification, and preservation;
- `liberado coder compare submit|status|await|cancel|report` reads and writes typed job records.

`submit` writes the job directory exactly as today, then spawns one detached executor process for
that job (`liberado-harness-worker run-job <id>`) and returns the job id immediately. Non-blocking
dispatch is a property of process spawning, not of a service. The executor inherits the submitter's
environment, so the credential alias resolves from the process environment; there is no installed
daemon, startup key, background binary, or registry read. `--no-spawn` creates the job without
dispatching it, for foreground diagnosis.

Two prerequisites are runtime state, tracked nowhere in Git, and neither is created for you:

- **The executor binary.** `submit` spawns `liberado-harness-worker` resolved as a sibling of the
  running `liberado` executable. Build both: `cargo build -p liberado-cli` and
  `cargo build -p liberado-harness-eval --bins` (or `cargo install --path` both). If the executor
  binary is missing, `submit` writes the job and then fails at spawn, leaving the job `Accepted`
  with no lease. Once the binary exists, run such a job — or any `--no-spawn` job — in the
  foreground with `liberado-harness-worker run-job <job-id> --source <repo>`.
- **The executor policy** at `.liberado/harness-worker.json`. Create it once per repository;
  `WorkerPolicy::for_repository` in `crates/harness-eval/src/contract.rs` is the canonical default
  (openrouter only, the alias `openrouter-default` mapping to `OPENROUTER_API_KEY`, a 400-turn
  ceiling, 20 GiB free-disk floor). `doctor` fails fast and names the path when it is missing.

The transport is a repository-scoped durable spool at `.liberado/harness-jobs/`. The repository
filesystem permissions are its access boundary. A request cannot contain a shell command or a
provider secret. The executor also applies its own repository, provider, model, turn, timeout, disk,
verifier, and credential-alias policy before it mutates a worktree or calls a model.

A spool-wide runner lock (`runner.lock`) serializes paid execution per repository: one comparison at
a time is a measurement policy, not a limitation. `submit` refuses while the lock is held. A dead
executor leaves a dead lease; the next `status`/`await` read marks the job `Failed` with a
host-infrastructure class. `await --stall-secs <n>` wakes the caller when neither the event log nor
the active harness's stdout/stderr log has grown in `n` seconds.

Submission resolves the requested Git ref to an exact commit before it creates `job.json`. The
executor permits only configured provider/base-URL pairs, so a request cannot redirect a credential
to another endpoint. Harness binary overrides are disabled by default. The executor also rechecks
the captured task and acceptance-overlay digests immediately before preflight.

The policy lives at `.liberado/harness-worker.json` and maps the alias `openrouter-default` to
`OPENROUTER_API_KEY`. The executor reads the credential from its own inherited process environment.
The key is passed only to each harness child. It is not written to the job, policy, event log,
report, or parent environment.

Any process with write access to the repository can submit and wait for a comparison:

```text
liberado coder compare submit --task target/compare/task.txt \
  --commit main --model deepseek/deepseek-v4-flash --provider openrouter \
  --credential openrouter-default --thinking high --max-turns 400 \
  --compile-timeout-secs 3600 --run-timeout-secs 14400 \
  --minimum-free-gib 20 --task-aware-context \
  --acceptance-overlay target/compare/acceptance-overlay \
  --hypothesis "task-aware routing improves acceptance" \
  --variable "task_aware_context=on"

liberado coder compare await <job-id>
liberado coder compare report <job-id> --json
```

For a single foreground workflow, add `--wait` (and optionally
`--timeout-secs <n>`). The command still creates the same durable job and watches the same
filesystem journal; it only combines submission, waiting, and final status reporting:

```text
liberado coder compare submit --task target/compare/task.txt \
  --model deepseek/deepseek-v4-flash --provider openrouter --credential openrouter-default \
  --wait
```

The job ID and report remain available in the spool even if the waiting client exits. `--wait` is
an interface convenience, not a comparison policy or retry mechanism.

Before submitting, `doctor` runs the same immutable-spec and host preflight checks without creating
a job or spending model tokens:

```text
liberado coder compare doctor --task target/compare/task.txt \
  --model deepseek/deepseek-v4-flash --provider openrouter \
  --credential openrouter-default
```

It checks the worker policy, repository and pinned revision, harness
launchers, credential availability, Git locks, and disk estimate. It never starts a harness and
does not replace the executor's execution-time preflight.

`await` is one blocking local process. It and the executor use operating-system filesystem events as
their wake hook, with a 30-second recovery check for missed or coalesced events. Waiting does not
consume model turns or require a Paseo hook. The executor writes every transition to append-and-flush
`events.jsonl`, and state is stored as immutable numbered records. A crash cannot replace the last
valid state with a partial JSON file.

Each job has one immutable `job.json`, one hash of the experiment pins, captured task and acceptance
inputs, and predictable outputs:

```text
.liberado/harness-jobs/<job-id>/
  job.json
  experiment.json
  events.jsonl
  state-00000000000000000000.json
  input/{task.txt,acceptance-overlay/}
  execution/{manifest.json,worktrees/,targets/,pins.txt}
  artifacts/harnesses/<name>/{result.json,session.*,verifier.*,git/,sessions/,traces/}
  report.json
  report.md
```

`report.json` carries, per harness, the exit and verifier codes, the archived HEAD, `accepted`,
and — when the harness artifacts are present and parseable — the wall-clock window (`started_at`,
`finished_at`, `duration_secs`), model turns used, and tokens in/out. Metrics are parsed from the
harness's own artifacts (`run-status.txt`, Liberado `traces/*.json`, pi `sessions/*.jsonl`) and are
omitted rather than invented when a transcript is missing or unparseable. Correctness — `accepted`
= harness exit 0 and verifier exit 0 — is unchanged.

The executor reports one terminal class: task failure, verifier failure, harness failure, timeout,
host infrastructure failure, or cancelled. It does not silently discard malformed
JSONL or malformed result JSON. On Windows, each paid harness process is assigned to a Job Object
with `KILL_ON_JOB_CLOSE`, so cancellation and the wall-clock limit terminate its process tree.

The adapter contract keeps harness-specific launch behavior narrow. The coordinator owns experiment
order, worktrees, common verification, result preservation, and classification. The initial adapters
are Liberado and Pi. A later Cline adapter must implement the same boundary and produce the common
result and MVL artifacts; it does not get to change comparison policy.

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

The command copies leftover `turbovault/` and `turbomcp/` trees into each worktree when those
directories are present locally. They are not required: Cargo fetches the ForrestThump git+tag
pins. When a leftover tree is copied, the copy rejects
symlinks and Windows reparse points, and excludes rebuildable or repository-local `.git/`,
`target/`, `.liberado/`, and `.fastembed_cache/` directories. It never links a worktree to a leftover
checkout.

The Cargo target directories are separate. Never share one target directory across comparison
worktrees. Cargo can otherwise reuse freshness state or a same-named workspace binary from the
wrong checkout.

`targets/` is rebuildable Cargo state. Durable jobs remove it after artifacts and archive
refs are saved unless `retain_build_caches` is enabled in the executor policy. They also remove
completed
worktrees by default; `retain_worktrees` keeps them for local inspection. Archive refs and captured
Git artifacts remain after cleanup. Direct legacy runs leave both directories for the operator.

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
Liberado then builds `liberado-coder-runner` in its own pinned worktree and target directory; it
never relies on an unrelated caller `target/debug` binary. The harnesses run in the declared run
order: `--run-order pi,liberado` flips a direct run, and submitted jobs alternate the order per job
and record it in `report.json` and `pins.txt`.
After each harness, the runner applies the same independent `cargo test --workspace --no-fail-fast`
gate. A harness exit of zero does not hide a red common gate. Pi receives the captured task through
its supported `@file` input, which avoids unsafe Windows batch-file quoting.

`--acceptance-overlay <dir>` captures an independent test overlay before either model runs. The
runner installs the same files only while it verifies each result, then removes them before it
saves or commits the harness worktree. An overlay cannot replace a model-visible file. The
captured oracle and its SHA-256 fingerprint stay under the run directory, so the acceptance rule
is reviewable without becoming model context. Use an overlay for behavior that the normal
workspace suite does not test; omit it when the task already has an adequate independent gate.

The comparison coordinator does not impose an allowlist or blacklist on either harness. This keeps
the benchmark fair: a native Liberado dispatch may use its own optional write-scope policy, but that
policy is not injected into Pi or enforced by the common verifier. The coordinator still records
changed files, patches, and test results for human or agent review. Base protections such as
`.git/**`, `target/**`, and restricted coding modes remain native harness policies.

Because Liberado's native write-scope gate can fail a task that Pi ignores, baseline tasks must stay
inside Liberado's granted scope — otherwise the comparison measures policy, not capability. A task
that needs out-of-scope writes is a declared experiment variable, not a silent default.

The baseline records its fairness decisions in `pins.txt` so no silent default can split two runs:
`tool_surface=native` (neither harness's tool catalog is narrowed), `pi_turn_cap=unset` (pi runs
its native turn budget while `liberado_max_turns` records the Liberado cap), `run_order` (the order
the harnesses ran, alternated per job so the systematic "first harness" bias cancels out), and
`sampling=omitted` (no temperature is passed to either client). `sampling` is also part of the
immutable job pins, so it appears in `experiment.json` and changes the experiment id when it
changes. `run_order` is recorded in `report.json` but is deliberately not part of the experiment
id: two runs of the same experiment differ only in order and must share an id. The coordinator
accepts only `sampling=omitted` today; a value that is not actually applied to both clients would
record a claim the run cannot keep.

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
