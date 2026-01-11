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

# Guardrail: an obsolete prototype GPU device crate must not be reintroduced.
# (The canonical AeroGPU protocol is A3A0; see drivers/aerogpu/protocol/.)
retired_gpu_device_dir="crates/aero-gpu""-device"
if [[ -d "$retired_gpu_device_dir" ]]; then
  die "$retired_gpu_device_dir is retired and must not exist in the repo"
fi
if grep -q "$retired_gpu_device_dir" Cargo.toml; then
  die "Cargo workspace must not include the retired $retired_gpu_device_dir member"
fi

# npm workspaces: enforce a single repo-root lockfile to prevent dependency drift.
# (Per-package lockfiles are ignored via .gitignore, but this catches forced adds.)
mapfile -t npm_lockfiles < <(git ls-files | grep -E '(^|/)package-lock\.json$' || true)
unexpected_lockfiles=()
for lf in "${npm_lockfiles[@]}"; do
  if [[ "$lf" != "package-lock.json" ]]; then
    unexpected_lockfiles+=("$lf")
  fi
done
if (( ${#unexpected_lockfiles[@]} > 0 )); then
  die "unexpected package-lock.json checked in outside the repo root (npm workspaces use a single root lockfile): ${unexpected_lockfiles[*]}"
fi

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

# Legacy Windows driver layout guardrails.
#
# The repo used to have a standalone GitHub Actions workflow for building a legacy Windows
# driver stack. It was removed in favor of the consolidated Win7 pipeline:
#   .github/workflows/drivers-win7.yml + ci/*.ps1 + drivers/*
legacy_windows_driver_workflow=".github/workflows/windows-""drivers.yml"
if [[ -f "$legacy_windows_driver_workflow" ]]; then
  die "legacy Windows driver workflow must not exist (use '.github/workflows/drivers-win7.yml')"
fi
legacy_guest_windows_dir="guest/""windows"
if [[ -d "$legacy_guest_windows_dir" ]]; then
  # The legacy driver directory is kept as a tombstone for old links. It must remain a stub
  # (no buildable driver projects or INFs).
  allowed_guest_windows_files=(
    "guest/""windows/README.md"
    "guest/""windows/docs/driver_install.md"
  )

  # Use a simple prefix scan instead of relying on pathspec glob support (`**`).
  legacy_guest_windows_prefix="$legacy_guest_windows_dir/"
  guest_windows_files=()
  while IFS= read -r f; do
    if [[ "$f" == "$legacy_guest_windows_prefix"* ]]; then
      guest_windows_files+=("$f")
    fi
  done < <(git ls-files || true)
  for f in "${guest_windows_files[@]}"; do
    allowed=0
    for allow in "${allowed_guest_windows_files[@]}"; do
      if [[ "$f" == "$allow" ]]; then
        allowed=1
        break
      fi
    done
    if [[ "$allowed" -ne 1 ]]; then
      die "unexpected file under ${legacy_guest_windows_dir}/ (tombstone should only contain README stub + driver_install stub): $f"
    fi
  done
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
