---
kind: plan
status: active
authority: implementation
domain: ops
canonical_for: local-build-ship
open_items: true
---

# Build on the dev machine, ship the artifact

**Status**: wanted, blocked on disk. Written 2026-08-02 after measuring why a deploy takes so long.
**Prerequisite the human owns**: install a **Debian WSL distro** and (optionally) Docker, plus
reclaim disk here. Shiloh has said he'll do this when there's time.

**Do the cheap half first** — [BuildKit cache mounts on the box](#what-shipped-instead-option-b) —
which is already done. This doc is the other half.

## The case

Deploys compile on the homelab, and the homelab is the slowest machine involved.

| | dev machine | homelab box |
|---|---|---|
| cores | 12 | 4 |
| RAM | 15 GB | 11 GB |
| `CARGO_BUILD_JOBS` | — | **2** (an OOM guard, not a throttle — linking is the RAM peak) |
| free disk | **46 GB** | 82 GB |

Measured: the cargo layer took **888 s (14.8 min)**, and before the cache mounts landed it did that
**from scratch on every deploy**, because `COPY . .` invalidates the layer on any source change.

The dev machine has 3× the cores and no memory cap forcing 2 jobs. It is the right place to compile.

## Why this is blocked

`target/` on the dev machine is **151.5 GB**, against **46 GB free**. A Linux `target/` has to live
somewhere, and a full release build of 43 crates plus turbovault is not small. Installing a distro
(~2 GB) and a toolchain (~1.5 GB) on top of that is tight enough that a build could fail on disk
rather than on code, which is the worst way to learn about it.

**A `cargo clean` would reclaim most of that 151.5 GB** and un-gate this. Worth doing regardless of
whether this plan happens.

Also blocking, but trivially: **WSL has no distro installed** and there is no Docker here. Both path
dependencies (`turbomcp`, `turbovault`) *are* present locally, so nothing else is missing.

## The design

Build a Linux binary here, ship the binary, let the box assemble the image in seconds.

1. **A Debian WSL distro**, ideally trixie — the runtime is `debian:trixie-slim` and the builder is
   `rust:1-trixie`, so matching the distro keeps glibc identical by construction rather than by
   luck. Older-glibc-builds-for-newer works, the reverse does not, so drifting the other way is the
   one thing to avoid.
2. **`cargo build --release`** in WSL against the same workspace, with a persistent `target/`. This
   is the actual win: not just more cores, but **incremental** — the box can never be incremental
   for a `COPY . .` layer, whereas a local target dir rebuilds only what changed.
3. **Ship the binary**, not the image. `liberado` + `liberado-conformance` compress to roughly
   30 MB; a `docker save` of the whole image is ~3-4× that, and every byte of the base layer is
   already on the box. A tiny runtime-only Dockerfile (`FROM debian:trixie-slim`, install the same
   runtime deps, `COPY` the two binaries) then builds in seconds.
4. **Everything downstream is unchanged**: same `GIT_SHA` build arg and provenance check, same
   config sync, same compose recreate, same health check.

### Why not `docker save` the whole image

It is closer to "stream the image" as usually described, and it needs Docker here rather than just a
toolchain. But it ships the Debian base layer the box already has, on every deploy, and it gives up
the incremental target dir unless the local Dockerfile also carries cache mounts. The binary is the
only thing that actually differs between two deploys.

## What must not regress

- **Provenance.** `GIT_SHA` must still be baked in and still verified against the running container
  after recreate. A faster deploy that cannot say what it deployed is worse than a slow one.
- **The Debian shakeout.** The current Dockerfile's header makes a real point: building in the
  container is how Unix-only code paths get compiled on the target platform. Building in Debian WSL
  keeps that; cross-compiling from Windows with a linker hack would quietly lose it.
- **A fallback.** Keep the box-build path working. When the dev machine is off, or WSL is broken, or
  someone else deploys, the box must still be able to build unaided — nothing on the box may depend
  on this machine being up.

## What shipped instead (option B)

BuildKit cache mounts for the cargo registry and `/build/target`, so the box stops rebuilding from
scratch. Two things had to change together:

- the binaries are copied to `/out` **inside** the `RUN`, because a cache mount is not part of the
  image and `/build/target` is empty again the moment that step ends;
- `deploy.sh` no longer runs an unconditional `docker builder prune -f`. That was correct when every
  build was from scratch and the cache was dead weight; with mounts it would leave them in place and
  deliver none of their benefit — slow builds with nothing explaining why. It bounds the cache with
  `--keep-storage` instead.

Option A is still worth doing after this. Cache mounts remove the *repeat* cost; they do not make
the box faster than it is, and a cold cache is still ~15 minutes on 4 cores.
