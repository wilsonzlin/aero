#!/bin/bash
# Source this file to set recommended environment variables for Aero development.
# Usage: source scripts/agent-env.sh

# Rust/Cargo - balance speed vs memory
export CARGO_BUILD_JOBS=4
export CARGO_INCREMENTAL=1

# Node.js - cap V8 heap to avoid runaway memory
export NODE_OPTIONS="--max-old-space-size=4096"

# Playwright - single worker to avoid memory multiplication
export PW_TEST_WORKERS=1

# Ensure enough file descriptors for Chrome/Playwright
ulimit -n 4096 2>/dev/null || true

echo "Aero agent environment configured:"
echo "  CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS"
echo "  CARGO_INCREMENTAL=$CARGO_INCREMENTAL"
echo "  NODE_OPTIONS=$NODE_OPTIONS"
echo "  PW_TEST_WORKERS=$PW_TEST_WORKERS"
