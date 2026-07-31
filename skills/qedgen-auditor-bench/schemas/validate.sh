#!/usr/bin/env bash
set -euo pipefail

bench_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fixture_root="${QEDGEN_BENCH_FIXTURE_ROOT:-$bench_root/fixtures/synthetic}"

manifest="$fixture_root/corpus-manifest.json"
report="$fixture_root/normalized-report.json"
score="$fixture_root/score.json"

fail() {
  echo "benchmark schema validation failed: $*" >&2
  exit 1
}

for file in "$manifest" "$report" "$score"; do
  [[ -f "$file" ]] || fail "missing fixture: $file"
  jq -e . "$file" >/dev/null || fail "invalid JSON: $file"
done

jq -e '
  .schema_version == 1 and
  .schema_uri == "https://qedgen.dev/schemas/auditor-bench/corpus-manifest-v1.schema.json" and
  ((keys_unsorted | sort) == ["entries", "schema_uri", "schema_version"]) and
  (.entries | type == "array" and length > 0) and
  (all(.entries[];
    ((keys_unsorted - [
      "id", "difficulty", "repository", "audited_commit", "program_root",
      "runtime", "setup_commands", "test_commands", "sanitization_rules",
      "labeled_findings", "domain_expectations"
    ]) | length == 0) and
    (.id | type == "string" and length > 0) and
    (.difficulty | IN("smoke", "standard", "hard", "adversarial")) and
    (.repository | type == "string" and length > 0) and
    (.audited_commit | test("^[0-9a-fA-F]{7,64}$")) and
    (.program_root | type == "string" and length > 0) and
    (.runtime | IN("anchor", "pinocchio", "quasar", "native-rust", "sbpf-assembly")) and
    (.setup_commands | type == "array") and
    (.test_commands | type == "array" and length > 0) and
    (.sanitization_rules | type == "array" and length > 0) and
    (.labeled_findings | type == "array") and
    (all(.labeled_findings[];
      ((keys_unsorted | sort) == ["category", "id", "location", "root_cause", "severity"]) and
      (all(.id, .category, .location, .root_cause;
        type == "string" and length > 0)) and
      (.severity | IN("critical", "high", "medium", "low", "info"))
    )) and
    ((.domain_expectations // null) == null or
      (((.domain_expectations | keys_unsorted) - [
        "units", "equations", "lifecycle", "authorities", "external_assumptions"
      ] | length == 0) and
       all(.domain_expectations[]; type == "array")))
  )) and
  ([.entries[].id] | length == (unique | length))
' "$manifest" >/dev/null || fail "corpus manifest violates its contract (including required difficulty)"

jq -e '
  .schema_version == 1 and
  .schema_uri == "https://qedgen.dev/schemas/auditor-bench/normalized-report-v1.schema.json" and
  ((keys_unsorted | sort) == [
    "corpus_entry_id", "findings", "run_id", "schema_uri", "schema_version"
  ]) and
  (.corpus_entry_id | type == "string" and length > 0) and
  (.run_id | type == "string" and length > 0) and
  (.findings | type == "array") and
  (all(.findings[];
    ((keys_unsorted | sort) == [
      "category", "evidence", "id", "location", "repro_status", "root_cause",
      "severity", "title"
    ]) and
    (all(.id, .category, .location, .root_cause, .title;
      type == "string" and length > 0)) and
    (.severity | IN("critical", "high", "medium", "low", "info")) and
    (.evidence | IN("confirmed", "structural", "hypothesis", "rejected")) and
    (.repro_status | IN("fired", "inconclusive", "silent", "not-required"))
  ))
' "$report" >/dev/null || fail "normalized report violates its contract"

entry_id="$(jq -r '.corpus_entry_id' "$report")"
jq -e --arg id "$entry_id" 'any(.entries[]; .id == $id)' "$manifest" >/dev/null ||
  fail "normalized report references unknown corpus entry: $entry_id"

jq -e '
  def counts:
    type == "object" and
    ((keys_unsorted | sort) == ["adversarial", "hard", "smoke", "standard"]) and
    all(.[]; type == "number" and floor == . and . >= 0);
  def metrics:
    type == "object" and
    ((keys_unsorted - [
      "entry_count", "ground_truth_count", "reported_count", "true_positives",
      "recall", "precision"
    ]) | length == 0) and
    all(.entry_count, .ground_truth_count, .reported_count, .true_positives;
      type == "number" and floor == . and . >= 0) and
    all(.recall, .precision; type == "number" and . >= 0 and . <= 1);
  .schema_version == 1 and
  .schema_uri == "https://qedgen.dev/schemas/auditor-bench/score-v1.schema.json" and
  ((keys_unsorted - [
    "schema_version", "schema_uri", "corpus_entry_ids", "tier_entry_counts",
    "per_difficulty", "aggregate", "comparison"
  ]) | length == 0) and
  (.corpus_entry_ids | type == "array" and length > 0 and length == (unique | length)) and
  (.tier_entry_counts | counts) and
  (.per_difficulty | type == "object" and length > 0) and
  (all(.per_difficulty | keys[];
    IN("smoke", "standard", "hard", "adversarial"))) and
  (all(.per_difficulty[]; metrics)) and
  (all(.per_difficulty | to_entries[];
    .value.entry_count == (.key as $tier | $ARGS.named.counts[$tier]))) and
  (all($ARGS.named.counts | to_entries[];
    .value == 0 or ($ARGS.named.per_difficulty[.key] | metrics))) and
  ((.aggregate // null) == null or (.aggregate | metrics)) and
  ((.comparison // null) == null or
    ((.comparison.kind | IN("skill-regression", "model-regression")) and
     (.comparison.baseline_tier_entry_counts | counts) and
     (.comparison.candidate_tier_entry_counts | counts) and
     (.comparison.composition_valid | type == "boolean")))
' --argjson counts "$(jq '.tier_entry_counts' "$score")" \
  --argjson per_difficulty "$(jq '.per_difficulty' "$score")" \
  "$score" >/dev/null ||
  fail "score violates its per-difficulty contract"

while IFS= read -r id; do
  jq -e --arg id "$id" 'any(.entries[]; .id == $id)' "$manifest" >/dev/null ||
    fail "score references unknown corpus entry: $id"
done < <(jq -r '.corpus_entry_ids[]' "$score")

expected_counts="$(jq --argjson ids "$(jq '.corpus_entry_ids' "$score")" '
  reduce (.entries[] | select(.id as $id | $ids | index($id))) as $entry
    ({smoke: 0, standard: 0, hard: 0, adversarial: 0};
     .[$entry.difficulty] += 1)
' "$manifest")"
actual_counts="$(jq -c '.tier_entry_counts' "$score")"
if [[ "$(jq -c . <<<"$expected_counts")" != "$actual_counts" ]]; then
  fail "tier_entry_counts do not match the referenced corpus entries"
fi

if jq -e '.comparison != null' "$score" >/dev/null; then
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
