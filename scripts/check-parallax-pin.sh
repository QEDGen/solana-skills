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
# Also run mechanically (#371): `scripts/release-gate.sh` calls it, and
# `.github/workflows/parallax-pin.yml` runs it weekly. The failing condition
# lands in a user's crate, not ours, so a human release step was the wrong
# owner for it.
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
# crates/qedgen/src/codegen/integration_test.rs, run the gate with
# `--ignored`, then regenerate the bundled examples. There is no fixture
# manifest to update any more — since #383 the gate generates its program
# crate, so the pin reaches it through the same dev-dependency upsert a user
# gets, and the unit test that held a hand-written copy to this constant is
# gone with the copy.
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

# Go through curl so the HTTP status is observable: only a 404 proves the
# revision is gone. Every other failure is "unreachable" and must not gate a
# release: a rate limit (403/429), a bad token (401), an outage (5xx), or no
# network at all. Reading only the response body cannot tell those apart,
# because a rate-limit body is JSON too, just without a "status" key. That is
# how a 403 was read as "revision not found" and failed the release gate.
#
# A token lifts the 60/hour anonymous cap to 5000. `gh auth token` supplies
# one in local dev; CI passes GH_TOKEN.
# Prints the response body, then a final line holding the HTTP status
# ("000" when the request never completed). `fetch` runs inside a command
# substitution, so a global assignment would not survive the subshell.
# Both values have to come back through stdout.
fetch() {
    local path="$1" token="" url
    url="https://api.github.com/repos/$repo_url/$path"
    if command -v gh >/dev/null 2>&1; then
        token="$(gh auth token 2>/dev/null || true)"
    fi
    [[ -n "$token" ]] || token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"

    # `curl -sS` without `-f` exits 0 on an HTTP error, so the status line
    # is written for 403 and 404 alike; `-w` still runs when the transfer
    # itself fails, reporting "000".
    if [[ -n "$token" ]]; then
        curl -sS -w $'\n%{http_code}' \
            -H "Accept: application/vnd.github+json" \
            -H "X-GitHub-Api-Version: 2022-11-28" \
            -H "Authorization: Bearer $token" \
            "$url" 2>/dev/null
    else
        curl -sS -w $'\n%{http_code}' \
            -H "Accept: application/vnd.github+json" \
            -H "X-GitHub-Api-Version: 2022-11-28" \
            "$url" 2>/dev/null
    fi
}

response="$(fetch "compare/$pinned_rev...HEAD")"
http_status="${response##*$'\n'}"
comparison="${response%$'\n'*}"
[[ "$http_status" =~ ^[0-9]{3}$ ]] || http_status=""

if [[ "$http_status" == "404" ]]; then
    echo "✗ pinned revision not found upstream — force-push, rebase, or deleted branch"
    echo "  the git dependency will not resolve; pick a revision that exists"
    exit 1
fi

if [[ "$http_status" != "200" || -z "$comparison" ]]; then
    echo "upstream unreachable (HTTP ${http_status:-none}): skipping staleness report (not a failure)"
    exit 0
fi

# A 200 whose body is not a comparison means an API change or a truncated
# response. It is never proof that the revision is gone: 404 covers that.
read -r status ahead subjects <<<"$(
    printf '%s' "$comparison" | python3 -c '
import json, sys
VALID = {"identical", "ahead", "behind", "diverged"}
try:
    data = json.load(sys.stdin)
except Exception:
    print("unreadable 0 ")
    sys.exit(0)
status = data.get("status")
if status not in VALID:
    print("unreadable 0 ")
    sys.exit(0)
commits = data.get("commits") or []
subjects = "; ".join(
    c.get("commit", {}).get("message", "").splitlines()[0] for c in commits[-5:]
)
print(status, data.get("ahead_by", 0), subjects)
'
)"

if [[ "$status" == "unreadable" ]]; then
    echo "upstream comparison unreadable: skipping staleness report (not a failure)"
    exit 0
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
