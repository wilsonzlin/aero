#!/usr/bin/env bash
set -euo pipefail

# Transitional wrapper.
#
# `cargo xtask test-all` is the canonical, cross-platform orchestrator used by both
# CI and developers. Keep this script around so existing workflows keep working,
# but avoid adding new logic here.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

exec cargo xtask test-all "$@"
