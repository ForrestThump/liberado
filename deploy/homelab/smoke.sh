#!/usr/bin/env bash
#
# Post-deploy smoke check — asserts the things you CANNOT see by chatting.
#
# Why this exists: on 2026-07-26 a change shipped, `deploy.sh` verified the build SHA, reported
# success, and the feature did nothing. The code had deployed; `topology.toml` had not, because it
# is a host mount the deploy script deliberately does not overwrite. From the outside that looks
# identical to a broken feature — the report simply went to chat instead of the vault, and finding
# out why took reading a container's whole log.
#
# So this checks deployment *facts*, not agent behaviour:
#
#   1. the daemon is up, with a dispatcher and orchestrator attached;
#   2. the running build SHA is the one you meant to ship;
#   3. the config INSIDE the container loads and validates (Decision 14 runs against the real files,
#      including the machine-owned grants overlay that is not in the repo);
#   4. the declared report sink is present — the exact thing that was silently missing;
#   5. the sink's write path is actually permitted, via `config explain`, which answers the five
#      authority guards from config alone.
#
# All of the above are free: no inference, no tokens, ~5 seconds. A live chat turn costs money and
# ~10s, so it is opt-in via SMOKE_CHAT=1 rather than run on every deploy.
#
# Usage:
#   bash deploy/homelab/smoke.sh                  # checks against whatever is running
#   bash deploy/homelab/smoke.sh <expected-sha>   # also asserts the running build matches
#   SMOKE_CHAT=1 bash deploy/homelab/smoke.sh     # add one real chat turn (costs tokens)
#
# Env (defaults match the current homelab):
#   LIBERADO_SSH   ssh target        (default: shiloh@192.168.0.144)
#   SMOKE_ZONE     zone to test the sink write against (default: Learning)
#   SMOKE_COMPONENT policy component the subagent runs as (default: dispatcher)
#
set -uo pipefail

SSH_TARGET="${LIBERADO_SSH:-shiloh@192.168.0.144}"
EXPECT_SHA="${1:-}"
ZONE="${SMOKE_ZONE:-Learning}"
COMPONENT="${SMOKE_COMPONENT:-dispatcher}"

pass() { printf '  \033[1;32mPASS\033[0m  %s\n' "$*"; }
fail() { printf '  \033[1;31mFAIL\033[0m  %s\n' "$*"; FAILED=$((FAILED + 1)); }
FAILED=0

echo "post-deploy smoke check against $SSH_TARGET"

# One ssh round trip for everything the container can tell us. Each block is delimited (and the
# last one terminated) so the assertions below parse from a single capture rather than
# re-connecting five times.
#
# The sink is read back out of `config check` rather than hardcoded here: the point of the check is
# to verify what the box actually has, so assuming `turbovault:write_note` would test this script's
# assumptions instead of the deployment's config.
REMOTE_SCRIPT="$(
    cat <<'REMOTE'
echo "---STATUS---"
# The trailing newline matters: `/api/status` returns JSON without one, and without this the next
# `---MARKER---` lands on the same line and stops matching at line start. It fails quietly, too —
# the status greps still hit inside the merged line, so only the *following* section comes back
# empty.
curl -fsS --max-time 5 http://127.0.0.1:4201/api/status 2>&1 || echo "STATUS_UNREACHABLE"
echo ""
echo "---SHA---"
docker exec liberado cat /etc/liberado-build-sha 2>&1 | tr -d '[:space:]'
echo ""
echo "---CONFIG---"
CFG="$(docker exec -e LIBERADO_CONFIG_DIR=/config liberado liberado config check 2>&1)"
printf '%s\n' "$CFG"
echo "---EXPLAIN---"
SINK="$(printf '%s' "$CFG" | sed -n 's/^ *report sink: \([^ ]*:[^ ]*\).*/\1/p' | head -1)"
if [ -n "$SINK" ]; then
  docker exec -e LIBERADO_CONFIG_DIR=/config liberado \
    liberado config explain __COMPONENT__ "$SINK" "__ZONE__/smoke-check.md" 2>&1
else
  echo "verdict: SKIPPED — no sink to check"
fi
echo "---END---"
REMOTE
)"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__COMPONENT__/$COMPONENT}"
REMOTE_SCRIPT="${REMOTE_SCRIPT//__ZONE__/$ZONE}"
REMOTE_OUT="$(ssh "$SSH_TARGET" bash -s <<<"$REMOTE_SCRIPT" 2>&1)"

section() { printf '%s\n' "$REMOTE_OUT" | sed -n "/^---$1---\$/,/^---/p" | sed '1d;$d'; }

# 1. Daemon up, with the pieces a dispatch needs.
STATUS="$(section STATUS)"
if printf '%s' "$STATUS" | grep -q '"running":true'; then
  pass "daemon running"
else
  fail "daemon not running or unreachable: $STATUS"
fi
for field in dispatcher_attached orchestrator_attached watcher_active; do
  if printf '%s' "$STATUS" | grep -q "\"$field\":true"; then
    pass "$field"
  else
    fail "$field is false — a dispatch would have nothing to route to"
  fi
done

# 2. The running build is the one you shipped. `deploy.sh` checks this too; repeated here so the
#    smoke check is meaningful when run standalone against a box someone else deployed to.
LIVE_SHA="$(section SHA | tr -d '[:space:]')"
if [ -n "$EXPECT_SHA" ]; then
  if [ "${LIVE_SHA:0:7}" = "${EXPECT_SHA:0:7}" ]; then
    pass "build sha ${LIVE_SHA:0:7}"
  else
    fail "build sha mismatch: running ${LIVE_SHA:0:7}, expected ${EXPECT_SHA:0:7}"
  fi
else
  pass "build sha ${LIVE_SHA:0:7} (not asserted — pass one to check)"
fi

# 3./4. Config loads in the container, and the sink is actually there. This is the assertion that
#       would have caught the incident in the header.
CONFIG="$(section CONFIG)"
if printf '%s' "$CONFIG" | grep -q "^config OK"; then
  pass "config loads and validates in-container"
else
  fail "config check failed in-container:"
  printf '%s\n' "$CONFIG" | sed 's/^/          /'
fi
if printf '%s' "$CONFIG" | grep -q "report sink: (none"; then
  fail "no report sink configured — vault delivery is silently unavailable"
elif printf '%s' "$CONFIG" | grep -q "report sink:"; then
  pass "$(printf '%s' "$CONFIG" | grep -o 'report sink:.*' | head -1)"
else
  fail "config check printed no report sink line (old binary?)"
fi

# 5. The sink's write path is permitted. Catches a grant or zone regression before a subagent
#    discovers it by raising an approval request at you.
EXPLAIN="$(section EXPLAIN)"
if printf '%s' "$EXPLAIN" | grep -q "^verdict: ALLOWED"; then
  pass "sink write to $ZONE/ is permitted for '$COMPONENT'"
else
  fail "sink write to $ZONE/ is NOT permitted for '$COMPONENT':"
  printf '%s\n' "$EXPLAIN" | grep -E "BLOCK|verdict|fix:" | sed 's/^/          /'
fi

# 6. Opt-in: one real chat turn. Costs tokens, so off by default.
if [ "${SMOKE_CHAT:-0}" = "1" ]; then
  REPLY="$(ssh "$SSH_TARGET" \
    "curl -sS --max-time 90 -X POST http://127.0.0.1:4201/api/chat \
       -H 'Content-Type: application/json' \
       -d '{\"message\":\"Reply with exactly: smoke ok\"}'" 2>&1)"
  if printf '%s' "$REPLY" | grep -qi "smoke ok"; then
    pass "live chat turn"
  else
    fail "live chat turn did not round-trip: $REPLY"
  fi
fi

echo
if [ "$FAILED" -eq 0 ]; then
  printf '\033[1;32msmoke OK\033[0m\n'
  exit 0
fi
printf '\033[1;31msmoke FAILED (%d)\033[0m — the deploy landed, but the daemon is not in the state you expect.\n' "$FAILED"
exit 1
