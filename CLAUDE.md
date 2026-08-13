# Working in this repo

Liberado is a personal AI operating layer: a daemon that runs agent sessions against an Obsidian
vault, with chat surfaces (TUI, WebUI, Telegram) and domain **packs** (coding first).

This file is the orientation an agent needs before touching anything. It is deliberately short —
everything else is a pointer, because 150+ docs read in full is worse than none.

## Build and test

**The workspace does not build without two sibling checkouts.** `turbovault/` and `turbomcp/` are
path dependencies expected *inside* this repo (both gitignored). If they are missing, `cargo` fails
at manifest resolution before compiling anything:

```bash
git clone <fork>/turbovault turbovault && git -C turbovault checkout develop
git clone <fork>/turbomcp  turbomcp  && git -C turbomcp  checkout develop
```

`.github/workflows/ci.yml` checks them out the same way — that file is the authority on what CI
runs, and preflight mirrors it.

```bash
cargo test --workspace --no-fail-fast          # --no-fail-fast matters; see below
cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p liberado-test-support             # the layer-rules gate
```

Toolchain is pinned in `rust-toolchain.toml` (1.94.1). Don't name a version anywhere else.

## Architecture in one paragraph

Crates are tagged with a **layer role** in `[package.metadata.liberado] role` and the dependency
direction between layers is **mechanically enforced** by `crates/test-support/tests/layer_rules.rs`.
A new crate with no role fails that test. Surfaces may depend only on `client` crates; packs never
sit beneath kernel/config/store. If a dependency feels awkward, the layering is usually telling you
something rather than being in the way.

- **What each crate is:** [`docs/spec/reference/crate-map.md`](docs/spec/reference/crate-map.md) —
  generated from the manifests; regenerate with `powershell -File scripts/gen-crate-map.ps1`.
- **Why the layers are what they are:** [`docs/spec/architecture/contracts.md`](docs/spec/architecture/contracts.md).
- **What is being worked on:** [`docs/roadmap.md`](docs/roadmap.md) — the living scoreboard.

## Which docs are true

`docs/` is large and not uniformly current. Before relying on a plan, check its `**Status**:` header
— most carry one, and it is usually accurate.

| Path | What it is |
|---|---|
| `docs/spec/` | Contracts and reference. Closest to truth; changes with the code. |
| `docs/roadmap.md` | Open work in priority order. Start here. |
| `docs/future-work/` | Plans and findings. **Read the status header.** Some are shipped and kept only for the rules or review record they carry. |
| `docs/future-work/*/archive/` | Finished or abandoned. **Not current truth.** |
| `docs/validation/` | Mutation-testing records. Historical evidence, not instructions. |

The module-level `//!` docs are unusually thorough here and explain *why*, not just what. For "how
does X work", read `crates/<x>/src/lib.rs` before searching `docs/` — it is more likely to be right,
because it cannot drift from the code as easily.

## Conventions that will trip you

**Run the mutation your test claims to catch.** Break the fix, watch the test fail, restore it. A
test that passes both ways reports coverage it does not have. This has caught fake tests in review
more than once, including ones written with care.

**And confirm the mutation applied.** A search-and-replace whose needle does not match leaves the
code intact, the suite green, and looks exactly like an escaped mutation. Assert on the substitution
(`assert old in source`) before running the tests. This has happened twice. Restore from a copy in a
scratch directory — **never `git checkout <file>` in a mutation loop**, which has twice destroyed
uncommitted work.

**`cargo test` stops at the first failing test binary.** Use `--no-fail-fast` whenever you need the
complete failure set — comparing a branch against its base is meaningless with a truncated list.

**Gates compare against the base commit, not against green.** Preflight
(`crates/coder-sandbox/src/preflight.rs`) fails only on failures *absent from the base*, so a red
base does not trap a correct change. Pre-existing failures are not yours to fix unless that is the
task.

**Tests that shell out to git must set an identity.** `user.email`/`user.name` exist on every dev
machine and on no CI runner, so `git commit` in a temp repo passes locally and fails in CI.

**Windows is a first-class CI target and differs in ways that pass locally.** Line endings
(`core.autocrlf` is on by default there), 8.3 short paths (`RUNNER~1` vs the canonical name), and
`cmd` vs `sh`. Reproduce the runner's condition rather than trusting a green local run —
`GIT_CONFIG_GLOBAL=<file with autocrlf=true>` is often enough.

**Never point a git worktree at the sibling checkouts with a junction.** `turbovault/` and
`turbomcp/` are gitignored path dependencies inside this repo. Creating a scratch worktree and
linking them in with `mklink /J` works — until `git worktree remove --force` follows the junction
and deletes the **contents of the originals**, leaving two empty directories and a workspace that
cannot resolve its own manifest. Re-clone from `ForrestThump/{turbovault,turbomcp}` at `develop`
(the same refs `.github/workflows/ci.yml` uses) and confirm with `cargo metadata --locked`. Copying
them, or putting the worktree somewhere you will delete by path rather than by `git worktree`, both
avoid it.

**A green suite does not prove the lockfile was committed.** CI resolves without `--locked` and
regenerates `Cargo.lock` in place, so adding a dependency and forgetting the lock passes every check
and lands a `main` that fails `--locked` builds. `cargo metadata --locked` is the check that catches
it, and it takes a second.

**Process-global state in tests wants removing, not locking.** `LIBERADO_DATA_DIR` and friends are
read by production code; `cargo test` runs a crate's tests concurrently in one binary, so unguarded
`set_var` / `remove_var` produces flakes that always pass when re-run alone. A lock is the obvious
fix and it is not enough: clippy's `await_holding_lock` forces the guard to drop before the first
`await`, so it covers the *set* and not the clearing, and the window it leaves is real. That window
cost a Windows-only CI failure in `coder-sandbox::checkpoint` — a test cleared `GIT_CONFIG_GLOBAL`
and deleted the file it named, and a concurrent `git init` in an unrelated test died with `fatal:
unknown error occurred while reading the configuration files`. (Git tolerates that variable naming
a file that does not exist; it fails that way when it cannot *access* the path, which on Windows
includes a file mid-deletion.) Prefer an argument the caller
supplies (see `ShadowGit::open_or_init_at`), leaving one test on the env var to pin that the
production entry point still reads it.

## Where the agent-facing machinery lives

- `crates/coder-agent/` — the coding pack (session lifecycle, intake, build, gates, fan-out).
- `crates/coder-tools/` — the tools the model actually calls.
- `crates/coder-sandbox/` — workspaces, worktrees, checkpoints, preflight.
- `crates/executor/` — the bounded decide/act loop shared by all agents.
- `Skills/` — task playbooks (e.g. `cold-review-pr.md`).
- `scripts/pr-shepherd.py` — drives agent PRs to ready-or-blocked on the same differential rule.

**A `succeeded` report is not accepted while `cargo check` is red.** The coding pack refuses it
in the same executor conversation (`WorkspaceCompileGate`, PR #163). Partial, Failed, wrap-up
and turn exhaustion keep the dirty tree. The post-execute ship bar still runs `cargo test`.

**Debugging an agent run: read its trace, do not re-derive it.** Every coding run writes
`<workspace>/coder-traces/<session>.json` recording, per turn, the tools the model was *offered*
(guards withdraw them mid-run, so this changes), its text verbatim, what it called, and why the turn
ended. Four consecutive failures were once each diagnosed by reading Rust and guessing, while the
model's own explanation of the problem sat unrecorded. `[coder] trace_formats` can additionally emit
`openai-messages` — the flat message shape Kilo Code and OpenHands persist — for comparing a run
against another harness on the same task.

The file is `{ session_id, request, events: [...] }` — one flat list, each event tagged by `type`.
The ones worth reading: `model_request_sent` (tools offered *and the system prompt*, once per
distinct hash), `model_turn_finished`, `tool_started` / `tool_finished`, `loop_guard_triggered`,
`critic_verdict`. Reads-per-successful-edit is a dozen lines of Python over `tool_finished` and is
the metric that has tracked real progress.

Traces written **before 2026-08-10 are incomplete** — until PR #124, an attempt that ended on an
unhandled error discarded its whole event log, so one run put 122 tool calls on the wire and 76 in
its traces. Counts from older traces are lower bounds. `CoderEvent::SessionAborted` is the event
that now records a crash and its reason; a trace ending without it ended by decision, not by
accident. The measurement history and its caveats are in
[`docs/future-work/coder-harness-reliability-2026-08.md`](docs/future-work/coder-harness-reliability-2026-08.md),
**including three reasonable hypotheses that were tried and did not work.** Read it before
proposing a fix to the coding pack.

**Check that the config file was loaded at all before blaming a setting.** `liberado-acp` read
`LIBERADO_CONFIG_DIR` directly instead of calling `liberado_config::config_dir()`, so it opted out
of the other three resolution tiers. Nothing set the variable — not Paseo's provider entry, not
`scripts/dispatch-acp-run.js` — so every dogfood run since Paseo landed read **no** `topology.toml`,
`policy.toml`, or `tuning.toml`: no declared project (hence no ship bar), and an empty capability
grant. The bridge now logs the resolved directory and which of the three files exist at startup;
read that line before concluding a setting does not work. Note the installed binary lives in
`~/.cargo/bin`, so the walk-up-from-the-binary tier finds no repo `config/` — an explicit
`LIBERADO_CONFIG_DIR` is the only reliable answer for an installed bridge.

**A config value that parses is not a config value that is read.** Ten settings have shipped green
while a consumer hardcoded a literal instead of reading them — `[coder.gate]`, `[coder.coder]`,
`[coder.progress]`, `trace_dir`, the coder role model, two in `coder-runner/src/main.rs`, the
critic model, and `[coder.workspace]`, which was a documented TOML key serde did not have. Symptom:
changing the setting does nothing, silently. When you add a field to `CoderTuning`, grep every
`CoderRunConfig {` initializer and make sure yours arrives.
`crates/test-support/tests/config_literal_rules.rs` is the mechanical guard — extend it rather than
relying on care.

**Disk exhaustion is a real failure mode here, not a hypothetical.** A coding run died at 0.1 GB
free. `target/` reached 71.6 GB in one checkout, and `cargo-mutants` copies the whole workspace into
`%TEMP%` per run and leaves it there when killed (~23 GB in leaked clones). Check free space before
a long dispatch and sweep `%TEMP%\cargo-mutants-*`. The harness now reports this honestly instead of
telling the model to fix it (PR #119), but it cannot prevent it.

**Reinstall `liberado-acp` after merging anything the bridge links.** A dispatched run tests the
installed binary, not your working tree. A run once silently tested a stale build; it was caught
only by an error string in the trace that no longer existed in the source. Verify with a string
only the new build contains.

When responding to the user, write in ASD-STE100, or Simplified Technical English

And also follow Zinsser's four principles of quality writing:

1. Simplicity
2. Brevity
3. Clarity
4. Humanity
