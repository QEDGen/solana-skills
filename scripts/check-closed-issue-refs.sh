#!/usr/bin/env bash
# Flags capability-gap text that cites a CLOSED GitHub issue (#356).
#
# The drift class: a doc or help string says a feature "is not implemented
# (#N)" or lists "#N" as a known gap, then issue #N closes and the text
# stays. This bit three surfaces before this script existed: a RELEASING.md
# caveat (#260, carried for ten releases), the `verify --strict` row in
# references/cli.md, and the emitted UnsupportedReason strings (#354).
#
# Mechanics: scan the user-facing surfaces below for lines that BOTH cite
# an issue number AND use open-gap language. Ask GitHub for each cited
# issue's state. A CLOSED issue on a gap line is drift.
#
# Run: bash scripts/check-closed-issue-refs.sh   (release gate, RELEASING.md §9)
# Needs: gh (authenticated) + network.
# Exit code: 0 = clean, 1 = drift found, 2 = cannot run (no gh).
#
# To cite a closed issue on a gap line on purpose (rare), append the
# marker: closed-issue-ok

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v gh >/dev/null 2>&1; then
    echo "check-closed-issue-refs: gh not found; cannot query issue states." >&2
    exit 2
fi

# User-facing surfaces where a stale gap claim misleads users or agents.
SURFACES=(
    "references"
    "docs/framework-support.md"
    "docs/RELEASING.md"
    "README.md"
    "SKILL.md"
    "crates/qedgen/src/cli.rs"
    "crates/qedgen/src/obligations/mod.rs"
    "crates/qedgen/src/obligations/inventory.rs"
)

# A line is a gap claim when it uses open-gap language. Keep this list in
# sync with how we actually phrase gaps; broaden before narrowing. Bare
# "unsupported" is excluded on purpose: it is the manifest status
# vocabulary (`emitted` / `unsupported` / `failed`) and fires on feature
# provenance lines, not gap claims — only a direct "unsupported (#N)"
# citation counts.
GAP_PATTERN='not implemented|not yet|yet \(#|known gap|open gap|known capability|cannot|does not support|do not support|unsupported \(#|deferred|parked|waiting on|blocked on'

# Collect "path:line:text" candidates: gap language + an issue reference,
# minus deliberate exceptions. release-history.md is a historical record;
# its gap claims were true at the release they describe.
candidates="$(grep -rnE "$GAP_PATTERN" "${SURFACES[@]}" 2>/dev/null \
    | grep -E '#[0-9]+' \
    | grep -v 'closed-issue-ok' \
    | grep -v 'references/release-history\.md:' || true)"

if [[ -z "$candidates" ]]; then
    echo "check-closed-issue-refs: no issue references on gap lines."
    exit 0
fi

# Query each distinct issue number once. "number state" lines; plain
# variables only — macOS ships bash 3.2 (no associative arrays).
numbers="$(printf '%s\n' "$candidates" | grep -oE '#[0-9]+' | tr -d '#' | sort -un)"

states=""
for n in $numbers; do
    # A merged PR cited on a gap line is the same drift as a closed issue.
    state="$(gh issue view "$n" --json state -q .state 2>/dev/null \
        || gh pr view "$n" --json state -q .state 2>/dev/null \
        || echo UNKNOWN)"
    states="${states}${n} ${state}
"
done

state_for() {
    printf '%s' "$states" | awk -v n="$1" '$1 == n { print $2; exit }'
}

drift=0
while IFS= read -r line; do
    for ref in $(printf '%s\n' "$line" | grep -oE '#[0-9]+' | tr -d '#' | sort -u); do
        state="$(state_for "$ref")"
        case "$state" in
            CLOSED | MERGED)
                if [[ $drift -eq 0 ]]; then
                    echo "check-closed-issue-refs: gap lines citing CLOSED issues:" >&2
                fi
                echo "  #${ref} (${state}): ${line}" >&2
                drift=1
                ;;
        esac
    done
done <<<"$candidates"

if [[ $drift -eq 1 ]]; then
    echo "Reword each line to describe the current gap (or mark 'closed-issue-ok' if the citation is deliberate)." >&2
    exit 1
fi

echo "check-closed-issue-refs: clean ($(printf '%s\n' "$numbers" | wc -l | tr -d ' ') issue(s) checked)."
