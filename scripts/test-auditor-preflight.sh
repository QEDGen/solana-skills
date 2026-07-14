#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
preflight="$repo_root/skills/qedgen-auditor/scripts/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tmp="$(cd "$tmp" && pwd -P)"

mkdir -p "$tmp/native/src"
printf '%s\n' '[package]' 'name = "fixture"' 'version = "0.1.0"' '[dependencies]' 'solana-program = "2"' > "$tmp/native/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/native/src/lib.rs"
printf '%s\n' 'program Fixture {}' > "$tmp/native/fixture.qedspec"

output="$($preflight --root "$tmp/native" --qedgen "$repo_root/bin/qedgen")"
grep -q '^runtime=native-rust$' <<<"$output"
expected_version="$(tr -d '[:space:]' < "$repo_root/skills/qedgen-auditor/VERSION")"
grep -q "^skill_version=$expected_version\$" <<<"$output"
grep -q '^mode=spec-aware$' <<<"$output"
grep -q "^spec=$tmp/native/fixture.qedspec$" <<<"$output"
grep -q '^compile_status=not-run$' <<<"$output"

printf '%s\n' 'program Other {}' > "$tmp/native/other.qedspec"
if "$preflight" --root "$tmp/native" --qedgen "$repo_root/bin/qedgen" >/dev/null 2>&1; then
  echo "expected ambiguous specs to fail" >&2
  exit 1
fi

mkdir -p "$tmp/assembly/src"
printf '%s\n' '.text' > "$tmp/assembly/src/program.s"
output="$($preflight --root "$tmp/assembly" --qedgen "$repo_root/bin/qedgen")"
grep -q '^runtime=sbpf-assembly$' <<<"$output"
grep -q '^audit_capability=unsupported-source-audit$' <<<"$output"

mkdir -p "$tmp/mixed/src"
cp "$tmp/native/Cargo.toml" "$tmp/mixed/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/mixed/src/lib.rs"
printf '%s\n' '.text' > "$tmp/mixed/src/helper.s"
output="$($preflight --root "$tmp/mixed" --qedgen "$repo_root/bin/qedgen")"
grep -q '^runtime=native-rust$' <<<"$output"

if "$repo_root/scripts/sync-auditor-skill.sh" >/dev/null 2>&1; then
  echo "expected skill sync without an explicit destination to fail" >&2
  exit 1
fi
"$repo_root/scripts/sync-auditor-skill.sh" "$tmp/installed-skill" >/dev/null
QEDGEN_AUDITOR_INSTALLED_ROOT="$tmp/installed-skill" \
  "$repo_root/scripts/check-auditor-skill.sh" >/dev/null

echo "auditor preflight tests passed"
