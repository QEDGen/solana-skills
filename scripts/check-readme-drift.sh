#!/usr/bin/env bash
# Checks that every CLI subcommand appears in README.md.
# Run: bash scripts/check-readme-drift.sh
# Exit code: 0 = no drift, 1 = drift detected.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
README="$REPO_ROOT/README.md"
MAIN_RS="$REPO_ROOT/crates/qedgen/src/main.rs"

# Extract subcommand names from the Commands enum in main.rs.
# Handles: explicit #[command(name = "...")] overrides, PascalCase -> kebab-case conversion,
# and #[command(subcommand)] variants.
get_commands() {
    local in_enum=0
    local next_name=""
    while IFS= read -r line; do
        if [[ "$line" =~ ^enum\ Commands ]]; then
            in_enum=1
            continue
        fi
        [[ $in_enum -eq 0 ]] && continue
        [[ "$line" =~ ^\} ]] && break

        # Explicit command name: #[command(name = "foo")]
        if [[ "$line" =~ \#\[command\(name\ =\ \"([^\"]+)\"\) ]]; then
            next_name="${BASH_REMATCH[1]}"
            continue
        fi

        # Subcommand variant: `    VariantName {` or `    VariantName(`
        if [[ "$line" =~ ^[[:space:]]+([A-Z][a-zA-Z0-9]+)[[:space:]]*[\{\(] ]]; then
            variant="${BASH_REMATCH[1]}"
            if [[ -n "$next_name" ]]; then
                echo "$next_name"
                next_name=""
            else
                # PascalCase -> kebab-case
                echo "$variant" | sed -E 's/([a-z0-9])([A-Z])/\1-\2/g' | tr '[:upper:]' '[:lower:]'
            fi
        fi
    done < "$MAIN_RS"
}

commands=$(get_commands)
readme_content=$(<"$README")
missing=""
total=0

for cmd in $commands; do
    total=$((total + 1))
    if ! echo "$readme_content" | grep -qi "$cmd"; then
        missing="$missing $cmd"
    fi
done

if [[ -n "$missing" ]]; then
    echo "README drift detected! The following CLI commands are not mentioned in README.md:"
    for cmd in $missing; do
        echo "  - $cmd"
    done
    echo ""
    echo "Update README.md to document these commands, or mark them as internal."
    exit 1
fi

echo "No README drift detected. All $total CLI commands are documented."
