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

# --- category identity: an unallowlisted catalog entry is rejected ---
cp "$repo_root/crates/qedgen/src/probe/mod.rs" "$tmp/probe-mod.rs"
cp "$repo_root/skills/qedgen-auditor/references/category-catalog.md" \
  "$tmp/category-catalog.md"
printf '%s\n' '' '### `category_that_does_not_exist` — HIGH' \
  'Synthetic drift fixture.' >> "$tmp/category-catalog.md"
if err="$(QEDGEN_CATEGORY_RUST="$tmp/probe-mod.rs" \
  QEDGEN_CATEGORY_CATALOG="$tmp/category-catalog.md" \
  "$repo_root/scripts/check-category-catalog.sh" 2>&1)"; then
  echo "expected orphan catalog category to fail identity reconciliation" >&2
  exit 1
fi
grep -q 'category identity drift' <<<"$err"
grep -q 'category_that_does_not_exist' <<<"$err"

# --- category identity: an unallowlisted Rust tag is rejected too ---
# The catalog direction above cannot catch an awk regression in the Rust
# extractor, so drive the other half of the comparison as well.
awk '
  injected || !/Category::ArbitraryCpi => "arbitrary_cpi",/ { print; next }
  {
    print
    print "            Category::TagThatHasNoCatalogEntry => \"tag_that_has_no_catalog_entry\","
    injected = 1
  }
' "$repo_root/crates/qedgen/src/probe/mod.rs" > "$tmp/probe-mod-rust-orphan.rs"
grep -q 'tag_that_has_no_catalog_entry' "$tmp/probe-mod-rust-orphan.rs" ||
  { echo "preflight fixture failed to inject a Rust tag" >&2; exit 1; }
if err="$(QEDGEN_CATEGORY_RUST="$tmp/probe-mod-rust-orphan.rs" \
  QEDGEN_CATEGORY_CATALOG="$repo_root/skills/qedgen-auditor/references/category-catalog.md" \
  "$repo_root/scripts/check-category-catalog.sh" 2>&1)"; then
  echo "expected orphan Rust category to fail identity reconciliation" >&2
  exit 1
fi
grep -q 'Rust categories missing from catalog/allowlist' <<<"$err"
grep -q 'tag_that_has_no_catalog_entry' <<<"$err"

# --- benchmark schemas: every machine-readable contract is required ---
cp -R "$repo_root/skills/qedgen-auditor-bench" "$tmp/bench-missing-schema"
rm -f "$tmp/bench-missing-schema/schemas/corpus-manifest.schema.json"
if QEDGEN_AUDITOR_BENCH_ROOT="$tmp/bench-missing-schema" \
  "$repo_root/scripts/check-auditor-skill.sh" >/dev/null 2>&1; then
  echo "expected missing benchmark corpus schema to fail the check" >&2
  exit 1
fi

# --- benchmark scoring: mixed tiers cannot be hidden in one headline ---
cp -R "$repo_root/skills/qedgen-auditor-bench" "$tmp/bench-missing-tier-rule"
sed -i.bak \
  '/MUST NOT collapse mixed difficulty tiers into one headline score/d' \
  "$tmp/bench-missing-tier-rule/SKILL.md"
if QEDGEN_AUDITOR_BENCH_ROOT="$tmp/bench-missing-tier-rule" \
  "$repo_root/scripts/check-auditor-skill.sh" >/dev/null 2>&1; then
  echo "expected missing benchmark tier rule to fail the check" >&2
  exit 1
fi

# --- benchmark fixtures: difficulty and comparison composition are validated ---
cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-missing-difficulty"
jq 'del(.entries[0].difficulty)' \
  "$tmp/bench-missing-difficulty/corpus-manifest.json" \
  > "$tmp/corpus-manifest-without-difficulty.json"
mv "$tmp/corpus-manifest-without-difficulty.json" \
  "$tmp/bench-missing-difficulty/corpus-manifest.json"
if err="$(QEDGEN_BENCH_FIXTURE_ROOT="$tmp/bench-missing-difficulty" \
  "$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" 2>&1)"; then
  echo "expected missing benchmark difficulty to fail validation" >&2
  exit 1
fi
grep -q 'required difficulty' <<<"$err"

cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-mismatched-composition"
jq '.comparison.candidate_tier_entry_counts.smoke = 0' \
  "$tmp/bench-mismatched-composition/score.json" \
  > "$tmp/score-with-mismatched-composition.json"
mv "$tmp/score-with-mismatched-composition.json" \
  "$tmp/bench-mismatched-composition/score.json"
if err="$(QEDGEN_BENCH_FIXTURE_ROOT="$tmp/bench-mismatched-composition" \
  "$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" 2>&1)"; then
  echo "expected mismatched benchmark composition to fail validation" >&2
  exit 1
fi
grep -q 'composition_valid must equal per-tier count equality' <<<"$err"

cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-invalid-command-type"
jq '.entries[0].setup_commands = [true]' \
  "$tmp/bench-invalid-command-type/corpus-manifest.json" \
  > "$tmp/corpus-manifest-with-invalid-command.json"
mv "$tmp/corpus-manifest-with-invalid-command.json" \
  "$tmp/bench-invalid-command-type/corpus-manifest.json"
if QEDGEN_BENCH_FIXTURE_ROOT="$tmp/bench-invalid-command-type" \
  "$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" >/dev/null 2>&1; then
  echo "expected non-string benchmark command to fail validation" >&2
  exit 1
fi

cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-null-domain-expectations"
jq '.entries[0].domain_expectations = null' \
  "$tmp/bench-null-domain-expectations/corpus-manifest.json" \
  > "$tmp/corpus-manifest-with-null-domain.json"
mv "$tmp/corpus-manifest-with-null-domain.json" \
  "$tmp/bench-null-domain-expectations/corpus-manifest.json"
if QEDGEN_BENCH_FIXTURE_ROOT="$tmp/bench-null-domain-expectations" \
  "$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" >/dev/null 2>&1; then
  echo "expected null domain expectations to fail validation" >&2
  exit 1
fi

cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-null-aggregate"
jq '.aggregate = null' "$tmp/bench-null-aggregate/score.json" \
  > "$tmp/score-with-null-aggregate.json"
mv "$tmp/score-with-null-aggregate.json" "$tmp/bench-null-aggregate/score.json"
if QEDGEN_BENCH_FIXTURE_ROOT="$tmp/bench-null-aggregate" \
  "$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" >/dev/null 2>&1; then
  echo "expected null score aggregate to fail validation" >&2
  exit 1
fi

# --- schema drift: a keyword the evaluator cannot enforce is rejected ---
# The schemas are the single source of truth only while `jsonschema.jq`
# implements every keyword they use. A schema that grows an unimplemented
# keyword must fail loudly instead of leaving that constraint unenforced.
cp -R "$repo_root/skills/qedgen-auditor-bench" "$tmp/bench-unsupported-keyword"
jq '.properties.entries.maxItems = 3' \
  "$tmp/bench-unsupported-keyword/schemas/corpus-manifest.schema.json" \
  > "$tmp/corpus-manifest-schema-with-maxitems.json"
mv "$tmp/corpus-manifest-schema-with-maxitems.json" \
  "$tmp/bench-unsupported-keyword/schemas/corpus-manifest.schema.json"
if err="$("$tmp/bench-unsupported-keyword/schemas/validate.sh" 2>&1)"; then
  echo "expected an unimplemented schema keyword to fail validation" >&2
  exit 1
fi
grep -q 'does not implement: maxItems' <<<"$err"

# --- schema is the contract: a structural violation is caught by the schema ---
cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-unexpected-property"
jq '.entries[0].undeclared_field = "x"' \
  "$tmp/bench-unexpected-property/corpus-manifest.json" \
  > "$tmp/corpus-manifest-with-unexpected-property.json"
mv "$tmp/corpus-manifest-with-unexpected-property.json" \
  "$tmp/bench-unexpected-property/corpus-manifest.json"
if err="$("$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" \
  "$tmp/bench-unexpected-property" 2>&1)"; then
  echo "expected an undeclared manifest property to fail validation" >&2
  exit 1
fi
grep -q 'unexpected property undeclared_field' <<<"$err"

# --- the validator runs against a real output directory, not just fixtures ---
cp -R "$repo_root/skills/qedgen-auditor-bench/fixtures/synthetic" \
  "$tmp/bench-run-output"
"$repo_root/skills/qedgen-auditor-bench/schemas/validate.sh" \
  "$tmp/bench-run-output" >/dev/null ||
  { echo "expected a positional output directory to validate" >&2; exit 1; }

# --- knowledge bases: every catalog entry and primer signal is attributable ---
knowledge_check="$repo_root/skills/qedgen-auditor/scripts/check-knowledge-bases.sh"
catalog="$repo_root/skills/qedgen-auditor/references/category-catalog.md"
primer="$repo_root/docs/security-primer.md"
allowlist="$repo_root/skills/qedgen-auditor/references/basis-legacy-allowlist.txt"

awk 'removed || !/^Basis:/ { print; next } { removed = 1 }' "$catalog" \
  > "$tmp/catalog-missing-basis.md"
if err="$(QEDGEN_CATEGORY_CATALOG="$tmp/catalog-missing-basis.md" \
  QEDGEN_SECURITY_PRIMER="$primer" QEDGEN_BASIS_ALLOWLIST="$allowlist" \
  "$knowledge_check" 2>&1)"; then
  echo "expected missing category basis to fail validation" >&2
  exit 1
fi
grep -q 'catalog entry missing Basis:' <<<"$err"

awk '
  replaced || !/^Basis: fixture:/ { print; next }
  { print "Basis: fixture:crates/qedgen/tests/fixtures/does-not-exist"; replaced = 1 }
' "$catalog" > "$tmp/catalog-bad-fixture.md"
if err="$(QEDGEN_CATEGORY_CATALOG="$tmp/catalog-bad-fixture.md" \
  QEDGEN_SECURITY_PRIMER="$primer" QEDGEN_BASIS_ALLOWLIST="$allowlist" \
  "$knowledge_check" 2>&1)"; then
  echo "expected nonexistent category fixture basis to fail validation" >&2
  exit 1
fi
grep -q 'fixture basis does not exist' <<<"$err"

cp "$catalog" "$tmp/catalog-unallowlisted-prose.md"
printf '%s\n' '' '### `unknown_prose_category` — HIGH' \
  'Basis: prose:synthetic unsupported provenance' \
  'Synthetic preflight entry.' >> "$tmp/catalog-unallowlisted-prose.md"
if err="$(QEDGEN_CATEGORY_CATALOG="$tmp/catalog-unallowlisted-prose.md" \
  QEDGEN_SECURITY_PRIMER="$primer" QEDGEN_BASIS_ALLOWLIST="$allowlist" \
  "$knowledge_check" 2>&1)"; then
  echo "expected unallowlisted prose basis to fail validation" >&2
  exit 1
fi
grep -q 'prose basis is not allowlisted: unknown_prose_category' <<<"$err"

awk 'removed || !/^\*\*Basis:\*\*/ { print; next } { removed = 1 }' "$primer" \
  > "$tmp/primer-missing-basis.md"
if err="$(QEDGEN_CATEGORY_CATALOG="$catalog" \
  QEDGEN_SECURITY_PRIMER="$tmp/primer-missing-basis.md" \
  QEDGEN_BASIS_ALLOWLIST="$allowlist" "$knowledge_check" 2>&1)"; then
  echo "expected missing primer grep basis to fail validation" >&2
  exit 1
fi
grep -q 'primer Grep for block missing Basis:' <<<"$err"

awk '
  replaced || !/^\*\*Basis:\*\*/ { print; next }
  { print "**Basis:** corpus:unregistered-incident"; replaced = 1 }
' "$primer" > "$tmp/primer-unregistered-corpus.md"
if err="$(QEDGEN_CATEGORY_CATALOG="$catalog" \
  QEDGEN_SECURITY_PRIMER="$tmp/primer-unregistered-corpus.md" \
  QEDGEN_BASIS_ALLOWLIST="$allowlist" "$knowledge_check" 2>&1)"; then
  echo "expected unregistered primer corpus basis to fail validation" >&2
  exit 1
fi
grep -q 'corpus basis is not registered' <<<"$err"

echo "auditor preflight tests passed"
