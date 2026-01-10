#!/bin/bash
# One-time setup for Aero development environment.
# Creates .cargo/config.toml with recommended settings.
# Safe to run multiple times.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "Setting up Aero development environment..."

# Create .cargo directory if needed
mkdir -p "$REPO_ROOT/.cargo"

# Create or update .cargo/config.toml
CONFIG_FILE="$REPO_ROOT/.cargo/config.toml"

if [[ -f "$CONFIG_FILE" ]]; then
    echo "  .cargo/config.toml already exists, checking settings..."
    
    # Check if our settings are already there
    if grep -q "codegen-units=4" "$CONFIG_FILE" 2>/dev/null; then
        echo "  Settings already configured."
    else
        echo "  WARNING: .cargo/config.toml exists but may not have recommended settings."
        echo "  Consider manually adding:"
        echo "    [build]"
        echo "    jobs = 4"
        echo "    [target.x86_64-unknown-linux-gnu]"
        echo '    rustflags = ["-C", "codegen-units=4"]'
    fi
else
    echo "  Creating .cargo/config.toml..."
    cat > "$CONFIG_FILE" << 'EOF'
# Aero development build settings
# Balances compilation speed with memory usage for concurrent agent environments

[build]
# 4 parallel rustc processes - good balance of speed vs memory
# Peak memory with these settings: ~8-12 GB
jobs = 4

[target.x86_64-unknown-linux-gnu]
# Reduce codegen parallelism per crate to limit memory spikes
rustflags = ["-C", "codegen-units=4"]

[target.wasm32-unknown-unknown]
rustflags = ["-C", "codegen-units=4"]

[profile.dev]
incremental = true
# Reduced debug info = faster links, less disk usage
debug = 1

[profile.dev.package."*"]
# No debug info for dependencies - faster builds
opt-level = 0
debug = false

[net]
# More reliable for concurrent git operations
git-fetch-with-cli = true
retry = 3
EOF
    echo "  Created .cargo/config.toml"
fi

# Source the environment
echo ""
echo "To activate the environment in your current shell, run:"
echo "  source $SCRIPT_DIR/agent-env.sh"
echo ""
echo "Setup complete."
