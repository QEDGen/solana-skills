#!/usr/bin/env bash
set -euo pipefail

skill_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
schemas="$skill_root/schemas"
fixtures="$skill_root/test-fixtures/domain-artifacts"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required to validate auditor domain artifacts" >&2
  exit 2
fi

for schema in \
  "$schemas/domain-dossier.schema.json" \
  "$schemas/audit-run-manifest.schema.json" \
  "$schemas/spec-handoff.schema.json"; do
  jq -e '
    .["$schema"] == "https://json-schema.org/draft/2020-12/schema" and
    (.["$id"] | type == "string" and length > 0) and
    .type == "object" and
    (.required | type == "array" and length > 0)
  ' "$schema" >/dev/null
done

validate_dossier() {
  jq -e '
    def enum($values): . as $value | ($values | index($value)) != null;
    def stable_id: type == "string" and test("^[a-z][a-z0-9_-]*$");
    def structural_id: type == "string" and test("^[A-Za-z0-9][A-Za-z0-9_.:-]*$");
    def anchor:
      type == "object" and
      (.path | type == "string" and length > 0) and
      (.line_start | type == "number" and . >= 1);
    def metadata:
      type == "object" and
      (.confidence | enum(["literal", "derived", "semantic"])) and
      (.ratification | enum(["auto", "user", "rejected", "bug", "pending"])) and
      (if .ratification == "auto" then .confidence == "literal" else true end) and
      (if .ratification == "rejected" then (.rationale | type == "string" and length > 0) else true end) and
      (if .ratification == "bug" then (.rationale | type == "string" and length > 0) else true end) and
      (.source_anchors | type == "array" and length > 0 and all(.[]; anchor)) and
      (.verification_lanes | type == "array" and all(.[]; enum(["manual", "mollusk", "miri", "crucible"])));
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/domain-dossier-v1.schema.json" and
    (.audit_id | stable_id) and
    (.target | type == "object") and
    (.target.program_root | type == "string" and length > 0) and
    (.target.runtime | enum(["anchor", "pinocchio", "native-rust", "quasar", "qedgen-codegen", "sbpf", "unknown"])) and
    (.target.mode | enum(["spec-aware", "spec-less"])) and
    (.handlers | type == "array" and all(.[];
      (.name | type == "string" and length > 0) and
      ((.source_path == null) or (.source_path | type == "string" and length > 0)) and
      ((.accounts_type == null) or (.accounts_type | type == "string" and length > 0)) and
      (.args | type == "array" and all(.[];
        (.name | type == "string" and length > 0) and
        ((.qedspec_type == null) or (.qedspec_type | type == "string" and length > 0)))))) and
    (.structural_candidates | type == "array" and all(.[];
      (.id | structural_id) and
      (.kind | type == "string" and length > 0) and
      (.scope | type == "string" and length > 0) and
      (.summary | type == "string" and length > 0) and
      (.suggested_syntax | type == "string" and length > 0) and
      (.probe_confidence | enum(["high", "medium", "low"])) and
      (.ratification | enum(["pending", "user", "rejected", "bug"])) and
      (if (.ratification == "rejected" or .ratification == "bug") then (.rationale | type == "string" and length > 0) else true end))) and
    (.asset_flows | type == "array" and all(.[];
      (.id | stable_id) and
      (.handler | type == "string" and length > 0) and
      (.asset | type == "string" and length > 0) and
      (.source | type == "string" and length > 0) and
      (.destination | type == "string" and length > 0) and
      (.nominal_amount | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.quantities | type == "array" and all(.[];
      (.id | stable_id) and
      (.symbol | type == "string" and length > 0) and
      (.unit | type == "string" and length > 0) and
      (.scale | type == "string" and length > 0) and
      (.rounding | enum(["exact", "floor", "ceil", "nearest", "unknown"])) and
      (.metadata | metadata))) and
    (.paired_operations | type == "array" and all(.[];
      (.id | stable_id) and
      (.left_operation | type == "string" and length > 0) and
      (.right_operation | type == "string" and length > 0) and
      (.relationship | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.lifecycle_edges | type == "array" and all(.[];
      (.id | stable_id) and
      (.account | type == "string" and length > 0) and
      (.handler | type == "string" and length > 0) and
      (.from | type == "string" and length > 0) and
      (.to | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.authority_capabilities | type == "array" and all(.[];
      (.id | stable_id) and
      (.role | type == "string" and length > 0) and
      (.identity_anchor | type == "string" and length > 0) and
      (.handler | type == "string" and length > 0) and
      (.effects | type == "array" and length > 0) and
      (.metadata | metadata))) and
    (.economic_equations | type == "array" and all(.[];
      (.id | stable_id) and
      (.name | type == "string" and length > 0) and
      (.expression | type == "string" and length > 0) and
      (.scope | type == "array" and length > 0) and
      (.tolerance | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.external_assumptions | type == "array" and all(.[];
      (.id | stable_id) and
      (.kind | enum(["oracle", "token", "cpi", "clock", "keeper", "governance", "dependency", "other"])) and
      (.claim | type == "string" and length > 0) and
      (.failure_effect | type == "string" and length > 0) and
      (.metadata | metadata))) and
    ([.structural_candidates[].id, .asset_flows[].id, .quantities[].id, .paired_operations[].id, .lifecycle_edges[].id,
      .authority_capabilities[].id, .economic_equations[].id,
      .external_assumptions[].id] as $ids |
      ($ids | length) == ($ids | unique | length))
  ' "$1" >/dev/null
}

validate_manifest() {
  jq -e '
    def enum($values): . as $value | ($values | index($value)) != null;
    def stable_id: type == "string" and test("^[a-z][a-z0-9_-]*$");
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/audit-run-manifest-v1.schema.json" and
    (.audit_id | stable_id) and
    (.status | enum(["running", "completed", "build-blocked", "tooling-blocked", "policy-interfered"])) and
    (.target | type == "object") and
    (.target.program_root | type == "string" and length > 0) and
    (.target.mode | enum(["spec-aware", "spec-less"])) and
    (.lanes | type == "array" and length > 0 and all(.[];
      (.name | enum(["source-review", "ordinary-probe", "compile", "mollusk", "miri", "crucible-protocol", "crucible-skeleton", "crucible-domain"])) and
      (.status | enum(["not-run", "queued", "running", "passed", "failed", "blocked", "not-applicable"])) and
      (if .status == "blocked" then
        (.reason | type == "string" and length > 0) and
        (.resume_command | type == "string" and length > 0)
       else true end))) and
    (.artifacts | type == "object") and
    (.artifacts.domain_dossier_json | type == "string" and length > 0) and
    (.artifacts.domain_dossier_markdown | type == "string" and length > 0) and
    ((.artifacts.report == null) or (.artifacts.report | type == "string" and length > 0))
  ' "$1" >/dev/null
}

validate_handoff() {
  jq -e '
    def enum($values): . as $value | ($values | index($value)) != null;
    def clause:
      type == "object" and
      (.candidate_id | type == "string" and length > 0) and
      (.disposition | enum(["emitted", "needs_authoring", "language_gap", "excluded"])) and
      (.verification_lanes | type == "array" and all(.[];
        enum(["check", "manual", "mollusk", "miri", "crucible", "kani", "lean"])));
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/spec-handoff-v1.schema.json" and
    (.spec_path | type == "string" and length > 0) and
    (.layers | type == "object") and
    (.layers.structural | type == "array" and all(.[]; clause)) and
    (.layers.domain | type == "array" and all(.[]; clause)) and
    (.layers.regression | type == "array" and all(.[]; clause)) and
    (.language_gaps | type == "array" and all(.[];
      (.candidate_id | type == "string" and length > 0) and
      (.reason | type == "string" and length > 0) and
      .disposition == "document_or_extend_language"))
  ' "$1" >/dev/null
}

if [[ $# -gt 0 ]]; then
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dossier)
        [[ $# -ge 2 ]] || { echo "--dossier requires a path" >&2; exit 2; }
        validate_dossier "$2"
        shift 2
        ;;
      --manifest)
        [[ $# -ge 2 ]] || { echo "--manifest requires a path" >&2; exit 2; }
        validate_manifest "$2"
        shift 2
        ;;
      --handoff)
        [[ $# -ge 2 ]] || { echo "--handoff requires a path" >&2; exit 2; }
        validate_handoff "$2"
        shift 2
        ;;
      *)
        echo "usage: check-auditor-domain-artifacts.sh [--dossier <json>] [--manifest <json>] [--handoff <json>]" >&2
        exit 2
        ;;
    esac
  done
  echo "auditor domain artifacts valid"
  exit 0
fi

validate_dossier "$fixtures/valid-domain-dossier.json"
if validate_dossier "$fixtures/invalid-domain-dossier.json"; then
  echo "invalid domain dossier fixture unexpectedly passed" >&2
  exit 1
fi

validate_manifest "$fixtures/valid-audit-run-manifest.json"
validate_manifest "$fixtures/valid-probe-failure-manifest.json"
if validate_manifest "$fixtures/invalid-audit-run-manifest.json"; then
  echo "invalid audit run manifest fixture unexpectedly passed" >&2
  exit 1
fi

validate_handoff "$fixtures/valid-spec-handoff.json"
if validate_handoff "$fixtures/invalid-spec-handoff.json"; then
  echo "invalid specification handoff fixture unexpectedly passed" >&2
  exit 1
fi

echo "auditor domain artifact checks passed"
