//! Structural / declaration lints: error-as-record misdeclaration, unknown
//! error variants, PDA seed collisions, and vacuous property lowering.

use super::*;

/// `type Error = { ... }` (record brace form) parses as a `Record` named
/// `Error` with `error_codes` left empty, so every error-variant consumer
/// (`WrongState` gate, `MathOverflow` check) misbehaves silently. P0
/// pointing at the pipe form; also fires when both forms are declared
/// (signals user confusion).
pub(crate) fn check_error_declared_as_record(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
pub(crate) fn check_unknown_error_variant(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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

pub(crate) fn check_pda_collisions(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
pub(crate) fn check_vacuous_property_lowering(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
