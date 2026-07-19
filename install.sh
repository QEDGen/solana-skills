#!/bin/bash
set -e

REPO="QEDGen/solana-skills"

# Resolve the directory where this script lives (= skill root)
SKILL_DIR="$(cd "$(dirname "$0")" && pwd)"

# Derive version from Cargo.toml (single source of truth)
VERSION="v$(grep '^version' "$SKILL_DIR/crates/qedgen/Cargo.toml" | head -1 | sed 's/.*"\(.*\)"/\1/')"
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
# The checkout's Cargo.toml is the source of truth for the expected
# version. A pre-existing bin/qedgen is kept ONLY if it matches $VERSION:
# a stale binary from an earlier install still runs, but fails on newer
# specs with errors that look like spec bugs, not version skew.
existing_version=""
if [ -f "$QEDGEN_BIN" ] && [ -x "$QEDGEN_BIN" ]; then
    existing_version="$("$QEDGEN_BIN" --version 2>/dev/null | awk '{print $2}')"
fi

if [ -n "$existing_version" ] && [ "v$existing_version" = "$VERSION" ]; then
    echo "✓ Pre-built qedgen binary is current (${VERSION})"
else
    if [ -n "$existing_version" ]; then
        echo "Pre-built binary is v${existing_version}; this checkout expects ${VERSION} — refreshing."
    else
        echo "Pre-built binary missing or not runnable."
    fi

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
        if command -v cargo &> /dev/null; then
            echo "  Release binary unavailable, falling back to source compilation..."
            build_from_source
            echo "✓ qedgen binary built from source"
        elif [ -n "$existing_version" ]; then
            # No download, no toolchain: keep the stale binary rather than
            # leave nothing, but say so loudly (never "✓ compatible").
            STALE_KEPT=true
            echo ""
            echo "  WARNING: could not refresh qedgen — keeping STALE binary v${existing_version},"
            echo "           but this skill checkout is ${VERSION}. Newer specs may fail with"
            echo "           confusing parse/validation errors. Install Rust (https://rustup.rs)"
            echo "           or restore network access, then re-run install.sh."
            echo ""
        else
            # No binary at all — surfaces the rustup instructions and exits.
            build_from_source
        fi
    fi
fi

# ── Put qedgen on PATH so `qedgen ...` works without the bin/ prefix ─────────
# The skill clones to a harness-specific dir; symlink the binary into a
# conventional PATH location so SKILL.md's bare `qedgen check` resolves.
# Idempotent (ln -sf), and we warn with the exact export if the dir isn't
# already on PATH.
LINK_DIR="$HOME/.local/bin"
mkdir -p "$LINK_DIR" 2>/dev/null || true
if ln -sf "$QEDGEN_BIN" "$LINK_DIR/qedgen" 2>/dev/null; then
    ON_PATH=false
    case ":$PATH:" in *":$LINK_DIR:"*) ON_PATH=true ;; esac
    # Rust/Solana devs almost always have ~/.cargo/bin on PATH too — link there
    # as well so it resolves even if ~/.local/bin isn't wired up.
    if [ -d "$HOME/.cargo/bin" ]; then
        ln -sf "$QEDGEN_BIN" "$HOME/.cargo/bin/qedgen" 2>/dev/null || true
        case ":$PATH:" in *":$HOME/.cargo/bin:"*) ON_PATH=true ;; esac
    fi
    PATH_LINKED=true
else
    PATH_LINKED=false
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "${STALE_KEPT:-false}" = true ]; then
    echo "  qedgen ${VERSION} NOT installed — stale v${existing_version} kept (see warning above)."
else
    echo "  qedgen ${VERSION} installed successfully!"
fi
echo ""
if [ "${PATH_LINKED:-false}" = true ] && [ "${ON_PATH:-false}" = true ]; then
    echo "  qedgen is on your PATH — run \`qedgen --help\` to confirm."
elif [ "${PATH_LINKED:-false}" = true ]; then
    echo "  Linked qedgen into $LINK_DIR."
    echo "  Add it to PATH:  export PATH=\"$LINK_DIR:\$PATH\""
else
    echo "  Binary: $QEDGEN_BIN  (add its dir to PATH to call \`qedgen\` directly)"
fi
echo ""
echo "  Next steps:"
echo "    1. Write a .qedspec for your program (or let your agent generate one)"
echo "    2. Run: qedgen check --spec my_program.qedspec"
echo "    3. Run: qedgen codegen --spec my_program.qedspec --all"
echo ""
echo "  Lean proofs, Kani harnesses, and API keys (MISTRAL_API_KEY,"
echo "  ARISTOTLE_API_KEY) are set up automatically when first needed."
echo "  Run 'qedgen setup' to configure them manually."
echo ""
echo "  Workspace: ~/.qedgen/"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
