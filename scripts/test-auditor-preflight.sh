#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
preflight="$repo_root/skills/qedgen-auditor/scripts/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tmp="$(cd "$tmp" && pwd -P)"

expected_version="$(tr -d '[:space:]' < "$repo_root/skills/qedgen-auditor/VERSION")"

# Stub qedgen so status/capability assertions don't depend on a local build.
printf '%s\n' '#!/bin/sh' 'echo "qedgen 2.42.0"' > "$tmp/qedgen-stub"
chmod +x "$tmp/qedgen-stub"

write_native_manifest() {
  printf '%s\n' '[package]' 'name = "fixture"' 'version = "0.1.0"' \
    '[dependencies]' 'solana-program = "2"' > "$1"
}

# --- single crate: root spec, qed.toml, ready qedgen ---
mkdir -p "$tmp/native/src"
write_native_manifest "$tmp/native/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/native/src/lib.rs"
printf '%s\n' 'program Fixture {}' > "$tmp/native/fixture.qedspec"
printf '%s\n' '[dependencies]' > "$tmp/native/qed.toml"

output="$("$preflight" --root "$tmp/native" --qedgen "$tmp/qedgen-stub")"
grep -q '^runtime=native-rust$' <<<"$output"
grep -q "^skill_version=$expected_version\$" <<<"$output"
grep -Eq '^skill_commit=[0-9a-f]{40}$|^skill_commit=unknown$' <<<"$output"
grep -q '^mode=spec-aware$' <<<"$output"
grep -q "^program_root=$tmp/native\$" <<<"$output"
grep -q "^spec=$tmp/native/fixture.qedspec\$" <<<"$output"
grep -q "^qed_manifest=$tmp/native/qed.toml\$" <<<"$output"
grep -q '^qedgen_status=ready$' <<<"$output"
grep -q '^audit_capability=full$' <<<"$output"
grep -q '^compile_status=not-run$' <<<"$output"

# --- explicit --spec (relative to root) wins without discovery ---
output="$("$preflight" --root "$tmp/native" --spec fixture.qedspec --qedgen "$tmp/qedgen-stub")"
grep -q "^spec=$tmp/native/fixture.qedspec\$" <<<"$output"

# --- ambiguous specs are rejected ---
printf '%s\n' 'program Other {}' > "$tmp/native/other.qedspec"
if err="$("$preflight" --root "$tmp/native" --qedgen "$tmp/qedgen-stub" 2>&1)"; then
  echo "expected ambiguous specs to fail" >&2
  exit 1
fi
grep -q 'multiple .qedspec' <<<"$err"
rm "$tmp/native/other.qedspec"

# --- nested spec is discovered ---
mkdir -p "$tmp/nested/src" "$tmp/nested/specs"
write_native_manifest "$tmp/nested/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/nested/src/lib.rs"
printf '%s\n' 'program Nested {}' > "$tmp/nested/specs/nested.qedspec"
output="$("$preflight" --root "$tmp/nested" --qedgen "$tmp/qedgen-stub")"
grep -q '^mode=spec-aware$' <<<"$output"
grep -q "^spec=$tmp/nested/specs/nested.qedspec\$" <<<"$output"

# --- spec-less target stays full-capability when the runtime is known ---
mkdir -p "$tmp/specless/src"
write_native_manifest "$tmp/specless/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/specless/src/lib.rs"
output="$("$preflight" --root "$tmp/specless" --qedgen "$tmp/qedgen-stub")"
grep -q '^mode=spec-less$' <<<"$output"
grep -q '^spec=none$' <<<"$output"
grep -q '^audit_capability=full$' <<<"$output"

# --- unknown runtime downgrades capability even with a ready qedgen ---
mkdir -p "$tmp/unknown"
printf '%s\n' 'not a program' > "$tmp/unknown/README.md"
output="$("$preflight" --root "$tmp/unknown" --qedgen "$tmp/qedgen-stub")"
grep -q '^runtime=unknown$' <<<"$output"
grep -q '^audit_capability=read-only$' <<<"$output"

# --- monorepo: a single program member is auto-selected; a
# --- workspace-dep mention at the root is not a program manifest ---
mkdir -p "$tmp/mono1/prog/src" "$tmp/mono1/tools/src"
printf '%s\n' '[workspace]' 'members = ["prog", "tools"]' \
  '[workspace.dependencies]' 'solana-program = "2"' > "$tmp/mono1/Cargo.toml"
write_native_manifest "$tmp/mono1/prog/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/mono1/prog/src/lib.rs"
printf '%s\n' '[package]' 'name = "tools"' 'version = "0.1.0"' > "$tmp/mono1/tools/Cargo.toml"
printf '%s\n' 'fn main() {}' > "$tmp/mono1/tools/src/main.rs"
output="$("$preflight" --root "$tmp/mono1" --qedgen "$tmp/qedgen-stub")"
grep -q '^runtime=native-rust$' <<<"$output"
grep -q "^program_root=$tmp/mono1/prog\$" <<<"$output"

# --- monorepo: several program members are rejected as ambiguous;
# --- an explicit member root resolves it ---
mkdir -p "$tmp/mono2/prog-a/src" "$tmp/mono2/prog-b/src"
printf '%s\n' '[workspace]' 'members = ["prog-a", "prog-b"]' > "$tmp/mono2/Cargo.toml"
write_native_manifest "$tmp/mono2/prog-a/Cargo.toml"
write_native_manifest "$tmp/mono2/prog-b/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/mono2/prog-a/src/lib.rs"
printf '%s\n' 'pub fn handler() {}' > "$tmp/mono2/prog-b/src/lib.rs"
if err="$("$preflight" --root "$tmp/mono2" --qedgen "$tmp/qedgen-stub" 2>&1)"; then
  echo "expected ambiguous program crates to fail" >&2
  exit 1
fi
grep -q 'multiple program crates' <<<"$err"
output="$("$preflight" --root "$tmp/mono2/prog-a" --qedgen "$tmp/qedgen-stub")"
grep -q '^runtime=native-rust$' <<<"$output"
grep -q "^program_root=$tmp/mono2/prog-a\$" <<<"$output"

# --- assembly-only target ---
mkdir -p "$tmp/assembly/src"
printf '%s\n' '.text' > "$tmp/assembly/src/program.s"
output="$("$preflight" --root "$tmp/assembly" --qedgen "$tmp/qedgen-stub")"
grep -q '^runtime=sbpf-assembly$' <<<"$output"
grep -q '^audit_capability=unsupported-source-audit$' <<<"$output"

# --- helper assembly does not flip a Rust target ---
mkdir -p "$tmp/mixed/src"
write_native_manifest "$tmp/mixed/Cargo.toml"
printf '%s\n' 'pub fn handler() {}' > "$tmp/mixed/src/lib.rs"
printf '%s\n' '.text' > "$tmp/mixed/src/helper.s"
output="$("$preflight" --root "$tmp/mixed" --qedgen "$tmp/qedgen-stub")"
grep -q '^runtime=native-rust$' <<<"$output"

# --- sync + installed-copy drift check ---
if "$repo_root/scripts/sync-auditor-skill.sh" >/dev/null 2>&1; then
  echo "expected skill sync without an explicit destination to fail" >&2
  exit 1
fi
"$repo_root/scripts/sync-auditor-skill.sh" "$tmp/installed-skill" >/dev/null
grep -Eq '^[0-9a-f]{40}$' "$tmp/installed-skill/SOURCE_COMMIT"
QEDGEN_AUDITOR_INSTALLED_ROOT="$tmp/installed-skill" \
  "$repo_root/scripts/check-auditor-skill.sh" >/dev/null

# --- a drifted installed copy is caught ---
printf '%s\n' 'drift' >> "$tmp/installed-skill/SKILL.md"
if QEDGEN_AUDITOR_INSTALLED_ROOT="$tmp/installed-skill" \
  "$repo_root/scripts/check-auditor-skill.sh" >/dev/null 2>&1; then
  echo "expected drifted installed skill to fail the check" >&2
  exit 1
fi

echo "auditor preflight tests passed"
