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

## The WebUI ships separately (and cheaply)

**<https://liberado.homelab.local>** — the browser UI, live.

It is a WASM bundle, **not** part of `liberado:dev`. The image carries no wasm32 toolchain on
purpose, so the bundle is built on the dev machine and **mounted** into the container:

```powershell
.\scripts\deploy-webui-homelab.ps1            # build + ship, ~1 min
.\scripts\deploy-webui-homelab.ps1 -SkipBuild # ship what is already in target/
```

`ServeDir` reads the mount per request, so shipping a new UI is a file copy — **no image rebuild,
no restart, no downtime**. Use this for anything that is frontend-only; save `deploy.sh` (20–40 min)
for daemon changes.

```
browser ──► https://liberado.homelab.local
             └─ Traefik, primary node 192.168.0.220:443   [~/homelab/services/traefik/dynamic/ai-node.yml]
                  └─ liberado daemon, AI node 192.168.0.144:4201
                       ├─ /api/*  → axum routes
                       └─ /*      → ServeDir($LIBERADO_WEBUI_DIST)
```

**One origin serves UI and API**, which is the whole design: the frontend calls
`window.location.origin`, so it needs no CORS and no second hostname. Never reintroduce a hardcoded
`:4201` in `crates/webui` — Traefik terminates TLS on 443 only, so such a call has nowhere to land.
Details in `crates/webui/README.md`.

Two things that bite when touching the mount:

- **Never `mv` the bundle directory** to swap it. Docker resolves a bind mount to an inode at
  container start; renaming the directory leaves the container serving an unlinked one (symptom: a
  sudden 404 on `/` that only `up -d --force-recreate` clears). The script uses
  `rsync -a --delete` into the existing directory for exactly this reason.
- `topology.toml`/`policy.toml` are unrelated to the UI — see the config-vs-image note below.

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
| Sync leftover nested `turbovault/` / `turbomcp/` clones | Optional; Cargo fetches the git+tag pins. Ships leftover clones when present |
| No way to know what's live | Bakes the SHA into the image (`/etc/liberado-build-sha`, `LIBERADO_BUILD_SHA`, image label) and **verifies it after boot** |
| "It didn't come up" goes unnoticed | Health-gates on `/api/status`; dumps logs and fails loudly if it doesn't converge |

### Leftover nested clones

`turbovault/` and `turbomcp/` directories are **gitignored** leftover clones. They are not
required: Cargo fetches the ForrestThump forks at tag `liberado-2026-08-27`. `deploy.sh` still
ships them as working copies when they exist locally. The build SHA identifies the **main**
commit.

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
| `docker-compose.yml` | Mirror of the box's `~/homelab/services/liberado/docker-compose.yml` (run config). **`deploy.sh` does not ship this** — it runs the copy already on the box, so edit there and mirror here. |
| `config/topology.toml`, `config/policy.toml` | Mirror of the box's `~/homelab/services/liberado/config/*`. Edit here, then copy to the box (config is **not** shipped by `deploy.sh` — it's a read-only mount, changed independently of the image). |
| `liberado-mcp-diagnosis.md` | Per-MCP wiring status/troubleshooting for the on-box agent. |
| `smoke-chat.sh` | Quick end-to-end chat probe against the live daemon (spends tokens). |
| `smoke.sh` | Post-deploy assertions on **deployment facts** — daemon up, running SHA, in-container config validity, report sink present and writable. No inference, ~5s. Run automatically at the end of `deploy.sh`; also standalone: `bash deploy/homelab/smoke.sh [expected-sha]`. |

> **Config vs image are separate.** `deploy.sh` ships **code**. `topology.toml`/`policy.toml` are
> host mounts read at boot — change them on the box (or copy the mirror over) and
> `docker compose ... up -d --force-recreate` to reload, no rebuild needed.
>
> This note existed and was still walked into (2026-07-26): a feature whose behaviour lived in
> `topology.toml` was added to the mirror, `deploy.sh` reported success on the SHA check, and the
> feature did nothing — indistinguishable from a bug, and it cost a container-log read to find. A
> documented footgun is still a footgun, which is why `smoke.sh` now asserts the config *in the
> container* rather than trusting that the mirror and the box agree.

## Diagnosing an authority refusal

`config explain <component> <mcp:tool> <vault/path.md>` answers "would this write be allowed, and if
not, which guard stops it?" from config alone — every guard's verdict, plus the config edit that
would fix each failure. Run it **in the container**, not locally:

```
ssh <box> 'docker exec -e LIBERADO_CONFIG_DIR=/config liberado     liberado config explain dispatcher turbovault:write_note Learning/x.md'
```

The box applies a machine-owned grants overlay (`/data/grants.overlay.toml`, written by Telegram
"Approve everywhere" taps) that is **not** in the repo mirror, so a local run can disagree with the
box — and the box's answer is the real one.

## Deploys are serialized

`deploy.sh` takes an `flock` on the box before touching `~/liberado-build`, and stages each
invocation into its own `~/liberado-build.incoming.<sha>.<pid>`. Both are needed: the build SHA is
stamped from an *argument* rather than derived from the compiled tree, so two overlapping deploys
could produce an image that lies about what is in it — and that stamp is what everyone trusts to
know what is live. A second deploy waits rather than failing; a queued deploy is what you wanted,
just later.

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
