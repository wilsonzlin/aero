#!/usr/bin/env bash
set -euo pipefail

# Repo layout guardrails.
#
# Goal:
# - Keep the repo from accidentally growing multiple "canonical" apps.
# - Make it explicit which Vite config is production vs harness.
#
# This script is intentionally lightweight and is safe to run in CI.

die() {
  echo "error: $*" >&2
  exit 1
}

need_file() {
  local path="$1"
  [[ -f "$path" ]] || die "expected file '$path' to exist"
}

cd "$(git rev-parse --show-toplevel)"

need_file "docs/repo-layout.md"
need_file "docs/adr/0001-repo-layout.md"

# Canonical frontend (ADR 0001).
need_file "web/package.json"
need_file "web/index.html"
need_file "web/vite.config.ts"

# Non-canonical prototype markers (repo hygiene).
need_file "poc/README.md"
need_file "prototype/README.md"
need_file "server/LEGACY.md"

# Repo-root Vite harness should be explicitly marked so it is not mistaken for the
# production app living under `web/`.
if [[ -f "index.html" ]]; then
  if ! grep -q "dev/test harness" index.html; then
    die "repo-root index.html exists but is not marked as a dev/test harness (expected the phrase 'dev/test harness')"
  fi
fi

need_file "vite.harness.config.ts"
if ! grep -q "repo-root dev harness" vite.harness.config.ts; then
  die "vite.harness.config.ts should include the phrase 'repo-root dev harness' to make its role unambiguous"
fi

# Fail if someone reintroduces an ambiguous Vite config file name at the repo root
# (it would be auto-picked up by `vite` and confuse dev/CI tooling).
if [[ -f "vite.config.ts" || -f "vite.config.js" || -f "vite.config.mjs" || -f "vite.config.cjs" ]]; then
  die "unexpected Vite config at repo root (vite.config.*). Use web/vite.config.ts for the production app or vite.harness.config.ts for the harness."
fi

# Fail if any new Vite config is introduced outside the allowlist.
mapfile -t vite_configs < <(git ls-files | grep -E '(^|/)vite\.config\.(ts|js|mjs|cjs)$' || true)
allowed_vite_configs=(
  "web/vite.config.ts"
)
for cfg in "${vite_configs[@]}"; do
  allowed=0
  for allow in "${allowed_vite_configs[@]}"; do
    if [[ "$cfg" == "$allow" ]]; then
      allowed=1
      break
    fi
  done
  if [[ "$allowed" -ne 1 ]]; then
    die "unexpected Vite config file '$cfg' (if this is intentional, add an ADR + update scripts/ci/check-repo-layout.sh)"
  fi
done

echo "Repo layout check: OK"
