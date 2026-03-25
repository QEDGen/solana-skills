#!/bin/bash
set -e

REPO="QEDGen/solana-skills"

# Resolve the directory where this script lives (= skill root)
SKILL_DIR="$(cd "$(dirname "$0")" && pwd)"
QEDGEN_BIN="$SKILL_DIR/bin/qedgen"

# ── Detect platform ──────────────────────────────────────────────────────
detect_asset_name() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin) os="apple-darwin" ;;
        Linux)  os="unknown-linux-gnu" ;;
        *)      return 1 ;;
    esac

    case "$arch" in
        arm64|aarch64) arch="aarch64" ;;
        x86_64)        arch="x86_64" ;;
        *)             return 1 ;;
    esac

    echo "qedgen-${arch}-${os}"
}

# ── Download from GitHub release ─────────────────────────────────────────
download_binary() {
    local asset_name="$1"

    # Get the latest release download URL
    local url="https://github.com/${REPO}/releases/latest/download/${asset_name}"
    echo "  Downloading from $url ..."

    mkdir -p "$SKILL_DIR/bin"
    if curl -fSL --retry 2 -o "$QEDGEN_BIN" "$url" 2>/dev/null; then
        chmod +x "$QEDGEN_BIN"
        if "$QEDGEN_BIN" --version &> /dev/null; then
            return 0
        fi
        rm -f "$QEDGEN_BIN"
    fi
    return 1
}

# ── Build from source ────────────────────────────────────────────────────
build_from_source() {
    echo "  Building from source..."

    if ! command -v cargo &> /dev/null; then
        echo "  Installing Rust toolchain via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source "$HOME/.cargo/env"
    fi

    cargo build --release --manifest-path "$SKILL_DIR/Cargo.toml"
    mkdir -p "$SKILL_DIR/bin"
    cp "$SKILL_DIR/target/release/qedgen" "$QEDGEN_BIN"
    chmod +x "$QEDGEN_BIN"
}

# ── Install qedgen binary ───────────────────────────────────────────────
if [ -f "$QEDGEN_BIN" ] && [ -x "$QEDGEN_BIN" ] && "$QEDGEN_BIN" --version &> /dev/null; then
    echo "✓ Pre-built qedgen binary is compatible"
else
    echo "Pre-built binary missing or incompatible."

    asset_name=$(detect_asset_name 2>/dev/null || true)
    installed=false

    if [ -n "$asset_name" ]; then
        echo "  Trying GitHub release for $asset_name..."
        if download_binary "$asset_name"; then
            echo "✓ Downloaded qedgen binary from release"
            installed=true
        fi
    fi

    if [ "$installed" = false ]; then
        echo "  Release binary unavailable, falling back to source compilation..."
        build_from_source
        echo "✓ qedgen binary built from source"
    fi
fi

# ── Lean / elan ───────────────────────────────────────────────────────────
if ! command -v lean &> /dev/null && ! command -v elan &> /dev/null; then
    echo "  Installing Lean toolchain via elan..."
    curl https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh -sSf | sh -s -- -y --default-toolchain leanprover/lean4:v4.15.0
    export PATH="$HOME/.elan/bin:$PATH"
fi

# ── Set up global validation workspace ────────────────────────────────────
# Pre-fetch Mathlib cache so the first `qedgen verify --validate` is fast.
# Runs in the background so it doesn't block the caller.

setup_global_workspace() {
    local ws_dir
    if [ -n "$QEDGEN_VALIDATION_WORKSPACE" ]; then
        ws_dir="$QEDGEN_VALIDATION_WORKSPACE"
    elif [ "$(uname)" = "Darwin" ]; then
        ws_dir="$HOME/Library/Caches/qedgen-solana-skills/validation-workspace"
    elif [ -n "$XDG_CACHE_HOME" ]; then
        ws_dir="$XDG_CACHE_HOME/qedgen-solana-skills/validation-workspace"
    else
        ws_dir="$HOME/.cache/qedgen-solana-skills/validation-workspace"
    fi

    if [ -f "$ws_dir/lakefile.lean" ] && [ -d "$ws_dir/.lake/packages/mathlib" ]; then
        echo "✓ Global validation workspace already exists at $ws_dir"
        return 0
    fi

    echo "Setting up global validation workspace at $ws_dir..."
    echo "  This downloads and caches Mathlib (~1-2 min with cache, 25+ min without)."
    echo "  Running in background — qedgen will work once this completes."

    mkdir -p "$ws_dir"
    "$QEDGEN_BIN" setup --workspace "$ws_dir" &
    echo "  Background PID: $!"
}

setup_global_workspace

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  qedgen installed successfully!"
echo ""
echo "  Binary: $QEDGEN_BIN"
echo ""
echo "  Requirements:"
echo "    - MISTRAL_API_KEY environment variable must be set"
echo "    - Lean toolchain (auto-installed via elan)"
echo ""
echo "  The global Mathlib cache may still be downloading in the background."
echo "  First run of 'qedgen verify --validate' may be slow if not ready."
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
