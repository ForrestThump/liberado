#!/usr/bin/env bash
#
# Per-role latency report from the daemon's inference journal.
#
# Reads the JSONL the MeteredProvider writes (one record per LLM call: role, wall_ms, ttft, tokens)
# and prints p50/p95 wall time + token totals per role — the baseline you compare before/after
# tuning models per role (see docs/roadmap/latency-and-routing-observability-plan.md §4).
#
# Usage:
#   deploy/homelab/latency-report.sh                 # read the live homelab journal over ssh
#   deploy/homelab/latency-report.sh path/to.jsonl   # read a local file instead
#   LIBERADO_SSH=user@host deploy/homelab/latency-report.sh
#
# The homelab mounts the container's /data at ~/homelab/services/liberado/data, so the journal is at
# ~/homelab/services/liberado/data/latency/events.jsonl on the box.
set -euo pipefail

SSH_TARGET="${LIBERADO_SSH:-shiloh@192.168.0.144}"
REMOTE_PATH="${LIBERADO_LATENCY_PATH:-\$HOME/homelab/services/liberado/data/latency/events.jsonl}"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

# Source the JSONL: a local file arg, or the box over ssh.
if [ "${1:-}" != "" ]; then
  [ -f "$1" ] || { echo "no such file: $1" >&2; exit 1; }
  read_json() { cat "$1"; }
  SRC="$1"
else
  read_json() { ssh "$SSH_TARGET" "cat $REMOTE_PATH"; }
  SRC="$SSH_TARGET:$REMOTE_PATH"
fi

echo "== Liberado latency (source: $SRC) =="

read_json "$@" | jq -rs '
  map(select(.kind == "llm_call"))
  | if length == 0 then
      "no llm_call records yet — send a chat turn through the daemon first" | [.] | .[]
    else
      ( ["role","calls","p50_ms","p95_ms","max_ms","ttft_p50","tokens"] ),
      ( group_by(.role)
        | map(
            ( map(.wall_ms) | sort ) as $w
            | ( [ .[].ttft_ms | select(. != null) ] | sort ) as $t
            | [ .[0].role,
                length,
                $w[ (( (length-1) * 0.50 ) | floor) ],
                $w[ (( (length-1) * 0.95 ) | floor) ],
                ($w | last),
                ( if ($t|length) > 0 then $t[ (( (($t|length)-1) * 0.50 ) | floor) ] else "-" end ),
                ( map(.total_tokens // 0) | add )
              ]
          )
        | sort_by(.[0]) | .[]
      )
      | @tsv
    end
' | column -t -s $'\t'
