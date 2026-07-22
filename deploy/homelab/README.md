# Liberado homelab deploy

**One command deploys. One command tells you what's live. Don't hand-run the steps.**

```bash
# from the repo root on your dev machine (needs ssh to the box):
bash deploy/homelab/deploy.sh            # deploy current branch HEAD
bash deploy/homelab/deploy.sh <ref>      # deploy a specific branch/tag/sha
```

That's the whole happy path. The script syncs the exact committed tree, rebuilds `liberado:dev`,
recreates the container, and **won't return success until the live daemon reports the SHA it just
built**. If it prints `OK  running=true  build-sha=<sha>`, the new code is actually running.

## What is deployed right now?

Never guess from uptime or file dates. Ask the running container:

```bash
ssh shiloh@192.168.0.144 'docker exec liberado cat /etc/liberado-build-sha'
# -> a git sha. `git show <sha>` in this repo tells you exactly what code is live.
curl -fsS http://192.168.0.144:4201/api/status          # running / model / attachments
```

## Why a script instead of `docker build` + `docker compose up`

Build and run are **decoupled** on the box: source lives in `~/liberado-build` (a plain copy, no
`.git`), the image is `liberado:dev`, and compose runs it. Hand-running those steps is how deploys
drift — the classic failures, all of which the script removes:

| Manual footgun | What the script does instead |
|---|---|
| Rebuild the image but forget to recreate the container (old code keeps running) | Always `up -d --force-recreate` after build |
| Recreate against a stale image | Rebuilds first, in the same run |
| Copy a half-synced / dirty tree; no record of what commit it was | Ships a **committed** ref via `git archive` + `rsync --delete`; writes `DEPLOYED_COMMIT` |
| Sync the main tree but drop the vendored `turbovault/` + `turbomcp/` → build dies on a missing `Cargo.toml` | Ships those nested repos too (see below); preflights that they exist before touching the box |
| No way to know what's live | Bakes the SHA into the image (`/etc/liberado-build-sha`, `LIBERADO_BUILD_SHA`, image label) and **verifies it after boot** |
| "It didn't come up" goes unnoticed | Health-gates on `/api/status`; dumps logs and fails loudly if it doesn't converge |

### The vendored-repo footgun

`turbovault/` and `turbomcp/` are **gitignored nested git repos** consumed as Cargo *path*
dependencies (see the co-dev note in the root `Cargo.toml`). Because they're gitignored, a
`git archive` of the main repo does **not** contain them — sync only that and the in-container build
fails with `failed to read turbomcp/crates/turbomcp/Cargo.toml`. `deploy.sh` ships them alongside the
main tree (as working copies) and refuses to run if they're missing locally. The build SHA identifies
the **main** commit; the vendored repos are developed in their own `.git` and change rarely.

## Guardrails (and their escape hatches)

- **Refuses a dirty working tree** — you deploy a real commit, not unsaved edits. Override with
  `ALLOW_DIRTY=1` only when you knowingly want to test uncommitted code on the box.
- **Warns if the ref isn't pushed** to a remote (proceeds anyway). Push first so GitHub == the box;
  an unpushed deploy re-creates the exact drift this setup exists to prevent.
- **Targets are env-overridable**, defaulting to the current homelab:
  `LIBERADO_SSH` (`shiloh@192.168.0.144`), `LIBERADO_API` (`http://192.168.0.144:4201`),
  `BUILD_DIR` (`liberado-build`).

## The build is a full in-container release build

`docker build` re-runs `cargo build --release` from scratch every deploy (the `COPY . .` layer
invalidates on any source change; there is no cross-build cargo cache). Expect **~20–40 min** on the
homelab (`CARGO_BUILD_JOBS=2`, LTO off — see the root `Dockerfile` for why). Let it run; the script
blocks on health at the end, so a returned success is a real success.

## Files in this deploy tree

| Path | Role |
|---|---|
| `deploy.sh` | The deploy command. Run from the dev machine. |
| `latency-report.sh` | Per-role p50/p95 inference latency from the daemon's journal (`<data>/latency/events.jsonl`). Baseline for model-tuning. |
| `docker-compose.yml` | Mirror of the box's `~/homelab/services/liberado/docker-compose.yml` (run config). |
| `config/topology.toml`, `config/policy.toml` | Mirror of the box's `~/homelab/services/liberado/config/*`. Edit here, then copy to the box (config is **not** shipped by `deploy.sh` — it's a read-only mount, changed independently of the image). |
| `liberado-mcp-diagnosis.md` | Per-MCP wiring status/troubleshooting for the on-box agent. |
| `smoke-chat.sh` | Quick end-to-end chat probe against the live daemon. |

> **Config vs image are separate.** `deploy.sh` ships **code**. `topology.toml`/`policy.toml` are
> host mounts read at boot — change them on the box (or copy the mirror over) and
> `docker compose ... up -d --force-recreate` to reload, no rebuild needed.

**Per-role model tuning (no rebuild).** `topology.toml` has a commented `[roles.main_agent|dispatcher|subagent]`
section: set `provider` / `model` / `temperature` / `reasoning` per role to run a fast cheap router
and a strong worker. Edit on the box + `up -d --force-recreate`. Then compare before/after with
`bash deploy/homelab/latency-report.sh`.

**Permission grants persist to a machine-owned overlay (not `policy.toml`).** When an agent hits a
zone it wasn't granted, Liberado sends a Telegram request (Deny / Once / Session / Everywhere). That
tappable notification is the **only** message for that request — the chat agent's own reply collapses
to a small "⏳ waiting on your tap ↑" marker instead of paraphrasing the request a second time, so you
act on the buttons above, not a duplicate.
Tapping **Everywhere** appends the grant to `grants.overlay.toml` in the **data dir** (the container's
`$LIBERADO_DATA_DIR`, a writable volume — *not* the read-only config mount), merged on top of
`policy.toml` at boot. So an "Everywhere" grant takes effect on the next `up -d --force-recreate`, and
`Once`/`Session` unblock only the immediate call. The daemon never rewrites your hand-edited
`policy.toml`, and agents can't touch the overlay (it lives outside every vault zone). To revoke all
such grants: delete `grants.overlay.toml` on the box and recreate the container.
