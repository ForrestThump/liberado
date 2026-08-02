#!/usr/bin/env bash
#
# Liberado homelab deploy — the ONE way to ship a change to the live daemon.
#
# Why this exists: build and run are decoupled on the box (source copy in ~/liberado-build, image
# `liberado:dev`, compose run). Doing those steps by hand drifts — you rebuild but forget to
# recreate, or recreate an old image, or copy a half-synced tree, and nothing records what commit is
# actually live. This script removes every one of those failure modes:
#
#   1. Ships the MAIN repo at a COMMITTED ref (never your dirty tree, unless you force it) so
#      "what's deployed" is a real commit you can `git show`.
#   2. Also ships the VENDORED nested repos `turbovault/` and `turbomcp/` — they are gitignored
#      path-dependencies with their own .git, so `git archive` of the main repo does NOT contain
#      them. This is a real footgun: sync the main tree alone and the build dies with
#      "failed to read turbomcp/crates/turbomcp/Cargo.toml". They are shipped as working copies.
#   3. Bakes the main git SHA into the image (see Dockerfile) and VERIFIES the running container
#      reports it after boot — a green run means the new code is actually live.
#   4. Rebuilds, `up -d --force-recreate`, and health-gates on /api/status.
#
# Usage (run from the repo root on your dev machine — needs ssh access to the box):
#   bash deploy/homelab/deploy.sh                 # deploy current branch HEAD (main tree must be clean)
#   bash deploy/homelab/deploy.sh <ref>           # deploy a specific branch/tag/sha of the MAIN repo
#   ALLOW_DIRTY=1 bash deploy/homelab/deploy.sh   # deploy your main working tree as-is (escape hatch)
#
# Note: <ref> selects the MAIN repo commit only. The vendored turbovault/turbomcp are always shipped
# from their on-disk working copies (that is how they are developed — see the co-dev note in Cargo.toml).
#
# Override the target with env vars (defaults match the current homelab):
#   LIBERADO_SSH   ssh target           (default: shiloh@192.168.0.144)
#   LIBERADO_API   base URL for health  (default: http://192.168.0.144:4201)
#   BUILD_DIR      source dir on box    (default: liberado-build, under $HOME)
#
set -euo pipefail

SSH_TARGET="${LIBERADO_SSH:-shiloh@192.168.0.144}"
API_BASE="${LIBERADO_API:-http://192.168.0.144:4201}"
BUILD_DIR="${BUILD_DIR:-liberado-build}"          # relative to $HOME on the box
REF="${1:-HEAD}"
VENDORED="turbovault turbomcp"                    # gitignored path-dep repos that MUST ship too

say() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# --- 1. Resolve the ref and refuse to ship a mystery -------------------------
command -v git >/dev/null || die "git not found"
cd "$(git rev-parse --show-toplevel)" || die "run this from inside the repo"
SHA="$(git rev-parse --verify "$REF" 2>/dev/null)" || die "not a valid git ref: $REF"
SHORT="$(git rev-parse --short "$SHA")"
DESC="$(git log -1 --format='%s' "$SHA")"

if [ "${ALLOW_DIRTY:-0}" != "1" ] && ! git diff-index --quiet HEAD -- 2>/dev/null; then
  die "main working tree is dirty. Commit your changes, or ALLOW_DIRTY=1 to ship as-is."
fi

# Warn (don't block) if the ref isn't on the remote — a lost box is worse than a slow push.
if ! git branch -r --contains "$SHA" 2>/dev/null | grep -q .; then
  printf '\033[1;33mWARN:\033[0m %s is not pushed to any remote. Deploying anyway.\n' "$SHORT"
fi

# Preflight: the vendored repos must exist locally, or --delete would wipe them off the box.
for v in $VENDORED; do
  [ -f "$v/Cargo.toml" ] || die "vendored path-dep '$v/' is missing locally — cannot deploy without it."
done

say "Deploying main @ $SHORT  ($DESC)"
say "  + vendored: $VENDORED"
say "  -> $SSH_TARGET   build dir ~/$BUILD_DIR"

# --- 2. Ship main (committed ref) + vendored repos (working copies) ----------
# Two tar streams into a fresh incoming dir: main via git archive (reproducible), vendored via tar
# (they have no entry in the main index). Excludes keep .git/target/node_modules out of the wire.
# Staging is per-invocation, not a shared `.incoming`. It happens in separate ssh calls BEFORE the
# build lock below can be taken, so a shared name would let two concurrent deploys untar over each
# other and the lock would then dutifully protect an already-corrupted tree.
STAGE="$BUILD_DIR.incoming.$SHORT.$$"
say "Syncing source into ~/$STAGE"
ssh "$SSH_TARGET" "rm -rf ~/$STAGE && mkdir -p ~/$STAGE"
git archive --format=tar "$SHA" \
  | ssh "$SSH_TARGET" "tar x -C ~/$STAGE"
tar -cf - --exclude='.git' --exclude='target' --exclude='node_modules' $VENDORED \
  | ssh "$SSH_TARGET" "tar x -C ~/$STAGE"

# --- 3. Rsync into the build dir, rebuild, recreate, verify ------------------
# Remote logic runs from a here-doc on stdin (not ssh args) to dodge Git-Bash path mangling.
# $SHA is passed as $1 so the quoted here-doc stays literal. `--exclude=target/` protects any cargo
# cache dirs on the box from --delete (docker ignores them via .dockerignore, but no need to churn).
ssh "$SSH_TARGET" bash -s "$SHA" "$BUILD_DIR" "$STAGE" <<'REMOTE'
set -euo pipefail
SHA="$1"; BUILD_DIR="$2"; STAGE="$3"
cd "$HOME"

# --- Serialize deploys ------------------------------------------------------
# `~/$BUILD_DIR` is a single shared tree and the build SHA is stamped from an ARGUMENT, not derived
# from the sources that were compiled. So two overlapping deploys can interleave: the second rsyncs
# its tree over the first's mid-build, and the first then stamps its own (now wrong) SHA onto an
# image built from the other commit. The result is an image that lies about what is in it — and the
# SHA check at the end of this script is precisely what everyone trusts to know what is live.
#
# This is not hypothetical; it happened twice in one session on 2026-07-26, both times because a
# second deploy was launched while the first was still compiling. Resolved benignly only because the
# later commit happened to be a superset of the earlier one.
#
# flock waits rather than failing: a queued deploy is what the operator wanted, just later. The
# timeout is long enough for a full from-scratch release build (~15 min) plus slack.
exec 9>"$HOME/.$BUILD_DIR.lock"
if ! flock -w 1800 9; then
  echo "!! another deploy has held the build lock for 30 minutes — refusing to interleave" >&2
  exit 1
fi
echo ">> build lock acquired"

echo ">> rsync source into ~/$BUILD_DIR (stale files removed; target/ caches kept)"
mkdir -p "$HOME/$BUILD_DIR"
rsync -a --delete --exclude='target/' "$HOME/$STAGE/" "$HOME/$BUILD_DIR/"
rm -rf "$HOME/$STAGE"
echo "$SHA" > "$HOME/$BUILD_DIR/DEPLOYED_COMMIT"

# Sanity: the vendored path-dep the whole build hinges on must be present post-sync.
test -f "$HOME/$BUILD_DIR/turbomcp/crates/turbomcp/Cargo.toml" \
  || { echo "!! turbomcp path-dep missing after sync — aborting before build" >&2; exit 1; }

echo ">> docker build liberado:dev (GIT_SHA=$SHA)"
docker build --build-arg "GIT_SHA=$SHA" -t liberado:dev "$HOME/$BUILD_DIR"

echo ">> compose up -d --force-recreate"
docker compose -f "$HOME/homelab/services/liberado/docker-compose.yml" up -d --force-recreate

echo ">> waiting for the daemon to report healthy + correct build-sha"
for i in $(seq 1 40); do
  running="$(curl -fsS --max-time 5 http://127.0.0.1:4201/api/status 2>/dev/null | grep -o '"running":true' || true)"
  live_sha="$(docker exec liberado cat /etc/liberado-build-sha 2>/dev/null | tr -d '[:space:]' || true)"
  if [ -n "$running" ] && [ "$live_sha" = "$SHA" ]; then
    echo ">> OK  running=true  build-sha=$live_sha"
    # Bound the docker cache; do not erase it. This used to be an unconditional `builder prune -f`,
    # correct when every deploy was a full from-scratch build and the cache was pure dead weight
    # piling up (~80 GB over a handful of deploys) until the box hit "No space left on device".
    #
    # The Dockerfile now carries cargo-registry and target cache mounts, so that same cache is what
    # makes a redeploy incremental rather than a 15-minute rebuild. Wiping it after every deploy
    # would leave the mounts in place and silently deliver none of their benefit — the worst
    # outcome, because the build stays slow and nothing says why.
    #
    # `--keep-storage` bounds it instead: old entries are evicted, the working set survives.
    echo ">> bounding build cache (keeping the working set) + reclaiming dangling images"
    docker builder prune -f --keep-storage 20GB >/dev/null 2>&1 || true
    docker image prune -f >/dev/null 2>&1 || true
    exit 0
  fi
  sleep 3
done

echo "!! daemon did not converge. Recent logs:" >&2
docker logs liberado --tail 60 2>&1 || true
echo "!! expected build-sha=$SHA  got=${live_sha:-<none>}  running=${running:-<no>}" >&2
exit 1
REMOTE

say "Deployed main @ $SHORT. Verify from anywhere:"
echo "  curl -fsS $API_BASE/api/status"
echo "  ssh $SSH_TARGET 'docker exec liberado cat /etc/liberado-build-sha'   # -> $SHORT"

# --- 6. Smoke the deployment ------------------------------------------------
# The SHA check above proves the BINARY shipped. It says nothing about the config, which is a host
# mount this script deliberately never overwrites — so a change whose behaviour lives in
# `topology.toml` can deploy "successfully" and do nothing at all. That happened (2026-07-26): a
# report sink was added to the repo config, the deploy went green, and vault delivery silently fell
# back to chat summaries because the box's own config had never been touched.
#
# Free (no inference) and a few seconds. Non-fatal to the deploy itself — the code IS live at this
# point, and pretending otherwise would be its own lie — but it exits non-zero so the failure is
# impossible to scroll past.
if [ -f "$(dirname "$0")/smoke.sh" ]; then
  echo
  bash "$(dirname "$0")/smoke.sh" "$SHA" || {
    printf '\033[1;33mNOTE:\033[0m the binary is live, but the smoke check above failed.\n' >&2
    exit 1
  }
fi
