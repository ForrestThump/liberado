---
kind: reference
status: active
authority: normative
domain: coding-harness
canonical_for: cargo-targets
open_items: false
last_verified: 2026-09-03
---

# Managed Cargo target directories

Liberado can reuse one Cargo target directory for ordinary coding builds, and it
keeps incompatible jobs on isolated targets. The allocator lives in
`crates/coder-sandbox/src/cargo_targets.rs`. It chooses paths and holds leases.
It does not spawn Cargo.

## Why this exists

A coding worktree starts with no `target/`. A cold check of this workspace
rebuilds hundreds of crates. Docs-only and other unchanged work can reuse
Cargo fingerprints when the cache is compatible. The old operator pattern
deleted finished trees with Disk Saver, so the next run paid the cold cost
again.

Symlinks into another checkout's `target/` and one blind shared directory are
not safe. Cargo can reuse freshness state or a same-named binary from the
wrong source root.

## Classes

| Class | May share | Typical caller | Why |
|---|---|---|---|
| `ordinary` | Yes, one source root | ACP coding, warm-up, ship-bar baseline | Default `dev` profile. Registry crates fingerprint by name and version. |
| `coverage` | No | `liberado ci crap`, Debian CRAP | `llvm-cov` objects contaminate a normal cache. |
| `mutation` | No | `liberado mutants run` | Mutated sources must not evict `target/debug`. |
| `comparison` | No | `liberado coder compare` / C3 | Each harness has its own pinned worktree. Sharing across roots has reused the wrong binary. |

## Operator settings

`[coder.workspace]` in `tuning.toml`. Both keys stay **unset by default**. This
does not change coding-pack defaults or C3 harness pins.

```toml
[coder.workspace]
# Exact CARGO_TARGET_DIR. C3 writes this per harness. Existing operators keep it.
# shared_target_dir = "C:/Users/you/.liberado/shared-target"

# Class-aware pool. Ordinary jobs share; incompatible jobs isolate.
# managed_target_root = "C:/Users/you/.liberado/cargo-targets"
```

Resolution for an ordinary coding run:

1. Non-empty `shared_target_dir` — use that exact path.
2. Non-empty `managed_target_root` — use
   `<root>/shared/<source-hash>/ordinary`.
3. Otherwise — worktree-local `target/`.

ACP applies this once per job from the session's project root (the
client cwd), after the worktree exists. Warm-up still builds in the
durable worktree, but the cache identity is that project root, so
worktrees of one repo share and unrelated repos do not. `run_command`
and ship-bar baseline inherit the same allocation.

Coverage, mutation, and comparison callers do not read these keys for their
artifact dirs. They keep the isolated paths they already own.

## Layout under `managed_target_root`

```text
<managed_target_root>/
  shared/<source-hash>/ordinary/
  isolated/<class>/<job-id>/
```

Each allocated directory carries `.liberado-target-class`. A later job that
asks for a different class is refused. Isolated jobs also take
`.liberado-target.lock` (`class=` + `pid=`). A live lock is exclusive. A
stale lock (pid 0 or a dead process) can be replaced.

`TargetPool::reclaim_isolated(older_than)` deletes isolated trees whose lock
is gone or dead and whose directory is older than the threshold. Shared
ordinary caches are kept. An isolated lease with `reclaim_on_drop` removes
its tree when the job ends.

## Concurrency

Cargo already takes an exclusive artifact lock on one target directory.
Two ordinary builds that share a cache queue; they do not corrupt each
other. A command timeout behind a cold build can still expire having done
no work. For one source root, run one ordinary compile-heavy job at a
time, or give the second job its own isolated target.

Incompatible classes never attach to the shared ordinary path. If a shared
directory is already stamped for another class, allocation fails instead of
mixing profiles.

Cross-process sharing of one ordinary cache is safe only while every job is
ordinary and from the same source root. Do not point `llvm-cov`,
cargo-mutants, or a C3 harness at that directory.

## What stays isolated

- **C3 / harness compare** — `execution/targets/<harness>`. The runner writes
  that path into Liberado's `shared_target_dir` so the Liberado harness uses
  its own cache. Do not change those pins.
- **Mutation** — `CARGO_TARGET_DIR=<crate>/target/mutants` (see
  [`local-readiness.md`](local-readiness.md)).
- **Coverage / CRAP** — separate driver and coverage targets. Linux-only
  coverage must not mix with Windows metadata or `target/debug`.

## Baseline and docs-only reuse

Ship-bar baseline computation honors a live `CARGO_TARGET_DIR` before it
falls back to `workspace/target`. When ACP applies a managed or exact
shared ordinary cache for the session's project root, warm-up, model
`run_command`, and baseline compare use the same directory. Unchanged
crates then increment instead of filling a new worktree `target/`.
