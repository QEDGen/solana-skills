#!/usr/bin/env bash
# Cold-start smoke (#274): simulate the outside user on a fresh checkout.
#
# The #252-254 bug batch all came from the first external dogfooder — the
# in-repo flow never exercises install.sh or the installed-binary
# quickstart. This script does, hermetically: every scenario runs in a
# scratch copy of the tree with HOME sandboxed, so the symlink step can
# never touch the invoking user's real ~/.local/bin or ~/.cargo/bin.
#
# Scenarios:
#   A fresh        — no bin/qedgen: install resolves the pinned release
#                    asset (or source-builds) and lands the right version
#   B match        — matching binary is kept, not re-downloaded
#   C stale        — version-skewed binary is refreshed (#252 regression)
#   D stranded     — stale binary + no download + no cargo: loud warning,
#                    honest "NOT installed" banner, exit 0
#   E quickstart   — the SKILL.md core loop driven with the INSTALLED
#                    binary (journey tests cover the workspace build; this
#                    covers what a user actually runs)
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

VERSION="v$(grep '^version' "$repo_root/crates/qedgen/Cargo.toml" | head -1 | sed 's/.*"\(.*\)"/\1/')"

failures=0
fail() {
  echo "✗ $*" >&2
  failures=$((failures + 1))
}
pass() { echo "✓ $*"; }

# A pristine copy of the checkout (sans build products), one per scenario.
base="$scratch/base"
mkdir -p "$base"
rsync -aq \
  --exclude=.git --exclude=target --exclude='.lake' \
  --exclude=node_modules --exclude='.anchor' \
  "$repo_root/" "$base/"

make_tree() { # <name> -> path on stdout
  local tree="$scratch/$1"
  cp -R "$base" "$tree"
  echo "$tree"
}

run_install() { # <tree> [extra env as K=V...]
  local tree="$1"
  shift
  mkdir -p "$scratch/home"
  env HOME="$scratch/home" "$@" "$tree/install.sh" 2>&1
}

installed_version() { # <tree>
  "$1/bin/qedgen" --version 2>/dev/null | awk '{print $2}'
}

write_stale_stub() { # <tree>
  mkdir -p "$1/bin"
  printf '#!/bin/sh\necho "qedgen 2.15.1"\n' > "$1/bin/qedgen"
  chmod +x "$1/bin/qedgen"
}

# ── A: fresh install ────────────────────────────────────────────────────
tree="$(make_tree fresh)"
rm -f "$tree/bin/qedgen"
out="$(run_install "$tree")" && rc=0 || rc=$?
if [[ $rc -ne 0 ]]; then
  fail "A fresh: install.sh exited $rc"
  tail -5 <<<"$out" | sed 's/^/    /' >&2
elif [[ "v$(installed_version "$tree")" != "$VERSION" ]]; then
  fail "A fresh: installed '$(installed_version "$tree")', expected ${VERSION#v}"
else
  pass "A fresh install lands $VERSION"
fi
fresh_tree="$tree"

# ── B: matching binary kept ─────────────────────────────────────────────
out="$(run_install "$fresh_tree")" && rc=0 || rc=$?
if [[ $rc -ne 0 ]] || ! grep -q "is current" <<<"$out"; then
  fail "B match: expected 'is current' keep-path (exit $rc)"
  tail -5 <<<"$out" | sed 's/^/    /' >&2
else
  pass "B matching binary kept"
fi

# ── C: stale binary refreshed ───────────────────────────────────────────
tree="$(make_tree stale)"
write_stale_stub "$tree"
out="$(run_install "$tree")" && rc=0 || rc=$?
if [[ $rc -ne 0 ]]; then
  fail "C stale: install.sh exited $rc"
  tail -5 <<<"$out" | sed 's/^/    /' >&2
elif [[ "v$(installed_version "$tree")" != "$VERSION" ]]; then
  fail "C stale: still '$(installed_version "$tree")' after install, expected refresh to ${VERSION#v}"
else
  pass "C stale binary refreshed to $VERSION"
fi

# ── D: stranded (stale + no download + no cargo) ────────────────────────
tree="$(make_tree stranded)"
write_stale_stub "$tree"
sed -i.bak 's/^version = .*/version = "99.99.99"/' "$tree/crates/qedgen/Cargo.toml"
out="$(run_install "$tree" PATH=/usr/bin:/bin)" && rc=0 || rc=$?
if [[ $rc -ne 0 ]]; then
  fail "D stranded: expected exit 0 with warning, got exit $rc"
elif ! grep -q "WARNING: could not refresh qedgen" <<<"$out"; then
  fail "D stranded: loud stale warning missing"
elif ! grep -q "NOT installed" <<<"$out"; then
  fail "D stranded: banner still claims success"
else
  pass "D stranded keeps stale binary with loud warning + honest banner"
fi

# ── E: quickstart with the installed binary ─────────────────────────────
qedgen="$fresh_tree/bin/qedgen"
proj="$scratch/proj"
mkdir -p "$proj"
cp "$repo_root/examples/rust/escrow/escrow.qedspec" "$proj/"
step() { # <label> <cmd...>
  local label="$1"
  shift
  local out rc=0
  out="$(cd "$proj" && "$@" 2>&1)" || rc=$?
  if [[ $rc -ne 0 ]]; then
    fail "E quickstart / $label: exit $rc"
    tail -5 <<<"$out" | sed 's/^/    /' >&2
    return 1
  fi
}
# NOTE on `check`: the installed binary is the pinned RELEASE, while the
# spec comes from HEAD — lint baselines legitimately skew between the two
# (the strict baseline is owned by the workspace journey tests and the
# release gate). Here `check` must run and produce its summary; exit 1
# from lint findings is acceptable, a crash / parse failure is not.
check_lenient() {
  local out rc=0
  out="$(cd "$proj" && "$qedgen" check 2>&1)" || rc=$?
  if [[ $rc -gt 1 ]] || ! grep -Eq "(error\(s\)|warning\(s\))," <<<"$out"; then
    fail "E quickstart / check: exit $rc without a lint summary"
    tail -5 <<<"$out" | sed 's/^/    /' >&2
    return 1
  fi
}
git -C "$proj" init -q . &&
  step "init" "$qedgen" init --name escrow --spec escrow.qedspec &&
  check_lenient &&
  step "codegen" "$qedgen" codegen --all &&
  {
    quickstart_ok=1
    for artifact in programs/src/lib.rs programs/tests/proptest.rs \
      formal_verification/Spec.lean; do
      if [[ ! -f "$proj/$artifact" ]]; then
        fail "E quickstart: missing $artifact"
        quickstart_ok=0
      fi
    done
    [[ $quickstart_ok -eq 1 ]] && pass "E quickstart clean with the installed binary"
  }

# ── Verdict ─────────────────────────────────────────────────────────────
if [[ $failures -gt 0 ]]; then
  echo "cold-start-smoke: $failures failure(s)" >&2
  exit 1
fi
echo "cold-start-smoke: all scenarios clean"
