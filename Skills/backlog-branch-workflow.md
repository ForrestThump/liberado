# Backlog branch workflow

## When to use

- Picking up self-scoped work from `docs/roadmap.md` / `docs/future-work/backlog.md`
- Shipping one backlog item per branch without merging it yet
- Keeping several unmerged agent branches straight without polluting git history

## Rules (read first)

1. **One item per branch.** Confirm the backlog row is still open (not struck through / not marked Landed) before you start.
2. **Do not edit** `docs/future-work/backlog.md` status rows in the PR or feature branch.
3. **`current_unmerged_work.md` is local only.** It lives at the repo root, is listed in `.git/info/exclude`, and is **never committed**. Update it when you finish or abandon a branch.
4. **CI-equivalent gates must pass** on the feature branch before you call the item done:
   - `cargo fmt --all --check`
   - `cargo test --workspace --no-fail-fast`
   - `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings`
5. **No test theatre.** Tests must drive the real shipped entry point. Break the production path once per claimed behaviour, watch a real test fail, restore, and keep the failure output.
6. Prefer **scoped** `cargo mutants` (or manual per-behaviour mutation) over unscoped workspace mutants.

## Flow

### 1. Pick an open item

```text
docs/roadmap.md          → why / priority
docs/future-work/backlog.md → what next (one row per PR)
```

Check dependencies: do not start an item that needs an unmerged prerequisite (e.g. 0.6 needs 0.5).

### 2. Branch from up-to-date `main`

```bash
git fetch origin
git checkout main
git pull --ff-only   # if remote is ahead
git checkout -b feat/<short-item-name>
```

### 3. Implement and test

- Keep the change focused on the backlog acceptance criteria.
- Add or extend real tests that fail if the shipped path is broken.
- For each changed behaviour: mutate production code → test fails → restore.
- Capture gate logs and mutation evidence under your session scratch dir (not a shared temp path).

### 4. Gates

```bash
cargo fmt --all --check
cargo test --workspace --no-fail-fast
cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings
# optional, scoped:
cargo mutants -p <crate> --file <path> ...
```

Fix failures before recording the branch as ready.

### 5. Record unmerged work (local only)

Edit `current_unmerged_work.md` at the repo root:

| Backlog | Branch | Status |
|---|---|---|
| **N.N** short title | `feat/...` | Implemented; not merged |

Confirm ignore:

```bash
git check-ignore -v current_unmerged_work.md
# must print a hit from .git/info/exclude
```

Do **not** `git add` this file.

### 6. Leave the branch for review / later merge

Push only if asked. Rebase onto `main` when `main` moves:

```bash
git checkout feat/<branch>
git rebase main
```

When the branch merges, remove its row from `current_unmerged_work.md` locally.

## Skill commits vs feature work

- Playbooks under `Skills/` that change the *process* may land on `main` as a small docs commit.
- Feature code stays on its own branch and is listed in `current_unmerged_work.md` until merged.
