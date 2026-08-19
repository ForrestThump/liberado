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

### 2. Surface the CRAP report

```bash
cargo llvm-cov --workspace --exclude liberado-webui --lcov \
  --output-path .liberado/crap.lcov --ignore-run-fail
cargo crap --workspace --lcov .liberado/crap.lcov --min 450        # ceiling
cargo crap --workspace --lcov .liberado/crap.lcov --format json    # full table
```

Rank the JSON two ways:

- Worst offenders by CRAP (the "cover or split" track)
- The CC >= 20 band regardless of coverage (the "must split" track)

cargo-crap scores a function at 0% coverage when it is absent from the
LCOV (binary `main`s, excluded crates, test-only modules).

### 3. Ratchet rules

`crap-baseline.json` is committed. Linux CI runs the per-function
ratchet (`--fail-regression`); local Windows is ceiling-only (450).

- Adding tests lowers scores — always safe
- Splitting a function creates new names with no baseline — ceiling only
- Never raise a function's score; if a refactor would, do not ship it

## Per-site flow

1. Read the function and its module `//!` docs
2. Bias: add unit tests that exercise the function
3. Binary `main`: extract args / decision logic into free functions;
   keep `main` thin; the verbatim move is documented, not unit-asserted
4. Mutation-verify the new tests: break a branch, the test must fail,
   restore from `$TEMP/mut_backup` — never `git checkout` in a mutation
   loop (it destroys uncommitted work)
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
```

## Rules that trip people

- Mutation must actually apply: assert the old text is in the source
  before replacing; a missed needle looks like an escaped mutation
- `cargo test` stops at the first failing binary: use `--no-fail-fast`
- Tests that shell out to git must set `user.email` / `user.name`
- Windows is a CI target: line endings, 8.3 paths, `cmd` vs `sh`
- A config value that parses is not a value that is read: when adding a
  `CoderTuning` field, grep every `CoderRunConfig {` initializer
- A mutation that does not compile is not a mutation test
