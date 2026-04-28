#!/usr/bin/env bash
# Checks that package metadata uses one QEDGen version.
# Run: bash scripts/check-version-consistency.sh
# Exit code: 0 = versions match, 1 = drift detected.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PACKAGE_JSON="$REPO_ROOT/package.json"
CARGO_TOML="$REPO_ROOT/crates/qedgen/Cargo.toml"

package_version="$(sed -nE 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' "$PACKAGE_JSON" | head -n 1)"
cargo_version="$(
    sed -nE '/^\[package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p;}' "$CARGO_TOML" | head -n 1
)"

if [[ -z "$package_version" ]]; then
    echo "Could not read version from $PACKAGE_JSON" >&2
    exit 1
fi

if [[ -z "$cargo_version" ]]; then
    echo "Could not read [package] version from $CARGO_TOML" >&2
    exit 1
fi

if [[ "$package_version" != "$cargo_version" ]]; then
    echo "Version drift detected:"
    echo "  package.json: $package_version"
    echo "  crates/qedgen/Cargo.toml: $cargo_version"
    exit 1
fi

echo "Version metadata consistent: $package_version"
