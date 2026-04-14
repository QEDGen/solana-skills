#!/bin/bash
set -e

REPO="QEDGen/solana-skills"
VERSION="v1.6.0"

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

# ── Verify SHA256 checksum ──────────────────────────────────────────────
verify_checksum() {
    local file="$1" expected="$2"
    local actual

    if command -v sha256sum &> /dev/null; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    elif command -v shasum &> /dev/null; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        echo "  ERROR: No sha256sum or shasum found. Cannot verify binary integrity."
        return 1
    fi

    if [ "$actual" != "$expected" ]; then
        echo "  ERROR: SHA256 checksum mismatch!"
        echo "    Expected: $expected"
        echo "    Actual:   $actual"
        return 1
    fi
    return 0
}

# ── Download from GitHub release ─────────────────────────────────────────
download_binary() {
    local asset_name="$1"

    # Use pinned version, not /latest/
    local url="https://github.com/${REPO}/releases/download/${VERSION}/${asset_name}"
    local checksum_url="https://github.com/${REPO}/releases/download/${VERSION}/${asset_name}.sha256"
    echo "  Downloading ${VERSION} from ${url} ..."

    mkdir -p "$SKILL_DIR/bin"

    local tmp_bin
    tmp_bin=$(mktemp)
    if ! curl -fSL --retry 2 -o "$tmp_bin" "$url" 2>/dev/null; then
        rm -f "$tmp_bin"
        return 1
    fi

    # Checksum verification is mandatory
    local checksum_file
    checksum_file=$(mktemp)
    if ! curl -fSL --retry 2 -o "$checksum_file" "$checksum_url" 2>/dev/null; then
        echo "  ERROR: Could not download checksum file. Refusing to install unverified binary."
        rm -f "$tmp_bin" "$checksum_file"
        return 1
    fi

    local expected
    expected=$(awk '{print $1}' "$checksum_file")
    rm -f "$checksum_file"

    if ! verify_checksum "$tmp_bin" "$expected"; then
        rm -f "$tmp_bin"
        return 1
    fi
    echo "  Checksum verified."

    mv "$tmp_bin" "$QEDGEN_BIN"
    chmod +x "$QEDGEN_BIN"

    if "$QEDGEN_BIN" --version &> /dev/null; then
        return 0
    fi
    rm -f "$QEDGEN_BIN"
    return 1
}

# ── Build from source ────────────────────────────────────────────────────
build_from_source() {
    echo "  Building from source..."

    if ! command -v cargo &> /dev/null; then
        echo ""
        echo "  ERROR: Rust toolchain not found."
        echo "  Please install Rust first: https://rustup.rs"
        echo "  Then re-run this install script."
        exit 1
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
            echo "✓ Downloaded qedgen binary from release (${VERSION})"
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
    echo ""
    echo "  Lean toolchain not found."
    echo "  Please install elan (Lean version manager):"
    echo "    https://github.com/leanprover/elan#installation"
    echo ""
    echo "  After installing elan, re-run this script or run:"
    echo "    elan toolchain install leanprover/lean4:v4.30.0-rc1"
    echo ""
fi

# ── Kani (optional) ──────────────────────────────────────────────────────
if ! command -v cargo-kani &> /dev/null; then
    echo ""
    echo "  Kani verifier not found (optional — needed for Kani proof harnesses)."
    echo "  To install:"
    echo "    cargo install --locked kani-verifier"
    echo "    cargo kani setup"
    echo ""
fi

# ── Set up global validation workspace ────────────────────────────────────
setup_global_workspace() {
    local ws_dir
    if [ -n "$QEDGEN_VALIDATION_WORKSPACE" ]; then
        ws_dir="$QEDGEN_VALIDATION_WORKSPACE"
    elif [ -n "$QEDGEN_HOME" ]; then
        ws_dir="$QEDGEN_HOME/workspace"
    else
        ws_dir="$HOME/.qedgen/workspace"
    fi

    if [ -f "$ws_dir/lakefile.lean" ]; then
        echo "✓ Global validation workspace exists at $ws_dir"
        return 0
    fi

    echo "Setting up global validation workspace at $ws_dir..."
    echo "  Running in background — qedgen will work once this completes."

    mkdir -p "$ws_dir"
    "$QEDGEN_BIN" setup --workspace "$ws_dir" &
    echo "  Background PID: $!"
}

setup_global_workspace

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  qedgen ${VERSION} installed successfully!"
echo ""
echo "  Binary: $QEDGEN_BIN"
echo ""
echo "  Requirements:"
echo "    - MISTRAL_API_KEY environment variable (for fill-sorry)"
echo "    - Lean toolchain via elan (https://github.com/leanprover/elan)"
echo ""
echo "  Workspace: ~/.qedgen/"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
