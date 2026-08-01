#!/usr/bin/env bash
set -euo pipefail

# Validate a benchmark run's three machine-readable artifacts.
#
# Usage:
#   validate.sh [<directory>]
#
# The directory must contain `corpus-manifest.json`, `normalized-report.json`,
# and `score.json`. It defaults to the checked-in synthetic fixtures, so the
# repository gate has something to run against, but the point of the script is
# to be pointed at a real run's output directory.
#
# Structure is checked against the `.schema.json` files by `jsonschema.jq`.
# Only rules JSON Schema cannot express live here: referential integrity
# between the three documents, and the tier-composition arithmetic that keeps
# a mixed-difficulty comparison from being read as a regression.

schemas_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
bench_root="$(cd "$schemas_dir/.." && pwd -P)"
fixture_root="${1:-${QEDGEN_BENCH_FIXTURE_ROOT:-$bench_root/fixtures/synthetic}}"

manifest="$fixture_root/corpus-manifest.json"
report="$fixture_root/normalized-report.json"
score="$fixture_root/score.json"

fail() {
  echo "benchmark schema validation failed: $*" >&2
  exit 1
}

command -v jq >/dev/null || fail "jq is required"

for file in "$manifest" "$report" "$score"; do
  [[ -f "$file" ]] || fail "missing artifact: $file"
  jq -e . "$file" >/dev/null || fail "invalid JSON: $file"
done

# --- structure: the schemas are the contract ---

conform() {
  local doc="$1" schema="$2" label="$3" violations
  violations="$(jq -r --slurpfile schema "$schemas_dir/$schema" \
    -f "$schemas_dir/jsonschema.jq" "$doc")"
  if [[ -n "$violations" ]]; then
    echo "benchmark schema validation failed: $label does not conform to $schema" >&2
    sed 's/^/  /' <<<"$violations" >&2
    exit 1
  fi
}

# The manifest message names `difficulty` explicitly: a corpus entry without a
# tier is the failure this contract exists to prevent, and it should be
# greppable in CI output rather than buried in a generic conformance error.
conform "$manifest" corpus-manifest.schema.json \
  "corpus manifest (every entry needs a required difficulty)"
conform "$report" normalized-report.schema.json "normalized report"
conform "$score" score.schema.json "score"

# --- referential integrity across documents ---

jq -e '[.entries[].id] | length == (unique | length)' "$manifest" >/dev/null ||
  fail "corpus manifest repeats an entry id"

entry_id="$(jq -r '.corpus_entry_id' "$report")"
jq -e --arg id "$entry_id" 'any(.entries[]; .id == $id)' "$manifest" >/dev/null ||
  fail "normalized report references unknown corpus entry: $entry_id"

while IFS= read -r id; do
  jq -e --arg id "$id" 'any(.entries[]; .id == $id)' "$manifest" >/dev/null ||
    fail "score references unknown corpus entry: $id"
done < <(jq -r '.corpus_entry_ids[]' "$score")

# --- tier composition ---

expected_counts="$(jq --argjson ids "$(jq '.corpus_entry_ids' "$score")" '
  reduce (.entries[] | select(.id as $id | $ids | index($id))) as $entry
    ({smoke: 0, standard: 0, hard: 0, adversarial: 0};
     .[$entry.difficulty] += 1)
' "$manifest")"
actual_counts="$(jq -c '.tier_entry_counts' "$score")"
if [[ "$(jq -c . <<<"$expected_counts")" != "$actual_counts" ]]; then
  fail "tier_entry_counts do not match the referenced corpus entries"
fi

# Every reported tier's entry count must agree with the headline tier counts,
# and no tier with entries may be silently omitted from `per_difficulty`.
jq -e '
  . as $score
  | all(.per_difficulty | to_entries[];
        .value.entry_count == $score.tier_entry_counts[.key])
    and all($score.tier_entry_counts | to_entries[];
            .key as $tier
            | .value == 0 or ($score.per_difficulty | has($tier)))
' "$score" >/dev/null ||
  fail "per_difficulty entry counts disagree with tier_entry_counts"

if jq -e 'has("comparison")' "$score" >/dev/null; then
  same="$(jq -r '
    .comparison.baseline_tier_entry_counts ==
    .comparison.candidate_tier_entry_counts
  ' "$score")"
  valid="$(jq -r '.comparison.composition_valid' "$score")"
  if [[ "$same" != "$valid" ]]; then
    fail "comparison composition_valid must equal per-tier count equality"
  fi
fi

echo "auditor benchmark schemas valid"
