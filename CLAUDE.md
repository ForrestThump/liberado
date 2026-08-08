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

**Process-global state in tests needs a lock.** `LIBERADO_DATA_DIR` and friends are set by several
tests; `cargo test` runs a crate's tests concurrently in one binary, so unguarded `set_var` /
`remove_var` produces flakes that always pass when re-run alone.

## Where the agent-facing machinery lives

- `crates/coder-agent/` — the coding pack (session lifecycle, intake, build, gates, fan-out).
- `crates/coder-tools/` — the tools the model actually calls.
- `crates/coder-sandbox/` — workspaces, worktrees, checkpoints, preflight.
- `crates/executor/` — the bounded decide/act loop shared by all agents.
- `Skills/` — task playbooks (e.g. `cold-review-pr.md`).
- `scripts/pr-shepherd.py` — drives agent PRs to ready-or-blocked on the same differential rule.
