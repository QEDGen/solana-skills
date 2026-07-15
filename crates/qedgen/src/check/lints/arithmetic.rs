//! Arithmetic-safety lints: unbounded ref_impl arithmetic, checked-effect
//! error-variant requirements, wrapping/saturating opt-in surfacing, and
//! `Map[N] T` / subscript validation.

use super::*;

/// Rule 3: add effect without explicit overflow bound (type-aware),
/// per-field. Sub effects get auto-guarded for underflow by codegen,
/// so only add overflow warns here.
pub(super) fn check_unguarded_arithmetic(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        // Collect all guard text for substring matching
        let all_guards: String = {
            let mut g = String::new();
            for req in &op.requires {
                g.push(' ');
                g.push_str(&req.lean_expr);
            }
            g
        };

        for eff in &op.effects {
            let (field, kind, val) = (&eff.field, &eff.op, &eff.value);
            if kind != "add" {
                continue;
            }
            // Check if any guard already bounds this field's addition.
            // Use contains_word on the val side to avoid "1" matching "10".
            let patterns = [
                format!("state.{} + {}", field, val),
                format!("{} + state.{}", val, field),
                format!("s.{} + {}", field, val),
                format!("{} + s.{}", val, field),
            ];
            let field_bounded = patterns.iter().any(|pat| contains_word(&all_guards, pat));
            if field_bounded {
                continue;
            }

            // Cumulative bound: `requires state.x + a + b <= U64_MAX`
            // bounds both `+= a` and `+= b`, but the per-pair patterns
            // above only match the first additive term. Accept when the
            // field appears in an additive expression AND the effect's
            // RHS appears as a bare word in the same guard string.
            let field_in_add = [
                format!("state.{} +", field),
                format!("s.{} +", field),
                format!("+ state.{}", field),
                format!("+ s.{}", field),
            ]
            .iter()
            .any(|pat| all_guards.contains(pat.as_str()));
            if field_in_add && contains_word(&all_guards, val) {
                continue;
            }

            let field_type = find_field_type(spec, op, field);
            let type_max = match field_type.as_deref() {
                Some("U8") => "U8_MAX (255)",
                Some("U16") => "U16_MAX (65535)",
                Some("U32") => "U32_MAX",
                Some("U128") => "U128_MAX",
                _ => "U64_MAX",
            };
            let type_label = field_type.as_deref().unwrap_or("U64");
            warnings.push(warn("unguarded_arithmetic", Severity::Info, 2, format!(
                    "handler '{}' adds to {} field '{}' without an explicit bound — codegen auto-inserts a {} guard, but an explicit `requires` with a tighter domain bound produces stronger proofs",
                    op.name, type_label, field, type_label
                )).subject(op.name.clone()).fix(format!(
                    "Add `requires state.{} + {} <= MY_BOUND` for a tighter bound than {} max",
                    field, val, type_label
                )).example(format!(
                    "  handler {}\n    requires state.{} + {} <= {}",
                    op.name, field, val, type_max
                )));
        }
    }
    warnings
}

/// Rule 7: takes params (U64) with no guard — suggest input validation.
/// Reads the already-accumulated warnings so it can skip handlers Rule 3
/// (`unguarded_arithmetic`) already flagged.
pub(super) fn check_missing_guard_from_takes(
    spec: &ParsedSpec,
    prior: &[CompletenessWarning],
) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        if op.has_guard() {
            continue;
        }
        // Skip if rule 3 (unguarded_arithmetic) already fired for this op
        let already_flagged = prior
            .iter()
            .any(|w| w.rule == "unguarded_arithmetic" && w.subject.as_deref() == Some(&op.name));
        if already_flagged {
            continue;
        }
        let u64_params: Vec<&str> = op
            .takes_params
            .iter()
            .filter(|(_, t)| t == "U64")
            .map(|(n, _)| n.as_str())
            .collect();
        if !u64_params.is_empty() {
            let guard_parts: Vec<String> =
                u64_params.iter().map(|p| format!("{} > 0", p)).collect();
            let guard_expr = guard_parts.join(" and ");
            warnings.push(
                warn(
                    "missing_guard_from_takes",
                    Severity::Warning,
                    1,
                    format!(
                        "handler '{}' takes U64 params but has no guard — no input validation",
                        op.name
                    ),
                )
                .subject(op.name.clone())
                .fix("Add input validation for takes parameters")
                .example(format!("  handler {}\n    guard {}", op.name, guard_expr)),
            );
        }
    }
    warnings
}

pub(super) fn check_ref_impl_unbounded_arith(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
        warnings.push(
            warn(
                "ref_impl_unbounded_arith",
                Severity::Info,
                2,
                format!(
                    "ref_impl '{}' uses {} over bounded-numeric params/return. \
                 Lean lowers this to `Nat`/`Int` (unbounded — no overflow), \
                 but the generated Rust runs on `u64`/`i64`/etc. where the \
                 same expression can wrap (release) or panic (debug). \
                 Bounded-arithmetic verification lives in Kani.",
                    r.name,
                    ops.join("/"),
                ),
            )
            .subject(r.name.clone())
            .fix(
                "Run `qedgen verify --kani` against the generated impl-targeted \
                Kani harness — auto-emitted starting v2.26 whenever a ref_impl \
                trips this lint. The harness drives every numeric param with \
                `kani::any()` and produces a concrete counterexample at the \
                bit-width boundary.",
            ),
        );
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
pub(super) fn check_checked_arith_needs_math_overflow(
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
        for eff in &h.effects {
            if eff.on_error.is_some() {
                continue; // per-site override handled elsewhere
            }
            match eff.op.as_str() {
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
    vec![warn("missing_math_overflow", Severity::Warning, 2, format!(
            "handler(s) [{}] use checked-arithmetic effects (`+=` / `-=`), but `type Error` doesn't declare a `{}` variant. The generated Rust references `{}Error::{}` and won't compile without it.",
            names,
            variants,
            crate::codegen_shared::to_pascal_case(&spec.program_name),
            variants,
        )).fix(format!(
            "Add `{}` to your `type Error | …` block. Example:\n\n    type Error\n{}\n      | …\n\nOr opt out of checked semantics per-effect with `+=!` (saturating) or `+=?` (wrapping), or override the variant inline with `pool += amount else MyVariant`.",
            variants, fix_block,
        ))]
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
pub(super) fn check_wrapping_arithmetic_opt_in(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        for eff in &op.effects {
            let (field, kind) = (&eff.field, &eff.op);
            let (severity, priority, label, default_op) = match kind.as_str() {
                "add_wrap" => (Severity::Warning, 1, "wrapping", "+="),
                "sub_wrap" => (Severity::Warning, 1, "wrapping", "-="),
                "add_sat" => (Severity::Info, 2, "saturating", "+="),
                "sub_sat" => (Severity::Info, 2, "saturating", "-="),
                _ => continue,
            };
            warnings.push(
                warn(
                    &format!("{}_arithmetic", label),
                    severity,
                    priority,
                    format!(
                        "handler `{}` uses {} arithmetic on `{}` (op `{}`) — silent overflow {}. Default `{}` (checked) aborts on overflow.",
                        op.name,
                        label,
                        field,
                        kind,
                        if label == "wrapping" { "modulo 2^N" } else { "saturating to MAX/MIN" },
                        default_op,
                    ),
                )
                .subject(format!("{}::{}::{}", op.name, field, kind))
                .fix(format!(
                    "If the {label} semantic is intentional (epoch wrap, rate limiter), document the invariant inline. Otherwise change `{kind}` to `{default_op}` (checked) — the spec's `type Error` block must declare `MathOverflow`.",
                    label = label,
                    kind = kind,
                    default_op = default_op,
                )),
            );
        }
    }
    warnings
}

/// Validate `Map[N] T` field declarations and subscript usage.
///   - `N` must be a declared `const`
///   - `T` must be either a declared record or a well-known primitive
///   - Effect LHS of form `field[i].x` must reference a Map-typed state field
pub(super) fn check_map_and_subscript(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
                    warnings.push(warn("map_bound_not_const", Severity::Error, 0, format!(
                            "field '{}.{}' uses Map[{}] but '{}' is neither a declared `const` nor a unit-only enum type",
                            acct.name, fname, bound, bound
                        )).subject(fname.clone()).fix(format!("Add `const {} = <size>` or declare `type {} | Variant1 | Variant2 | …` at the top of the spec", bound, bound)).example(format!("  const {} = 1024", bound)));
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
                            | "Bytes32"
                            | "Bytes64"
                    );
                if !is_known {
                    warnings.push(warn("map_value_unknown", Severity::Error, 0, format!(
                            "field '{}.{}' uses Map[{}] {} but '{}' is neither a declared record nor a primitive",
                            acct.name, fname, bound, inner, inner
                        )).subject(fname.clone()).fix(format!("Declare `type {} = {{ ... }}`", inner)).example(format!(
                            "  type {} = {{\n    active : Bool,\n    capital : U128,\n  }}",
                            inner
                        )));
                }

                map_fields.insert(fname.as_str(), (acct.name.as_str(), bound, inner));
            }
        }
    }

    // Effect LHS validation: any `name[i]...` must refer to a Map-typed field.
    for op in &spec.handlers {
        for eff in &op.effects {
            let field = &eff.field;
            if let Some(bracket) = field.find('[') {
                let root = &field[..bracket];
                if !map_fields.contains_key(root) {
                    warnings.push(warn("subscript_not_map", Severity::Error, 0, format!(
                            "handler '{}' has effect `{}` but '{}' is not a Map-typed state field",
                            op.name, field, root
                        )).subject(op.name.clone()).fix(format!(
                            "Declare `{} : Map[MAX_...] SomeRecord` in the state type, or remove the subscript",
                            root
                        )));
                }
            }
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::test_support::*;

    #[test]
    fn wrapping_arithmetic_lint_fires_on_wrap() {
        let mut spec = empty_spec();
        let mut h = make_handler("tick");
        h.effects
            .push(ParsedEffect::from_triple("epoch", "add_wrap", "1"));
        spec.handlers.push(h);
        let warnings = check_wrapping_arithmetic_opt_in(&spec);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule, "wrapping_arithmetic");
        assert_eq!(warnings[0].severity, Severity::Warning);
        assert!(warnings[0].message.contains("wrapping"));
    }

    #[test]
    fn wrapping_arithmetic_lint_fires_on_saturating() {
        let mut spec = empty_spec();
        let mut h = make_handler("apply");
        h.effects
            .push(ParsedEffect::from_triple("balance", "add_sat", "delta"));
        spec.handlers.push(h);
        let warnings = check_wrapping_arithmetic_opt_in(&spec);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule, "saturating_arithmetic");
        assert_eq!(warnings[0].severity, Severity::Info);
    }

    #[test]
    fn wrapping_arithmetic_lint_silent_on_default_checked() {
        let mut spec = empty_spec();
        let mut h = make_handler("deposit");
        h.effects
            .push(ParsedEffect::from_triple("total", "add", "amount"));
        h.effects
            .push(ParsedEffect::from_triple("fee_pool", "sub", "amount"));
        spec.handlers.push(h);
        assert!(check_wrapping_arithmetic_opt_in(&spec).is_empty());
    }

    #[test]
    fn wrapping_arithmetic_lint_fires_per_op() {
        let mut spec = empty_spec();
        let mut h = make_handler("complex");
        h.effects
            .push(ParsedEffect::from_triple("a", "add_wrap", "1"));
        h.effects
            .push(ParsedEffect::from_triple("b", "sub_sat", "1"));
        spec.handlers.push(h);
        let warnings = check_wrapping_arithmetic_opt_in(&spec);
        assert_eq!(warnings.len(), 2);
    }

    // `state { fields }` sugar must expose Map-typed fields to
    // `check_map_and_subscript` — otherwise `subscript_not_map` fires on
    // every effect LHS that subscripts a sugared Map field.
    #[test]
    fn state_sugar_map_field_is_visible_to_subscript_lint() {
        let src = r#"
    spec Probe
    const MAX = 8
    type User = { active : Bool, balance : U64, }
    state {
      lsts : Map[MAX] User,
    }
    type Error
      | InvalidAmount
    handler deposit (idx : U64) (amt : U64) {
      effect { lsts[idx].balance := amt }
    }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_map_and_subscript(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "subscript_not_map"),
            "spurious subscript_not_map on `state {{ ... }}` sugar: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ----- missing_math_overflow lint -----

    #[test]
    fn missing_math_overflow_fires_when_checked_arith_used_without_declaration() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | InvalidAmount

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
            .find(|w| w.rule == "missing_math_overflow")
            .expect("expected missing_math_overflow warning");
        assert!(hit.message.contains("deposit"));
        assert!(hit.message.contains("PoolError::MathOverflow"));
    }

    #[test]
    fn missing_math_overflow_silent_when_variant_is_declared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow | InvalidAmount

    handler deposit (n : U64) : State.Active -> State.Active {
      permissionless
      effect { balance += n }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "should not warn when MathOverflow is declared in Error sum"
        );
    }

    #[test]
    fn missing_math_overflow_silent_when_no_checked_arithmetic() {
        // Spec uses only `effect { x := ... }` (set, no overflow path).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Reset
    program_id "11111111111111111111111111111111"
    type State | Active of { counter : U64 }
    type Error | InvalidAmount

    handler clear : State.Active -> State.Active {
      permissionless
      effect { counter := 0 }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "no checked arith → no MathOverflow obligation"
        );
    }

    // ----- -= raises MathUnderflow (with back-compat) -----

    #[test]
    fn missing_math_overflow_fires_on_sub_without_underflow_or_overflow() {
        // Pure `-=` with neither MathOverflow nor MathUnderflow declared
        // → fires for MathUnderflow (the default for `-=`).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | InvalidAmount

    handler withdraw (n : U64) : State.Active -> State.Active {
      permissionless
      effect { balance -= n }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "missing_math_overflow")
            .expect("expected missing_math_overflow warning for MathUnderflow");
        assert!(
            hit.message.contains("MathUnderflow"),
            "v2.24: `-=` defaults to MathUnderflow; message was {:?}",
            hit.message
        );
    }

    #[test]
    fn missing_math_overflow_silent_on_sub_with_only_overflow_declared() {
        // Back-compat: declared MathOverflow but not MathUnderflow →
        // `-=` falls back to MathOverflow; lint stays silent.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow

    handler withdraw (n : U64) : State.Active -> State.Active {
      permissionless
      effect { balance -= n }
    }
    "#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_math_overflow"),
            "back-compat: only MathOverflow declared → -= falls back; no warning"
        );
    }

    /// ref_impl with multiplication over U64 params trips the lint: Lean
    /// lowers to `Nat` (no overflow); Rust runs `u64 * u64` which can wrap
    /// or panic.
    #[test]
    fn ref_impl_with_multiplication_over_u64_fires_unbounded_arith_lint() {
        let src = r#"spec Pool
    type Error | InvalidAmount
    type State = { x : U64 }

    ref_impl scaled (a : U64) (b : U64) : U64 = a * b

    handler set (amt : U64) {
      requires amt > 0 else InvalidAmount
      effect { x := amt }
    }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_ref_impl_unbounded_arith(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"
                    && w.subject.as_deref() == Some("scaled")),
            "expected ref_impl_unbounded_arith on `scaled`; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
    }

    /// Pure-division ref_impl doesn't trip the lint — `/` cannot produce
    /// values exceeding the inputs in unsigned arithmetic.
    #[test]
    fn ref_impl_with_division_only_does_not_fire_unbounded_arith_lint() {
        let src = r#"spec Pool
    type Error | InvalidAmount
    type State = { x : U64 }

    ref_impl half (a : U64) : U64 = a / 2

    handler set (amt : U64) {
      requires amt > 0 else InvalidAmount
      effect { x := amt }
    }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_ref_impl_unbounded_arith(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"),
            "lint should not fire on division-only ref_impl; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
    }

    /// Ref impls without bounded-numeric params (e.g., Pubkey predicates)
    /// don't trip the lint even when they do arithmetic on other inputs.
    /// Lean and Rust agree on Bool / Pubkey semantics, so no gap.
    #[test]
    fn ref_impl_with_no_numeric_params_does_not_fire_unbounded_arith_lint() {
        let src = r#"spec Pool
    type Error | InvalidAmount
    type State = { admin : Pubkey }

    ref_impl is_admin (who : Pubkey) (admin : Pubkey) : Bool = who == admin

    handler set (amt : U64) {
      requires amt > 0 else InvalidAmount
      effect {}
    }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_ref_impl_unbounded_arith(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "ref_impl_unbounded_arith"),
            "lint should not fire when ref_impl has no bounded-numeric IO; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_missing_guard_from_takes_fires() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "missing_guard_from_takes"),
            "expected missing_guard_from_takes, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_guard_from_takes_skips_when_guard_exists() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "amount > 0".to_string(),
            ..Default::default()
        });
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "missing_guard_from_takes"),
            "should not fire when guard exists"
        );
    }

    #[test]
    fn unguarded_arithmetic_accepts_cumulative_bound_across_multiple_adds() {
        // A single `requires state.x + a + b <= U64_MAX` logically bounds
        // both `state.x += a` and `state.x += b`; the lint must accept the
        // cumulative form, not just per-pair patterns.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow

    handler deposit (a : U64) (b : U64) : State.Active -> State.Active {
      permissionless
      requires state.balance + a + b <= U64_MAX
      effect {
        balance += a
        balance += b
      }
    }
    "#,
        )
        .expect("cumulative-bound spec must parse");
        let warnings = check_completeness(&spec);
        let arith_hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_arithmetic")
            .collect();
        assert!(
            arith_hits.is_empty(),
            "cumulative bound should satisfy unguarded_arithmetic for all adds; got: {arith_hits:#?}"
        );
    }

    #[test]
    fn u64_max_builtin_resolves_in_requires_clause() {
        // `U64_MAX` (and friends) are seeded as builtin consts so users
        // don't have to declare `const U64_MAX = …` per spec.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Pool
    program_id "11111111111111111111111111111111"
    type State | Active of { balance : U64 }
    type Error | MathOverflow

    handler deposit (n : U64) : State.Active -> State.Active {
      permissionless
      requires state.balance + n <= U64_MAX
      effect { balance += n }
    }
    "#,
        )
        .expect("U64_MAX should resolve as a builtin");
        let warnings = check_completeness(&spec);
        // With the U64_MAX guard, unguarded_arithmetic should be silent.
        assert!(
            !warnings.iter().any(|w| w.rule == "unguarded_arithmetic"),
            "U64_MAX builtin should satisfy unguarded_arithmetic; got: {warnings:#?}"
        );
    }
}
