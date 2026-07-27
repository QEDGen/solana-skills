#!/usr/bin/env bash
# Reports how far the pinned Parallax revision has fallen behind upstream.
#
# `parallax-svm` is not published to crates.io, so the generated integration
# scaffold pins a git revision. `parallax_integration_gate` proves the
# scaffold compiles against THAT revision, but nothing otherwise says the
# revision has aged — and Parallax is 0.1 on a fast-moving repository.
#
# Run: bash scripts/check-parallax-pin.sh
#
# Exit codes, chosen so this never blocks a release for being merely old:
#   0 = pin current, OR pin behind upstream (expected — pinning is the point),
#       OR upstream unreachable (offline dev / rate limit / CI flake)
#   1 = the pinned revision no longer exists upstream (force-push, rebase,
#       deleted branch). The dependency will not resolve; this is real
#       breakage, not staleness.
#   2 = could not read the pin from source (the single-source const moved)
#
# Bumping the pin: edit PARALLAX_GIT_REV in
# crates/qedgen/src/codegen/integration_test.rs, update the gate fixture
# manifest (a unit test enforces they match), run the gate with `--ignored`,
# then regenerate the bundled examples.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_file="$repo_root/crates/qedgen/src/codegen/integration_test.rs"

# Read the pin from its single source rather than duplicating it here.
pinned_rev="$(
    sed -nE 's/^pub\(crate\) const PARALLAX_GIT_REV: &str = "([0-9a-f]+)";.*/\1/p' "$source_file" \
        | head -n 1
)"
repo_url="$(
    sed -nE 's|^pub\(crate\) const PARALLAX_GIT_URL: &str = "https://github.com/([^"]+)";.*|\1|p' "$source_file" \
        | head -n 1
)"

if [[ -z "$pinned_rev" || -z "$repo_url" ]]; then
    echo "check-parallax-pin: could not read PARALLAX_GIT_REV/PARALLAX_GIT_URL from" >&2
    echo "  $source_file" >&2
    echo "  (did the const get renamed or moved?)" >&2
    exit 2
fi

echo "pinned: $pinned_rev ($repo_url)"

# `gh` carries auth, so it dodges the 60/hour unauthenticated rate limit.
# Both clients print the response body on an HTTP error and exit nonzero, so
# capture body and status separately — concatenating a failed gh body with a
# curl retry produces unparseable JSON.
fetch() {
    local path="$1" body=""
    if command -v gh >/dev/null 2>&1; then
        if body="$(gh api "repos/$repo_url/$path" 2>/dev/null)"; then
            printf '%s' "$body"
            return 0
        fi
        # gh reached GitHub and got a real error body (404 on a missing rev).
        [[ -n "$body" ]] && { printf '%s' "$body"; return 0; }
    fi
    curl -sS -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/$repo_url/$path" 2>/dev/null
}

comparison="$(fetch "compare/$pinned_rev...HEAD")"
if [[ -z "$comparison" ]]; then
    echo "upstream unreachable — skipping staleness report (not a failure)"
    exit 0
fi

# GitHub's ERROR bodies also carry a "status" key ("404"), so a missing
# revision would otherwise read as status=404/ahead_by=0 and be reported as
# "at upstream HEAD". Accept only the documented compare statuses.
read -r status ahead subjects <<<"$(
    printf '%s' "$comparison" | python3 -c '
import json, sys
VALID = {"identical", "ahead", "behind", "diverged"}
try:
    data = json.load(sys.stdin)
except Exception:
    print("missing 0 ")
    sys.exit(0)
status = data.get("status")
if status not in VALID:
    print("missing 0 ")
    sys.exit(0)
commits = data.get("commits") or []
subjects = "; ".join(
    c.get("commit", {}).get("message", "").splitlines()[0] for c in commits[-5:]
)
print(status, data.get("ahead_by", 0), subjects)
'
)"

if [[ "$status" == "missing" ]]; then
    echo "✗ pinned revision not found upstream — force-push, rebase, or deleted branch"
    echo "  the git dependency will not resolve; pick a revision that exists"
    exit 1
fi

if [[ "${ahead:-0}" -eq 0 ]]; then
    echo "✓ pin is at upstream HEAD"
    exit 0
fi

echo "pin is $ahead commit(s) behind upstream HEAD"
[[ -n "${subjects:-}" ]] && echo "  recent upstream commits: $subjects"
echo
echo "  Staleness is expected — the pin exists so upstream churn cannot break"
echo "  generated code. Bump only when you want the newer API, and re-run:"
echo "    cargo test -p qedgen-solana-skills --test parallax_integration_gate -- --ignored"
exit 0
