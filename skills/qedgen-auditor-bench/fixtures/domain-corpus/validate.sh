#!/usr/bin/env bash
set -euo pipefail

fixture_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
artifact_validator="$fixture_root/../../../qedgen-auditor/scripts/check-domain-artifacts.sh"

for dossier in "$fixture_root"/*-dossier.json; do
  "$artifact_validator" --dossier "$dossier" >/dev/null
done

jq -e --arg root "$fixture_root" '
  .schema_version == 1 and
  (.entries | length == 7) and
  ([.entries[].domain] | unique | length == 7) and
  all(.entries[];
    (.dossier | type == "string" and endswith("-dossier.json")) and
    (.expected_candidate_categories | type == "array" and length > 0) and
    (.expected_units | type == "array" and length > 0) and
    (.expected_paired_operations | type == "array" and length > 0) and
    (.intended_language_gaps | type == "array" and length > 0))
' "$fixture_root/expectations.json" >/dev/null

while IFS= read -r expectation; do
  dossier="$(jq -r '.dossier' <<<"$expectation")"
  dossier_path="$fixture_root/$dossier"
  [[ -f "$dossier_path" ]] || {
    echo "missing corpus dossier: $dossier" >&2
    exit 1
  }

  jq -e --argjson expected "$expectation" '
    . as $dossier |
    all($expected.expected_candidate_categories[]; . as $category |
      ($dossier[$category] | type == "array" and length > 0)) and
    (($expected.expected_units - [$dossier.quantities[].unit]) | length == 0) and
    (($expected.expected_paired_operations - [
      $dossier.paired_operations[] | (.left_operation + "/" + .right_operation)
    ]) | length == 0)
  ' "$dossier_path" >/dev/null || {
    echo "corpus expectation does not match dossier: $dossier" >&2
    exit 1
  }
done < <(jq -c '.entries[]' "$fixture_root/expectations.json")

echo "auditor domain corpus fixtures valid"
