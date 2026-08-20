# CRAP Harden Campaign

## When to use

- Reducing CRAP / cyclomatic complexity across the workspace
- One site per commit; the full CI gate must pass before each commit

## Repo-state assessment

### 1. Branch and dependency check

```bash
git status --short
git branch --show-current
git log origin/main..HEAD --oneline
cargo metadata --locked          # fails when turbovault/ or turbomcp/ is missing
```

The sibling checkouts are gitignored path dependencies. Never link them
with a junction; re-clone instead:

```bash
git clone -q -b develop git@github.com:ForrestThump/turbovault.git turbovault
git clone -q -b develop git@github.com:ForrestThump/turbomcp.git  turbomcp
cargo metadata --locked          # confirm the manifest resolves
```

Missing siblings fail llvm-cov with "The system cannot find the path
specified" from inside `cargo clean` — the message does not name the
siblings. Check this first.

### 2. Surface the CRAP report

The LCOV file is mandatory: without `--lcov`, cargo-crap reports
"No functions found." and exits 0. Generate it once per batch (it is a
full instrumented rebuild + suite run, 20-40 minutes), not per site:

```bash
cargo llvm-cov --workspace --exclude liberado-webui --lcov \
  --output-path .liberado/crap.lcov --ignore-run-fail

cargo crap --workspace --lcov .liberado/crap.lcov --min 450        # ceiling
cargo crap --workspace --lcov .liberado/crap.lcov --format json \
  --sort crap --output .liberado/crap-current.json                 # ranked table
python - <<'PY'
import json
rows = json.load(open(".liberado/crap-current.json"))["entries"]
for e in rows[:25]:                                    # worst offenders
    print(f'{e["crap"]:6.0f} {e["cyclomatic"]:3.0f} {e["function"]} @ {e["file"]}:{e["line"]}')
cc20 = [e for e in rows if e["cyclomatic"] >= 20]      # must-split band
print(f'CC >= 20: {len(cc20)}')
PY
```

Rank two ways:

- Worst offenders by CRAP (the "cover or split" track)
- The CC >= 20 band regardless of coverage (the "must split" track)

cargo-crap scores a function at 0% coverage when it is absent from the
LCOV (binary `main`s, excluded crates, test-only modules), and counts
every `?` / `ok_or_else` as a branch. Prefer `let-else` over chains of
`?` in the function you are measuring; a tail `?` is free.

### 3. Ratchet rules

`crap-baseline.json` is committed at the repo root (the last best
per-function score). Ubuntu CI runs the per-function ratchet
(`liberado ci crap` — `--fail-regression`); Windows and other hosts are
ceiling-only (450). To compare locally run:

```bash
cargo run --locked --quiet -p liberado-cli -- ci crap
```

- Adding tests lowers scores — always safe
- Splitting a function creates new names with no baseline — ceiling only
- Never raise a function's score; if a refactor would, do not ship it

## Per-site flow

1. Read the function and its module `//!` docs
2. Bias: add unit tests that exercise the function
3. Binary `main`: prefer integration tests that spawn the real binary
   via `env!("CARGO_BIN_EXE_<name>")` with `std::process::Command` — no
   new dev-deps needed, and llvm-cov attributes the spawned run. If the
   binary cannot run headless (a TTY loop), extract the decision logic
   into free functions, keep `main` thin, and document the verbatim move
4. Mutation-verify the new tests: save a backup of the CURRENT working
   state, break a branch, the test must fail, restore from that backup.
   Label backups per state (`file_refactored.rs`) — a backup taken before
   the refactor will silently revert it. Never `git checkout` in a
   mutation loop
5. Full CI gate green
6. One commit per site; push; re-surface at the end

## Full CI gate (before each commit)

```bash
cargo clippy --workspace --exclude liberado-webui --all-targets -- \
  -D warnings -D clippy::cognitive_complexity
cargo fmt --check
cargo test --workspace --no-fail-fast
cargo run --locked -p liberado-cli -- shepherd --self-test
cargo run --locked -p liberado-cli -- docs check-links
cargo run --locked -p liberado-cli -- docs metadata self-test
cargo run --locked -p liberado-cli -- docs metadata lint
cargo run --locked -p liberado-cli -- docs crate-map
cargo run --locked -p liberado-cli -- docs metadata check-stale-rs
cargo run --locked -p liberado-cli -- docs site --out "$TEMP/docs-site"
```

CI-only surfaces (run once per batch, not per commit):

```bash
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc \
  --workspace --no-deps --document-private-items
cargo deny check        # needs cargo-deny; the yaml: taiki-e/install-action
```

CI runs `fmt --check` and the per-function ratchet on Ubuntu only; a
Windows host lints the `#[cfg(windows)]` code the Ubuntu leg cannot see.
Clippy runs on both OSes for exactly that reason.

## Rules that trip people

- Mutation must actually apply: assert the old text is in the source
  before replacing; a missed needle looks like an escaped mutation
- A mutation that does not compile is not a mutation test: labels bind
  loops, so `'label: while let` cannot become `if let` without also
  dropping the label and its `break 'label`
- `cargo test` stops at the first failing binary: use `--no-fail-fast`
- Gate against the base, not against green: a failing test that passes
  in isolation is likely a pre-existing flake, not yours
- Tests that shell out to git must set `user.email` / `user.name`
- Windows is a CI target: line endings, 8.3 paths, `cmd` vs `sh`
- A config value that parses is not a value that is read: when adding a
  `CoderTuning` field, grep every `CoderRunConfig {` initializer

## Campaign record (2026-08, branch `harden-main-crap`)

This playbook went through one full campaign and reached its terminal state — **zero
functions above CRAP 150** (3,285 analyzed). Recorded here so a future reader does not mistake
0-over-150 for unfinished 300-ceiling work.

What cleared each band (each site committed once, full CI gate green per commit):

- **450 → 300:** `cost main` (380), `tui run_loop` (380), `tui spawn_poller`, server
  `goals_rewind`, `session_event_to_sse`, `acp-bridge dispatch_sse`, `cli cmd_smoke`,
  `tui start_chat_stream` — extraction + integration/binary tests.
- **300 → 200:** `tui join_goal_session`, `cli docs_meta run`, `acp-bridge
  run_coding_prompt`, `bootstrap build_dispatch_pack`.
- **CC ≥ 20 reductions** (below 20 by construction): `EffectRunner::run`,
  `attach_conversation_stream`, `extract_ts_symbol`, `draw`.
- **200 → 150:** `to_goal_event`, `cli tick`, `chat_stream_core`, the three tuner loops
  (`run_tuner`/`run_coder_tuner`/`run_tool_loop_tuner` — `gather_*`/`score_pool_*`
  extracted, pure `finalize_result*` seams), `tuner main`.

Two PRs merged through the branch: **#190** (Telegram mock tests, −562 CRAP across
`telegram.rs`) and **#191** (coder-agent test expansion, +49 tests). Two CI regressions were
fixed on the way: the ubuntu per-function ratchet regression, and the `GIT_CONFIG_GLOBAL`
config-deletion race in `coder-runner`. Full surface at the 150 band: **0 functions over 150**;
workspace suite 3782 green.

Known ceiling scoring gotcha this campaign confirmed: the tuner and `tui main` binary entry
points score CC 13 at 0% cover even after their tail is extracted, because each `?` in the body
counts as a branch — a binary `main` that shells out or writes files carries many `?`s. The
reliable lever is extracting the model-bound loops into untested helpers (driver drops to ~4
CC) or pure `finalize_*` seams (tested).