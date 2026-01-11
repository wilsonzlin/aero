#!/usr/bin/env bash
set -euo pipefail

# Verify that all tracked Cargo.lock files are present and consistent with their
# corresponding Cargo.toml manifests.
#
# Why this exists:
# - Aero commits lockfiles for reproducible Rust builds (see ADR 0012).
# - `cargo metadata --locked` is the most reliable drift check (it fails if the
#   lockfile would need to be updated), without the flakiness of
#   `cargo generate-lockfile --locked` which re-resolves versions.
#
# Important: do not add `--no-deps`. `cargo metadata --locked --no-deps` can
# succeed even when the lockfile is stale.

cd "$(git rev-parse --show-toplevel)"

if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
  in_ci=true
else
  in_ci=false
fi

start_group() {
  local title="$1"
  if [[ "$in_ci" == "true" ]]; then
    echo "::group::$title"
  else
    echo "==> $title"
  fi
}

end_group() {
  if [[ "$in_ci" == "true" ]]; then
    echo "::endgroup::"
  fi
}

mapfile -t lockfiles < <(git ls-files | grep -E '(^|/)Cargo\.lock$' || true)

if [[ "${#lockfiles[@]}" -eq 0 ]]; then
  echo "error: no Cargo.lock files are tracked (Aero policy requires committed lockfiles)." >&2
  exit 1
fi

start_group "Cargo.lock drift check (cargo metadata --locked)"

for lockfile in "${lockfiles[@]}"; do
  dir="$(dirname "$lockfile")"
  manifest="$dir/Cargo.toml"

  if [[ ! -f "$manifest" ]]; then
    echo "error: $lockfile is tracked but $manifest is missing." >&2
    exit 1
  fi

  echo "- $manifest"

  # `cargo metadata` does not need to print anything; we only care that it
  # succeeds with --locked.
  cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null
done

end_group

echo "Cargo.lock drift check: OK"
