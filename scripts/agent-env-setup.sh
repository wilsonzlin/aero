#!/bin/bash
# One-time setup for Aero agent development environment.
#
# DEFENSIVE: This script validates the environment and warns about potential issues.
#
# NOTE: This repo *tracks* `.cargo/config.toml` (used for the `cargo xtask` alias),
# so this script does NOT overwrite it with agent-only build settings.
#
# Recommended memory-friendly Cargo settings are applied via environment variables
# (see `scripts/agent-env.sh`).
#
# Safe to run multiple times.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Some agent environments can end up with a partially broken working tree
# (lost executable bits, missing tracked fixtures, etc). When that happens, the
# minimal targeted restore is usually these paths:
RESTORE_TARGETED=(scripts tools/packaging/aero_packager/testdata tools/disk-streaming-browser-e2e/fixtures)

echo "============================================================"
echo "  Aero Agent Environment Setup"
echo "============================================================"
echo ""

WARNINGS=()
ERRORS=()

# ---- Check required tools ----

echo "Checking required tools..."

# Rust
if command -v rustc &>/dev/null; then
    RUSTC_VERSION=$(rustc --version 2>/dev/null || echo "unknown")
    echo "  ✓ rustc: $RUSTC_VERSION"
else
    ERRORS+=("rustc not found. Install Rust: https://rustup.rs/")
fi

# Cargo
if command -v cargo &>/dev/null; then
    CARGO_VERSION=$(cargo --version 2>/dev/null || echo "unknown")
    echo "  ✓ cargo: $CARGO_VERSION"
else
    ERRORS+=("cargo not found. Install Rust: https://rustup.rs/")
fi

# Node.js
if command -v node &>/dev/null; then
    NODE_VERSION=$(node --version 2>/dev/null || echo "unknown")
    echo "  ✓ node: $NODE_VERSION"
    
    # Check against .nvmrc if it exists
    if [[ -f "$REPO_ROOT/.nvmrc" ]]; then
        EXPECTED_NODE=$(cat "$REPO_ROOT/.nvmrc" | tr -d '\r\n ')
        CURRENT_MAJOR=$(node -p "process.versions.node.split('.')[0]" 2>/dev/null || echo "")
        EXPECTED_MAJOR=$(echo "$EXPECTED_NODE" | cut -d. -f1)
        if [[ "$CURRENT_MAJOR" != "$EXPECTED_MAJOR" ]]; then
            WARNINGS+=("Node.js version mismatch: have $NODE_VERSION, want $EXPECTED_NODE")
        fi
    fi
else
    WARNINGS+=("node not found. Some features may not work. Install Node.js: https://nodejs.org/")
fi

# npm
if command -v npm &>/dev/null; then
    NPM_VERSION=$(npm --version 2>/dev/null || echo "unknown")
    echo "  ✓ npm: $NPM_VERSION"
else
    WARNINGS+=("npm not found. Install Node.js: https://nodejs.org/")
fi

# ---- Check defensive tools ----

echo ""
echo "Checking defensive tools..."

# timeout (for with-timeout.sh / safe-run.sh)
# Prefer GNU coreutils timeout. On macOS, it is typically installed as `gtimeout`.
TIMEOUT_CMD=""
if command -v timeout &>/dev/null; then
    TIMEOUT_CMD="timeout"
    echo "  ✓ timeout: available"
elif command -v gtimeout &>/dev/null; then
    TIMEOUT_CMD="gtimeout"
    echo "  ✓ gtimeout: available (macOS coreutils)"
else
    WARNINGS+=("timeout not found. Install coreutils: brew install coreutils (macOS) or apt install coreutils (Linux)")
fi

# prlimit or ulimit (for run_limited.sh)
if command -v prlimit &>/dev/null; then
    # Test if prlimit actually works (some CI environments have broken prlimit)
    if prlimit --as=67108864 --cpu=1 -- true >/dev/null 2>&1; then
        echo "  ✓ prlimit: available and working"
    else
        echo "  ✓ prlimit: available (may use ulimit fallback)"
    fi
else
    # ulimit is a shell builtin, always available
    echo "  ✓ ulimit: available (prlimit not found, using fallback)"
fi

# ---- Check Cargo config ----

echo ""
echo "Checking Cargo configuration..."

CONFIG_FILE="$REPO_ROOT/.cargo/config.toml"
mkdir -p "$REPO_ROOT/.cargo"

if [[ -f "$CONFIG_FILE" ]]; then
    if grep -qE '^[[:space:]]*xtask[[:space:]]*=' "$CONFIG_FILE" 2>/dev/null; then
        echo "  ✓ .cargo/config.toml: xtask alias present"
    else
        WARNINGS+=(".cargo/config.toml exists but missing xtask alias. Run: git checkout -- .cargo/config.toml")
    fi
else
    echo "  Creating .cargo/config.toml with xtask alias..."
    cat > "$CONFIG_FILE" << 'EOF'
[alias]
# `cargo xtask <subcommand>` is shorthand for running the `xtask` helper binary.
xtask = "run --locked -p xtask --"
EOF
    echo "  ✓ .cargo/config.toml: created"
fi

# ---- Check for orphaned processes ----

echo ""
echo "Checking for orphaned processes..."

# Use timeout to prevent pgrep from hanging
ORPHANS_FOUND=0

# Simple check - just look for long-running cargo/rustc processes
# Use a subshell with timeout to prevent hangs
if command -v pgrep &>/dev/null; then
    for pattern in "cargo" "rustc" "wasm-pack"; do
        # Get count, handling edge cases carefully
        if [[ -n "${TIMEOUT_CMD}" ]]; then
            RAW_COUNT=$("$TIMEOUT_CMD" 2 pgrep -u "$(whoami)" -c "$pattern" 2>/dev/null || echo "0")
        else
            RAW_COUNT=$(pgrep -u "$(whoami)" -c "$pattern" 2>/dev/null || echo "0")
        fi
        COUNT="${RAW_COUNT//[^0-9]/}"  # Strip non-digits
        COUNT="${COUNT:-0}"  # Default to 0 if empty
        if [[ "$COUNT" =~ ^[0-9]+$ ]] && [[ "$COUNT" -gt 0 ]]; then
            echo "  ! Found $COUNT process(es) matching '$pattern'"
            ORPHANS_FOUND=$((ORPHANS_FOUND + COUNT))
        fi
    done
fi

if [[ $ORPHANS_FOUND -gt 0 ]]; then
    WARNINGS+=("Found $ORPHANS_FOUND potentially orphaned processes. Check with: pgrep -u \$(whoami) -af 'cargo|rustc'")
else
    echo "  ✓ No orphaned build processes found"
fi

# ---- Check disk space ----

echo ""
echo "Checking disk space..."

# Get available space in GB (works on both Linux and macOS)
if command -v df &>/dev/null; then
    AVAIL_KB=$(df -k "$REPO_ROOT" 2>/dev/null | tail -1 | awk '{print $4}')
    if [[ -n "$AVAIL_KB" && "$AVAIL_KB" =~ ^[0-9]+$ ]]; then
        AVAIL_GB=$((AVAIL_KB / 1024 / 1024))
        if [[ $AVAIL_GB -lt 10 ]]; then
            WARNINGS+=("Low disk space: ${AVAIL_GB}GB available. Builds may fail. Clean target/ directories.")
        else
            echo "  ✓ Disk space: ${AVAIL_GB}GB available"
        fi
    fi
fi

# ---- Check file descriptor limit ----

echo ""
echo "Checking file descriptor limits..."

FD_LIMIT=$(ulimit -n 2>/dev/null || echo "unknown")
if [[ "$FD_LIMIT" =~ ^[0-9]+$ ]]; then
    if [[ "$FD_LIMIT" -lt 1024 ]]; then
        WARNINGS+=("Low file descriptor limit: $FD_LIMIT. Run: ulimit -n 4096")
    else
        echo "  ✓ File descriptors: $FD_LIMIT (limit)"
    fi
fi

# ---- Check working tree integrity (exec bits + pinned fixtures) ----

echo ""
echo "Checking repo checkout integrity (scripts + fixtures)..."

NONEXEC_SCRIPTS=()
MISSING_SCRIPTS=()
MISSING_FIXTURES=()

for rel in \
  "scripts/safe-run.sh" \
  "scripts/run_limited.sh" \
  "scripts/with-timeout.sh"
do
  path="$REPO_ROOT/$rel"
  # Treat 0-byte scripts as missing too. An empty script is almost always a broken checkout
  # (and can cause confusing behavior when invoked via `bash`).
  if [[ ! -s "$path" ]]; then
    MISSING_SCRIPTS+=("$rel")
    continue
  fi
  if [[ ! -x "$path" ]]; then
    NONEXEC_SCRIPTS+=("$rel")
  fi
done

# Pinned fixtures relied on by tooling/tests.
for rel in \
  "tools/disk-streaming-browser-e2e/fixtures/win7.img" \
  "tools/disk-streaming-browser-e2e/fixtures/secret.img" \
  "tools/packaging/aero_packager/testdata/drivers/amd64/testdrv/test.sys" \
  "tools/packaging/aero_packager/testdata/drivers/x86/testdrv/test.sys" \
  "tools/packaging/aero_packager/testdata/drivers-aero-virtio/amd64/aero_virtio_blk/aero_virtio_blk.sys" \
  "tools/packaging/aero_packager/testdata/drivers-aero-virtio/amd64/aero_virtio_net/aero_virtio_net.sys" \
  "tools/packaging/aero_packager/testdata/drivers-aero-virtio/x86/aero_virtio_blk/aero_virtio_blk.sys" \
  "tools/packaging/aero_packager/testdata/drivers-aero-virtio/x86/aero_virtio_net/aero_virtio_net.sys"
do
  path="$REPO_ROOT/$rel"
  # -s requires the file exist and be non-empty (a 0-byte fixture is almost always a broken checkout).
  if [[ ! -s "$path" ]]; then
    MISSING_FIXTURES+=("$rel")
  fi
done

if [[ ${#NONEXEC_SCRIPTS[@]} -gt 0 || ${#MISSING_SCRIPTS[@]} -gt 0 || ${#MISSING_FIXTURES[@]} -gt 0 ]]; then
  echo "  ! Detected an incomplete/broken working tree (common in some agent environments)"
fi

if [[ ${#MISSING_SCRIPTS[@]} -gt 0 ]]; then
  msg="Missing tracked scripts:"
  for rel in "${MISSING_SCRIPTS[@]}"; do msg+=$'\n'"  - ${rel}"; done
  msg+=$'\n'"Fix:"
  msg+=$'\n'"  git checkout -- scripts"
  msg+=$'\n'"  # or (also restores common fixtures that some tools/tests rely on):"
  msg+=$'\n'"  git checkout -- ${RESTORE_TARGETED[*]}"
  msg+=$'\n'"  # or restore just these paths:"
  msg+=$'\n'"  git checkout -- ${MISSING_SCRIPTS[*]}"
  msg+=$'\n'"  # bigger hammer (resets the whole working tree):"
  msg+=$'\n'"  git checkout -- ."
  WARNINGS+=("$msg")
fi

if [[ ${#NONEXEC_SCRIPTS[@]} -gt 0 ]]; then
  msg="Scripts not executable (lost executable bits?):"
  for rel in "${NONEXEC_SCRIPTS[@]}"; do msg+=$'\n'"  - ${rel}"; done
  msg+=$'\n'"Workaround (does not require +x):"
  msg+=$'\n'"  bash ./scripts/safe-run.sh cargo build --locked"
  msg+=$'\n'"Fix (preferred):"
  msg+=$'\n'"  git checkout -- scripts"
  msg+=$'\n'"  # bigger hammer (resets the whole working tree):"
  msg+=$'\n'"  git checkout -- ."
  msg+=$'\n'"Non-git fallback:"
  msg+=$'\n'"  chmod +x ${NONEXEC_SCRIPTS[*]}"
  msg+=$'\n'"  # or fix all scripts under scripts/:"
  msg+=$'\n'"  find scripts -name '*.sh' -exec chmod +x {} +"
  WARNINGS+=("$msg")
else
  echo "  ✓ Script executability looks OK"
fi

if [[ ${#MISSING_FIXTURES[@]} -gt 0 ]]; then
  msg="Missing tracked fixtures (some tools/tests rely on these):"
  for rel in "${MISSING_FIXTURES[@]}"; do msg+=$'\n'"  - ${rel}"; done
  msg+=$'\n'"Fix:"
  msg+=$'\n'"  git checkout -- tools/packaging/aero_packager/testdata tools/disk-streaming-browser-e2e/fixtures"
  msg+=$'\n'"  # or (also restores scripts/ if your checkout lost executable bits):"
  msg+=$'\n'"  git checkout -- ${RESTORE_TARGETED[*]}"
  msg+=$'\n'"  # bigger hammer (resets the whole working tree):"
  msg+=$'\n'"  git checkout -- ."
  WARNINGS+=("$msg")
else
  echo "  ✓ Required fixtures present"
fi

# ---- Report results ----

echo ""
echo "============================================================"

if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo "ERRORS (must fix):"
    for err in "${ERRORS[@]}"; do
        echo "  ✗ $err"
    done
    echo ""
fi

if [[ ${#WARNINGS[@]} -gt 0 ]]; then
    echo "WARNINGS (may cause issues):"
    for warn in "${WARNINGS[@]}"; do
        # WARNINGS may contain newlines (copy/paste remediation commands). Prefix each
        # line so multi-line entries stay readable.
        while IFS= read -r line; do
            echo "  ! $line"
        done <<<"$warn"
    done
    echo ""
fi

if [[ ${#ERRORS[@]} -eq 0 && ${#WARNINGS[@]} -eq 0 ]]; then
    echo "All checks passed!"
    echo ""
fi

echo "Next steps:"
echo "  1. Activate environment:  source $SCRIPT_DIR/agent-env.sh"
# Some agent environments lose executable bits in the working tree. Using `bash`
# avoids hard-requiring `chmod +x` to have been preserved by the checkout.
echo "  2. Run builds safely:     bash ./scripts/safe-run.sh cargo build --locked"
echo ""
echo "Windows 7 test ISO available at: /state/win7.iso"
echo ""
echo "============================================================"

if [[ ${#ERRORS[@]} -gt 0 ]]; then
    exit 1
fi
