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

# Cargo registry cache contention can be a major slowdown when many agents share the same host
# (cargo prints: "Blocking waiting for file lock on package cache"). Mirror the convenient
# per-checkout Cargo home behavior from `scripts/agent-env.sh` / `scripts/safe-run.sh`: if a
# `$REPO_ROOT/.cargo-home` directory already exists, prefer it automatically as long as the caller
# hasn't configured a custom `CARGO_HOME`.
_aero_default_cargo_home=""
if [[ -n "${HOME:-}" ]]; then
  _aero_default_cargo_home="${HOME%/}/.cargo"
fi
_aero_effective_cargo_home="${CARGO_HOME:-}"
_aero_effective_cargo_home="${_aero_effective_cargo_home%/}"
if [[ -d "$PWD/.cargo-home" ]] \
  && { [[ -z "${_aero_effective_cargo_home}" ]] || [[ -n "${_aero_default_cargo_home}" && "${_aero_effective_cargo_home}" == "${_aero_default_cargo_home}" ]]; }
then
  export CARGO_HOME="$PWD/.cargo-home"
fi
unset _aero_default_cargo_home _aero_effective_cargo_home 2>/dev/null || true

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

  echo "- $lockfile (manifest: $manifest)"

  # `cargo metadata` does not need to print anything; we only care that it
  # succeeds with --locked.
  if ! cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null; then
    echo "error: Cargo.lock drift detected for $lockfile." >&2
    echo "hint: run: cargo metadata --format-version 1 --manifest-path \"$manifest\" >/dev/null" >&2
    echo "hint: then commit the updated lockfile(s)." >&2
    exit 1
  fi
done

end_group

echo "Cargo.lock drift check: OK"
