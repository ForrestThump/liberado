# Mutants Campaign

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

### 2. One crate from the ledger (recency + counts)

When you need `recorded_at` or the full commit SHA, grep the ledger:

```bash
python - <<'PY'
import json
from pathlib import Path

DIR = "executor"          # crates/<dir>
ledger = json.loads(Path("mutants-ledger.json").read_text())
# Map dir → package via crate-map or report output; package names are liberado-* except chat-client-contract.
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
    print(f"command:     {latest.get('command', '')}")
PY
```

**Trust a row only when `counts.viable > 0`.** A row with all zeros is a crashed or partial
run that still appended — treat it as no campaign. (The CLI now refuses to record zero-viable
runs, so new rows should always be real; older rows predate the guard.)

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
fixing the failure (config parse, disk full, file lock, killed process).

Manual ingest when `mutants.out/` already exists:

```bash
just mutants-record <crate-dir>
# or without just:
CARGO_TARGET_DIR=target/liberado-invoke cargo run --locked --quiet -p liberado-cli -- mutants record <crate-dir>
```

### Disk and Windows (read before long runs)

- Check free space: mutants need headroom for `target/mutants/` (often 2–20 GB per large crate).
- Delete stale `%TEMP%\cargo-mutants-*` if copy mode was used elsewhere.
- If `target/` is huge, `cargo clean` or remove `target/` before a batch — compile cost returns,
  disk does not refill to hundreds of GB when using `target/mutants/` and cleaning after each crate.
- Bulk baseline (local only): `.liberado/mutants-baseline-campaign.ps1` — one crate at a time,
  deletes `target/mutants/` after each crate. Not for CI.
- **`--in-place` is required everywhere now** — `liberado mutants run` passes it unconditionally
  (it used to be Windows-only). On any host, cargo-mutants' temp-dir copy drops the gitignored
  sibling checkouts (`turbovault/`, `turbomcp/`), so manifest resolution fails and the run dies
  at the baseline build. If you invoke `cargo mutants` by hand, pass `--in-place` yourself.
- **An interrupted in-place run leaves live mutations.** Kill a run mid-test and the mutated
  source may stay on disk (found: `describe_failures` replaced by `vec!["xyzzy"]`, plus stray
  test files under the crate dir). After any interruption: `git status --short crates/`, restore
  modified files from git, delete untracked litter, and re-run clippy before trusting the tree.
- **Linux disk dies quietly too**: a fresh `target/mutants/` plus baseline builds consumed
  ~12 GB of headroom mid-campaign and hit 100% full, which surfaces only as
  `cargo build failed in an unmutated tree`. Check `df -h .` before a batch; `rm -rf
  target/mutants target/debug/incremental` reclaims the bulk (the latter costs warm-rebuild time).
- **First `just mutants-*` call per session is slow**: every recipe builds the CLI into its own
  `CARGO_TARGET_DIR=target/liberado-invoke`, which starts cold even when `target/debug` is warm
  (~5 min here). Read-only recipes (`mutants-report`, `mutants-next`) pay it too. Subsequent
  calls are instant until the CLI changes.

Config: `.cargo/mutants.toml` — do **not** put `timeout` there (removed in cargo-mutants 27.x);
timeouts come from the CLI (`--timeout 3.0` in `mutants_cmd.rs`). Crates whose integration tests
need longer than 3s on a cold cache get an entry in the timeout table there (`liberado-cli`,
`liberado-memory-mcp`); without one, the *unmutated baseline* times out and the whole campaign
dies before any mutant runs.

## List survivors (for fixing)

After `just mutants <crate-dir>`, before deleting `mutants.out/`:

```bash
python - <<'PY'
import json
from pathlib import Path

p = Path("mutants.out/outcomes.json")
if not p.is_file():
    raise SystemExit("no outcomes.json — run just mutants <crate-dir> first")

data = json.loads(p.read_text())
for entry in data.get("outcomes", []):
    if entry.get("status") != "missed":
        continue
    m = entry["scenario"]["Mutant"]
    loc = f"{m.get('source_file')}:{m.get('line', '?')}"
    name = m.get("display_name") or m.get("function_name") or "?"
    print(f"{loc}  {name}")
PY
```

`cargo mutants` also prints missed mutants at the end of the run. Copy that list into your
working notes if you will clean `mutants.out/` immediately.

## Fix survivors (per mutant)

Follow the repo mutation rule from `AGENTS.md`:

1. **Save a backup** of the file under test (scratch copy — not `git checkout` in a loop).
2. **Apply the mutation** (assert the old text is present before replacing).
3. **Run the targeted test** — it must fail.
4. **Restore from backup**, add or strengthen the test.
5. **Re-run** `just mutants <crate-dir>` and append a new ledger row (never edit old rows).

Squashing survivors means a **new campaign row with lower `survived`**, not editing history.

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

**Not wired into** `just ci`, `just preflight`, or `just ready`.

## Gaps to know

| Gap | Workaround |
|---|---|
| No `just mutants-status <dir>` | `just mutants-report` line + ledger Python snippet above |
| `recorded_at` not in report | Read last matching row in `mutants-ledger.json` |
| Ledger has counts, not locations | `mutants.out/outcomes.json` or terminal output |
| `historical only` crates | Need a fresh `just mutants <dir>` to set drift clock |
| Multiple rows per package | **Last** `scope: "package"` row with a commit is current |
