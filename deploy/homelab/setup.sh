#!/usr/bin/env bash
# Pull the GHCR image GitHub Actions built for this checkout and recreate the
# existing homelab Compose service. Host config, .env, and secrets stay in place.
set -euo pipefail

usage() {
  cat <<'EOF'
Pull ghcr.io/forrestthump/liberado:sha-<HEAD> and recreate the homelab Compose service.

Usage:
  deploy/homelab/setup.sh [--dry-run] [--no-wait] [--help]

Homelab steps:
  git fetch origin
  git checkout <this-pr-branch>
  ./deploy/homelab/setup.sh

This script never writes:
  ~/homelab/services/liberado/config/
  ~/homelab/services/liberado/.env

Environment:
  LIBERADO_HOMELAB_DIR   Compose project (default: ~/homelab/services/liberado)
  LIBERADO_GHCR_IMAGE    Image repo (default: ghcr.io/forrestthump/liberado)
  LIBERADO_IMAGE_TAG     Tag to pull (default: sha-<git HEAD>)
  LIBERADO_CONTAINER     Container name (default: liberado)
  LIBERADO_NO_WAIT       Skip the post-recreate health wait (default: wait)
  LIBERADO_HEALTH_TIMEOUT_SECS   Health wait budget (default: 180)
EOF
}

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "${SCRIPT_DIR}/../.." && pwd)
DRY_RUN=0
WAIT_HEALTH=1
if [ -n "${LIBERADO_NO_WAIT:-}" ]; then
  WAIT_HEALTH=0
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --no-wait) WAIT_HEALTH=0 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if ! git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: ${REPO_ROOT} is not a git checkout" >&2
  exit 1
fi

COMMIT_SHA=$(git -C "${REPO_ROOT}" rev-parse HEAD)
IMAGE_REPO=${LIBERADO_GHCR_IMAGE:-ghcr.io/forrestthump/liberado}
IMAGE_TAG=${LIBERADO_IMAGE_TAG:-sha-${COMMIT_SHA}}
IMAGE="${IMAGE_REPO}:${IMAGE_TAG}"
HOMELAB_DIR=${LIBERADO_HOMELAB_DIR:-${HOME}/homelab/services/liberado}
CONTAINER=${LIBERADO_CONTAINER:-liberado}
COMPOSE_FILE="${HOMELAB_DIR}/docker-compose.yml"
OVERLAY="${SCRIPT_DIR}/docker-compose.ghcr.yml"
WEBUI_OVERLAY="${SCRIPT_DIR}/docker-compose.ghcr-webui.yml"
CONFIG_DIR="${HOMELAB_DIR}/config"
ENV_FILE="${HOMELAB_DIR}/.env"

fingerprint() {
  local path=$1
  if [ -f "${path}" ]; then
    sha256sum -- "${path}"
  elif [ -d "${path}" ]; then
    # Hashes only. Do not print file contents (the host .env holds secrets).
    (cd "${path}" && find . -type f | sort | while IFS= read -r file; do
      sha256sum -- "${file}"
    done)
  else
    printf 'absent %s\n' "${path}"
  fi
}

pull_help() {
  cat <<EOF >&2

The image ${IMAGE} is not pullable.

Wait until GitHub Actions job "deploy image (GHCR)" is green for commit ${COMMIT_SHA}.

The package is often private on first publish, even when the repo is public.
GitHub then answers 404 for anonymous pulls.

One-time visibility (no token in git):
  GitHub → Packages → liberado → Package settings → Change visibility → Public

Or log in with a token that has read:packages (do not commit the token):
  printf '%s' "\$GITHUB_TOKEN" | docker login ghcr.io -u ForrestThump --password-stdin
  docker pull ${IMAGE}
EOF
}

if [ ! -d "${HOMELAB_DIR}" ]; then
  echo "error: homelab directory does not exist: ${HOMELAB_DIR}" >&2
  echo "set LIBERADO_HOMELAB_DIR if the Compose project lives elsewhere" >&2
  exit 1
fi
if [ ! -f "${COMPOSE_FILE}" ]; then
  echo "error: missing ${COMPOSE_FILE}" >&2
  exit 1
fi
if [ ! -f "${OVERLAY}" ] || [ ! -f "${WEBUI_OVERLAY}" ]; then
  echo "error: missing GHCR Compose overlay next to setup.sh" >&2
  exit 1
fi

BEFORE_CONFIG=$(fingerprint "${CONFIG_DIR}")
BEFORE_ENV=$(fingerprint "${ENV_FILE}")

echo "checkout: ${REPO_ROOT}"
echo "commit:   ${COMMIT_SHA}"
echo "image:    ${IMAGE}"
echo "compose:  ${COMPOSE_FILE}"

export LIBERADO_IMAGE="${IMAGE}"
COMPOSE=(docker compose --project-directory "${HOMELAB_DIR}" -f "${COMPOSE_FILE}" -f "${OVERLAY}")

run() {
  if [ "${DRY_RUN}" -eq 1 ]; then
    printf 'dry-run:'
    printf ' %q' "$@"
    printf '\n'
    return 0
  fi
  "$@"
}

wait_healthy() {
  local timeout_secs=${LIBERADO_HEALTH_TIMEOUT_SECS:-180}
  local deadline=$(( $(date +%s) + timeout_secs ))
  local status
  while [ "$(date +%s)" -lt "${deadline}" ]; do
    status=$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}nodefined{{end}}' "${CONTAINER}" 2>/dev/null || echo absent)
    case "${status}" in
      healthy)
        echo "container ${CONTAINER} healthy"
        return 0
        ;;
      nodefined)
        if [ "$(docker inspect --format '{{.State.Running}}' "${CONTAINER}" 2>/dev/null)" = "true" ]; then
          echo "container ${CONTAINER} running (no healthcheck defined)"
          return 0
        fi
        ;;
    esac
    sleep 5
  done
  echo "error: container ${CONTAINER} was not healthy within ${timeout_secs}s (last status: ${status})" >&2
  echo "inspect: docker logs ${CONTAINER} --tail 100" >&2
  return 1
}

if [ "${DRY_RUN}" -eq 1 ]; then
  echo "dry-run: would pull ${IMAGE} and recreate ${CONTAINER}"
  echo "dry-run: would not write ${CONFIG_DIR} or ${ENV_FILE}"
  echo "dry-run: WebUI overlay added only if the pulled image bakes /usr/share/liberado/webui"
  run "${COMPOSE[@]}" -f "${WEBUI_OVERLAY}" up -d --force-recreate --no-build --pull never
  exit 0
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker is not on PATH" >&2
  exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "error: docker compose is not available" >&2
  exit 1
fi

if ! docker pull "${IMAGE}"; then
  pull_help
  exit 1
fi

if docker run --rm --entrypoint test "${IMAGE}" -f /usr/share/liberado/webui/index.html; then
  echo "using WebUI baked into ${IMAGE}"
  COMPOSE+=(-f "${WEBUI_OVERLAY}")
else
  echo "image has no baked WebUI; leaving host LIBERADO_WEBUI_DIST / /webui mount unchanged"
fi

run "${COMPOSE[@]}" up -d --force-recreate --no-build --pull never

if [ "${WAIT_HEALTH}" -eq 1 ]; then
  wait_healthy
fi

AFTER_CONFIG=$(fingerprint "${CONFIG_DIR}")
AFTER_ENV=$(fingerprint "${ENV_FILE}")
if [ "${BEFORE_CONFIG}" != "${AFTER_CONFIG}" ] || [ "${BEFORE_ENV}" != "${AFTER_ENV}" ]; then
  echo "error: host config or .env changed; setup.sh must not write those paths" >&2
  exit 1
fi

if ! docker inspect "${CONTAINER}" >/dev/null 2>&1; then
  echo "error: container ${CONTAINER} was not created" >&2
  exit 1
fi

ACTUAL=$(docker exec "${CONTAINER}" cat /etc/liberado-build-sha | tr -d '[:space:]')
if [ "${ACTUAL}" != "${COMMIT_SHA}" ]; then
  echo "error: live SHA ${ACTUAL} does not match checkout ${COMMIT_SHA}" >&2
  echo "the GHCR tag ${IMAGE} is not this checkout" >&2
  exit 1
fi

echo "recreated ${CONTAINER} from ${IMAGE}"
echo "live SHA ${ACTUAL}"
echo "host config and .env were not modified"
