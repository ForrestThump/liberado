# Backlog branch workflow

## When to use

- Picking up self-scoped work from `docs/roadmap.md` / `docs/future-work/backlog.md`
- Shipping one backlog item per branch without merging it yet
- Keeping several unmerged agent branches straight without polluting git history

## Rules (read first)

1. **One item per branch.** Confirm the backlog row is still open (not struck through / not marked Landed) before you start.
2. **Follow the total order.** Take the first open, unblocked item in the backlog implementation
   order. Do not select by convenience. Skip only an external wait or access blocker, and record it.
3. **Declare the integration shape before branching.** Record the base SHA, predecessor, shared
   files, and merge order in `current_unmerged_work.md`.
4. **Do not edit** `docs/future-work/backlog.md` status rows in the PR or feature branch.
5. **`current_unmerged_work.md` is local only.** It lives at the repo root, is listed in `.git/info/exclude`, and is **never committed**. Update it when you finish or abandon a branch.
6. **CI-equivalent gates must pass** on the feature branch before you call the item done:
   - `cargo fmt --all --check`
   - `cargo test --workspace --no-fail-fast`
   - `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings`
7. **No test theatre.** Tests must drive the real shipped entry point. Break the production path once per claimed behaviour, watch a real test fail, restore, and keep the failure output.
8. Prefer **scoped** `cargo mutants` (or manual per-behaviour mutation) over unscoped workspace mutants.

## Flow

### 1. Pick an open item

```text
docs/roadmap.md          → why / priority
docs/future-work/backlog.md → what next (one row per PR)
```

Check dependencies: do not start an item that needs an unmerged prerequisite (e.g. 0.6 needs 0.5).

### 2. Choose the base and branch

If the item depends on an unmerged predecessor or changes the same integration points, branch from
that predecessor. Otherwise, branch from current `main`. Do not stack unrelated work.

```bash
git fetch origin
git checkout <main-or-predecessor>
git pull --ff-only   # only for a tracked remote branch
git checkout -b feat/<short-item-name>
```

For a stacked branch, either open the PR against its predecessor or wait for the predecessor to
merge, then rebase onto `main` before opening. Put the base SHA, predecessor, shared files, and merge
order in the PR body.

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
| **N.N** short title | `feat/...` | Base `<sha>`; predecessor `<item-or-none>`; overlaps `<paths-or-none>`; merge after `<item-or-main>`; implemented, not merged |

Confirm ignore:

```bash
git check-ignore -v current_unmerged_work.md
# must print a hit from .git/info/exclude
```

Do **not** `git add` this file.

### 6. Leave the branch for review / later merge

Push only if asked. After a predecessor merges, rebase its dependent branches onto the new `main`,
rerun all gates, and require fresh GitHub CI:

```bash
git checkout feat/<branch>
git rebase main
git push --force-with-lease
```

When the branch merges, remove its row from `current_unmerged_work.md` locally.

## Skill commits vs feature work

- Playbooks under `Skills/` that change the *process* may land on `main` as a small docs commit.
- Feature code stays on its own branch and is listed in `current_unmerged_work.md` until merged.
