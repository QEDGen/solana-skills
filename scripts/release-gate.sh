#!/usr/bin/env bash
# Mechanical release gate (#271): asserts the bundled-example baseline by
# exit code and summary line — never by eyeball. The escrow E-lint (#260)
# survived multiple release cuts under the prose version of this gate.
#
# Covers RELEASING.md:
#   §7 zero-sorry sweep over examples/**/*.lean (Tier-0 CPI carve-outs,
#      marked by the "ensures @ `" comment, are the only allowed sorry)
#   §8 `qedgen check --frozen` over every bundled example with a qed.toml,
#      asserted against the expectation table below
#
# Usage: scripts/release-gate.sh
#   QEDGEN_BIN overrides the binary (default bin/qedgen; CI passes the
#   workspace debug build).
#
# Changing the baseline (a new example, an intentional new warning)
# requires editing the expectation table here, in the same PR.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
QEDGEN_BIN="${QEDGEN_BIN:-$repo_root/bin/qedgen}"

if [[ ! -x "$QEDGEN_BIN" ]]; then
  echo "release-gate: qedgen binary not found/executable at $QEDGEN_BIN — build it or set QEDGEN_BIN" >&2
  exit 2
fi

failures=0
fail() {
  echo "✗ $*" >&2
  failures=$((failures + 1))
}

# ── §8: frozen-check baseline over bundled examples ─────────────────────
# Expectation table: "<expected-exit> <expected-summary-prefix>".
# Baseline (post-v2.44 cleanup + #260): every example error- AND
# warning-clean, except multisig's one intentional P2
# (excluded_op_modifies_property — see the comment in multisig.qedspec).
expectation_for() {
  case "$1" in
    multisig) echo "1 0 error(s), 1 warning(s)" ;;
    *)        echo "0 0 error(s), 0 warning(s)" ;;
  esac
}

checked=0
for dir in "$repo_root"/examples/rust/*/; do
  name="$(basename "$dir")"
  [[ -f "$dir/qed.toml" ]] || continue
  checked=$((checked + 1))

  expected="$(expectation_for "$name")"
  expected_exit="${expected%% *}"
  expected_summary="${expected#* }"

  out="$("$QEDGEN_BIN" check --frozen --spec "$dir" 2>&1)"
  ec=$?
  summary="$(tail -1 <<<"$out")"

  if [[ "$ec" -ne "$expected_exit" ]]; then
    fail "$name: exit $ec, expected $expected_exit — last lines:"
    tail -5 <<<"$out" | sed 's/^/    /' >&2
  fi
  if [[ "$summary" != "$expected_summary"* ]]; then
    fail "$name: summary '$summary', expected '$expected_summary …'"
  fi
done

if [[ "$checked" -eq 0 ]]; then
  fail "no bundled examples with qed.toml found under examples/rust/ — gate is vacuous"
fi

# ── §7: zero-sorry sweep over example Lean artifacts ────────────────────
# Scope: generated/authored proof artifacts only — vendored copies of the
# `lean_solana` support package (metaprogramming code whose *strings* and
# doc comments mention sorry) are excluded. Tier-0 CPI theorems (callee
# declared no `ensures`) are the only allowed `sorry` in scope, and those
# files carry the "ensures @ `" marker comment.
while IFS= read -r f; do
  if grep -qw sorry "$f" && ! grep -q 'ensures @ `' "$f"; then
    fail "unintended sorry in ${f#"$repo_root"/}:"
    grep -nw sorry "$f" | head -3 | sed 's/^/    /' >&2
  fi
done < <(find "$repo_root/examples" -name '*.lean' \
  -not -path '*/.lake/*' -not -path '*/lean_solana/*')

# ── Verdict ─────────────────────────────────────────────────────────────
if [[ "$failures" -gt 0 ]]; then
  echo "release-gate: $failures failure(s) — baseline drifted (or update the expectation table in this script, in the same PR)" >&2
  exit 1
fi
echo "release-gate: $checked example baseline(s) + zero-sorry sweep clean"
