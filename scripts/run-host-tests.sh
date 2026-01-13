#!/usr/bin/env bash
set -euo pipefail

# Wrapper for running host-buildable driver unit tests.
#
# Today this runs the Win7 virtio-snd host-buildable suite, which is implemented as a
# standalone CMake project under:
#   drivers/windows7/virtio-snd/tests
#
# This script exists at the repo root so CI/dev docs can provide a single, stable
# entrypoint without requiring callers to know driver-internal paths.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec "${repo_root}/drivers/windows7/virtio-snd/scripts/run-host-tests.sh" "$@"

