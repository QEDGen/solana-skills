#!/usr/bin/env bash
# Checks that every CLI subcommand appears in README.md.
# Run: bash scripts/check-readme-drift.sh
# Exit code: 0 = no drift, 1 = drift detected.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
README="$REPO_ROOT/README.md"
# The `Commands` enum lives in cli.rs since the v2.36 main.rs split (PR #115).
CLI_RS="$REPO_ROOT/crates/qedgen/src/cli.rs"

# Extract subcommand names from the Commands enum in cli.rs.
# Handles: explicit #[command(name = "...")] overrides, PascalCase -> kebab-case conversion,
# and #[command(subcommand)] variants.
get_commands() {
    local in_enum=0
    local next_name=""
    while IFS= read -r line; do
        if [[ "$line" =~ ^(pub\(crate\)\ )?enum\ Commands ]]; then
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
    done < "$CLI_RS"
}

commands=$(get_commands)
readme_content=$(<"$README")
missing=""
total=0

for cmd in $commands; do
    total=$((total + 1))
    # Use a here-string instead of `echo | grep` — under `set -o pipefail`,
    # `grep -q` closes the pipe early on first match, which makes `echo` exit
    # non-zero with "Broken pipe" and fails the whole pipeline despite the
    # match succeeding. CI runners hit this intermittently on large READMEs.
    if ! grep -qi "$cmd" <<<"$readme_content"; then
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

# Internal planning files under docs/prds/ are ignored and disposable. Tracked
# documentation or source comments must point to maintained references or
# shipped release notes, never to a local-only PRD/plan/handoff.
if stale_prd_refs="$(git -C "$REPO_ROOT" grep -nE \
    'docs/prds/(AUDITOR|CODEGEN|EVAL|HANDOFF|MANUAL|PLAN|PRD|REVIEW|SCOPING|SMOKE|SPEC|SPIKE)[^[:space:]`]*\.md' \
    -- '*.md' '*.rs' '*.sh' || true)" && [[ -n "$stale_prd_refs" ]]; then
    echo "Documentation drift detected! Tracked files reference ignored planning docs:"
    echo "$stale_prd_refs"
    echo "Point these references at maintained docs, release notes, issues, or PRs."
    exit 1
fi

echo "No README drift detected. All $total CLI commands are documented."
