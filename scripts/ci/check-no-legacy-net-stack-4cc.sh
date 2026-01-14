#!/usr/bin/env bash
set -euo pipefail

# Guard against reintroducing the legacy 4CC spelling for the user-space network stack snapshot
# blob.
#
# The canonical 4CC is `NETS`, and the repo intentionally avoids spelling the legacy 4CC in tracked
# text to prevent confusion. This CI check runs even for docs-only PRs (which skip Rust tests).
#
# Note: we generate the legacy 4CC at runtime from raw bytes rather than embedding it as a string.

legacy="$(printf '\x4e\x53\x54\x4b')"

# `git grep -I` ignores binary files; use a fixed-string search.
if git grep -n -I -F -e "$legacy" -- ':!fuzz/corpus' >/dev/null; then
  echo "ERROR: found legacy net-stack 4CC spelling in the repository."
  echo "Use the canonical net-stack snapshot 4CC `NETS` and numeric-byte compat handling."
  echo
  git grep -n -I -F -e "$legacy" -- ':!fuzz/corpus' || true
  exit 1
fi
