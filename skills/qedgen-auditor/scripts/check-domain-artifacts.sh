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
  "$schemas/domain-sequences.schema.json" \
  "$schemas/domain-sequence-bindings.schema.json" \
  "$schemas/resolved-domain-sequences.schema.json" \
  "$schemas/account-binding-overlay.schema.json" \
  "$schemas/domain-replay-report.schema.json" \
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
    def obj: type == "object";
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
    (.handlers | type == "array" and all(.[]; obj and
      (.name | type == "string" and length > 0) and
      ((.source_path == null) or (.source_path | type == "string" and length > 0)) and
      ((.accounts_type == null) or (.accounts_type | type == "string" and length > 0)) and
      (.args | type == "array" and all(.[]; obj and
        (.name | type == "string" and length > 0) and
        ((.qedspec_type == null) or (.qedspec_type | type == "string" and length > 0)))))) and
    (.structural_candidates | type == "array" and all(.[]; obj and
      (.id | structural_id) and
      (.kind | type == "string" and length > 0) and
      (.scope | type == "string" and length > 0) and
      (.summary | type == "string" and length > 0) and
      (.suggested_syntax | type == "string" and length > 0) and
      (.probe_confidence | enum(["high", "medium", "low"])) and
      (.ratification | enum(["pending", "user", "rejected", "bug"])) and
      (if (.ratification == "rejected" or .ratification == "bug") then (.rationale | type == "string" and length > 0) else true end))) and
    (.asset_flows | type == "array" and all(.[]; obj and
      (.id | stable_id) and
      (.handler | type == "string" and length > 0) and
      (.asset | type == "string" and length > 0) and
      (.source | type == "string" and length > 0) and
      (.destination | type == "string" and length > 0) and
      (.nominal_amount | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.quantities | type == "array" and all(.[]; obj and
      (.id | stable_id) and
      (.symbol | type == "string" and length > 0) and
      (.unit | type == "string" and length > 0) and
      (.scale | type == "string" and length > 0) and
      (.rounding | enum(["exact", "floor", "ceil", "nearest", "unknown"])) and
      (.metadata | metadata))) and
    (.paired_operations | type == "array" and all(.[]; obj and
      (.id | stable_id) and
      (.left_operation | type == "string" and length > 0) and
      (.right_operation | type == "string" and length > 0) and
      (.relationship | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.lifecycle_edges | type == "array" and all(.[]; obj and
      (.id | stable_id) and
      (.account | type == "string" and length > 0) and
      (.handler | type == "string" and length > 0) and
      (.from | type == "string" and length > 0) and
      (.to | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.authority_capabilities | type == "array" and all(.[]; obj and
      (.id | stable_id) and
      (.role | type == "string" and length > 0) and
      (.identity_anchor | type == "string" and length > 0) and
      (.handler | type == "string" and length > 0) and
      (.effects | type == "array" and length > 0) and
      (.metadata | metadata))) and
    (.economic_equations | type == "array" and all(.[]; obj and
      (.id | stable_id) and
      (.name | type == "string" and length > 0) and
      (.expression | type == "string" and length > 0) and
      (.scope | type == "array" and length > 0) and
      (.tolerance | type == "string" and length > 0) and
      (.metadata | metadata))) and
    (.external_assumptions | type == "array" and all(.[]; obj and
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
    def obj: type == "object";
    def stable_id: type == "string" and test("^[a-z][a-z0-9_-]*$");
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/audit-run-manifest-v1.schema.json" and
    (.audit_id | stable_id) and
    (.status | enum(["running", "completed", "build-blocked", "tooling-blocked", "policy-interfered"])) and
    (.target | type == "object") and
    (.target.program_root | type == "string" and length > 0) and
    (.target.mode | enum(["spec-aware", "spec-less"])) and
    (.lanes | type == "array" and length > 0 and all(.[]; obj and
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
    def obj: type == "object";
    def clause:
      type == "object" and
      (.candidate_id | type == "string" and length > 0) and
      (.disposition | enum(["emitted", "needs_authoring", "language_gap", "excluded"])) and
      (if .disposition == "needs_authoring" then
        (.authoring | type == "object") and
        (.authoring.constructs | type == "array" and length > 0) and
        (.authoring.template | type == "string" and length > 0) and
        (.authoring.notes | type == "array")
       else true end) and
      (.verification_lanes | type == "array" and all(.[];
        enum(["check", "manual", "mollusk", "miri", "crucible", "kani", "lean"])));
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/spec-handoff-v1.schema.json" and
    (.spec_path | type == "string" and length > 0) and
    (.layers | type == "object") and
    (.layers.structural | type == "array" and all(.[]; clause)) and
    (.layers.domain | type == "array" and all(.[]; clause)) and
    (.layers.regression | type == "array" and all(.[]; clause)) and
    (.language_gaps | type == "array" and all(.[]; obj and
      (.candidate_id | type == "string" and length > 0) and
      (.reason | type == "string" and length > 0) and
      (.current_language_support | type == "string" and length > 0) and
      .disposition == "document_or_extend_language"))
  ' "$1" >/dev/null
}

validate_sequences() {
  jq -e '
    def enum($values): . as $value | ($values | index($value)) != null;
    def obj: type == "object";
    def unresolved:
      type == "object" and
      (.name | type == "string" and length > 0) and
      (.kind | enum(["handler_argument", "account_bindings", "lifecycle_association"])) and
      (.reason | type == "string" and length > 0);
    def action:
      type == "object" and
      (.handler | type == "string" and length > 0) and
      (.role | enum(["setup", "forward", "reverse", "teardown", "lifecycle_transition"])) and
      (.provenance_candidate_ids | type == "array" and length > 0) and
      (.unresolved_parameters | type == "array" and all(.[]; unresolved));
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/domain-sequences-v1.schema.json" and
    (.plans | type == "array" and all(.[]; obj and
      (.id | type == "string" and length > 0) and
      (.kind | enum(["paired_round_trip", "lifecycle_transition"])) and
      (.title | type == "string" and length > 0) and
      (.setup | type == "array" and all(.[]; action)) and
      (.forward | type == "array" and all(.[]; action)) and
      (.reverse | type == "array" and all(.[]; action)) and
      (.teardown | type == "array" and all(.[]; action)) and
      (.provenance_candidate_ids | type == "array" and length > 0) and
      (.unresolved_parameters | type == "array" and all(.[]; unresolved)))) and
    (.exclusions | type == "array" and all(.[]; obj and
      (.candidate_id | type == "string" and length > 0) and
      (.collection | enum(["paired_operations", "lifecycle_edges"])) and
      (.ratification | enum(["auto", "user", "pending", "rejected", "bug"])) and
      (.reason | type == "string" and length > 0)))
  ' "$1" >/dev/null
}

validate_sequence_bindings() {
  jq -e '
    def kind: . == "handler_argument" or . == "account_bindings" or . == "lifecycle_association";
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/domain-sequence-bindings-v1.schema.json" and
    .source_sequence_schema_uri == "https://qedgen.dev/schemas/auditor/domain-sequences-v1.schema.json" and
    ((.source_audit_id == null) or (.source_audit_id | type == "string" and length > 0)) and
    (.bindings | type == "array" and all(.[]; (type == "object") and
      (.plan_id | type == "string" and length > 0) and
      ((.action == null) or
       ((.action | type == "object") and
        (.action.phase | IN("setup", "forward", "reverse", "teardown")) and
        (.action.index | type == "number" and . >= 0))) and
      (.parameter | type == "object") and
      (.parameter.name | type == "string" and length > 0) and
      (.parameter.kind | kind)))
  ' "$1" >/dev/null
}

validate_resolved_sequences() {
  jq -e '
    def resolved_binding:
      type == "object" and
      (.parameter | type == "object") and
      (.parameter.name | type == "string" and length > 0) and
      (.provenance | type == "object") and
      .provenance.source == "user" and
      (.provenance.plan_id | type == "string" and length > 0);
    def action:
      type == "object" and
      (.handler | type == "string" and length > 0) and
      (.resolved_bindings | type == "array" and all(.[]; resolved_binding));
    def actions: type == "array" and all(.[]; action);
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/resolved-domain-sequences-v1.schema.json" and
    .source_sequence_schema_uri == "https://qedgen.dev/schemas/auditor/domain-sequences-v1.schema.json" and
    .source_bindings_schema_uri == "https://qedgen.dev/schemas/auditor/domain-sequence-bindings-v1.schema.json" and
    (.plans | type == "array" and all(.[]; (type == "object") and
      (.id | type == "string" and length > 0) and
      (.setup | actions) and
      (.forward | actions) and
      (.reverse | actions) and
      (.teardown | actions) and
      (.resolved_plan_bindings | type == "array" and all(.[]; resolved_binding))))
  ' "$1" >/dev/null
}

validate_account_overlay() {
  jq -e '
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/account-binding-overlay-v1.schema.json" and
    .source_resolved_sequence_schema_uri == "https://qedgen.dev/schemas/auditor/resolved-domain-sequences-v1.schema.json" and
    (.handlers | type == "object" and all(to_entries[];
      (.key | length > 0) and
      (.value | type == "object") and
      (.value.accounts | type == "object" and all(to_entries[];
        (.key | length > 0) and
        (.value | type == "string" and test("^fixture:[A-Za-z0-9_][A-Za-z0-9_.-]*$")))) and
      (.value.provenance | type == "object")))
  ' "$1" >/dev/null
}

validate_replay_report() {
  jq -e '
    def sha256: type == "string" and test("^[0-9a-f]{64}$");
    .schema_version == 1 and
    .schema_uri == "https://qedgen.dev/schemas/auditor/domain-replay-report-v1.schema.json" and
    (.resolved_document_sha256 | sha256) and
    (.account_binding_overlay_sha256 | sha256) and
    (.harness_sha256 | sha256) and
    (.records | type == "array" and length > 0 and all(.[]; (type == "object") and
      (.plan_id | type == "string" and length > 0) and
      (.seed_path | type == "string" and length > 0) and
      (.seed_sha256 | sha256) and
      (.action_count | type == "number" and . >= 1) and
      (.command | type == "array" and length >= 8) and
      (.status | IN("completed_zero_exit", "completed_nonzero_exit", "terminated_by_signal", "spawn_failed")) and
      (if .status == "completed_zero_exit" then .exit_code == 0 and .error == null
       elif .status == "spawn_failed" then .exit_code == null and (.error | type == "string" and length > 0)
       elif .status == "terminated_by_signal" then .exit_code == null and .error == null
       else (.exit_code | type == "number") and .error == null end)))
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
      --sequences)
        [[ $# -ge 2 ]] || { echo "--sequences requires a path" >&2; exit 2; }
        validate_sequences "$2"
        shift 2
        ;;
      --bindings)
        [[ $# -ge 2 ]] || { echo "--bindings requires a path" >&2; exit 2; }
        validate_sequence_bindings "$2"
        shift 2
        ;;
      --resolved-sequences)
        [[ $# -ge 2 ]] || { echo "--resolved-sequences requires a path" >&2; exit 2; }
        validate_resolved_sequences "$2"
        shift 2
        ;;
      --account-overlay)
        [[ $# -ge 2 ]] || { echo "--account-overlay requires a path" >&2; exit 2; }
        validate_account_overlay "$2"
        shift 2
        ;;
      --replay-report)
        [[ $# -ge 2 ]] || { echo "--replay-report requires a path" >&2; exit 2; }
        validate_replay_report "$2"
        shift 2
        ;;
      *)
        echo "usage: check-auditor-domain-artifacts.sh [--dossier <json>] [--manifest <json>] [--handoff <json>] [--sequences <json>] [--bindings <json>] [--resolved-sequences <json>] [--account-overlay <json>] [--replay-report <json>]" >&2
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

# A type-wrong (shape-wrong) field must be a CLEAN validation failure
# (jq -e false → exit 1), never a jq runtime crash (exit 5) — the crash
# was GH #250; one shape-wrong fixture per validator is policy (#273).
expect_clean_invalid() {
  local validator="$1" fixture="$2" rc=0
  "$validator" "$fixtures/$fixture" || rc=$?
  if [[ $rc -ne 1 ]]; then
    echo "$fixture: expected clean invalid (exit 1) from $validator, got exit $rc" >&2
    exit 1
  fi
}

expect_clean_invalid validate_dossier invalid-args-domain-dossier.json

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

validate_sequences "$fixtures/valid-domain-sequences.json"
if validate_sequences "$fixtures/invalid-domain-sequences.json"; then
  echo "invalid domain sequences fixture unexpectedly passed" >&2
  exit 1
fi

validate_sequence_bindings "$fixtures/valid-domain-sequence-bindings.json"
validate_resolved_sequences "$fixtures/valid-resolved-domain-sequences.json"
validate_account_overlay "$fixtures/valid-account-binding-overlay.json"
validate_replay_report "$fixtures/valid-domain-replay-report.json"

# Shape-wrong sweep (#273): one type-confused fixture per validator, each
# targeting a field an outside author would plausibly get wrong.
expect_clean_invalid validate_manifest invalid-shape-audit-run-manifest.json
expect_clean_invalid validate_handoff invalid-shape-spec-handoff.json
expect_clean_invalid validate_sequences invalid-shape-domain-sequences.json
expect_clean_invalid validate_sequence_bindings invalid-shape-domain-sequence-bindings.json
expect_clean_invalid validate_resolved_sequences invalid-shape-resolved-domain-sequences.json
expect_clean_invalid validate_account_overlay invalid-shape-account-binding-overlay.json
expect_clean_invalid validate_replay_report invalid-shape-domain-replay-report.json

echo "auditor domain artifact checks passed"
