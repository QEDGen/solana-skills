#!/bin/bash
# Explicit QEDGen installation. Availability checks belong in tools/qedgen.
set -euo pipefail

REPO="QEDGen/solana-skills"
SKILL_DIR="$(cd "$(dirname "$0")" && pwd)"
QEDGEN_BIN="$SKILL_DIR/bin/qedgen"
LINK_DIR=""
FROM_SOURCE=false
staged=""
checksums=""

usage() {
    cat <<'USAGE'
Usage: bash install.sh [--link-dir DIRECTORY] [--from-source]

Install QEDGen into this skill's bin/ directory. Downloads a pinned release
and verifies its checksum and version before replacing an existing binary.
A source checkout can fall back to a locked Cargo build if the binary download
is unavailable. Integrity failures never fall back to another install method.

  --link-dir DIRECTORY  Also link qedgen into this explicitly chosen directory.
                        Existing unrelated files or links are never replaced.
  --from-source         Build from this source checkout without downloading a release.
  -h, --help            Show this help without installing anything.

No default PATH links, shell-profile changes, or toolchain installation.
USAGE
}

die() { echo "ERROR: $*" >&2; exit 1; }
cleanup() {
    if [[ -n "$staged" ]]; then rm -f -- "$staged"; fi
    if [[ -n "$checksums" ]]; then rm -f -- "$checksums"; fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
    case "$1" in
        --link-dir)
            [[ $# -ge 2 && -n "$2" && "$2" != -* ]] || die "--link-dir requires a directory"
            LINK_DIR="$2"
            shift 2 ;;
        --from-source) FROM_SOURCE=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "Unknown argument: $1 (see --help)" ;;
    esac
done

# Cargo.toml remains authoritative in source checkouts. The package builder
# generates VERSION for portable skills, which do not contain the source tree.
if [[ -f "$SKILL_DIR/crates/qedgen/Cargo.toml" ]]; then
    version="$(sed -nE 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$SKILL_DIR/crates/qedgen/Cargo.toml")"
elif [[ -f "$SKILL_DIR/VERSION" ]]; then
    version="$(tr -d '\r\n' < "$SKILL_DIR/VERSION")"
else
    die "Missing release version metadata."
fi
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || die "Invalid release version metadata."
VERSION="v$version"

# Validate a requested link before any download/build, preserving user files.
if [[ -n "$LINK_DIR" ]]; then
    [[ "$LINK_DIR" = /* ]] || LINK_DIR="$PWD/$LINK_DIR"
    link="$LINK_DIR/qedgen"
    if [[ -e "$link" || -L "$link" ]]; then
        [[ -L "$link" && "$(readlink "$link")" = "$QEDGEN_BIN" ]] || die "Refusing to replace existing $link"
    fi
fi

detect_asset_name() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in Darwin) os=apple-darwin ;; Linux) os=unknown-linux-gnu ;; *) return 1 ;; esac
    case "$arch" in arm64|aarch64) arch=aarch64 ;; x86_64) ;; *) return 1 ;; esac
    echo "qedgen-${arch}-${os}"
}

verify_checksum() {
    local file="$1" expected="$2" actual
    [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] || { echo "ERROR: Invalid SHA256 checksum file." >&2; return 1; }
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$file" | awk '{print $1}')" || return 1
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$file" | awk '{print $1}')" || return 1
    else
        echo "ERROR: No SHA256 verifier available; refusing installation." >&2
        return 1
    fi
    [[ "$actual" = "$expected" ]] || { echo "ERROR: SHA256 checksum mismatch; existing binary preserved." >&2; return 1; }
}

# A candidate always resides beside the destination, so the final rename is
# atomic. Never move an unverified or non-running candidate over a working CLI.
activate_candidate() {
    local reported
    # mktemp creates 0600, so `chmod +x` alone would leave the installed CLI
    # user-only-executable. A --link-dir target is often shared, and the skill
    # directory itself may be read by another account.
    chmod 755 "$staged" || return 1
    if ! reported="$("$staged" --version 2>/dev/null)" || [[ "$reported" != "qedgen $version" ]]; then
        echo "ERROR: Candidate is not runnable as qedgen $version; existing binary preserved." >&2
        return 1
    fi
    mv -f -- "$staged" "$QEDGEN_BIN" || return 1
    staged=""
}

# Return 1 for an unavailable binary (source fallback permitted), 2 for an
# integrity or validation failure (fail closed, never silently fall back).
download_binary() {
    local asset="$1" expected
    local base="https://github.com/${REPO}/releases/download/${VERSION}"
    mkdir -p "$SKILL_DIR/bin" || return 2
    staged="$(mktemp "$SKILL_DIR/bin/.qedgen.XXXXXX")" || return 2
    echo "Downloading ${VERSION} from ${base}/${asset} ..."
    if ! curl --proto '=https' --proto-redir '=https' -fSL --retry 2 -o "$staged" "$base/$asset"; then
        return 1
    fi
    checksums="$(mktemp "$SKILL_DIR/bin/.checksum.XXXXXX")" || return 2
    if ! curl --proto '=https' --proto-redir '=https' -fSL --retry 2 -o "$checksums" "$base/$asset.sha256"; then
        echo "ERROR: Checksum unavailable; refusing to install unverified binary." >&2
        return 2
    fi
    expected="$(awk '{print $1}' "$checksums")"
    verify_checksum "$staged" "$expected" || return 2
    echo "Checksum verified."
    activate_candidate || return 2
}

build_from_source() {
    [[ -f "$SKILL_DIR/Cargo.toml" && -f "$SKILL_DIR/crates/qedgen/Cargo.toml" ]] || die \
        "Verified release unavailable. This portable skill has no source checkout; retry or build ${VERSION} from https://github.com/${REPO}."
    command -v cargo >/dev/null 2>&1 || die "Rust is required for source builds. Install it yourself from https://rustup.rs and retry."
    echo "Building ${VERSION} from source..."
    cargo build --locked --release --manifest-path "$SKILL_DIR/Cargo.toml" --target-dir "$SKILL_DIR/target"
    mkdir -p "$SKILL_DIR/bin"
    cleanup
    staged="$(mktemp "$SKILL_DIR/bin/.qedgen.XXXXXX")"
    cp "$SKILL_DIR/target/release/qedgen" "$staged"
    activate_candidate || die "Source build validation failed."
    echo "qedgen binary built from source."
}

existing=""
if [[ -x "$QEDGEN_BIN" ]]; then
    existing="$("$QEDGEN_BIN" --version 2>/dev/null || true)"
fi
if [[ "$existing" = "qedgen $version" && "$FROM_SOURCE" = false ]]; then
    echo "Pre-built qedgen binary is current (${VERSION})."
elif [[ "$FROM_SOURCE" = true ]]; then
    build_from_source
else
    asset="$(detect_asset_name)" || die "Unsupported platform; use --from-source in a source checkout."
    if download_binary "$asset"; then
        echo "Downloaded qedgen binary from release (${VERSION})."
    else
        result=$?
        [[ "$result" -eq 1 ]] || die "Release validation failed; existing binary preserved."
        echo "Release binary unavailable; trying this source checkout."
        build_from_source
    fi
fi

if [[ -n "$LINK_DIR" ]]; then
    mkdir -p "$LINK_DIR"
    link="$LINK_DIR/qedgen"
    # Recheck after installation and never clobber a concurrently created path.
    if [[ -L "$link" && "$(readlink "$link")" = "$QEDGEN_BIN" ]]; then
        :
    elif [[ -e "$link" || -L "$link" ]]; then
        die "Refusing to replace existing $link"
    else
        ln -s "$QEDGEN_BIN" "$link"
    fi
    echo "Linked qedgen into $LINK_DIR (add that directory to PATH if needed)."
fi

echo "qedgen ${VERSION} installed successfully!"
echo "Binary: $QEDGEN_BIN"
echo "Run: $SKILL_DIR/tools/qedgen --help"
echo "Lean, Kani, Rust, and provider API keys are user-managed prerequisites."
echo "Optional: qedgen setup [--mathlib] prepares the Lean validation workspace."
