#!/bin/bash
set -e

# ── Rust / Cargo ────────────────────────────────────────────────────────────
if ! command -v cargo &> /dev/null; then
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Rust toolchain not found. Installing via rustup..."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
fi

# ── Lean / elan ─────────────────────────────────────────────────────────────
if ! command -v lean &> /dev/null && ! command -v elan &> /dev/null; then
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Lean toolchain not found. Installing via elan..."
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    curl https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -sSf | sh -s -- -y --default-toolchain leanprover/lean4:v4.15.0
    export PATH="$HOME/.elan/bin:$PATH"
fi

# ── Build leanstral binary ──────────────────────────────────────────────────
if [ -f "bin/leanstral" ] && [ -x "bin/leanstral" ]; then
    echo "✓ Pre-built leanstral binary found"
    if ./bin/leanstral --version &> /dev/null; then
        echo "✓ Binary is compatible with this platform"
    else
        echo "  Pre-built binary is not compatible, rebuilding..."
        cargo build --release
        mkdir -p bin
        cp target/release/leanstral bin/
        chmod +x bin/leanstral
    fi
else
    echo "Building leanstral binary..."
    cargo build --release
    mkdir -p bin
    cp target/release/leanstral bin/
    chmod +x bin/leanstral
fi

echo "✓ leanstral binary built successfully"

# ── Set up global validation workspace ──────────────────────────────────────
# Pre-fetch Mathlib cache so the first `leanstral verify --validate` is fast.
# This runs in the background so it doesn't block npm install.

setup_global_workspace() {
    local ws_dir
    if [ -n "$LEANSTRAL_VALIDATION_WORKSPACE" ]; then
        ws_dir="$LEANSTRAL_VALIDATION_WORKSPACE"
    elif [ "$(uname)" = "Darwin" ]; then
        ws_dir="$HOME/Library/Caches/leanstral-solana-skill/validation-workspace"
    elif [ -n "$XDG_CACHE_HOME" ]; then
        ws_dir="$XDG_CACHE_HOME/leanstral-solana-skill/validation-workspace"
    else
        ws_dir="$HOME/.cache/leanstral-solana-skill/validation-workspace"
    fi

    # Skip if already set up
    if [ -f "$ws_dir/lakefile.lean" ] && [ -d "$ws_dir/.lake/packages/mathlib" ]; then
        echo "✓ Global validation workspace already exists at $ws_dir"
        return 0
    fi

    echo "Setting up global validation workspace at $ws_dir..."
    echo "  This downloads and caches Mathlib (~1-2 min with cache, 25+ min without)."
    echo "  Running in background — leanstral will work once this completes."

    mkdir -p "$ws_dir"

    # Use leanstral setup command if available
    if [ -x "bin/leanstral" ]; then
        bin/leanstral setup --workspace "$ws_dir" &
        echo "  Background PID: $!"
        echo "  Check progress: tail -f $ws_dir/setup.log"
    else
        echo "  Warning: binary not available, workspace will be set up on first use."
    fi
}

setup_global_workspace

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  leanstral-solana-skill installed successfully!"
echo ""
echo "  Requirements:"
echo "    - MISTRAL_API_KEY environment variable must be set"
echo "    - Lean toolchain (auto-installed via elan)"
echo ""
echo "  The global Mathlib cache may still be downloading in the background."
echo "  First run of 'leanstral verify --validate' may be slow if not ready."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
