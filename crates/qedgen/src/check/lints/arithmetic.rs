//! Arithmetic-safety lints: unbounded ref_impl arithmetic, checked-effect
//! error-variant requirements, wrapping/saturating opt-in surfacing, and
//! `Map[N] T` / subscript validation.

use super::*;

pub(crate) fn check_ref_impl_unbounded_arith(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for r in &spec.ref_impls {
        if !ref_impl_has_overflow_risk(r) {
            continue;
        }
        let mut ops: Vec<&str> = Vec::new();
        if r.rust_body.contains('*') {
            ops.push("*");
        }
        if r.rust_body.contains("<<") {
            ops.push("<<");
        }
        if r.rust_body.contains('+') {
            ops.push("+");
        }
        if r.rust_body.contains('-') {
            ops.push("-");
        }
        warnings.push(CompletenessWarning {
            rule: "ref_impl_unbounded_arith".to_string(),
            severity: Severity::Info,
            priority: 2,
            message: format!(
                "ref_impl '{}' uses {} over bounded-numeric params/return. \
                 Lean lowers this to `Nat`/`Int` (unbounded — no overflow), \
                 but the generated Rust runs on `u64`/`i64`/etc. where the \
                 same expression can wrap (release) or panic (debug). \
                 Bounded-arithmetic verification lives in Kani.",
                r.name,
                ops.join("/"),
            ),
            subject: Some(r.name.clone()),
            fix: "Run `qedgen verify --kani` against the generated impl-targeted \
                Kani harness — auto-emitted starting v2.26 whenever a ref_impl \
                trips this lint. The harness drives every numeric param with \
                `kani::any()` and produces a concrete counterexample at the \
                bit-width boundary."
                .to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[missing_math_overflow]`: checked effects (`+=` / `-=`) lower to
/// `checked_add` / `checked_sub` returning `<ProgramName>Error::MathOverflow`
/// / `::MathUnderflow`; without the variant declared, the generated code
/// fails `cargo build` with "unknown variant" — surface at lint time.
/// Per-effect overrides and pragma defaults defer to
/// `check_unknown_error_variant`. Back-compat fallback honored: declared
/// `MathOverflow` but not `MathUnderflow` → `-=` raises `MathOverflow`.
pub(crate) fn check_checked_arith_needs_math_overflow(
    spec: &ParsedSpec,
) -> Vec<CompletenessWarning> {
    let has_decl = |name: &str| spec.error_codes.iter().any(|c| c == name);
    let has_overflow = has_decl("MathOverflow");
    let has_underflow = has_decl("MathUnderflow");
    let pragma_overflow = spec.pragma_value("checked_overflow_error");
    let pragma_underflow = spec.pragma_value("checked_underflow_error");

    // Collect handlers whose builtin-default lowering would reference a
    // variant the spec didn't declare. Per-site overrides skip this lint
    // (their variant check lives in `check_unknown_error_variant`).
    let mut missing: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    let mut handlers_missing: Vec<String> = Vec::new();

    for h in &spec.handlers {
        let mut handler_fires = false;
        for (idx, (_, op_kind, _)) in h.effects.iter().enumerate() {
            let on_error = h.effect_on_error.get(idx).and_then(|o| o.as_deref());
            if on_error.is_some() {
                continue; // per-site override handled elsewhere
            }
            match op_kind.as_str() {
                "add" => {
                    if pragma_overflow.is_some() {
                        continue;
                    }
                    if !has_overflow {
                        missing.insert("MathOverflow");
                        handler_fires = true;
                    }
                }
                "sub" => {
                    if pragma_underflow.is_some() {
                        continue;
                    }
                    // Back-compat: declared MathOverflow but not
                    // MathUnderflow → `-=` falls back to MathOverflow.
                    if has_underflow {
                        continue;
                    }
                    if has_overflow {
                        continue; // back-compat path
                    }
                    missing.insert("MathUnderflow");
                    handler_fires = true;
                }
                _ => {}
            }
        }
        if handler_fires {
            handlers_missing.push(h.name.clone());
        }
    }

    if missing.is_empty() {
        return Vec::new();
    }
    let names = handlers_missing.join(", ");
    let variants_list: Vec<String> = missing.iter().map(|s| s.to_string()).collect();
    let variants = variants_list.join(" / ");
    let fix_block = variants_list
        .iter()
        .map(|v| format!("      | {}", v))
        .collect::<Vec<_>>()
        .join("\n");
    vec![CompletenessWarning {
        rule: "missing_math_overflow".to_string(),
        severity: Severity::Warning,
        priority: 2,
        message: format!(
            "handler(s) [{}] use checked-arithmetic effects (`+=` / `-=`), but `type Error` doesn't declare a `{}` variant. The generated Rust references `{}Error::{}` and won't compile without it.",
            names,
            variants,
            crate::codegen_shared::to_pascal_case(&spec.program_name),
            variants,
        ),
        subject: None,
        fix: format!(
            "Add `{}` to your `type Error | …` block. Example:\n\n    type Error\n{}\n      | …\n\nOr opt out of checked semantics per-effect with `+=!` (saturating) or `+=?` (wrapping), or override the variant inline with `pool += amount else MyVariant`.",
            variants, fix_block,
        ),
        example: None,
        counterexample: None,
        fix_options: vec![],
    }]
}

/// `[wrapping_arithmetic]` / `[saturating_arithmetic]` — explicit
/// non-default arithmetic opt-ins (default `+=` / `-=` is checked):
///
/// - **Wrapping** (`+=?` / `-=?`): silent overflow modulo 2^N; almost always
///   wrong on monetary amounts. Warning, P1.
/// - **Saturating** (`+=!` / `-=!`): caps at MAX/MIN, hiding bugs that should
///   error; sometimes legitimate (rate limiters, epoch counters). Info, P2.
///
/// Lives in check, not probe: a real structural pattern but a spec-authoring
/// concern, not a reproducible vulnerability (probe ships reproducer-bearing
/// findings only).
pub(crate) fn check_wrapping_arithmetic_opt_in(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        for (field, kind, _value) in &op.effects {
            let (severity, priority, label, default_op) = match kind.as_str() {
                "add_wrap" => (Severity::Warning, 1, "wrapping", "+="),
                "sub_wrap" => (Severity::Warning, 1, "wrapping", "-="),
                "add_sat" => (Severity::Info, 2, "saturating", "+="),
                "sub_sat" => (Severity::Info, 2, "saturating", "-="),
                _ => continue,
            };
            warnings.push(CompletenessWarning {
                rule: format!("{}_arithmetic", label),
                severity,
                priority,
                message: format!(
                    "handler `{}` uses {} arithmetic on `{}` (op `{}`) — silent overflow {}. Default `{}` (checked) aborts on overflow.",
                    op.name,
                    label,
                    field,
                    kind,
                    if label == "wrapping" { "modulo 2^N" } else { "saturating to MAX/MIN" },
                    default_op,
                ),
                subject: Some(format!("{}::{}::{}", op.name, field, kind)),
                fix: format!(
                    "If the {label} semantic is intentional (epoch wrap, rate limiter), document the invariant inline. Otherwise change `{kind}` to `{default_op}` (checked) — the spec's `type Error` block must declare `MathOverflow`.",
                    label = label,
                    kind = kind,
                    default_op = default_op,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// Validate `Map[N] T` field declarations and subscript usage.
///   - `N` must be a declared `const`
///   - `T` must be either a declared record or a well-known primitive
///   - Effect LHS of form `field[i].x` must reference a Map-typed state field
pub(crate) fn check_map_and_subscript(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    use std::collections::{HashMap, HashSet};

    let mut warnings = Vec::new();

    let const_names: HashSet<&str> = spec.constants.iter().map(|(n, _)| n.as_str()).collect();
    let record_names: HashSet<&str> = spec.records.iter().map(|r| r.name.as_str()).collect();
    // Enum-typed Map bounds (`Map[AddressField] T`): a unit-only sum type
    // gives one slot per variant (per-variant PDAs). Mixed-variant sums are
    // rejected by the second pass so the slot shape stays homogeneous.
    let unit_only_sum_names: HashSet<&str> = spec
        .sum_types
        .iter()
        .filter(|s| s.variants.iter().all(|v| v.fields.is_empty()))
        .map(|s| s.name.as_str())
        .collect();

    // Collect Map-typed fields across all account types, keyed by field name.
    let mut map_fields: HashMap<&str, (&str, &str, &str)> = HashMap::new(); // field → (owner, bound, inner)

    for acct in &spec.account_types {
        for (fname, ftype) in &acct.fields {
            if let FieldTypeShape::Map { bound, inner } = classify_field_type(ftype) {
                // Rule: bound must be a declared const OR a unit-only sum type.
                if !const_names.contains(bound) && !unit_only_sum_names.contains(bound) {
                    warnings.push(CompletenessWarning {
                        rule: "map_bound_not_const".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "field '{}.{}' uses Map[{}] but '{}' is neither a declared `const` nor a unit-only enum type",
                            acct.name, fname, bound, bound
                        ),
                        subject: Some(fname.clone()),
                        fix: format!("Add `const {} = <size>` or declare `type {} | Variant1 | Variant2 | …` at the top of the spec", bound, bound),
                        example: Some(format!("  const {} = 1024", bound)),
                        counterexample: None,
                        fix_options: vec![],
                    });
                }

                // Rule: inner must be a record or a known primitive
                let is_known = record_names.contains(inner)
                    || matches!(
                        inner,
                        "Bool"
                            | "U8"
                            | "U16"
                            | "U32"
                            | "U64"
                            | "U128"
                            | "I8"
                            | "I16"
                            | "I32"
                            | "I64"
                            | "I128"
                            | "Pubkey"
                    );
                if !is_known {
                    warnings.push(CompletenessWarning {
                        rule: "map_value_unknown".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "field '{}.{}' uses Map[{}] {} but '{}' is neither a declared record nor a primitive",
                            acct.name, fname, bound, inner, inner
                        ),
                        subject: Some(fname.clone()),
                        fix: format!("Declare `type {} = {{ ... }}`", inner),
                        example: Some(format!(
                            "  type {} = {{\n    active : Bool,\n    capital : U128,\n  }}",
                            inner
                        )),
                        counterexample: None,
                        fix_options: vec![],
                    });
                }

                map_fields.insert(fname.as_str(), (acct.name.as_str(), bound, inner));
            }
        }
    }

    // Effect LHS validation: any `name[i]...` must refer to a Map-typed field.
    for op in &spec.handlers {
        for (field, _, _) in &op.effects {
            if let Some(bracket) = field.find('[') {
                let root = &field[..bracket];
                if !map_fields.contains_key(root) {
                    warnings.push(CompletenessWarning {
                        rule: "subscript_not_map".to_string(),
                        severity: Severity::Error,
                        priority: 0,
                        message: format!(
                            "handler '{}' has effect `{}` but '{}' is not a Map-typed state field",
                            op.name, field, root
                        ),
                        subject: Some(op.name.clone()),
                        fix: format!(
                            "Declare `{} : Map[MAX_...] SomeRecord` in the state type, or remove the subscript",
                            root
                        ),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
        }
    }

    warnings
}
