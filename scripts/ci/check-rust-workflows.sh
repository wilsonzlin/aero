#!/usr/bin/env bash
set -euo pipefail

# Enforce Aero's Rust CI policy:
# - Workflows must use the shared setup-rust composite action rather than
#   inlining dtolnay/rust-toolchain + rust-cache snippets.
# - Any workflow that runs `cargo` should have a setup-rust step.
#
# This guard helps prevent drift as workflows evolve.

cd "$(git rev-parse --show-toplevel)"

workflows_dir=".github/workflows"

failures=0

report_error() {
  local message="$1"
  failures=1
  echo "error: $message" >&2
}

if [[ ! -d "$workflows_dir" ]]; then
  echo "Rust workflow check: no .github/workflows directory; skipping."
  exit 0
fi

# Disallow the old copy/paste snippets.
if grep -R -n -E 'dtolnay/rust-toolchain|Swatinem/rust-cache@v2' "$workflows_dir" >/dev/null; then
  echo "Found forbidden Rust setup actions in workflows:" >&2
  grep -R -n -E 'dtolnay/rust-toolchain|Swatinem/rust-cache@v2' "$workflows_dir" >&2 || true
  report_error "workflows must use ./.github/actions/setup-rust instead of dtolnay/rust-toolchain or Swatinem/rust-cache"
fi

# Lockfile policy: Aero commits Cargo.lock and CI must run with --locked.
if grep -R -n -E 'locked:\s*(auto|never)\b' "$workflows_dir" >/dev/null; then
  echo "Found noncompliant setup-rust lockfile policy in workflows:" >&2
  grep -R -n -E 'locked:\s*(auto|never)\b' "$workflows_dir" >&2 || true
  report_error "workflows must not set setup-rust locked policy to auto/never (Aero policy is locked: always)"
fi

shopt -s nullglob
for wf in "$workflows_dir"/*.yml "$workflows_dir"/*.yaml; do
  if grep -n -E '\bcargo\b' "$wf" >/dev/null; then
    if ! grep -n -E '\./\.github/actions/setup-rust' "$wf" >/dev/null; then
      report_error "workflow '$wf' runs cargo but does not use the setup-rust composite action"
    fi
  fi
done

if [[ "$failures" -ne 0 ]]; then
  exit 1
fi

echo "Rust workflow check: OK"
