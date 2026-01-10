#!/usr/bin/env bash
set -euo pipefail

DOCKER_BIN="${DOCKER_BIN:-docker}"
IMAGE="${AERO_WIN7_SLIPSTREAM_IMAGE:-aero/win7-slipstream}"

if ! command -v "${DOCKER_BIN}" >/dev/null 2>&1; then
  echo "error: '${DOCKER_BIN}' not found in PATH (install Docker, or set DOCKER_BIN=podman)." >&2
  exit 127
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../../.." && pwd -P)"
DOCKERFILE="${REPO_ROOT}/tools/win7-slipstream/container/Dockerfile"

# Resolve a path relative to the current working directory without relying on
# `realpath(1)` (macOS portability).
abs_path() {
  local input="$1"
  if [[ "${input}" = /* ]]; then
    printf '%s\n' "${input}"
    return
  fi
  printf '%s/%s\n' "$PWD" "${input}"
}

# Build the image if it's missing. This keeps the wrapper "one command" for
# users who just want to run the tool.
if ! "${DOCKER_BIN}" image inspect "${IMAGE}" >/dev/null 2>&1; then
  echo "info: image '${IMAGE}' not found; building it..." >&2
  "${DOCKER_BIN}" build -t "${IMAGE}" -f "${DOCKERFILE}" "${REPO_ROOT}"
fi

TTY_ARGS=()
if [[ -t 0 && -t 1 ]]; then
  TTY_ARGS=(-it)
fi

USER_ARGS=()
if command -v id >/dev/null 2>&1; then
  USER_ARGS=(--user "$(id -u):$(id -g)")
fi

INPUT_ISO_HOST=""
DRIVERS_HOST=""
OUTPUT_ISO_HOST=""
OUTPUT_ISO_BASENAME=""
FORWARDED_ARGS=()
while (($#)); do
  case "$1" in
    --input-iso)
      if (($# < 2)); then
        echo "error: --input-iso requires a path" >&2
        exit 2
      fi
      INPUT_ISO_HOST="$2"
      FORWARDED_ARGS+=(--input-iso /input/win7.iso)
      shift 2
      ;;
    --drivers)
      if (($# < 2)); then
        echo "error: --drivers requires a path" >&2
        exit 2
      fi
      DRIVERS_HOST="$2"
      FORWARDED_ARGS+=(--drivers /drivers)
      shift 2
      ;;
    --output-iso)
      if (($# < 2)); then
        echo "error: --output-iso requires a path" >&2
        exit 2
      fi
      OUTPUT_ISO_HOST="$2"
      OUTPUT_ISO_BASENAME="$(basename "$2")"
      FORWARDED_ARGS+=(--output-iso "/out/${OUTPUT_ISO_BASENAME}")
      shift 2
      ;;
    *)
      FORWARDED_ARGS+=("$1")
      shift
      ;;
  esac
done

MOUNT_ARGS=(
  --mount type=bind,source="$PWD",target=/work
)

if [[ -n "${INPUT_ISO_HOST}" ]]; then
  INPUT_ISO_HOST="$(abs_path "${INPUT_ISO_HOST}")"
  if [[ ! -f "${INPUT_ISO_HOST}" ]]; then
    echo "error: input ISO not found: ${INPUT_ISO_HOST}" >&2
    exit 2
  fi
  MOUNT_ARGS+=(--mount type=bind,source="${INPUT_ISO_HOST}",target=/input/win7.iso,readonly)
fi

if [[ -n "${DRIVERS_HOST}" ]]; then
  DRIVERS_HOST="$(abs_path "${DRIVERS_HOST}")"
  if [[ ! -d "${DRIVERS_HOST}" ]]; then
    echo "error: drivers directory not found: ${DRIVERS_HOST}" >&2
    exit 2
  fi
  MOUNT_ARGS+=(--mount type=bind,source="${DRIVERS_HOST}",target=/drivers,readonly)
fi

if [[ -n "${OUTPUT_ISO_HOST}" ]]; then
  OUTPUT_DIR_HOST="$(abs_path "$(dirname "${OUTPUT_ISO_HOST}")")"
  mkdir -p "${OUTPUT_DIR_HOST}"
  MOUNT_ARGS+=(--mount type=bind,source="${OUTPUT_DIR_HOST}",target=/out)
fi

exec "${DOCKER_BIN}" run --rm "${TTY_ARGS[@]}" \
  "${MOUNT_ARGS[@]}" \
  -w /work \
  -e HOME=/tmp \
  "${USER_ARGS[@]}" \
  "${IMAGE}" \
  "${FORWARDED_ARGS[@]}"
