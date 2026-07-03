//! Structural / declaration lints: error-as-record misdeclaration, unknown
//! error variants, PDA seed collisions, and vacuous property lowering.

use super::*;

/// `type Error = { ... }` (record brace form) parses as a `Record` named
/// `Error` with `error_codes` left empty, so every error-variant consumer
/// (`WrongState` gate, `MathOverflow` check) misbehaves silently. P0
/// pointing at the pipe form; also fires when both forms are declared
/// (signals user confusion).
pub(super) fn check_error_declared_as_record(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let has_error_record = spec.records.iter().any(|r| r.name == "Error");
    if !has_error_record {
        return warnings;
    }
    let fields_hint = spec
        .records
        .iter()
        .find(|r| r.name == "Error")
        .map(|r| {
            r.fields
                .iter()
                .map(|(n, _)| format!("  | {}", n))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "  | InvalidAmount\n  | Unauthorized".to_string());
    warnings.push(CompletenessWarning {
        rule: "error_declared_as_record".to_string(),
        severity: Severity::Error,
        priority: 0,
        message: "`type Error = { ... }` (record brace form) does not declare error \
                  variants — the parser treats it as a struct named `Error` and \
                  `spec.error_codes` ends up empty. Downstream lowering then \
                  misbehaves silently (CPI error refs unresolved, `WrongState` / \
                  `MathOverflow` gates don't fire)."
            .to_string(),
        subject: Some("Error".to_string()),
        fix: "Use the pipe form instead of `= { ... }`. Each variant goes on its \
              own line with a leading `|`."
            .to_string(),
        example: Some(format!("  type Error\n{}", fields_hint)),
        counterexample: None,
        fix_options: vec![],
    });
    warnings
}

/// `unknown_error_variant`: a per-site `or X` override or checked_overflow/
/// underflow pragma references a variant not declared in `type Error | …` —
/// the generated Rust references `<ProgramName>Error::X` and won't compile.
pub(super) fn check_unknown_error_variant(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let mut warnings = Vec::new();

    // Pragma references — fire once per pragma, not once per handler.
    for (key, value) in &spec.pragma_assignments {
        if (key == "checked_overflow_error" || key == "checked_underflow_error") && !has_decl(value)
        {
            warnings.push(CompletenessWarning {
                rule: "unknown_error_variant".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "`pragma {} = {}` references a variant absent from `type Error | …`. Generated Rust references `{}Error::{}` and won't compile.",
                    key,
                    value,
                    crate::codegen_shared::to_pascal_case(&spec.program_name),
                    value,
                ),
                subject: Some(value.clone()),
                fix: format!(
                    "Add `{}` to your `type Error | …` block, drop the pragma, or replace it with a declared variant name.",
                    value,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Per-site `or X` references.
    for h in &spec.handlers {
        for on_error in h.effect_on_error.iter().flatten() {
            if !has_decl(on_error) {
                warnings.push(CompletenessWarning {
                    rule: "unknown_error_variant".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "handler '{}' has an effect with `else {}` referencing a variant absent from `type Error | …`. Generated Rust references `{}Error::{}` and won't compile.",
                        h.name,
                        on_error,
                        crate::codegen_shared::to_pascal_case(&spec.program_name),
                        on_error,
                    ),
                    subject: Some(h.name.clone()),
                    fix: format!(
                        "Add `{}` to your `type Error | …` block, drop the `else {}` suffix to fall back to the default, or use a declared variant.",
                        on_error, on_error,
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }
    warnings
}

pub(super) fn check_pda_collisions(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let pdas = &spec.pdas;

    // Classify a seed token: is it a literal/constant or a variable reference?
    // Seeds from the adapter: string literals are stored with surrounding quotes
    // (e.g. `"vault"`), named constants are ALL_CAPS, variables are lowercase idents.
    let is_literal = |s: &str| -> bool {
        s.starts_with('"')
            || s.chars()
                .all(|c| c.is_uppercase() || c.is_ascii_digit() || c == '_')
    };

    for i in 0..pdas.len() {
        for j in (i + 1)..pdas.len() {
            let a = &pdas[i];
            let b = &pdas[j];

            if a.seeds == b.seeds {
                // Exact collision — same seed tuple → same address always.
                warnings.push(CompletenessWarning {
                    rule: "pda_seed_collision".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "PDA '{}' and PDA '{}' have identical seed tuples [{}] — they will always resolve to the same on-chain address",
                        a.name, b.name, a.seeds.join(", ")
                    ),
                    subject: Some(a.name.clone()),
                    fix: format!(
                        "Add a distinguishing seed to '{}' or '{}' (e.g., a discriminator byte or unique program-specific tag)",
                        a.name, b.name
                    ),
                    example: Some(format!(
                        "  pda {} [\"{}_tag\", {}]\n  pda {} [\"{}_tag\", {}]",
                        a.name,
                        a.name.to_lowercase(),
                        a.seeds.join(", "),
                        b.name,
                        b.name.to_lowercase(),
                        b.seeds.join(", ")
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
                continue;
            }

            // Possible collision: same literal seeds, differing only in variable positions.
            let a_literals: Vec<&str> = a
                .seeds
                .iter()
                .filter(|s| is_literal(s))
                .map(|s| s.as_str())
                .collect();
            let b_literals: Vec<&str> = b
                .seeds
                .iter()
                .filter(|s| is_literal(s))
                .map(|s| s.as_str())
                .collect();

            if !a_literals.is_empty() && a_literals == b_literals && a.seeds.len() == b.seeds.len()
            {
                // Same structure, same literals — variable seeds could collide at runtime.
                warnings.push(CompletenessWarning {
                    rule: "pda_seed_possible_collision".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "PDA '{}' and PDA '{}' share all literal seeds [{}] and differ only in variable positions — they can collide at runtime when variables hold the same values",
                        a.name, b.name, a_literals.join(", ")
                    ),
                    subject: Some(a.name.clone()),
                    fix: format!(
                        "Add a unique literal discriminator seed to '{}' or '{}' so their namespaces cannot overlap",
                        a.name, b.name
                    ),
                    example: Some(format!(
                        "  pda {} [\"{}\", ...]\n  pda {} [\"{}\", ...]",
                        a.name,
                        a.name.to_lowercase(),
                        b.name,
                        b.name.to_lowercase()
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    warnings
}

/// Defense-in-depth lint for three vacuous-property-body shapes in the
/// *rendered Rust*:
///
/// 1. **Codegen-induced tautology (P1, AST-gated).** AST body contains
///    `Expr::Old(_)` AND `rust_expression` reduces to `<expr> cmp <expr>`
///    with structurally identical sides — the temporal marker was dropped
///    during lowering. Should be unreachable from current codegen; kept as
///    a regression net.
/// 2. **Unsupported-quantifier marker (P1).** `rust_expression` contains
///    `QEDGEN_UNSUPPORTED_QUANTIFIER` — codegen emitted a stub `true` body.
///    Unlike `unsupported_quantifier_shape`, fires regardless of `per_slot`.
/// 3. **Literal `true` body (P1).** Catches any other codegen path that
///    short-circuited to a constant.
///
/// **Author-written tautologies are silently accepted**: no `Expr::Old(_)`
/// in the AST + identical sides is an authored choice (the "field tracking"
/// pattern). Rule 1 gates on `Expr::Old(_)` precisely so this passes.
pub(super) fn check_vacuous_property_lowering(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for prop in &spec.properties {
        let Some(rs) = prop.rust_expression.as_deref() else {
            continue;
        };
        let trimmed = rs.trim();

        // Rule 2 — unconditional: marker present, body is a stub.
        if rs.contains(QEDGEN_UNSUPPORTED_MARKER) {
            warnings.push(CompletenessWarning {
                rule: "vacuous_property_lowering".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "property '{}' lowered Rust contains \
                     QEDGEN_UNSUPPORTED_QUANTIFIER — the harness emits a `true` \
                     body and skips the real check",
                    prop.name
                ),
                subject: Some(prop.name.clone()),
                fix: "Rewrite the quantifier in a shape qedgen can lower \
                      (see docs/limitations.md#unsupported-quantifier-shapes) \
                      or split the property into per-element guards."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            continue;
        }

        // Rule 3 — unconditional: bare `true` body.
        if trimmed == "true" {
            warnings.push(CompletenessWarning {
                rule: "vacuous_property_lowering".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "property '{}' lowered to the literal `true` — the harness \
                     can never fail. Check the spec body and re-run check.",
                    prop.name
                ),
                subject: Some(prop.name.clone()),
                fix: "Inspect the property body for a spec construct that \
                      lowered to a constant. If the property is genuinely \
                      trivial, remove it; otherwise file a codegen bug."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            continue;
        }

        // Rule 1 — AST-gated; without the gate this would fire on
        // author-written tautologies (`state.admin == state.admin`
        // field-tracking), which the lint must not override.
        let Some(ast) = &prop.ast_body else {
            continue;
        };
        if !crate::chumsky_adapter::expr_contains_old(ast) {
            continue;
        }
        let Some((lhs, _op, rhs)) = parse_top_level_cmp(trimmed) else {
            continue;
        };
        if lhs == rhs {
            warnings.push(CompletenessWarning {
                rule: "vacuous_property_lowering".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "property '{}' uses `old(...)` but lowered Rust collapses to a \
                     structural tautology (`{} {} {}`). The temporal marker was \
                     dropped during lowering — this indicates a codegen regression.",
                    prop.name, lhs, _op, rhs
                ),
                subject: Some(prop.name.clone()),
                fix: "File a qedgen issue with the spec snippet. Pre-v2.23 this \
                      was the default behavior for `old(...)` in proptest/Kani; \
                      post-Slices 2-4 it should be unreachable."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_declared_as_record_lint_fires_and_suggests_pipe_form() {
        let src = r#"
    spec Probe
    state { balance : U64 }
    type Error = {
      InvalidAmount : U64,
      Unauthorized : U64,
    }
    handler init { effect { balance := 0 } }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_error_declared_as_record(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "error_declared_as_record")
            .expect("error_declared_as_record fires");
        assert_eq!(hit.severity, Severity::Error);
        let example = hit.example.as_deref().unwrap_or("");
        assert!(
            example.contains("type Error\n  | InvalidAmount"),
            "example should suggest pipe form, got: {}",
            example
        );
    }

    // ----- PDA seed collision -----

    #[test]
    fn pda_seed_collision_fires_for_identical_seeds() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"
                spec CollisionTest

                pda vault ["vault", user]
                pda escrow ["vault", user]

                state { dummy : U64 }
                "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "pda_seed_collision"),
            "must warn on identical seed tuples; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn pda_seed_collision_no_false_positive_for_distinct_seeds() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"
                spec CollisionTest

                pda vault ["vault", user]
                pda escrow ["escrow", user]

                state { dummy : U64 }
                "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "pda_seed_collision"),
            "must NOT warn when seeds differ by literal discriminator"
        );
    }

    #[test]
    fn pda_seed_possible_collision_fires_when_literals_match_but_vars_differ() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"
                spec CollisionTest

                pda order_a ["order", user_a]
                pda order_b ["order", user_b]

                state { dummy : U64 }
                "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "pda_seed_possible_collision"),
            "must warn on same literals but different variable seeds; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ----- unknown_error_variant lint -----

    #[test]
    fn unknown_error_variant_fires_on_per_site_override_with_undeclared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow | MathUnderflow

    handler deposit (n : U64) : State.Active -> State.Active {
      permissionless
      effect { balance += n else MintOverflow }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unknown_error_variant")
            .expect("expected unknown_error_variant warning");
        assert!(hit.message.contains("MintOverflow"));
        assert!(hit.message.contains("deposit"));
    }

    #[test]
    fn unknown_error_variant_fires_on_pragma_with_undeclared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow | MathUnderflow

    pragma checked_overflow_error = MintOverflow

    handler deposit (n : U64) : State.Active -> State.Active {
      permissionless
      effect { balance += n }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unknown_error_variant")
            .expect("expected unknown_error_variant warning for pragma");
        assert!(hit.message.contains("checked_overflow_error"));
        assert!(hit.message.contains("MintOverflow"));
    }

    #[test]
    fn unknown_error_variant_silent_when_override_is_declared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow | MintOverflow

    handler deposit (n : U64) : State.Active -> State.Active {
      permissionless
      effect { balance += n else MintOverflow }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "unknown_error_variant"),
            "per-site override referencing a declared variant should not fire"
        );
        // The site provides an override, so missing_math_overflow defers
        // (the `+=` doesn't fall back to the builtin default).
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "per-site override defers missing_math_overflow"
        );
    }

    // ----- Rule 17: invariant_no_body -----

    #[test]
    fn invariant_no_body_fires_on_doc_only_invariant() {
        // The escrow / escrow-split shape: invariant declared with only a
        // description string, no `expr` body. Lean codegen would emit
        // `theorem conservation : True := trivial`.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Demo
    type State | Active of { counter : U64 }

    invariant conservation "total tokens preserved across all handlers"

    handler bump : State.Active -> State.Active {
      auth admin
      accounts { admin : signer }
      effect { counter += 1 }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "invariant_no_body")
            .collect();
        assert_eq!(hits.len(), 1, "expected one finding: {hits:#?}");
        assert!(hits[0].message.contains("conservation"));
    }

    #[test]
    fn invariant_no_body_silent_on_real_body() {
        // An invariant with a proper expression body — no finding.
        // The DSL form: `invariant <name> : <expr>` (one-liner, no
        // preserved_by — the expression body alone is what matters
        // for this lint).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Demo
    type State | Active of { counter : U64 }

    invariant counter_nonneg : state.counter >= 0

    handler bump : State.Active -> State.Active {
      auth admin
      accounts { admin : signer }
      effect { counter += 1 }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "invariant_no_body"),
            "real expr body should suppress: {warnings:#?}"
        );
    }

    // ========================================================================
    // vacuous_property_lowering lint
    // ========================================================================

    const VPL_SPEC_HEAD: &str = r#"
    spec VplTest
    program_id "11111111111111111111111111111111"

    type State
      | Active of { balance : U64, admin : U64 }

    type Error
      | E

    handler bump (delta : U64) : State.Active -> State.Active {
      permissionless
      effect { balance := balance + delta }
    }
    "#;

    #[test]
    fn vpl_lint_silent_on_author_tautology_without_old() {
        // pool.qedspec:660-662 pattern — `state.x == state.x` with no
        // `old(...)` in the AST. The author wants the field surfaced in
        // proofs; the lint must NOT fire.
        let src = format!(
            "{}{}",
            VPL_SPEC_HEAD,
            r#"property admin_tracked : state.admin == state.admin preserved_by all"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings.is_empty(),
            "author-written tautology (no Expr::Old) must not fire; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_silent_on_distinct_sides() {
        // Distinct comparison — silent regardless of `old(...)`.
        let src = format!(
            "{}{}",
            VPL_SPEC_HEAD, r#"property balance_le_max : state.balance <= 1000 preserved_by all"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings.is_empty(),
            "distinct-sides comparison must not fire; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_silent_on_binary_property_post_slice_2() {
        // A binary property (`old(...)` in body) lowers to
        // `post.balance >= pre.balance` — distinct sides, no tautology.
        // If the lint fires here, codegen regressed.
        let src = format!(
            "{}{}",
            VPL_SPEC_HEAD,
            r#"property balance_monotonic : state.balance >= old(state.balance) preserved_by all"#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_vacuous_property_lowering(&spec);
        let vpl: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "vacuous_property_lowering")
            .collect();
        assert!(
            vpl.is_empty(),
            "binary property correctly lowered to pre/post must not fire VPL; got: {:?}",
            vpl.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_fires_on_literal_true_body() {
        // Construct a property whose rust_expression is the literal "true"
        // — Rule 3 unconditionally fires.
        let mut spec = ParsedSpec::default();
        spec.properties.push(ParsedProperty {
            name: "always_true".to_string(),
            expression: Some("True".to_string()),
            rust_expression: Some("true".to_string()),
            rust_expression_pod: Some("true".to_string()),
            rust_expression_math: None,
            preserved_by: vec![],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        });
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "vacuous_property_lowering"),
            "literal `true` body must fire VPL; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vpl_lint_fires_on_unsupported_quantifier_marker() {
        // Construct a property whose rust_expression carries the marker
        // — Rule 2 unconditionally fires.
        let mut spec = ParsedSpec::default();
        spec.properties.push(ParsedProperty {
            name: "stub_forall".to_string(),
            expression: Some("forall x : U64, x > 0".to_string()),
            rust_expression: Some(format!(
                "/* {} : forall x : U64, x > 0 */ true",
                QEDGEN_UNSUPPORTED_MARKER
            )),
            rust_expression_pod: Some("true".to_string()),
            rust_expression_math: None,
            preserved_by: vec![],
            per_slot: None,
            quantifier_lint: None,
            class: PropertyClass::Unary,
            ast_body: None,
        });
        let warnings = check_vacuous_property_lowering(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "vacuous_property_lowering"
                    && w.message.contains("QEDGEN_UNSUPPORTED_QUANTIFIER")),
            "marker body must fire VPL with marker mention; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn unknown_guard_identifier_fires_on_typos_only() {
        let src = r#"spec TypoVault
program_id "11111111111111111111111111111111"

const LIMIT = 100

type State = {
  active : U8,
  fee : U64,
}

type Error | Unauthorized

handler execute (amount : U64) : State -> State {
  permissionless
  requires actve == 0 else Unauthorized
  requires state.fe > 0 else Unauthorized
  requires active == 0 else Unauthorized
  requires amount > 0 else Unauthorized
  requires state.fee < LIMIT else Unauthorized
  effect { fee := amount }
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_unknown_guard_identifier(&spec);
        let subjects: Vec<&str> = warnings
            .iter()
            .filter(|w| w.rule == "unknown_guard_identifier")
            .map(|w| w.message.as_str())
            .collect();
        assert_eq!(
            subjects.len(),
            2,
            "exactly the two typos fire — resolvable refs (state field, \
             param, const) stay silent; got: {subjects:?}"
        );
        assert!(subjects.iter().any(|m| m.contains("`actve`")));
        assert!(subjects.iter().any(|m| m.contains("`state.fe`")));
        assert!(warnings
            .iter()
            .all(|w| w.severity == Severity::Error && w.priority == 0));
    }

    #[test]
    fn unknown_guard_identifier_skips_sbpf_specs() {
        let src = r#"spec SbpfCounter
program_id "11111111111111111111111111111111"

pragma sbpf {}

type State
  | Uninitialized
  | Active

type Error | BadPda

handler initialize : State.Uninitialized -> State.Active {
  permissionless
  requires pda_derivation_succeeds else BadPda
}
"#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        assert!(
            check_unknown_guard_identifier(&spec).is_empty(),
            "sBPF requires vocabulary resolves against the input layout, not state"
        );
    }
}

/// `unknown_guard_identifier` (issue #139 follow-up): a `requires` clause
/// references a name that resolves to nothing — not a state field (those
/// were canonicalized to `state.`-rooted paths at adapt time), not a param,
/// account, const, `let` binding, abstract binder, CPI result binding, or
/// auth actor. The string projections carry the name verbatim, so every
/// generated backend (Lean transition, Kani harness, proptest model) fails
/// to compile while `check` used to stay green. Also catches `state.<typo>`
/// where the field segment isn't declared.
///
/// sBPF specs are exempt: their handler requires speak a runtime-input
/// vocabulary (`instruction_data_len`, `pda_derivation_succeeds`,
/// `derived_pda`, account attrs) that resolves against the input layout,
/// not the state model.
pub(super) fn check_unknown_guard_identifier(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    use crate::chumsky_adapter::GuardPathRef;

    let mut warnings = Vec::new();
    if spec.is_assembly_target() {
        return warnings;
    }

    // Every declared state-field name, across representations: flat view,
    // per-account-type fields + ADT variant fields, and ghosts (rendered as
    // state fields).
    let mut state_fields: std::collections::BTreeSet<&str> =
        spec.state_fields.iter().map(|(n, _)| n.as_str()).collect();
    for at in &spec.account_types {
        state_fields.extend(at.fields.iter().map(|(n, _)| n.as_str()));
        for v in &at.variants {
            state_fields.extend(v.fields.iter().map(|(n, _)| n.as_str()));
        }
    }
    if let Some(r) = spec.records.iter().find(|r| r.name == "State") {
        state_fields.extend(r.fields.iter().map(|(n, _)| n.as_str()));
    }
    state_fields.extend(spec.ghosts.iter().map(|g| g.name.as_str()));

    let consts: std::collections::BTreeSet<&str> =
        spec.constants.iter().map(|(n, _)| n.as_str()).collect();

    for h in &spec.handlers {
        let mut known: std::collections::BTreeSet<&str> = consts.clone();
        known.extend(h.takes_params.iter().map(|(n, _)| n.as_str()));
        known.extend(h.accounts.iter().map(|a| a.name.as_str()));
        known.extend(h.let_bindings.iter().map(|(n, _, _)| n.as_str()));
        known.extend(h.abstract_binders.iter().map(|(n, _)| n.as_str()));
        known.extend(h.calls.iter().filter_map(|c| c.result_binding.as_deref()));
        if let Some(who) = &h.who {
            known.insert(who.as_str());
        }

        // Dedup per handler: one finding per unresolved name.
        let mut reported: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for req in &h.requires {
            let Some(ast) = &req.ast_body else { continue };
            for r in crate::chumsky_adapter::collect_guard_path_refs(ast) {
                let (name, is_state_ref) = match r {
                    GuardPathRef::Bare(n) => (n, false),
                    GuardPathRef::StateField(f) => (f, true),
                };
                let resolves = if is_state_ref {
                    state_fields.contains(name.as_str())
                } else {
                    known.contains(name.as_str()) || state_fields.contains(name.as_str())
                };
                if resolves || !reported.insert(name.clone()) {
                    continue;
                }
                let display = if is_state_ref {
                    format!("state.{name}")
                } else {
                    name.clone()
                };
                warnings.push(CompletenessWarning {
                    rule: "unknown_guard_identifier".to_string(),
                    severity: Severity::Error,
                    priority: 0,
                    message: format!(
                        "handler '{}' references `{}` in a `requires` clause, but it \
                         resolves to nothing — not a state field, parameter, account, \
                         const, or binding. Generated code carries the name verbatim \
                         and won't compile in any backend.",
                        h.name, display
                    ),
                    subject: Some(h.name.clone()),
                    fix: format!(
                        "Declare `{name}` (state field, param, or const) or fix the \
                         reference to an existing name."
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }
    warnings
}
