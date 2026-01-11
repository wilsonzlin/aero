#!/usr/bin/env sh
set -eu

# Cargo passes the path to the real rustc as the first argument when using
# `RUSTC_WRAPPER`. Execute the compiler directly (this wrapper intentionally
# performs no caching).
rustc="$1"
shift
exec "$rustc" "$@"

