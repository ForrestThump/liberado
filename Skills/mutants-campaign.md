# Mutation Campaign Skill

## When to use

- Assessing how fresh mutation coverage is for a crate
- Finding survivor counts and commit drift before changing tests
- Running `cargo mutants` so results land in `mutants-ledger.json`
- Fixing survivors and documenting what you triaged

**Start here.** The ledger is the machine scoreboard; markdown reports under
`docs/validation/mutation-testing/` are human triage notes (often historical).

Plan reference: [`docs/future-work/mutants-campaign-ledger-plan.md`](../docs/future-work/mutants-campaign-ledger-plan.md).

## Three layers (do not merge)

| Layer | Path | What it answers |
|---|---|---|
| Ledger | `mutants-ledger.json` | When was the last campaign? At which commit? How many survived? |
| Raw output | `mutants.out/` (gitignored) | Which mutants survived (file, line, diff)? |
| Evidence | `docs/validation/mutation-testing/*.md` | Why each survivor was kept or fixed (historical) |

The ledger stores **counts only**, not per-mutant locations. Read `mutants.out/outcomes.json`
before deleting it if you need the survivor list.

## Fast path: per-mutant verification (do this first)

A full `just mutants <crate>` cycle costs 30+ minutes on a mid-size crate. The fast path
verifies each fix individually and runs the **full campaign once, at the end**. This is what
turns a 100-survivor crate into an afternoon instead of a week:

1. **Save a scratch copy** of the file under test (never rely on `git checkout` mid-loop —
   your own uncommitted tests live in the same files).
2. **Apply the exact mutant by hand.** Assert the original text was present *before*
   replacing it, or a missed needle silently verifies nothing.
3. **Force recompilation.** Same-second edits do not bump mtime and cargo will reuse the
   stale binary — you would be testing the *unmutated* code. Sleep ~1s after editing, or
   confirm `Compiling <crate>` appears in the output.
4. **Run one filtered test:** `cargo test -p <crate> <test_name>`. It must **FAIL**. A pass
   means the test does not kill this mutant — fix the test, not the mutant.
5. **Restore from the scratch copy**, land your new/strengthened test, rerun it green.
6. Repeat per survivor. Then run the recorded campaign once.

## Stale counts: record a fresh baseline first

Ledger survivor counts go stale badly. Seed-era and markdown-seeded rows have lied by 40×
(a row claimed 1 survivor; the fresh run found 40). A row with `viable > 0` can still be
months old. Before committing to a crate:

1. Run `just mutants <crate>` once and let it append a fresh row (append-only keeps history
   honest). Commit that row **before fixing anything** — it makes drift visible to the next
   agent instead of hiding it in a working tree.
2. Re-rank targets from the fresh numbers.

Gap worth building: a `mutants next` flag that flags rows whose counts predate N commits of
drift on the crate directory.

## Baseline timeout: diagnose from debug.log, not guesswork

Symptom: `[mutants] ERROR cargo test failed in an unmutated tree`, exit code 4, no mutants
tested. Three crates died identically before anyone read the log.

Truth source: `mutants.out/debug.log`. The signature line is:

```text
run{phase=Test} ... process_status=Timeout elapsed=3.0s
```

Cause: a cold `target/mutants` makes the first unmutated test phase (which also compiles
doctests) exceed the CLI's `--timeout` floor, and cargo-mutants refuses to start.

Fix: add a per-crate entry to the timeout table in `build_mutants_command`
(`crates/cli/src/mutants_cmd.rs`) plus its unit test. Crates needing entries so far:
`liberado-cli` (120s), `liberado-memory-mcp` (60s), `liberado-conversation-store` (60s),
`liberado-acp-bridge` (10s/120s), `liberado-coder-core`.

## Interrupted runs: recovery ritual

`--in-place` runs mutate this very tree. An aborted or killed run leaves the last-applied
mutation live plus stray files. After ANY interrupted run — before believing any test
failure:

1. `git status --short crates/` — an applied mutant shows as a modified file.
2. `grep -rn "changed by cargo-mutants" crates/` — cargo-mutants marks its edits with
   `/* ~ changed by cargo-mutants ~ */`.
3. Hand-revert marked hunks **from your scratch copy**. Never `git checkout` — your own
   uncommitted tests live in the same files and have been destroyed this way twice.
4. Delete litter: stray `*.json` session fixtures under crate dirs, `xyzzy/` directories,
   marker files. Never `git add -A` in these crates — that is how litter gets committed.
5. `git worktree list` — a killed run has left a registered worktree behind before.
6. `cargo clippy -p <crate>` passes before you trust anything again.

## Known-equivalent mutants: check this list before chasing

These cannot be killed by any test. Recognising them saved four of coder-core's ten
acceptances:

- `usize` bound `>` ↔ `>=` where the loop exits at the boundary anyway; `>= 0` is always true.
- Deleted match arm identical to the `_ => {}` catch-all or to the fallback it would hit
  (e.g. a deny-arm whose fallback is also deny).
- `Ok(Default)` / `vec![]` replacements equal to the hand-built empty return.
- Trait impl bodies already equal to the proposed replacement.
- Duplicate removal absorbed by a following `sort()` + `dedup()`; off-by-one erased by a
  trailing `.trim()` or empty-filter.
- Logging-only side effects (`report_config_dir`-style functions).
- Arithmetic that only matters when it subtracts zero.
- Predicates shielded by an earlier early-return carrying a reset invariant.
- serde visitor methods the deserializer never dispatches to (`visit_str` under
  `from_value`; scalar methods inside `visit_seq` consumers).
- Struct-literal field deletions where the production value equals `Default::default()` —
  unkillable by any test. The legitimate production change is enumerating fields explicitly
  in constructors.

## Test-design pitfalls that hide kills

- **Env-asserting tests must compare captured values, never presence.** cargo-mutants itself
  sets `CARGO_TARGET_DIR`, so a presence-check passes under its own mutant. Save the old
  value, assert against it, restore.
- **Never derive fixture sizes from the constant under test.** A `MAX/2` fixture cannot see
  `1024*1024` become `1024+1024`. Use independent literals.
- **Assert through accessors, not struct fields** — a stubbed accessor survives a field-level
  assert.
- **Capture all tracing fields, not just the message** — the distinguishing evidence (which
  file warned) lives in structured fields. Working examples: orchestrator's `Captured`
  layer, `coder-core/src/prompts_guard_survivor_tests.rs`. Gotcha: `tracing-subscriber`
  needs its `registry` feature or `-p <crate>` builds fail to resolve
  `SubscriberExt::with`.
- **lib-only scope trap:** for `--lib-only` crates, kills made in `tests/` integration files
  do not count toward the ledger. Port the coverage into `#[cfg(test)]` modules or accept the
  inflated number knowingly.
- **Timing traps:** tokio virtual clocks do not move `std::time::Instant` — quiet-window
  logic needs small real durations. SSE bodies never reach EOF under keep-alive — read
  frames under a deadline.

## Cold-start: one crate

Use the **crate directory name** under `crates/` (e.g. `executor`, not `liberado-executor`).

### 1. Workspace health (all crates)

```bash
just mutants-report
# or: just mutants-next          # one dir name: never-campaigned first, else highest drift
```

Report sections:

- **Never campaigned** — no `scope: "package"` row in the ledger
- **Historical only** — rows exist but `commit: null` (markdown-era seeds); drift unknown
- **Most drift** — latest SHA campaign per crate, ranked by commits touching `crates/<dir>/`

Each drift line looks like:

```text
  executor [kernel] — 4 commits since 82b2855826c3 — 12 files changed, 340 insertions — viable 168 caught 139 survived 29 timeout 0
```

That line gives you **commits ahead** (`git rev-list --count <sha>..HEAD -- crates/<dir>`),
**survivors** (`survived`), and **catch rate inputs** (`viable`, `caught`, `timeout`).
Treat these counts as stale until proven otherwise — see the fresh-baseline rule above.

### 2. One crate from the ledger (recency + counts)

When you need `recorded_at` or the full commit SHA, grep the ledger:

```bash
python - <<'PY'
import json
from pathlib import Path

DIR = "executor"          # crates/<dir>
ledger = json.loads(Path("mutants-ledger.json").read_text())
PKG = "liberado-executor" # adjust if needed

rows = [
    c for c in ledger["campaigns"]
    if c["package"] == PKG and c.get("scope") == "package" and c.get("commit")
]
if not rows:
    print(f"{DIR}: never campaigned with a commit SHA")
else:
    latest = rows[-1]
    c = latest["counts"]
    print(f"package:     {latest['package']}")
    print(f"recorded_at: {latest['recorded_at']}")
    print(f"commit:      {latest['commit']}")
    print(f"survived:    {c['survived']}  (viable {c['viable']}, caught {c['caught']}, timeout {c['timeout']})")
PY
```

**Trust a row only when `counts.viable > 0` — and even then only after the drift check.**

### 3. Commits ahead (independent check)

```bash
SHA=82b2855826c3654e490402ce35665476c853f614   # from ledger or report
DIR=executor
git rev-list --count "${SHA}..HEAD" -- "crates/${DIR}"
git diff --shortstat "${SHA}..HEAD" -- "crates/${DIR}"
```

If the campaign commit is not an ancestor of `HEAD`, `just mutants-report` prints
`commit not in this history` — re-run the crate on the current branch.

## Run a campaign (save results)

### Preferred: one crate via just

```bash
just mutants <crate-dir>
# coder-agent only (lib tests; e2e hangs under mutants):
just mutants-agent
```

This runs `liberado mutants run`, which:

1. Invokes `cargo mutants` with repo timeout flags
2. Builds into `target/mutants/` (isolated from `target/debug/`)
3. On Windows, uses `--in-place` (no `%TEMP%` workspace copy)
4. Appends one row to `mutants-ledger.json` when `mutants.out/outcomes.json` is complete

### Verify the ledger append

```bash
just mutants-report | grep -E '^  <crate-dir> '
# stderr from the run should include:
#   [mutants] recorded campaign for liberado-<crate> at <sha>
```

If you see `outcomes were incomplete; nothing recorded`, the run did not append. Re-run after
fixing the failure (config parse, disk full, file lock, killed process). A completed-but-empty
campaign (zero viable) is refused outright — it must never shadow the crate's real last row.

### Record the row at the SHA you tested

Commit your work **first**, then record. A row whose test files were still uncommitted points
drift accounting at the wrong base.

Manual ingest when `mutants.out/` already exists:

```bash
just mutants-record <crate-dir>
# or without just:
CARGO_TARGET_DIR=target/liberado-invoke cargo run --locked --quiet -p liberado-cli -- mutants record <crate-dir>
```

### Disk and Windows (read before long runs)

- Check free space: mutants need headroom for `target/mutants/` (often 2–20 GB per large crate).
- Reclaim with `rm -rf target/mutants target/debug/incremental` — these are the two sinks.
  The symptom mapping: **"`cargo test failed in an unmutated tree`" is often disk-full at
  100%**, not a config problem.
- Sweep stale `%TEMP%\cargo-mutants-*` if copy mode was used elsewhere.
- Bulk baseline (local only): `.liberado/mutants-baseline-campaign.ps1` — one crate at a time,
  deletes `target/mutants/` after each crate. Not for CI.
- **`--in-place` is needed on Linux too** when `turbovault/` or `turbomcp/` are path dependencies
  inside the repo. `cargo-mutants` copies the workspace to a temp dir and the sibling paths break.
  Pass `--in-place` alongside the timeout flags (same as Windows). This is now unconditional in
  the CLI's generated command.

Config: `.cargo/mutants.toml` — do **not** put `timeout` there (removed in cargo-mutants 27.x);
timeouts come from the CLI (`build_mutants_command` in `crates/cli/src/mutants_cmd.rs`).

Tooling note: `just` itself may need `cargo install just` on a fresh Linux box, and even
read-only recipes pay one cold `target/liberado-invoke` rebuild.

## List survivors (for fixing)

After `just mutants <crate-dir>`, before deleting `mutants.out/`. **cargo-mutants 27.x
schema**: outcomes carry `summary` (`MissedMutant` | `CaughtMutant` | `Unviable` |
`Timeout` | `Success` for the baseline), and the location lives in
`scenario.Mutant.name` (`file.rs:LINE:COL: description`). There is no `status: "missed"`
and no `display_name`; `function` (and its `span`) is **absent on some mutants**, so guard
before reading it.

```bash
python - <<'PY'
import json
from pathlib import Path

data = json.loads(Path("mutants.out/outcomes.json").read_text())
for entry in data.get("outcomes", []):
    if entry.get("summary") != "MissedMutant":
        continue
    scenario = entry.get("scenario", {})
    if not isinstance(scenario, dict):
        continue                      # the baseline row is a plain string
    mu = scenario.get("Mutant", {})
    print(f"{mu.get('file', '?')}:{mu.get('span', {}).get('start', {}).get('line', '?')}: {mu.get('name', '?')}")
PY
```

`cargo mutants` also prints missed mutants at the end of the run. Copy that list into your
working notes if you will clean `mutants.out/` immediately.

## Fix survivors

Follow the repo mutation rule, using the per-mutant fast path above:

1. **Save a backup** of the file under test (scratch copy — not `git checkout` in a loop).
2. **Apply the mutation** (assert the old text is present before replacing).
3. **Force recompile** (sleep ≥1s or confirm `Compiling`), **run the targeted test** — it must
   fail.
4. **Restore from backup**, add or strengthen the test.
5. **Re-run** `just mutants <crate-dir>` once at the end and append a new ledger row (never
   edit old rows).

Squashing survivors means a **new campaign row with lower `survived`**, not editing history.

## Ledger etiquette for concurrent agents

Parallel agents appending to one ledger guarantee conflicts:

- **Resolution is always the union of rows.** Both sides' appends are valid campaigns;
  drop only exact duplicates.
- Two branches hardening `mutants_cmd.rs` (timeout tables, guards) **will** collide with each
  other — whoever merges second resolves by combining entries.
- Merge order matters; coordinate through the integrating agent.
- **Never push the shared ledger branch directly.** Land on a feature branch and let the
  integrator apply module-health treatment (inline test mods regress file metrics) and gates.
- **Keep new tests out of `src/*.rs`** — use `#[path]` sibling files like the ones already on
  the branch. Module-health waivers exist for load-bearing size only; a waiver reason that
  reads as laziness gets the whole contribution pushed back for rework. See the acceptance
  bar in `module-health.toml`.
- Push after every verified batch. On push rejection: fetch, merge immediately, re-run the
  crate's tests, then push again.

## Document survivors (human layer)

When you fix or accept survivors, update or add a report under
`docs/validation/mutation-testing/`:

- Name: `mutation-testing-report-<crate-dir>.md` (existing pattern)
- Header: `status: historical`, `authority: evidence`
- Table: location, mutant, action (test added / accepted false positive / timeout)
- Link the ledger row: `recorded_at`, full commit, `survived` count after your run

Historical reports describe July 2026 campaigns. The ledger may be newer — treat markdown as
triage notes, ledger as the scoreboard.

## Quick reference

| Goal | Command |
|---|---|
| All crates health | `just mutants-report` |
| Next crate to run | `just mutants-next` |
| Run + record | `just mutants <crate-dir>` |
| Ingest existing output | `just mutants-record <crate-dir>` |
| Commits since campaign | `git rev-list --count <sha>..HEAD -- crates/<dir>` |
| Survivor locations | Parse `mutants.out/outcomes.json` (see above) |
| After an interrupted run | Recovery ritual (see above) |

**Not wired into** `just ci`, `just preflight`, or `just ready`.

## Gaps to know

| Gap | Workaround |
|---|---|
| No `just mutants-status <dir>` | `just mutants-report` line + ledger Python snippet above |
| `recorded_at` not in report | Read last matching row in `mutants-ledger.json` |
| Ledger has counts, not locations | `mutants.out/outcomes.json` or terminal output |
| Historical-only crates | Need a fresh `just mutants <dir>` to set drift clock |
| Multiple rows per package | **Last** `scope: "package"` row with a commit is current |
| Drift-stale counts not flagged | Fresh baseline first (see above); a `mutants next` drift flag is unbuilt |
| Log-capture subscriber hand-copied per crate | Copy orchestrator's `Captured` layer; promotion into `test-support` is unbuilt |
