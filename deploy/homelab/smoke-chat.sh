#!/usr/bin/env bash
set -euo pipefail
printf '%s' '{"message":"Reply with exactly the three words: liberado is live"}' > /tmp/chat.json
echo "=== POST /api/chat ==="
curl -sS -X POST http://127.0.0.1:4201/api/chat \
  -H 'Content-Type: application/json' \
  --data-binary @/tmp/chat.json \
  --max-time 120
echo
echo "=== POST /api/chat/stream ==="
printf '%s' '{"message":"Say hi in five words."}' > /tmp/chat2.json
curl -sS -N -X POST http://127.0.0.1:4201/api/chat/stream \
  -H 'Content-Type: application/json' \
  --data-binary @/tmp/chat2.json \
  --max-time 120 | head -c 4000
echo
echo "=== status after ==="
curl -fsS http://127.0.0.1:4201/api/status
echo
