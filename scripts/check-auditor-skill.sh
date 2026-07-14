#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
skill_root="$repo_root/skills/qedgen-auditor"
bench_skill="$repo_root/skills/qedgen-auditor-bench/SKILL.md"
installed_root="${QEDGEN_AUDITOR_INSTALLED_ROOT:-}"
if [[ -z "$installed_root" && -d "$repo_root/.claude/skills/qedgen-auditor" ]]; then
  installed_root="$repo_root/.claude/skills/qedgen-auditor"
fi

fail=0

package_version="$(sed -nE 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' "$repo_root/package.json" | head -1)"
skill_version="$(tr -d '[:space:]' < "$skill_root/VERSION")"
if [[ -z "$package_version" || "$skill_version" != "$package_version" ]]; then
  echo "auditor skill version drift: skill=$skill_version package=$package_version" >&2
  fail=1
fi

line_count="$(wc -l < "$skill_root/SKILL.md" | tr -d ' ')"
if [[ "$line_count" -gt 500 ]]; then
  echo "SKILL.md is $line_count lines; route detail to references (maximum 500)" >&2
  fail=1
fi

if [[ ! -f "$bench_skill" ]] || ! grep -q '^name: qedgen-auditor-bench$' "$bench_skill"; then
  echo "portable auditor benchmark skill is missing or invalid" >&2
  fail=1
fi

if grep -En 'model:[[:space:]]*(opus|gpt-5\.5)|Default audit worker:.*GPT-5\.5' \
  "$bench_skill" >/dev/null; then
  echo "benchmark dispatcher uses a stale model default" >&2
  fail=1
fi

if [[ "$(sed -n '1p' "$skill_root/SKILL.md")" != "---" ]] ||
   ! sed -n '2,8p' "$skill_root/SKILL.md" | grep -q '^name: qedgen-auditor$' ||
   ! sed -n '2,8p' "$skill_root/SKILL.md" | grep -q '^description:'; then
  echo "invalid qedgen-auditor SKILL.md frontmatter" >&2
  fail=1
fi

if find "$skill_root" -type f \( -name '*.md' -o -name '*.sh' \) \
  ! -path '*/hooks/*' -print0 | \
  xargs -0 grep -En '\$HOME/\.(codex|claude)|CODEX_HOME|CLAUDE_HOME' >/dev/null; then
  echo "auditor skill contains a harness-specific home-directory assumption" >&2
  find "$skill_root" -type f \( -name '*.md' -o -name '*.sh' \) \
    ! -path '*/hooks/*' -print0 | \
    xargs -0 grep -En '\$HOME/\.(codex|claude)|CODEX_HOME|CLAUDE_HOME' >&2
  fail=1
fi

if find "$skill_root" -type f -name '*.md' ! -path '*/hooks/*' -print0 | \
  xargs -0 grep -En 'Default audit worker:.*GPT-5\.5|Use GPT-5\.5 as (the )?default|recommended default.*GPT-5\.5' >/dev/null; then
  echo "auditor skill still recommends GPT-5.5 as a default" >&2
  fail=1
fi

while IFS= read -r ref; do
  relative="${ref#references/}"
  if [[ ! -f "$skill_root/references/$relative" ]]; then
    echo "missing skill reference: $ref" >&2
    fail=1
  fi
done < <(grep -oE '\]\(references/[^)#]+' "$skill_root/SKILL.md" | cut -c3- | sort -u)

for required in \
  'domain-invariant-extraction.md' \
  'Always extract a domain dossier' \
  'Start spec-less protocol mode' \
  'Start skeleton mode' \
  'Re-run domain mode'; do
  if ! grep -Fq "$required" "$skill_root/SKILL.md"; then
    echo "canonical auditor workflow is missing required domain/fuzz guidance: $required" >&2
    fail=1
  fi
done

for required in \
  "$skill_root/schemas/domain-dossier.schema.json" \
  "$skill_root/schemas/audit-run-manifest.schema.json" \
  "$skill_root/schemas/spec-handoff.schema.json" \
  "$skill_root/schemas/domain-sequences.schema.json" \
  "$skill_root/schemas/domain-sequence-bindings.schema.json" \
  "$skill_root/schemas/resolved-domain-sequences.schema.json" \
  "$skill_root/schemas/account-binding-overlay.schema.json" \
  "$skill_root/schemas/domain-replay-report.schema.json" \
  "$skill_root/scripts/check-domain-artifacts.sh"; do
  if [[ ! -f "$required" ]]; then
    echo "auditor domain artifact schema is missing: $required" >&2
    fail=1
  fi
done

if ! "$repo_root/scripts/check-auditor-domain-artifacts.sh" >/dev/null; then
  echo "auditor domain artifact validation failed" >&2
  fail=1
fi

for required in \
  'Asset-flow graph' \
  'Quantity and unit table' \
  'Lifecycle graph' \
  'Authority-capability matrix' \
  'Economic equations' \
  'Probe-failure behavior'; do
  if ! grep -Fq "$required" "$skill_root/references/domain-invariant-extraction.md"; then
    echo "domain invariant reference is missing required section: $required" >&2
    fail=1
  fi
done

while IFS=: read -r source _ token; do
  ref="${token#']('}"
  ref="${ref%')'}"
  if [[ ! -f "$(dirname "$source")/$ref" ]]; then
    echo "broken local markdown reference: $source -> $ref" >&2
    fail=1
  fi
done < <(find "$skill_root" -type f -name '*.md' -print0 | \
  xargs -0 grep -HnEo '\]\([^):#]+\.md\)' || true)

for field in 'probe_repros' 'emit_spec_candidates' 'bootstrap' 'fuzz' 'frozen'; do
  if ! grep -Eq "^[[:space:]]*${field}:" "$repo_root/crates/qedgen/src/cli.rs"; then
    echo "documented CLI flag is not a clap field in cli.rs: --${field//_/-}" >&2
    fail=1
  fi
done

if [[ -n "$installed_root" ]]; then
  if [[ ! -d "$installed_root" ]]; then
    echo "installed skill directory does not exist: $installed_root" >&2
    fail=1
  elif ! diff -qr --exclude=SOURCE_COMMIT "$skill_root" "$installed_root" >/dev/null; then
    echo "installed qedgen-auditor skill differs from repository canonical copy" >&2
    diff -qr --exclude=SOURCE_COMMIT "$skill_root" "$installed_root" >&2 || true
    echo "re-sync with: bash scripts/sync-auditor-skill.sh $installed_root" >&2
    fail=1
  fi
fi

if [[ $fail -ne 0 ]]; then
  exit 1
fi
echo "auditor skill checks passed"
