#!/bin/bash
# One-time setup for Aero agent development environment.
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

echo "Setting up Aero agent environment..."

# Create .cargo directory if needed
mkdir -p "$REPO_ROOT/.cargo"

CONFIG_FILE="$REPO_ROOT/.cargo/config.toml"

if [[ -f "$CONFIG_FILE" ]]; then
    if grep -qE '^[[:space:]]*xtask[[:space:]]*=' "$CONFIG_FILE" 2>/dev/null; then
        echo "  Found .cargo/config.toml (cargo xtask alias present)."
    else
        echo "  WARNING: .cargo/config.toml exists but `cargo xtask` alias was not found."
        echo "           If you want the alias back, restore the repo version:"
        echo "             git checkout -- .cargo/config.toml"
    fi
else
    echo "  Creating missing .cargo/config.toml (cargo xtask alias)..."
    cat > "$CONFIG_FILE" << 'EOF'
[alias]
# `cargo xtask <subcommand>` is shorthand for running the `xtask` helper binary.
xtask = "run --locked -p xtask --"
EOF
fi

echo ""
echo "Cargo memory-friendly settings are applied via environment variables."
echo "Activate them in your current shell:"
echo "  source $SCRIPT_DIR/agent-env.sh"
echo ""
echo "Setup complete."
