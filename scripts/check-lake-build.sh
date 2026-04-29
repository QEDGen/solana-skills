#!/usr/bin/env bash
# Builds every bundled `examples/*/formal_verification/` Lean project.
# Run: bash scripts/check-lake-build.sh
# Exit code: 0 = all built clean, 1 = at least one build failed.
#
# Pre-release-checklist gate (CLAUDE.md item 6). Exists because earlier
# releases shipped examples whose Spec.lean did not compile — `qedgen
# check --regen-drift` and `cargo check` per example only verify the
# Rust-side scaffold, not the Lean side. This script closes that gap.
#
# Skips a project (with a warning) if `lake` is not on PATH or if
# `.lake/` and `lake-manifest.json` are absent (cold checkout). Use
# `--strict` to turn skip-into-failure for release-day runs where every
# example must build.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

STRICT=0
ONLY_PATTERN=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --strict)
            STRICT=1
            shift
            ;;
        --only)
            ONLY_PATTERN="$2"
            shift 2
            ;;
        -h|--help)
            cat <<EOF
Usage: bash scripts/check-lake-build.sh [--strict] [--only <pattern>]

  --strict           Treat skipped projects (missing toolchain or .lake/) as failures.
  --only <pattern>   Run only projects whose path matches <pattern> (substring).
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if ! command -v lake >/dev/null 2>&1; then
    if [[ $STRICT -eq 1 ]]; then
        echo "lake not found on PATH (required in --strict mode). Install via elan: https://github.com/leanprover/elan" >&2
        exit 1
    fi
    echo "lake not found on PATH; skipping all examples. Install via elan to enable this gate." >&2
    exit 0
fi

projects=()
while IFS= read -r -d '' lakefile; do
    project_dir="$(dirname "$lakefile")"
    if [[ -n "$ONLY_PATTERN" && "$project_dir" != *"$ONLY_PATTERN"* ]]; then
        continue
    fi
    projects+=("$project_dir")
done < <(find examples -maxdepth 4 -name 'lakefile.lean' -print0 | sort -z)

if [[ ${#projects[@]} -eq 0 ]]; then
    echo "No lake projects found under examples/." >&2
    exit 1
fi

failed=()
skipped=()
ok=()

for project in "${projects[@]}"; do
    rel="${project#"$REPO_ROOT/"}"
    rel="${rel#./}"

    if [[ ! -d "$project/.lake" || ! -f "$project/lake-manifest.json" ]]; then
        if [[ $STRICT -eq 1 ]]; then
            echo "FAIL  $rel  (no .lake/ or lake-manifest.json — run 'lake update' once)"
            failed+=("$rel")
            continue
        fi
        echo "SKIP  $rel  (no .lake/ or lake-manifest.json — run 'lake update' once)"
        skipped+=("$rel")
        continue
    fi

    log="$(mktemp)"
    if ( cd "$project" && lake build ) >"$log" 2>&1; then
        echo "OK    $rel"
        ok+=("$rel")
    else
        echo "FAIL  $rel"
        sed 's/^/        /' "$log" | tail -40
        failed+=("$rel")
    fi
    rm -f "$log"
done

echo
echo "Built ${#ok[@]} / ${#projects[@]} examples (${#skipped[@]} skipped, ${#failed[@]} failed)."

if [[ ${#failed[@]} -gt 0 ]]; then
    echo
    echo "Lake build failures:" >&2
    for f in "${failed[@]}"; do echo "  - $f" >&2; done
    exit 1
fi
exit 0
