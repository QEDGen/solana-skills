//! Proptest codegen — emits `programs/tests/proptest.rs`: Tier-1 property
//! harnesses for the spec's state machine (~100ms counterexamples; lighter
//! than Kani BMC). Snapshot-gated by `tests/proptest_snapshot.rs`.
//!
//! Effect-body lowering is MIR-driven via `rust_codegen_util::stmt_effect_triple`
//! (a new `Stmt` variant is a compile error at this backend); the harness
//! sub-emitters read `ParsedSpec` directly — they derive from the
//! account/property/requires surface, not effect-body IR.

use anyhow::Result;
use std::path::Path;

use crate::check::{ParsedHandler, ParsedInvariant, ParsedProperty, ParsedSpec};
use crate::codegen_shared::{map_type, write_generated_file, DslTypeExt};
use crate::mir::Mir;
use crate::obligations::{
    ObligationBackend, ObligationEntry, ObligationKind, ObligationRecorder, UnsupportedReason,
};
use crate::rust_codegen_util;

/// Small typed syntax for proptest strategy expressions. Strategy selection
/// decides the shape; this renderer owns calls, ranges, macro alternatives,
/// and method chaining so nested strategies are not assembled by interpolating
/// already-rendered Rust fragments.
#[derive(Clone, Debug, PartialEq, Eq)]
enum StrategyExpr {
    Atom(String),
    Range {
        start: String,
        end: Option<String>,
        inclusive: bool,
    },
    Call {
        callee: String,
        args: Vec<StrategyExpr>,
    },
    OneOf(Vec<StrategyExpr>),
    Method {
        receiver: Box<StrategyExpr>,
        method: String,
        args: Vec<StrategyExpr>,
    },
}

impl StrategyExpr {
    fn atom(value: impl Into<String>) -> Self {
        Self::Atom(value.into())
    }

    fn range(start: impl Into<String>, end: impl Into<String>) -> Self {
        Self::Range {
            start: start.into(),
            end: Some(end.into()),
            inclusive: true,
        }
    }

    fn half_open_range(start: impl Into<String>, end: impl Into<String>) -> Self {
        Self::Range {
            start: start.into(),
            end: Some(end.into()),
            inclusive: false,
        }
    }

    fn range_from(start: impl Into<String>) -> Self {
        Self::Range {
            start: start.into(),
            end: None,
            inclusive: false,
        }
    }

    fn call(callee: impl Into<String>, args: Vec<Self>) -> Self {
        Self::Call {
            callee: callee.into(),
            args,
        }
    }

    fn one_of(choices: Vec<Self>) -> Self {
        Self::OneOf(choices)
    }

    fn method(self, method: impl Into<String>, args: Vec<Self>) -> Self {
        Self::Method {
            receiver: Box::new(self),
            method: method.into(),
            args,
        }
    }

    fn render(&self) -> String {
        match self {
            Self::Atom(value) => value.clone(),
            Self::Range {
                start,
                end,
                inclusive,
            } => match (end, inclusive) {
                (Some(end), true) => format!("{start}..={end}"),
                (Some(end), false) => format!("{start}..{end}"),
                (None, _) => format!("{start}.."),
            },
            Self::Call { callee, args } => format!(
                "{callee}({})",
                args.iter().map(Self::render).collect::<Vec<_>>().join(", ")
            ),
            Self::OneOf(choices) => format!(
                "prop_oneof![{}]",
                choices
                    .iter()
                    .map(Self::render)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Method {
                receiver,
                method,
                args,
            } => format!(
                "{}.{method}({})",
                receiver.render(),
                args.iter().map(Self::render).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

impl std::fmt::Display for StrategyExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// Generate the proptest harness file at `output_path`.
pub fn generate(mir: &Mir, parsed: &ParsedSpec, output_path: &Path) -> Result<()> {
    generate_with_obligations(mir, parsed, output_path).map(|_| ())
}

/// `generate` + the backend-obligation record (#332).
pub fn generate_with_obligations(
    mir: &Mir,
    parsed: &ParsedSpec,
    output_path: &Path,
) -> Result<Vec<ObligationEntry>> {
    if parsed.handlers.is_empty() {
        anyhow::bail!("No operations found in the spec — is this a valid qedspec file?");
    }
    let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
    generate_impl(mir, parsed, Some(output_path), &mut rec)?;
    Ok(rec.into_entries())
}

/// Obligation collection without artifact generation: run the render with
/// a recorder and discard the output. Used by `check --coverage` and
/// `verify --strict`, which must not write files.
pub fn collect_obligations(mir: &Mir, parsed: &ParsedSpec) -> Vec<ObligationEntry> {
    let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
    let _ = generate_impl(mir, parsed, None, &mut rec);
    rec.into_entries()
}

/// Proptest strategy for a primitive scalar `Ty`. `None` means the type
/// has no primitive strategy — the structural dispatch in
/// `strategy_for_ty` owns those shapes, and unhandled ones are explicit
/// errors, never a numeric default (#330: the old string-keyed match
/// here ended `_ => 0u64..=u64::MAX`, silently sampling the wrong
/// domain). No `_` arm: a new `Ty` variant is a compile error here.
fn strategy_for_prim(ty: &crate::mir::Ty) -> Option<StrategyExpr> {
    use crate::mir::Ty;
    Some(match ty {
        Ty::U8 => StrategyExpr::range("0u8", "255u8"),
        Ty::U16 => StrategyExpr::range("0u16", "u16::MAX"),
        Ty::U32 => StrategyExpr::range("0u32", "u32::MAX"),
        Ty::U64 => StrategyExpr::range("0u64", "u64::MAX"),
        Ty::U128 => StrategyExpr::range("0u128", "u128::MAX"),
        Ty::I8 => StrategyExpr::range("i8::MIN", "i8::MAX"),
        Ty::I16 => StrategyExpr::range("i16::MIN", "i16::MAX"),
        Ty::I32 => StrategyExpr::range("i32::MIN", "i32::MAX"),
        Ty::I64 => StrategyExpr::range("i64::MIN", "i64::MAX"),
        Ty::I128 => StrategyExpr::call("any::<i128>", Vec::new()),
        Ty::Bool => StrategyExpr::call("any::<bool>", Vec::new()),
        Ty::Pubkey => StrategyExpr::call(
            "prop::array::uniform32",
            vec![StrategyExpr::range_from("0u8")],
        ),
        // Byte tokens (#191): `uniform32` maxes out at 32, so Bytes64 uses
        // proptest's const-generic array `Arbitrary` (proptest ≥ 1.0).
        Ty::Bytes32 => StrategyExpr::call(
            "prop::array::uniform32",
            vec![StrategyExpr::range_from("0u8")],
        ),
        Ty::Bytes64 => StrategyExpr::call("any::<[u8; 64]>", Vec::new()),
        Ty::Fin { .. } | Ty::Vec { .. } | Ty::Option { .. } | Ty::Map { .. } | Ty::Custom(_) => {
            return None
        }
    })
}

/// Boundary-biased primitive strategy for guard rejection tests: mixes
/// near-0 and near-MAX values so both `> 0` and `<= LARGE_CONST` guards
/// reject often. Same `None` contract as `strategy_for_prim`.
fn boundary_strategy_for_prim(ty: &crate::mir::Ty) -> Option<StrategyExpr> {
    use crate::mir::Ty;
    let edge_ranges = |min: &str, low: &str, high: &str, max: &str| {
        StrategyExpr::one_of(vec![
            StrategyExpr::range(min, low),
            StrategyExpr::range(high, max),
        ])
    };
    Some(match ty {
        Ty::U8 => edge_ranges("0u8", "3u8", "252u8", "255u8"),
        Ty::U16 => edge_ranges("0u16", "3u16", "(u16::MAX - 3)", "u16::MAX"),
        Ty::U32 => edge_ranges("0u32", "3u32", "(u32::MAX - 3)", "u32::MAX"),
        Ty::U64 => edge_ranges("0u64", "3u64", "(u64::MAX - 3)", "u64::MAX"),
        Ty::U128 => edge_ranges("0u128", "3u128", "(u128::MAX - 3)", "u128::MAX"),
        Ty::I8 => edge_ranges("i8::MIN", "(i8::MIN + 3)", "(i8::MAX - 3)", "i8::MAX"),
        Ty::I16 => edge_ranges("i16::MIN", "(i16::MIN + 3)", "(i16::MAX - 3)", "i16::MAX"),
        Ty::I32 => edge_ranges("i32::MIN", "(i32::MIN + 3)", "(i32::MAX - 3)", "i32::MAX"),
        Ty::I64 => edge_ranges("i64::MIN", "(i64::MIN + 3)", "(i64::MAX - 3)", "i64::MAX"),
        Ty::I128 => StrategyExpr::call("any::<i128>", Vec::new()),
        Ty::Bool => StrategyExpr::call("any::<bool>", Vec::new()),
        Ty::Pubkey | Ty::Bytes32 => StrategyExpr::call(
            "prop::array::uniform32",
            vec![StrategyExpr::half_open_range("0u8", "1u8")],
        ),
        Ty::Bytes64 => StrategyExpr::call(
            "prop::collection::vec",
            vec![
                StrategyExpr::half_open_range("0u8", "1u8"),
                StrategyExpr::atom("64"),
            ],
        )
        .method(
            "prop_map",
            vec![StrategyExpr::atom("|v| <[u8; 64]>::try_from(v).unwrap()")],
        ),
        Ty::Fin { .. } | Ty::Vec { .. } | Ty::Option { .. } | Ty::Map { .. } | Ty::Custom(_) => {
            return None
        }
    })
}

/// Rust literal-suffix type for a primitive numeric `Ty` (bound-capped
/// strategies interpolate it into range endpoints).
fn prim_rust_suffix(ty: &crate::mir::Ty) -> Option<&'static str> {
    use crate::mir::Ty;
    match ty {
        Ty::U8 => Some("u8"),
        Ty::U16 => Some("u16"),
        Ty::U32 => Some("u32"),
        Ty::U64 => Some("u64"),
        Ty::U128 => Some("u128"),
        Ty::I8 => Some("i8"),
        Ty::I16 => Some("i16"),
        Ty::I32 => Some("i32"),
        Ty::I64 => Some("i64"),
        Ty::I128 => Some("i128"),
        Ty::Bool
        | Ty::Pubkey
        | Ty::Bytes32
        | Ty::Bytes64
        | Ty::Fin { .. }
        | Ty::Vec { .. }
        | Ty::Option { .. }
        | Ty::Map { .. }
        | Ty::Custom(_) => None,
    }
}

/// Per-field strategy dispatch: `Map[N] T` → strict-length vec + try_into;
/// records / unit-variant sums → `arb_<Name>()`; primitives fall through to
/// `strategy_for_type` / `boundary_strategy_for_type`.
fn strategy_for_field(
    dsl_type: &str,
    spec: &ParsedSpec,
    mode: StrategyMode,
    field_bound: Option<&str>,
) -> Result<StrategyExpr> {
    let dsl_type = dsl_type.trim();
    // Type alias: resolve transitively, then dispatch structurally.
    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return strategy_for_field(rhs, spec, mode, field_bound);
    }
    strategy_for_ty(&crate::mir::parse_ty(dsl_type), spec, mode, field_bound)
}

/// Structural strategy dispatch over the canonical type IR (#330).
/// Exhaustive over `Ty` with no numeric fallback: every shape either
/// generates a correctly-typed strategy or fails with an explicit error
/// before any artifact is written.
fn strategy_for_ty(
    ty: &crate::mir::Ty,
    spec: &ParsedSpec,
    mode: StrategyMode,
    field_bound: Option<&str>,
) -> Result<StrategyExpr> {
    use crate::mir::Ty;
    match ty {
        // Map[BOUND] T → strict-length Vec<T> → [T; N] via TryInto.
        // proptest's `prop::array::uniform*` combinators only go up to 32;
        // the vec-with-prop_map form works for any N.
        Ty::Map { capacity, value } => {
            let n = spec.resolve_map_bound(capacity)?;
            let inner_strategy = strategy_for_ty(value, spec, mode, None)?;
            Ok(StrategyExpr::call(
                "prop::collection::vec",
                vec![
                    inner_strategy,
                    StrategyExpr::range(n.to_string(), n.to_string()),
                ],
            )
            .method(
                "prop_map",
                vec![StrategyExpr::atom("|v| v.try_into().ok().unwrap()")],
            ))
        }
        // Fin[N] → exactly [0, N) after resolving the bound — never a
        // value outside the declared domain (#330: the old path sampled
        // a hard-coded 0..=1024 regardless of N).
        Ty::Fin { bound } => {
            let n: usize = spec.resolve_map_bound(bound)?.parse().map_err(|_| {
                anyhow::anyhow!("Fin bound `{}` did not resolve to a numeric value", bound)
            })?;
            Ok(match mode {
                StrategyMode::Full => StrategyExpr::half_open_range("0usize", format!("{n}usize")),
                StrategyMode::Boundary => {
                    if n <= 8 {
                        StrategyExpr::half_open_range("0usize", format!("{n}usize"))
                    } else {
                        StrategyExpr::one_of(vec![
                            StrategyExpr::range("0usize", "3usize"),
                            StrategyExpr::half_open_range(
                                format!("{}usize", n - 4),
                                format!("{n}usize"),
                            ),
                        ])
                    }
                }
            })
        }
        Ty::Option { value } => {
            let inner_strategy = strategy_for_ty(value, spec, mode, None)?;
            Ok(StrategyExpr::call("prop::option::of", vec![inner_strategy]))
        }
        // No bound policy exists for `Vec` in the DSL — an unbounded
        // strategy would be a wrong-domain model, so this is an explicit
        // capability error, not a default (#330).
        Ty::Vec { .. } => anyhow::bail!(
            "proptest cannot generate a strategy for a `Vec` field: the DSL \
             has no length-bound policy yet. Model a bounded collection as \
             `Map[N] T`."
        ),
        Ty::Custom(name) => {
            // Record type → arb_<Name>() (emit_record_prop_composes).
            if spec.records.iter().any(|r| &r.name == name) {
                return Ok(StrategyExpr::call(format!("arb_{}", name), Vec::new()));
            }
            // Unit-variant sum type → arb_<Name>() (emit_unit_sum_prop_oneofs).
            // Payload-variant sums are flattened into the State struct and
            // never appear as field types.
            if spec.sum_types.iter().any(|s| {
                &s.name == name
                    && !s.variants.is_empty()
                    && s.variants.iter().all(|v| v.fields.is_empty())
            }) {
                return Ok(StrategyExpr::call(format!("arb_{}", name), Vec::new()));
            }
            // Aliases nested inside compound types reach here unresolved
            // (top-level aliases resolve in `strategy_for_field`).
            if spec.type_aliases.iter().any(|(n, _)| n == name) {
                return strategy_for_field(name, spec, mode, field_bound);
            }
            anyhow::bail!(
                "no proptest strategy for type `{}` — it is not a built-in, \
                 record, unit sum type, or alias (`qedgen check` reports this \
                 as unknown_type)",
                name
            )
        }
        Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::U128
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::I128
        | Ty::Bool
        | Ty::Pubkey
        | Ty::Bytes32
        | Ty::Bytes64 => {
            // Primitive path — apply any bound extracted from property
            // expressions before falling to the full-domain strategy.
            if let Some(bound) = field_bound {
                if let Some(rust_type) = prim_rust_suffix(ty) {
                    return Ok(match mode {
                        StrategyMode::Boundary => {
                            let n: u128 = bound.parse().unwrap_or(u128::MAX);
                            if n < 3 {
                                StrategyExpr::range(
                                    format!("0{rust_type}"),
                                    format!("{bound}{rust_type}"),
                                )
                            } else {
                                StrategyExpr::one_of(vec![
                                    StrategyExpr::range(
                                        format!("0{rust_type}"),
                                        format!("3{rust_type}"),
                                    ),
                                    StrategyExpr::range(
                                        format!("({bound} - 3)"),
                                        format!("{bound}{rust_type}"),
                                    ),
                                ])
                            }
                        }
                        StrategyMode::Full => StrategyExpr::range(
                            format!("0{rust_type}"),
                            format!("{bound}{rust_type}"),
                        ),
                    });
                }
            }
            let strategy = match mode {
                StrategyMode::Boundary => boundary_strategy_for_prim(ty),
                StrategyMode::Full => strategy_for_prim(ty),
            };
            Ok(strategy.expect("primitive arm covers exactly the prim-strategy domain"))
        }
    }
}

/// Emit a `prop_compose!` strategy per spec record. Order matters: after
/// `emit_record_structs`, before `emit_state_strategy` (which references
/// `arb_<Name>()`).
fn emit_record_prop_composes(out: &mut String, spec: &ParsedSpec) -> Result<()> {
    for rec in &spec.records {
        if rec.fields.is_empty() {
            continue;
        }
        // The flat-state `State` record becomes the state-machine struct with
        // its own `arb_state()`; skip to avoid a colliding `arb_State()`.
        // Mirrors `rust_codegen_util::emit_record_structs`.
        if rec.name == "State" {
            continue;
        }
        out.push_str("prop_compose! {\n");
        out.push_str(&format!("    fn arb_{}()(", rec.name));
        for (i, (fname, ftype)) in rec.fields.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let strategy = strategy_for_field(ftype, spec, StrategyMode::Full, None)?;
            out.push_str(&format!("{fname} in {strategy}"));
        }
        out.push_str(&format!(") -> {} {{\n", rec.name));
        out.push_str(&format!("        {} {{\n", rec.name));
        for (fname, _) in &rec.fields {
            out.push_str(&format!("            {fname},\n"));
        }
        out.push_str("        }\n    }\n");
        out.push_str("}\n\n");
    }
    Ok(())
}

/// Emit a `prop_oneof!` strategy per unit-variant sum type. Payload-variant
/// sums are skipped — they're flattened into the State struct.
fn emit_unit_sum_prop_oneofs(out: &mut String, spec: &ParsedSpec) -> Result<()> {
    for sum in &spec.sum_types {
        let all_unit = sum.variants.iter().all(|v| v.fields.is_empty());
        if !all_unit || sum.variants.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "fn arb_{}() -> impl Strategy<Value = {}> {{\n",
            sum.name, sum.name
        ));
        out.push_str("    prop_oneof![\n");
        for variant in &sum.variants {
            out.push_str(&format!("        Just({}::{}),\n", sum.name, variant.name));
        }
        out.push_str("    ]\n}\n\n");
    }
    Ok(())
}

/// Return the Rust type max value for overflow testing.
fn type_max(dsl_type: &str) -> Option<&str> {
    match dsl_type {
        "U8" => Some("u8::MAX"),
        "U16" => Some("u16::MAX"),
        "U32" => Some("u32::MAX"),
        "U64" => Some("u64::MAX"),
        "U128" => Some("u128::MAX"),
        _ => None,
    }
}

/// Extract constant upper bounds for state fields from property expressions.
/// E.g., `state.V <= MAX_VAULT_TVL` where MAX_VAULT_TVL is a known constant yields
/// `("V", "10000000000000000")`. Used to cap arb_state() ranges.
fn extract_field_upper_bounds(
    properties: &[&ParsedProperty],
    constants: &[(String, String)],
) -> std::collections::HashMap<String, String> {
    let mut bounds = std::collections::HashMap::new();
    for prop in properties {
        if let Some(ref expr) = prop.expression {
            // Match patterns like "state.FIELD <= CONST" or "state.FIELD ≤ NUMBER"
            // Split on "and" / "∧" to handle conjunctive properties
            let parts_iter: Vec<&str> = expr.split(" and ").flat_map(|p| p.split('∧')).collect();
            for part in parts_iter {
                let part = part.trim();
                if let Some(rest) = part.strip_suffix(")").or(Some(part)) {
                    for op in &[" ≤ ", " <= "] {
                        if let Some(pos) = rest.find(op) {
                            let lhs = rest[..pos].trim();
                            let rhs = rest[pos + op.len()..].trim();
                            if let Some(field) = lhs
                                .strip_prefix("state.")
                                .or_else(|| lhs.strip_prefix("s."))
                            {
                                // Check if RHS is a constant name or a number
                                let resolved = constants
                                    .iter()
                                    .find(|(n, _)| n == rhs)
                                    .map(|(_, v)| v.replace('_', ""))
                                    .or_else(|| {
                                        let clean = rhs.replace('_', "");
                                        if clean.chars().all(|c| c.is_ascii_digit()) {
                                            Some(clean)
                                        } else {
                                            None
                                        }
                                    });
                                if let Some(val) = resolved {
                                    bounds.insert(field.to_string(), val);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    bounds
}

/// Generate proptest harnesses: random-input state-machine tests checking
/// invariants after every transition.
fn generate_impl(
    mir: &Mir,
    spec: &ParsedSpec,
    output_path: Option<&Path>,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    rust_codegen_util::check_effect_targets(spec)?;

    let fp = crate::fingerprint::compute_fingerprint(spec);

    let is_multi = spec.account_types.len() > 1;

    let mut out = String::new();

    out.push_str(&crate::codegen_shared::marker_unlabeled(
        &fp,
        "tests/proptest.rs",
    ));
    out.push_str("//\n");
    out.push_str("// Proptest harnesses — property-based testing for the spec's state machine.\n");
    out.push_str(
        "// Tier 1 of the verification waterfall: finds counterexamples in milliseconds.\n",
    );
    out.push_str("//\n");
    out.push_str("//   Proptest: random testing, fast counterexamples (~100ms)\n");
    out.push_str("//   Kani:     bounded model checking, exhaustive within bounds (~5-30s)\n");
    out.push_str("//   Lean:     mathematical proof, universal guarantees (minutes-hours)\n");
    out.push_str("//\n");
    out.push_str("// To run:  cargo test --test proptest\n");
    out.push_str("// Deep:    PROPTEST_CASES=10000 cargo test --test proptest\n");
    out.push_str(
        "// ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ---- ----\n\n",
    );

    // proptest's nested `TupleValueTree<...>` instantiations overflow rustc's
    // default recursion_limit=128 when `arb_state()` composes ≳40 fields
    // ("queries overflow the depth limit"). Emit a 512 override (validated
    // against a 99-field flat State) only above 32 fields so small specs
    // keep the rustc default; bigger specs fail with the same clear
    // diagnostic and can override locally.
    let total_field_count: usize = rust_codegen_util::field_refs(&spec.state_fields).len()
        + spec
            .account_types
            .iter()
            .map(|a| rust_codegen_util::field_refs(&a.fields).len())
            .sum::<usize>();
    if total_field_count > 32 {
        out.push_str("#![recursion_limit = \"512\"]\n\n");
    }

    out.push_str("use proptest::prelude::*;\n\n");

    // Brownfield `--proptest` (no `--all`) never generates src/math.rs, so
    // `mul_div_*_u128` calls emitted by expr_to_rust would be undefined.
    // Inline the helpers ONLY when the spec uses them — unconditional
    // inlining would ship a second source of truth with silent-divergence
    // risk. Detection reuses `codegen::guards_use_math_helpers`, the same
    // predicate as the `--all` math.rs emission.
    if crate::codegen_shared::guards_use_math_helpers(spec) {
        out.push_str(
            "#[allow(dead_code)]\n\
#[inline]\n\
fn mul_div_floor_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 { return 0; }\n\
    a.saturating_mul(b) / d\n\
}\n\n\
#[allow(dead_code)]\n\
#[inline]\n\
fn mul_div_ceil_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 { return 0; }\n\
    let prod = a.saturating_mul(b);\n\
    if prod % d == 0 { prod / d } else { (prod / d).saturating_add(1) }\n\
}\n\n",
        );
        out.push_str(
            "#[allow(dead_code)]\n\
#[inline]\n\
fn mul_div_round_half_up_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 { return 0; }\n\
    let prod = a.saturating_mul(b);\n\
    let q = prod / d;\n\
    let r = prod % d;\n\
    let threshold = d / 2 + d % 2;\n\
    if r >= threshold { q.saturating_add(1) } else { q }\n\
}\n\n",
        );
    }

    rust_codegen_util::emit_constants(&mut out, &spec.constants);

    if is_multi {
        // #331 — spec-global ghosts. When no per-account transition reads
        // or writes a ghost ("liftable"), the ghost moves to the product
        // state module: the
        // per-account sections drop it entirely (fields, strategies, and
        // the shared transition emitter ghost updates), and `mod
        // product` carries the single global value, updated atomically by
        // the transition wrappers. Guard-ghost specs keep the Kani-parity
        // per-account ghost copy so the artifact still compiles, and every
        // ghost-reading property obligation stays reported unsupported.
        let ghosts_liftable = rust_codegen_util::multi_account_ghosts_liftable(spec);
        let section_spec_owned: Option<ParsedSpec> = if ghosts_liftable && !spec.ghosts.is_empty() {
            let mut ghostless = spec.clone();
            ghostless.ghosts.clear();
            Some(ghostless)
        } else {
            None
        };
        let section_spec: &ParsedSpec = section_spec_owned.as_ref().unwrap_or(spec);

        if !ghosts_liftable {
            for prop in &spec.properties {
                let Some(rust) = &prop.rust_expression else {
                    continue;
                };
                if spec.ghosts.iter().any(|g| references_field(rust, &g.name)) {
                    for op_name in &prop.preserved_by {
                        rec.unsupported(
                            ObligationKind::PropertyPreservation,
                            op_name,
                            &prop.name,
                            UnsupportedReason::ProptestMultiAccountGhost,
                        );
                    }
                }
            }
        }

        // Multi-account: generate per-account sections in separate modules
        let mut components: Vec<ProptestComponent> = Vec::new();
        for acct in &spec.account_types {
            let acct_fields_owned: Vec<(String, String)> = if ghosts_liftable {
                acct.fields.clone()
            } else {
                // Guard-ghost shape: chain the ghost into every account
                // State (Kani parity) so guard reads compile. Duplicated
                // values are why this stays unsupported in the manifest.
                acct.fields
                    .iter()
                    .cloned()
                    .chain(spec.ghosts.iter().map(|g| (g.name.clone(), g.ty.clone())))
                    .collect()
            };
            let acct_fields = rust_codegen_util::field_refs(&acct_fields_owned);
            if rust_codegen_util::field_refs(&acct.fields).is_empty() {
                rec.unsupported(
                    ObligationKind::AccountModel,
                    &acct.name,
                    &acct.name,
                    UnsupportedReason::AccountHasNoFields,
                );
                continue;
            }
            let acct_handlers: Vec<&ParsedHandler> = spec
                .handlers
                .iter()
                .filter(|h| h.on_account.as_deref() == Some(&acct.name))
                .collect();
            if acct_handlers.is_empty() {
                rec.unsupported(
                    ObligationKind::AccountModel,
                    &acct.name,
                    &acct.name,
                    UnsupportedReason::AccountHasNoHandlers,
                );
                continue;
            }
            let acct_field_names: Vec<&str> = acct_fields.iter().map(|(n, _)| n.as_str()).collect();
            let acct_props: Vec<&ParsedProperty> = spec
                .properties
                .iter()
                .filter(|p| {
                    let scoped = if let Some(ref expr) = p.expression {
                        acct_field_names.iter().any(|f| expr.contains(f))
                    } else {
                        false
                    };
                    // Liftable ghosts live only in the product state — a
                    // ghost-reading predicate cannot compile against the
                    // ghost-free per-account State.
                    let reads_ghost = ghosts_liftable
                        && spec
                            .ghosts
                            .iter()
                            .any(|g| property_references_field(p, &g.name));
                    scoped && !reads_ghost
                })
                .collect();

            let mod_name = acct.name.to_lowercase();
            out.push_str(&format!("mod {} {{\n", mod_name));
            out.push_str("    use super::*;\n\n");

            emit_account_section(
                &mut out,
                mir,
                &acct.name,
                &acct_fields,
                &acct_fields_owned,
                &acct_handlers,
                &acct_props,
                &acct.lifecycle,
                section_spec,
                rust_codegen_util::VIS_PUB,
                rec,
            )?;

            out.push_str(&format!("}} // mod {}\n\n", mod_name));

            components.push(ProptestComponent {
                acct: acct.clone(),
                mod_name,
                handler_names: acct_handlers.iter().map(|h| h.name.clone()).collect(),
            });
        }

        emit_product_module(&mut out, spec, &components, ghosts_liftable, rec)?;
    } else {
        // Single-account: generate flat (no module wrapper).
        // Ghosts are spec-only verification-State fields: present in the
        // harness State + `arb_state` + transitions so properties can read
        // them, but NEVER in on-chain codegen (reads `spec.state_fields`).
        let state_fields_owned: Vec<(String, String)> = spec
            .state_fields
            .iter()
            .cloned()
            .chain(spec.ghosts.iter().map(|g| (g.name.clone(), g.ty.clone())))
            .collect();
        let state_fields: &[(String, String)] = &state_fields_owned;
        let mutable_fields = rust_codegen_util::field_refs(state_fields);
        let all_handlers: Vec<&ParsedHandler> = spec.handlers.iter().collect();
        let all_props: Vec<&ParsedProperty> = spec.properties.iter().collect();
        emit_account_section(
            &mut out,
            mir,
            &spec.program_name,
            &mutable_fields,
            state_fields,
            &all_handlers,
            &all_props,
            &spec.lifecycle_states,
            spec,
            rust_codegen_util::VIS_PRIVATE,
            rec,
        )?;
    }

    if let Some(output_path) = output_path {
        write_generated_file(output_path, &out)?;
        eprintln!("Generated proptest harnesses at {}", output_path.display());
    }
    Ok(())
}

/// Emit a complete test section for one account type (or the single account in non-multi specs).
#[allow(clippy::too_many_arguments)]
fn emit_account_section(
    out: &mut String,
    mir: &Mir,
    _acct_name: &str,
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    handlers: &[&ParsedHandler],
    properties: &[&ParsedProperty],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
    vis: &str,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    // Records/enums referenced by State are declared first, then their
    // `arb_<Name>()` strategies so `arb_state` can call into them. `Default`
    // is required by the seed-state path (`default_value_for_type` emits
    // `<Name>::default()`); a non-Default field type fails at the record
    // struct itself — clearer than a cascading E0599 at the call site.
    rust_codegen_util::emit_record_structs(out, spec, "Debug, Clone, Copy, Default", vis, |t| {
        map_type(t, spec)
    })?;
    rust_codegen_util::emit_unit_enum_sums(out, spec, "Debug, Clone, Copy, PartialEq, Eq", vis)?;
    // Per-account `Status` from the `lifecycle_states` param, NOT
    // `spec.lifecycle_states` — in multi-ADT mode the caller passes
    // `&acct.lifecycle` so each module gets its own variants.
    rust_codegen_util::emit_lifecycle_status_enum_from(
        out,
        lifecycle_states,
        "Debug, Clone, Copy, PartialEq, Eq",
        vis,
    );
    emit_record_prop_composes(out, spec)?;
    emit_unit_sum_prop_oneofs(out, spec)?;

    let section_has_lifecycle = lifecycle_states.len() >= 2;

    // State struct (with synthetic `status: Status` when this section has a
    // multi-state lifecycle). Uses the per-account lifecycle so a single-ADT
    // section without a lifecycle doesn't get a stray `status` field.
    rust_codegen_util::emit_state_struct_with_lifecycle(
        out,
        mutable_fields,
        "Debug, Clone, Copy",
        |t| map_type(t, spec),
        section_has_lifecycle,
        vis,
    )?;

    // Extract constant upper bounds from properties to cap arb_state() ranges.
    // E.g., `state.V <= MAX_VAULT_TVL` caps V to 10^16 instead of u128::MAX.
    let mut field_bounds = extract_field_upper_bounds(properties, &spec.constants);
    if !field_bounds.is_empty() {
        // A relational property like `V >= C_tot + I` is unsatisfiable in
        // random states unless the fields it relates share a compatible
        // upper bound: if `V <= MAX` but `C_tot` samples the full u64
        // domain, `V >= C_tot + I` almost never holds and the harness
        // rejects everything. Propagate each explicit `state.F <= CONST`
        // bound to the OTHER fields in F's connected component — fields
        // transitively linked by co-occurring in a property expression —
        // using that component's own tightest bound. A single global
        // minimum leaks a tight bound from one relational group into an
        // unrelated one (`a <= 10; a >= b` next to `c <= 1000; c >= d`
        // would cap `d` at 10, not 1000); capping every field leaks it
        // further still, onto fields in NO relational property (a type
        // discriminant with its own wide range).
        let names: Vec<&str> = mutable_fields
            .iter()
            .filter(|(_, t)| t.as_str() != "Pubkey")
            .map(|(f, _)| f.as_str())
            .collect();
        // Union-find: join fields that co-occur in a property expression.
        let mut parent: Vec<usize> = (0..names.len()).collect();
        for prop in properties {
            if prop.expression.is_none() {
                continue;
            }
            let mentioned: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|(_, n)| property_references_field(prop, n))
                .map(|(i, _)| i)
                .collect();
            for pair in mentioned.windows(2) {
                let (a, b) = (uf_find(&mut parent, pair[0]), uf_find(&mut parent, pair[1]));
                if a != b {
                    parent[a] = b;
                }
            }
        }
        // Tightest explicit bound per component (numeric compare, not
        // string length — `"9"` is not tighter than `"1000"`).
        let mut comp_bound: std::collections::HashMap<usize, u128> = Default::default();
        for (i, name) in names.iter().enumerate() {
            if let Some(b) = field_bounds.get(*name).and_then(|v| v.parse::<u128>().ok()) {
                let root = uf_find(&mut parent, i);
                let slot = comp_bound.entry(root).or_insert(u128::MAX);
                *slot = (*slot).min(b);
            }
        }
        // Apply each component's bound to its still-unbounded members.
        for (i, name) in names.iter().enumerate() {
            if field_bounds.contains_key(*name) {
                continue;
            }
            let root = uf_find(&mut parent, i);
            if let Some(bound) = comp_bound.get(&root) {
                field_bounds.insert(name.to_string(), bound.to_string());
            }
        }
    }
    // Conservation-invariant repairs for `arb_state` (see `state_repairs`).
    // Source the assumed predicates the preservation tests filter random
    // states against: unary (non-binary) properties and the spec's
    // invariants. Repairing to satisfy them keeps the reject rate low
    // enough that the preservation tests actually run.
    let constraint_trees: Vec<&crate::mir::ExprTree> = properties
        .iter()
        .filter(|p| p.class != crate::check::PropertyClass::Binary)
        .filter_map(|p| p.tree.as_ref())
        .chain(spec.invariants.iter().filter_map(|i| i.tree.as_ref()))
        .collect();
    let repairs = state_repairs(&constraint_trees, mutable_fields);

    emit_state_strategy(
        out,
        mutable_fields,
        all_fields,
        &field_bounds,
        &repairs,
        lifecycle_states,
        spec,
        vis,
    )?;

    // Property predicates — shared emitter with the Kani backend
    // (`rust_codegen_util::emit_property_predicates_with`), wrapping
    // arithmetic (proptest evaluates predicates on arbitrary states).
    let props_with_expr: Vec<&&ParsedProperty> = properties
        .iter()
        .filter(|p| p.expression.is_some())
        .collect();
    let owned_props_for_predicates: Vec<ParsedProperty> =
        properties.iter().map(|p| (*p).clone()).collect();
    rust_codegen_util::emit_property_predicates_with(out, &owned_props_for_predicates, vis, |t| {
        map_type(t, spec)
    });

    // Invariant predicates — only those referenced by at least one handler
    // AND carrying a rust_expr body (not description-only).
    let linked_invs: Vec<&ParsedInvariant> = spec
        .invariants
        .iter()
        .filter(|i| {
            i.rust_expr
                .as_ref()
                .map(|r| !crate::check::rust_expr_is_unsupported(r))
                .unwrap_or(false)
        })
        .filter(|i| {
            handlers
                .iter()
                .any(|h| h.invariants.contains(&i.name) || h.establishes.contains(&i.name))
        })
        .collect();
    rust_codegen_util::emit_invariant_predicates(out, &linked_invs, vis);

    // Transition functions
    emit_transition_functions_for(out, mir, handlers, spec, vis)?;

    // Clone properties once for sections that need owned copies
    let owned_props: Vec<ParsedProperty> = properties.iter().map(|p| (*p).clone()).collect();

    // Computed early because the ghost-property record in
    // `emit_preservation_tests_for` needs to know whether the sequence
    // harness (which is what validates ghost properties) will exist.
    // Must stay in sync with the `want_sequence` gate below.
    let will_emit_sequence = ((!owned_props.is_empty() && handlers.len() > 1)
        || !mir.hooks.is_empty())
        && !handlers.is_empty();

    // Property preservation tests
    if !props_with_expr.is_empty() {
        emit_preservation_tests_for(
            out,
            handlers,
            &owned_props,
            mutable_fields,
            lifecycle_states,
            spec,
            will_emit_sequence,
            rec,
        )?;
    }

    // Invariant preservation tests — one per (handler, invariant) pair,
    // iterated from the handler side (handler.invariants) where the spec
    // records it; properties iterate from prop.preserved_by.
    if !linked_invs.is_empty() {
        emit_invariant_preservation_tests_for(
            out,
            handlers,
            &linked_invs,
            mutable_fields,
            lifecycle_states,
            spec,
            rec,
        )?;
    }

    // Guard enforcement tests
    let guard_ops: Vec<&&ParsedHandler> = handlers.iter().filter(|op| op.has_guard()).collect();
    if !guard_ops.is_empty() {
        let guard_refs: Vec<&ParsedHandler> = guard_ops.iter().map(|op| **op).collect();
        emit_guard_tests(out, &guard_refs, mutable_fields, all_fields, spec, rec)?;
    }

    // Overflow detection tests — the checked-add filter reads the lowered
    // MIR body, not `op.effects`. Deep walk: adds inside `Stmt::Branch` arms
    // count (the `>= pre` assertion holds even when the arm doesn't fire).
    let overflow_ops: Vec<&&ParsedHandler> = handlers
        .iter()
        .filter(|op| {
            mir.handler_block(&op.name).is_some_and(|body| {
                rust_codegen_util::block_effect_triples_deep(body)
                    .iter()
                    .any(|(_, k, _)| *k == "add")
            })
        })
        .collect();
    if !overflow_ops.is_empty() {
        let overflow_refs: Vec<&ParsedHandler> = overflow_ops.iter().map(|op| **op).collect();
        emit_overflow_tests_for(
            out,
            mir,
            &overflow_refs,
            mutable_fields,
            all_fields,
            spec,
            &owned_props,
            rec,
        )?;
    }

    // Sequence test — emitted for multi-handler property checks OR when the
    // spec declares hooks: the harness drives random op sequences from
    // `init`, which is what fires the injected `after_store` assertions.
    let want_sequence = (!owned_props.is_empty() && handlers.len() > 1) || !mir.hooks.is_empty();
    if want_sequence && !handlers.is_empty() {
        rec.emitted(
            ObligationKind::BackendExtra,
            "file",
            "state_machine_sequence",
            "state_machine_sequence",
        );
        emit_sequence_test_for(
            out,
            handlers,
            &owned_props,
            mutable_fields,
            all_fields,
            lifecycle_states,
            spec,
        )?;
    }
    Ok(())
}

/// Direction of a single-field constraint recovered for `arb_state` repair.
#[derive(Clone, Copy, PartialEq)]
enum RepairDir {
    /// `field >= rhs` — raise `field` to at least `rhs`.
    Lower,
    /// `field <= rhs` — lower `field` to at most `rhs`.
    Upper,
    /// `field == rhs` — set `field` to `rhs`.
    Exact,
}

/// A single-field constraint of the conservation shape
/// `field (>=|<=|==) <sum of other fields (+ constants)>`.
struct FieldConstraint {
    field: String,
    dir: RepairDir,
    /// Rust for the right-hand side, a saturating-`+` chain over field names.
    rhs: String,
    deps: std::collections::BTreeSet<String>,
}

/// If `tree` is a bare state/ghost field read (`state.f`, no subscript),
/// return its name.
fn tree_state_field(tree: &crate::mir::ExprTree) -> Option<String> {
    use crate::mir::expr_tree::{BindingKind, TreeSeg};
    let crate::mir::ExprTree::Path(p) = tree else {
        return None;
    };
    if !matches!(p.binding, BindingKind::StateField | BindingKind::Ghost) {
        return None;
    }
    match p.segments.as_slice() {
        [TreeSeg::Field(f)] => Some(f.clone()),
        _ => None,
    }
}

/// Render `tree` as a saturating-`+` chain over bare field names / int
/// literals — the RHS of a conservation constraint. `None` for any shape
/// outside `field` / `int` / `a + b` (so `-`, `*`, subscripts fall through
/// to the reject-sampling path). Collects the referenced field names into
/// `deps`; a field not in `known` (e.g. a subscripted one) makes it `None`.
fn tree_linear_sum(
    tree: &crate::mir::ExprTree,
    known: &std::collections::BTreeSet<String>,
    deps: &mut std::collections::BTreeSet<String>,
) -> Option<String> {
    use crate::mir::expr_tree::TreeArithOp;
    use crate::mir::ExprTree;
    match tree {
        ExprTree::Int(v) => Some(v.to_string()),
        ExprTree::Arith {
            op: TreeArithOp::Add,
            lhs,
            rhs,
        } => {
            let l = tree_linear_sum(lhs, known, deps)?;
            let r = tree_linear_sum(rhs, known, deps)?;
            Some(format!("({l}).saturating_add({r})"))
        }
        _ => {
            let f = tree_state_field(tree)?;
            if !known.contains(&f) {
                return None;
            }
            deps.insert(f.clone());
            Some(f)
        }
    }
}

fn flip_cmp(op: crate::mir::expr_tree::TreeCmpOp) -> crate::mir::expr_tree::TreeCmpOp {
    use crate::mir::expr_tree::TreeCmpOp::*;
    match op {
        Ge => Le,
        Le => Ge,
        Gt => Lt,
        Lt => Gt,
        Eq => Eq,
        Ne => Ne,
    }
}

/// Recover a `field (>=|<=|==) sum` constraint from a comparison, trying
/// both operand orders (`b + c <= a` is `a >= b + c`).
fn tree_field_constraint(
    tree: &crate::mir::ExprTree,
    known: &std::collections::BTreeSet<String>,
) -> Option<FieldConstraint> {
    use crate::mir::expr_tree::TreeCmpOp;
    use crate::mir::ExprTree;
    let ExprTree::Cmp { op, lhs, rhs } = tree else {
        return None;
    };
    // Put the single field on the left, the sum on the right — trying both
    // operand orders (`b + c <= a` is `a >= b + c`).
    let (field, sum_side, op) = match (tree_state_field(lhs), tree_state_field(rhs)) {
        (Some(f), _) => (f, rhs.as_ref(), *op),
        (None, Some(f)) => (f, lhs.as_ref(), flip_cmp(*op)),
        (None, None) => return None,
    };
    let mut deps = std::collections::BTreeSet::new();
    let rhs_str = tree_linear_sum(sum_side, known, &mut deps)?;
    if deps.contains(&field) {
        return None; // self-referential (`a >= a + b`)
    }
    let (dir, rhs) = match op {
        TreeCmpOp::Ge => (RepairDir::Lower, rhs_str),
        TreeCmpOp::Gt => (RepairDir::Lower, format!("({rhs_str}).saturating_add(1)")),
        TreeCmpOp::Le => (RepairDir::Upper, rhs_str),
        TreeCmpOp::Lt => (RepairDir::Upper, format!("({rhs_str}).saturating_sub(1)")),
        TreeCmpOp::Eq => (RepairDir::Exact, rhs_str),
        TreeCmpOp::Ne => return None,
    };
    Some(FieldConstraint {
        field,
        dir,
        rhs,
        deps,
    })
}

/// Split a boolean tree into its top-level `and` conjuncts.
fn tree_conjuncts<'a>(tree: &'a crate::mir::ExprTree, out: &mut Vec<&'a crate::mir::ExprTree>) {
    use crate::mir::expr_tree::TreeBoolOp;
    use crate::mir::ExprTree;
    if let ExprTree::BoolOp {
        op: TreeBoolOp::And,
        lhs,
        rhs,
    } = tree
    {
        tree_conjuncts(lhs, out);
        tree_conjuncts(rhs, out);
    } else {
        out.push(tree);
    }
}

/// Best-effort repair statements for `arb_state`: raise / lower / set
/// fields so the common conservation invariants (`field (>=|<=|==) sum`)
/// hold by construction, instead of `prop_assume` rejecting nearly every
/// random state (tight relational invariants exhaust `max_global_rejects`,
/// so the preservation test aborts having validated nothing).
///
/// Correctness rests on the preservation tests KEEPING their `prop_assume`
/// as the safety net: this only reduces the reject rate, so any shape it
/// can't repair (subscripts, `-`/`*`, mixed `>=`/`<=` on one field, cyclic
/// dependencies) simply reject-samples as before. Returns `(field, rhs)` in
/// dependency order — a field whose RHS reads another repaired field is
/// emitted after it.
fn state_repairs(
    trees: &[&crate::mir::ExprTree],
    mutable_fields: &[&(String, String)],
) -> Vec<(String, String)> {
    use std::collections::{BTreeMap, BTreeSet};
    // Only numeric fields carry `.max()` / `.saturating_add()` repairs.
    let known: BTreeSet<String> = mutable_fields
        .iter()
        .filter(|(_, t)| prim_rust_suffix(&crate::mir::parse_ty(t)).is_some())
        .map(|(n, _)| n.clone())
        .collect();

    let mut per_field: BTreeMap<String, Vec<FieldConstraint>> = BTreeMap::new();
    for tree in trees {
        let mut conjuncts = Vec::new();
        tree_conjuncts(tree, &mut conjuncts);
        for c in conjuncts {
            if let Some(fc) = tree_field_constraint(c, &known) {
                per_field.entry(fc.field.clone()).or_default().push(fc);
            }
        }
    }

    struct Repair {
        expr: String,
        deps: BTreeSet<String>,
    }
    let mut repairs: BTreeMap<String, Repair> = BTreeMap::new();
    for (field, cs) in &per_field {
        if let Some(exact) = cs.iter().find(|c| c.dir == RepairDir::Exact) {
            repairs.insert(
                field.clone(),
                Repair {
                    expr: exact.rhs.clone(),
                    deps: exact.deps.clone(),
                },
            );
            continue;
        }
        let has_lower = cs.iter().any(|c| c.dir == RepairDir::Lower);
        let has_upper = cs.iter().any(|c| c.dir == RepairDir::Upper);
        // A range (both directions) can be infeasible after clamping — leave
        // it to `prop_assume` rather than risk producing a violating state.
        if has_lower && has_upper {
            continue;
        }
        let (method, dir) = if has_lower {
            ("max", RepairDir::Lower)
        } else {
            ("min", RepairDir::Upper)
        };
        let mut expr = field.clone();
        let mut deps = BTreeSet::new();
        for c in cs.iter().filter(|c| c.dir == dir) {
            expr = format!("{expr}.{method}({})", c.rhs);
            deps.extend(c.deps.iter().cloned());
        }
        repairs.insert(field.clone(), Repair { expr, deps });
    }

    // Dependency order (Kahn): emit a field once its repaired deps are done;
    // fields left in a cycle are dropped (they reject-sample).
    let repaired: BTreeSet<String> = repairs.keys().cloned().collect();
    let mut ordered: Vec<(String, String)> = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut progressed = false;
        for (field, r) in &repairs {
            if done.contains(field) {
                continue;
            }
            if r.deps
                .iter()
                .all(|d| !repaired.contains(d) || done.contains(d))
            {
                ordered.push((field.clone(), r.expr.clone()));
                done.insert(field.clone());
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    ordered
}

/// Emit proptest `Arbitrary`-like strategy for State.
#[allow(clippy::too_many_arguments)]
fn emit_state_strategy(
    out: &mut String,
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    field_bounds: &std::collections::HashMap<String, String>,
    repairs: &[(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
    vis: &str,
) -> Result<()> {
    // Full-range strategy (capped by property bounds when available, and
    // repaired to satisfy conservation invariants so preservation tests
    // don't reject-exhaust).
    emit_state_strategy_inner(
        out,
        "arb_state",
        mutable_fields,
        all_fields,
        StrategyMode::Full,
        field_bounds,
        repairs,
        lifecycle_states,
        spec,
        vis,
    )?;
    // Boundary-biased strategy for guard rejection tests — deliberately
    // NOT repaired: guard tests want boundary / invalid states and assume
    // no invariant.
    emit_state_strategy_inner(
        out,
        "arb_boundary_state",
        mutable_fields,
        all_fields,
        StrategyMode::Boundary,
        field_bounds,
        &[],
        lifecycle_states,
        spec,
        vis,
    )?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum StrategyMode {
    Full,
    Boundary,
}

#[allow(clippy::too_many_arguments)]
fn emit_state_strategy_inner(
    out: &mut String,
    fn_name: &str,
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    mode: StrategyMode,
    field_bounds: &std::collections::HashMap<String, String>,
    repairs: &[(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
    vis: &str,
) -> Result<()> {
    match mode {
        StrategyMode::Boundary => {
            out.push_str("/// Boundary-biased strategy for guard rejection tests.\n");
        }
        StrategyMode::Full => {
            out.push_str("/// Proptest strategy for generating arbitrary State values.\n");
        }
    }
    // `prop_compose!` instead of an inline tuple `.prop_map(…)`: proptest's
    // tuple `Strategy` impl caps at arity 12; `prop_compose!` has no limit.
    let emit_status =
        lifecycle_states.len() >= 2 && !mutable_fields.iter().any(|(n, _)| n == "status");
    out.push_str("prop_compose! {\n");
    out.push_str(&format!("    {}fn {}()(\n", vis, fn_name));
    for (fname, _ftype) in mutable_fields.iter() {
        let dsl_type = all_fields
            .iter()
            .find(|(n, _)| n.as_str() == fname.as_str())
            .map(|(_, t)| t.as_str())
            .unwrap_or("U64");
        let bound = field_bounds.get(fname.as_str()).map(|s| s.as_str());
        let strategy = strategy_for_field(dsl_type, spec, mode, bound)?;
        out.push_str(&format!("        {} in {},\n", fname, strategy));
    }
    if emit_status {
        let variants = lifecycle_states
            .iter()
            .map(|s| format!("Just(Status::{})", s))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("        status in prop_oneof![{}],\n", variants));
    }
    out.push_str("    ) -> State {\n");
    // Repair the generated fields so conservation invariants hold by
    // construction (raise/lower/set in dependency order). Shadows the
    // generated binding; `State { field }` shorthand then uses the repaired
    // value. Only `arb_state` (Full) is repaired.
    for (field, rhs) in repairs {
        out.push_str(&format!("        let {field} = {rhs};\n"));
    }
    out.push_str("        State {\n");
    for (fname, _) in mutable_fields {
        out.push_str(&format!("            {},\n", fname));
    }
    if emit_status {
        out.push_str("            status,\n");
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");
    Ok(())
}

/// Emit transition functions. Each effect block iterates the handler's
/// lowered MIR body (`rust_codegen_util::stmt_effect_triple`), not `op.effects`.
///
/// `wrapping: false` — the model must carry the spec's per-effect
/// semantics: default `+=`/`-=` are checked (reject the transition on
/// overflow, like the deployed `checked_add(..).ok_or(err)?`), `+=!`
/// saturates, `+=?` wraps. The old `true` here was a leftover from the
/// pre-checked-default era: it forced default `+=` to wrap AND report
/// success, so every overflow test asserting "success means no wrap"
/// failed on a correct spec (#296; the Kani lane already passed `false`).
fn emit_transition_functions_for(
    out: &mut String,
    mir: &Mir,
    handlers: &[&ParsedHandler],
    spec: &ParsedSpec,
    vis: &str,
) -> Result<()> {
    for op in handlers {
        rust_codegen_util::emit_transition_fn(out, mir, op, spec, false, vis, |t| {
            map_type(t, spec)
        })?;
    }
    Ok(())
}

/// True iff `rust` references the state field `name` as `s.<name>`,
/// word-bounded so `s.total` doesn't match `s.total_supply`.
fn references_field(rust: &str, name: &str) -> bool {
    let needle = format!("s.{}", name);
    let mut from = 0;
    while let Some(pos) = rust[from..].find(&needle) {
        let end = from + pos + needle.len();
        let next_is_word = rust[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !next_is_word {
            return true;
        }
        from = end;
    }
    false
}

fn property_references_field(prop: &ParsedProperty, name: &str) -> bool {
    if let Some(tree) = prop.tree.as_ref() {
        let mut found = false;
        tree.for_each_node(&mut |node| {
            let crate::mir::ExprTree::Path(path) = node else {
                return;
            };
            if !matches!(
                path.binding,
                crate::mir::expr_tree::BindingKind::StateField
                    | crate::mir::expr_tree::BindingKind::Ghost
            ) {
                return;
            }
            found |= matches!(
                path.segments.first(),
                Some(crate::mir::expr_tree::TreeSeg::Field(field)) if field == name
            );
        });
        return found;
    }
    prop.rust_expression
        .as_deref()
        .is_some_and(|rust| references_field(rust, name))
}

#[allow(clippy::too_many_arguments)]
fn emit_preservation_tests_for(
    out: &mut String,
    handlers: &[&ParsedHandler],
    properties: &[ParsedProperty],
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
    will_emit_sequence: bool,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    for prop in properties {
        if prop.expression.is_none() {
            continue;
        }

        // Ghost-reading properties are validated by the init-seeded sequence
        // harness instead: a ghost is a function of handler history, so an
        // arbitrary pre-state rarely satisfies the invariant and rejection
        // sampling exhausts ("too many global rejects").
        if let Some(rust) = &prop.rust_expression {
            if spec.ghosts.iter().any(|g| references_field(rust, &g.name)) {
                for op_name in &prop.preserved_by {
                    if !handlers.iter().any(|o| &o.name == op_name) {
                        continue;
                    }
                    if will_emit_sequence {
                        // The obligation is exercised by the sequence
                        // harness, not a dedicated per-pair test.
                        rec.emitted(
                            ObligationKind::PropertyPreservation,
                            op_name,
                            &prop.name,
                            "state_machine_sequence",
                        );
                    } else {
                        rec.failed(
                            ObligationKind::PropertyPreservation,
                            op_name,
                            &prop.name,
                            "ghost property needs the sequence harness, which this section does not emit",
                        );
                    }
                }
                continue;
            }
        }

        for op_name in &prop.preserved_by {
            let op = handlers.iter().find(|o| &o.name == op_name).copied();

            // Multi-account: `preserved_by all` expands to all handlers; only
            // emit for handlers in this account section.
            if op.is_none() {
                continue;
            }
            rec.emitted(
                ObligationKind::PropertyPreservation,
                op_name,
                &prop.name,
                &format!("{}_preserves_{}", op_name, prop.name),
            );

            let is_init = op
                .map(|o| o.pre_status.as_deref() == Some("Uninitialized"))
                .unwrap_or(false);

            // `forall <binder>` with no same-named handler param: bind the
            // binder via a fresh proptest variable so the post-assert
            // exercises a real value (not the silent `true` stub).
            let handler_takes_binder = match (&prop.per_slot, op) {
                (Some(slot), Some(op)) => {
                    model_params(op).any(|(n, t)| n == &slot.binder_name && t == &slot.binder_type)
                }
                _ => false,
            };
            let local_binder = match &prop.per_slot {
                Some(slot) if !handler_takes_binder => Some(slot.clone()),
                _ => None,
            };

            out.push_str("proptest! {\n");
            // High reject limit: prop_assume on multiple invariants filters aggressively
            out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
            out.push_str("    #[test]\n");

            let mut param_parts = Vec::new();
            if is_init {
                // For init handlers, use fixed zero state
            } else {
                param_parts.push("s in arb_state()".to_string());
            }
            if let Some(op) = op {
                for (pname, ptype) in model_params(op) {
                    // Type-dispatched strategy, not a numeric range format:
                    // `0[u8; 32]..=[u8; 32]::MAX` for a Pubkey param is a
                    // syntax error (#295).
                    let strategy = strategy_for_field(ptype, spec, StrategyMode::Full, None)?;
                    param_parts.push(format!("{} in {}", pname, strategy));
                }
            }
            // Bind the forall binder when no handler param shadows it;
            // strategy_for_field lets the binder's type drive the strategy.
            if let Some(slot) = &local_binder {
                let strategy =
                    strategy_for_field(&slot.binder_type, spec, StrategyMode::Full, None)?;
                param_parts.push(format!("{} in {}", slot.binder_name, strategy));
            }

            if param_parts.is_empty() && is_init {
                // Need at least a dummy parameter for proptest
                param_parts.push("_dummy in 0u8..1u8".to_string());
            }

            out.push_str(&format!(
                "    fn {}_preserves_{}({}) {{\n",
                op_name,
                prop.name,
                param_parts.join(", ")
            ));

            // Capture pre-state before the handler runs so binary properties
            // assert against real (pre, post) — without the capture,
            // `old(...)` properties compare post to itself and pass on a
            // tautology.
            if is_init {
                out.push_str("        let mut post = State {\n");
                for (fname, ftype) in mutable_fields {
                    if let Some(default) = spec.default_value_for_type(ftype) {
                        out.push_str(&format!("            {}: {},\n", fname, default));
                    }
                    // No sensible default: skip — the struct-init E0063
                    // diagnostic points at the missing field.
                }
                // Seed `status` to the spec's declared initial state, not a
                // hardcoded "Uninitialized".
                if lifecycle_states.len() >= 2 {
                    if let Some(initial) = lifecycle_states.first() {
                        out.push_str(&format!("            status: Status::{},\n", initial));
                    }
                }
                out.push_str("        };\n");
                // Init handlers have no pre-state; bind a synthetic
                // `pre = post` so binary assertions have a defined shape.
                out.push_str("        let pre = post;\n");
            } else {
                out.push_str("        let pre = s.clone();\n");
                out.push_str("        let mut post = s;\n");
                // Assume unary properties hold pre-handler. Binary ones are
                // skipped — `(pre, pre)` would be trivially true.
                for pre_prop in properties {
                    if pre_prop.expression.is_none() {
                        continue;
                    }
                    if pre_prop.class == crate::check::PropertyClass::Binary {
                        continue;
                    }
                    match &pre_prop.per_slot {
                        Some(slot) if pre_prop.name == prop.name => {
                            out.push_str(&format!(
                                "        prop_assume!({}_at(&pre, {}));\n",
                                pre_prop.name, slot.binder_name
                            ));
                        }
                        _ => {
                            out.push_str(&format!(
                                "        prop_assume!({}(&pre));\n",
                                pre_prop.name
                            ));
                        }
                    }
                }
            }

            // Emit strict bounds for add effects (against pre-state).
            if let Some(op) = op {
                rust_codegen_util::emit_add_strict_bounds(
                    out,
                    op,
                    properties,
                    "        prop_assume!(pre.{field} < pre.{bound}); // strict bound for add\n",
                );
            }

            let args: String = op
                .map(|o| {
                    o.takes_params
                        .iter()
                        .chain(o.abstract_binders.iter())
                        .map(|(n, _)| format!(", {}", n))
                        .collect()
                })
                .unwrap_or_default();
            out.push_str(&format!("        if {}(&mut post{}) {{\n", op_name, args));
            // Assertion arity dispatches on `prop.class`:
            //   unary             → <prop>(&post)
            //   unary + per_slot  → <prop>_at(&post, binder)
            //   binary            → <prop>(&pre, &post); a Binary × per_slot
            //   property falls through to the plain binary form (joint
            //   lowering deferred).
            let is_binary_prop = prop.class == crate::check::PropertyClass::Binary;
            let assert_call = if is_binary_prop {
                format!("{}(&pre, &post)", prop.name)
            } else {
                match &prop.per_slot {
                    Some(slot) => format!("{}_at(&post, {})", prop.name, slot.binder_name),
                    None => format!("{}(&post)", prop.name),
                }
            };
            out.push_str(&format!("            prop_assert!({},\n", assert_call));
            out.push_str(&format!(
                "                \"{} must hold after {}\");\n",
                prop.name, op_name
            ));
            out.push_str("        }\n");
            out.push_str("    }\n");
            out.push_str("}\n\n");
        }
    }
    Ok(())
}

/// One proptest per `(handler, invariant)` clause. Mirrors
/// `emit_preservation_tests_for` but iterates from the handler side
/// (`handler.invariants`) and only emits when the invariant has a
/// `rust_expr` body — description-only / unsupported-quantifier invariants
/// are skipped silently.
#[allow(clippy::too_many_arguments)]
fn emit_invariant_preservation_tests_for(
    out: &mut String,
    handlers: &[&ParsedHandler],
    invariants: &[&ParsedInvariant],
    mutable_fields: &[&(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    for op in handlers {
        let op_name = &op.name;
        // Walk both clauses. `invariant Name` means "preserves" — assume the
        // invariant pre-state. `establishes Name` skips the pre-assume; the
        // handler only owes us the invariant at post-state.
        let pairs: Vec<(&String, bool)> = op
            .invariants
            .iter()
            .map(|n| (n, false))
            .chain(op.establishes.iter().map(|n| (n, true)))
            .collect();
        for (inv_name, is_establish) in pairs {
            // Skip dangling references — the section-level `linked_invs`
            // filter doesn't cover the per-handler join.
            let Some(inv) = invariants.iter().find(|i| &i.name == inv_name) else {
                continue;
            };
            let is_init = op.pre_status.as_deref() == Some("Uninitialized");
            {
                let verb = if is_establish {
                    "establishes"
                } else {
                    "preserves"
                };
                rec.emitted(
                    ObligationKind::InvariantPreservation,
                    op_name,
                    &format!("{}_{}", verb, inv.name),
                    &format!("{}_{}_{}", op_name, verb, inv.name),
                );
            }

            out.push_str("proptest! {\n");
            out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
            out.push_str("    #[test]\n");

            let mut param_parts = Vec::new();
            if !is_init {
                param_parts.push("s in arb_state()".to_string());
            }
            for (pname, ptype) in model_params(op) {
                // Type-dispatched strategy — see the effect-conformance
                // site above (#295).
                let strategy = strategy_for_field(ptype, spec, StrategyMode::Full, None)?;
                param_parts.push(format!("{} in {}", pname, strategy));
            }
            if param_parts.is_empty() && is_init {
                param_parts.push("_dummy in 0u8..1u8".to_string());
            }

            let verb = if is_establish {
                "establishes"
            } else {
                "preserves"
            };
            out.push_str(&format!(
                "    fn {}_{}_{}({}) {{\n",
                op_name,
                verb,
                inv.name,
                param_parts.join(", ")
            ));

            if is_init {
                out.push_str("        let mut s = State {\n");
                for (fname, ftype) in mutable_fields {
                    if let Some(default) = spec.default_value_for_type(ftype) {
                        out.push_str(&format!("            {}: {},\n", fname, default));
                    }
                }
                if lifecycle_states.len() >= 2 {
                    if let Some(initial) = lifecycle_states.first() {
                        out.push_str(&format!("            status: Status::{},\n", initial));
                    }
                }
                out.push_str("        };\n");
            } else {
                out.push_str("        let mut s = s;\n");
                // preserves: assume X pre-state; establishes: no pre-assume.
                if !is_establish {
                    out.push_str(&format!("        prop_assume!({}(&s));\n", inv.name));
                }
            }

            let args: String = op
                .takes_params
                .iter()
                .chain(op.abstract_binders.iter())
                .map(|(n, _)| format!(", {}", n))
                .collect();
            out.push_str(&format!("        if {}(&mut s{}) {{\n", op_name, args));
            out.push_str(&format!("            prop_assert!({}(&s),\n", inv.name));
            out.push_str(&format!(
                "                \"invariant {} must hold after {}\");\n",
                inv.name, op_name
            ));
            out.push_str("        }\n");
            out.push_str("    }\n");
            out.push_str("}\n\n");
        }
    }
    Ok(())
}

/// Emit guard enforcement tests.
fn emit_guard_tests(
    out: &mut String,
    guard_ops: &[&ParsedHandler],
    _mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    spec: &ParsedSpec,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    for op in guard_ops {
        // Skip handlers whose only guards reference handler-account pubkeys —
        // `collect_full_guard` filters those clauses (the simplified State
        // drops Pubkey fields), and a `"true"` fallback would emit
        // `prop_assume!(!(true))` → always rejects → "Too many global
        // rejects". Real guard checks still emit in the runtime handler.
        let Some(rust_guard) = rust_codegen_util::collect_full_guard(op, true) else {
            rec.unsupported(
                ObligationKind::GuardRejection,
                &op.name,
                &op.name,
                UnsupportedReason::ProptestGuardNotExpressible,
            );
            continue;
        };
        rec.emitted(
            ObligationKind::GuardRejection,
            &op.name,
            &op.name,
            &format!("{}_rejects_invalid", op.name),
        );

        out.push_str("proptest! {\n");
        // High reject limit: guard negation filters most inputs by design
        out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
        out.push_str("    #[test]\n");

        // Use boundary-biased ranges for guard rejection tests so that
        // prop_assume!(negated guard) has a reasonable acceptance rate.
        let mut param_parts = vec!["s in arb_boundary_state()".to_string()];
        for (pname, ptype) in &op.takes_params {
            // Typed dispatch (#330) — compound param types previously hit
            // the string match's u64 catch-all here.
            let boundary = strategy_for_field(ptype, spec, StrategyMode::Boundary, None)?;
            param_parts.push(format!("{} in {}", pname, boundary));
        }
        // Abstract binders: same strategy shape as takes_params; `requires`
        // clauses referencing the binder are negated in the prop_assume
        // below so the harness explores rejecting values.
        for (binder_name, binder_ty) in &op.abstract_binders {
            let boundary = strategy_for_field(binder_ty, spec, StrategyMode::Boundary, None)?;
            param_parts.push(format!("{} in {}", binder_name, boundary));
        }

        out.push_str(&format!(
            "    fn {}_rejects_invalid({}) {{\n",
            op.name,
            param_parts.join(", ")
        ));

        out.push_str("        let mut s = s;\n");
        out.push_str(&format!("        prop_assume!(!({rust_guard}));\n"));

        let args: String = op
            .takes_params
            .iter()
            .chain(op.abstract_binders.iter())
            .map(|(n, _)| format!(", {}", n))
            .collect();
        out.push_str(&format!(
            "        prop_assert!(!{}(&mut s{}),\n",
            op.name, args
        ));
        out.push_str(&format!(
            "            \"{} must reject when guard is violated\");\n",
            op.name
        ));
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }
    let _ = all_fields; // suppress unused
    Ok(())
}

/// Emit overflow detection tests for add effects.
#[allow(clippy::too_many_arguments)]
fn emit_overflow_tests_for(
    out: &mut String,
    mir: &Mir,
    overflow_ops: &[&ParsedHandler],
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    spec: &ParsedSpec,
    properties: &[ParsedProperty],
    rec: &mut ObligationRecorder,
) -> Result<()> {
    for op in overflow_ops {
        let body = mir
            .handler_block(&op.name)
            .ok_or_else(|| anyhow::anyhow!("MIR has no handler `{}`", op.name))?;
        for (field_raw, kind, _value) in rust_codegen_util::block_effect_triples_deep(body) {
            if kind != "add" {
                continue;
            }
            // Strip variant prefix so `Active.balance` resolves to `balance`
            // for the flat-State model (fn name + field lookup).
            let field_owned = rust_codegen_util::strip_variant_prefix_for_flat_state(
                &rust_codegen_util::effect_path_source(field_raw),
                spec,
            );
            let field = field_owned.as_str();

            let dsl_type = all_fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, t)| t.as_str())
                .unwrap_or("U64");
            let max_val = match type_max(dsl_type) {
                Some(m) => m,
                None => {
                    rec.unsupported(
                        ObligationKind::Overflow,
                        &op.name,
                        field,
                        UnsupportedReason::ProptestNonNumericOverflowTarget,
                    );
                    continue;
                }
            };
            let rust_type = map_type(dsl_type, spec)?;
            rec.emitted(
                ObligationKind::Overflow,
                &op.name,
                field,
                &format!("{}_no_overflow_on_{}", op.name, field),
            );

            out.push_str("proptest! {\n");
            out.push_str("    #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
            out.push_str("    #[test]\n");

            let mut param_parts = vec!["s in arb_state()".to_string()];
            for (pname, ptype) in model_params(op) {
                // Type-dispatched strategy — see the effect-conformance
                // site above (#295).
                let strategy = strategy_for_field(ptype, spec, StrategyMode::Full, None)?;
                param_parts.push(format!("{} in {}", pname, strategy));
            }

            out.push_str(&format!(
                "    fn {}_no_overflow_on_{}({}) {{\n",
                op.name,
                field,
                param_parts.join(", ")
            ));

            out.push_str("        let mut s = s;\n");

            // Assume all properties hold (they constrain valid state space)
            for pre_prop in properties {
                if pre_prop.expression.is_some()
                    && pre_prop.class != crate::check::PropertyClass::Binary
                {
                    out.push_str(&format!("        prop_assume!({}(&s));\n", pre_prop.name));
                }
            }

            out.push_str(&format!("        let pre = s.{};\n", field));

            let args: String = op
                .takes_params
                .iter()
                .chain(op.abstract_binders.iter())
                .map(|(n, _)| format!(", {}", n))
                .collect();
            out.push_str(&format!("        if {}(&mut s{}) {{\n", op.name, args));
            out.push_str("            // If transition succeeded, the add must not have wrapped\n");
            out.push_str(&format!("            prop_assert!(s.{} >= pre,\n", field));
            out.push_str(&format!(
                "                \"overflow: {}.{} wrapped around after add\");\n",
                op.name, field
            ));
            out.push_str("        }\n");
            out.push_str("    }\n");
            out.push_str("}\n\n");

            let _ = (max_val, rust_type, mutable_fields); // suppress unused
        }
    }
    Ok(())
}

/// Emit state machine sequence test — random op sequences checking invariants.
fn emit_sequence_test_for(
    out: &mut String,
    handlers: &[&ParsedHandler],
    properties: &[ParsedProperty],
    mutable_fields: &[&(String, String)],
    all_fields: &[(String, String)],
    lifecycle_states: &[String],
    spec: &ParsedSpec,
) -> Result<()> {
    out.push_str("#[derive(Debug, Clone)]\n");
    out.push_str("enum Op {\n");
    for op in handlers {
        let params: String = model_params(op)
            .map(|(_, t)| map_type(t, spec))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        if params.is_empty() {
            out.push_str(&format!(
                "    {},\n",
                crate::codegen_shared::to_pascal_case(&op.name)
            ));
        } else {
            out.push_str(&format!(
                "    {}({}),\n",
                crate::codegen_shared::to_pascal_case(&op.name),
                params
            ));
        }
    }
    out.push_str("}\n\n");

    out.push_str("fn arb_op() -> impl Strategy<Value = Op> {\n");
    out.push_str("    prop_oneof![\n");
    for op in handlers {
        let pascal = crate::codegen_shared::to_pascal_case(&op.name);
        let params: Vec<_> = model_params(op).collect();
        if params.is_empty() {
            out.push_str(&format!("        Just(Op::{}),\n", pascal));
        } else {
            let strategies: Vec<String> = params
                .iter()
                // Type-dispatched strategy — see the effect-conformance
                // site above (#295).
                .map(|(_, t)| strategy_for_field(t, spec, StrategyMode::Full, None))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .map(|strategy| strategy.render())
                .collect();
            let names: Vec<&str> = params.iter().map(|(n, _)| n.as_str()).collect();
            // proptest's tuple `Strategy` impl caps at arity 12; >12-arg
            // handlers (common for brownfield init handlers) hit E0599.
            // Chunk into sub-tuples of ≤12 with nested destructuring.
            const MAX_PROPTEST_TUPLE_ARITY: usize = 12;
            if params.len() == 1 {
                out.push_str(&format!(
                    "        ({}).prop_map(|v| Op::{}(v)),\n",
                    strategies[0], pascal
                ));
            } else if params.len() <= MAX_PROPTEST_TUPLE_ARITY {
                out.push_str(&format!(
                    "        ({}).prop_map(|({})| Op::{}({})),\n",
                    strategies.join(", "),
                    names.join(", "),
                    pascal,
                    names.join(", ")
                ));
            } else {
                let strat_chunks: Vec<String> = strategies
                    .chunks(MAX_PROPTEST_TUPLE_ARITY)
                    .map(|c| format!("({})", c.join(", ")))
                    .collect();
                let pat_chunks: Vec<String> = names
                    .chunks(MAX_PROPTEST_TUPLE_ARITY)
                    .map(|c| format!("({})", c.join(", ")))
                    .collect();
                out.push_str(&format!(
                    "        ({}).prop_map(|({})| Op::{}({})),\n",
                    strat_chunks.join(", "),
                    pat_chunks.join(", "),
                    pascal,
                    names.join(", ")
                ));
            }
        }
    }
    out.push_str("    ]\n");
    out.push_str("}\n\n");

    out.push_str("fn apply_op(s: &mut State, op: &Op) -> bool {\n");
    out.push_str("    match op {\n");
    for op in handlers {
        let pascal = crate::codegen_shared::to_pascal_case(&op.name);
        let params: Vec<_> = model_params(op).collect();
        if params.is_empty() {
            out.push_str(&format!("        Op::{} => {}(s),\n", pascal, op.name));
        } else {
            let bindings: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
            out.push_str(&format!(
                "        Op::{}({}) => {}(s, {}),\n",
                pascal,
                bindings.join(", "),
                op.name,
                bindings
                    .iter()
                    .map(|b| format!("*{}", b))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // assert_all_properties: unary only — binary `(pre, post)` signatures
    // can't be satisfied by this single-state aggregate; per-handler
    // preservation tests cover them.
    out.push_str("fn assert_all_properties(s: &State, context: &str) {\n");
    for prop in properties {
        if prop.expression.is_none() {
            continue;
        }
        if prop.class == crate::check::PropertyClass::Binary {
            out.push_str(&format!(
                "    // {} — binary (pre/post) property; checked at handler \
                 boundaries via the preservation harness below, not here.\n",
                prop.name
            ));
            continue;
        }
        out.push_str(&format!(
            "    assert!({}(s), \"{{}} violated: {}\", context);\n",
            prop.name, prop.name
        ));
    }
    out.push_str("}\n\n");

    // Lifecycle tracking: if spec has lifecycle states, track current state
    // and only check properties after the first state-modifying transition.
    let has_lifecycle = !lifecycle_states.is_empty();
    let initial_state = lifecycle_states.first().cloned();

    if has_lifecycle {
        out.push_str("#[derive(Debug, Clone, Copy, PartialEq)]\n");
        out.push_str("enum Lifecycle {\n");
        for state in lifecycle_states {
            out.push_str(&format!("    {},\n", state));
        }
        out.push_str("}\n\n");

        out.push_str(
            "fn lifecycle_transition(current: Lifecycle, op: &Op) -> Option<Lifecycle> {\n",
        );
        out.push_str("    match (current, op) {\n");
        for op in handlers {
            if let (Some(ref pre), Some(ref post)) = (&op.pre_status, &op.post_status) {
                let pascal = crate::codegen_shared::to_pascal_case(&op.name);
                if model_params(op).next().is_none() {
                    out.push_str(&format!(
                        "        (Lifecycle::{}, Op::{}) => Some(Lifecycle::{}),\n",
                        pre, pascal, post
                    ));
                } else {
                    out.push_str(&format!(
                        "        (Lifecycle::{}, Op::{}(..)) => Some(Lifecycle::{}),\n",
                        pre, pascal, post
                    ));
                }
            }
        }
        out.push_str("        _ => None, // transition not allowed in this state\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    let all_props: Vec<&ParsedProperty> = properties
        .iter()
        .filter(|p| p.expression.is_some() && p.class != crate::check::PropertyClass::Binary)
        .collect();

    let seq_len = 20;
    out.push_str("proptest! {\n");
    out.push_str("    #![proptest_config(ProptestConfig::with_cases(256))]\n");
    out.push_str("    #[test]\n");
    out.push_str(&format!(
        "    fn state_machine_sequence(ops in proptest::collection::vec(arb_op(), 1..{})) {{\n",
        seq_len
    ));

    // Seed a valid initial state via type-aware defaults; `status` seeds to
    // the spec's first declared lifecycle state.
    out.push_str("        let mut s = State {\n");
    for (fname, ftype) in mutable_fields {
        if let Some(default) = spec.default_value_for_type(ftype) {
            out.push_str(&format!("            {}: {},\n", fname, default));
        }
    }
    if has_lifecycle {
        if let Some(initial) = lifecycle_states.first() {
            out.push_str(&format!("            status: Status::{},\n", initial));
        }
    }
    out.push_str("        };\n");

    if has_lifecycle {
        if let Some(ref init) = initial_state {
            out.push_str(&format!(
                "        let mut lifecycle = Lifecycle::{};\n",
                init
            ));
        }
        out.push_str("        let mut initialized = false;\n");
    }

    out.push_str("        for (i, op) in ops.iter().enumerate() {\n");

    if has_lifecycle {
        // Check lifecycle transition is valid before applying
        out.push_str("            let next_lifecycle = lifecycle_transition(lifecycle, op);\n");
        out.push_str("            if next_lifecycle.is_none() {\n");
        out.push_str(
            "                continue; // skip ops not valid in current lifecycle state\n",
        );
        out.push_str("            }\n");
    }

    out.push_str("            if apply_op(&mut s, op) {\n");

    if has_lifecycle {
        out.push_str("                if let Some(next) = next_lifecycle {\n");
        out.push_str("                    lifecycle = next;\n");
        out.push_str("                }\n");
        // Mark as initialized after the first transition out of Uninitialized
        if initial_state.as_deref() == Some("Uninitialized") {
            out.push_str("                if !initialized {\n");
            out.push_str("                    initialized = true;\n");
            out.push_str(
                "                    continue; // skip property checks on init transition\n",
            );
            out.push_str("                }\n");
        }
    }

    out.push_str("                // Check all properties after each successful transition\n");
    if !all_props.is_empty() {
        for prop in &all_props {
            out.push_str(&format!(
                "                prop_assert!({}(&s),\n",
                prop.name
            ));
            out.push_str(&format!(
                "                    \"{} violated after op {{:?}} (step {{}})\", op, i);\n",
                prop.name
            ));
        }
    }

    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n");

    let _ = all_fields; // suppress unused
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Product-state module (#331) — multi-account proptest
// ────────────────────────────────────────────────────────────────────

/// One emitted per-account module, as the product lowering sees it.
struct ProptestComponent {
    acct: crate::check::ParsedAccountType,
    mod_name: String,
    handler_names: Vec<String>,
}

fn model_params(op: &ParsedHandler) -> impl Iterator<Item = &(String, String)> {
    op.takes_params.iter().chain(op.abstract_binders.iter())
}

/// Union-find root with path halving, over a flat `parent` slice. Used to
/// group state fields into connected components for per-component bound
/// propagation (`emit_account_section`).
fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn rust_type_mentions(rust_ty: &str, name: &str) -> bool {
    rust_ty
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|ident| ident == name)
}

/// How a property relates to the product state.
enum ProductPropScope<'a> {
    /// Owned by one component; only pairs with handlers routed to OTHER
    /// components need product tests (component index, prop).
    Component(usize, &'a ParsedProperty),
    /// Reads a liftable ghost — validated by the product sequence
    /// harness (arbitrary ghost pre-states reject too aggressively for
    /// single-step tests, same rationale as the single-account lane).
    Ghost(&'a ParsedProperty),
    /// Reads fields of two or more components (no ghosts): gets a
    /// product predicate and per-pair single-step tests.
    MultiComponent(&'a ParsedProperty),
}

/// Emit `mod product` for a multi-account spec: ProductState (one
/// component per emitted account module + the global ghosts), delegating
/// transition wrappers with atomic ghost updates, an `arb_product_state`
/// strategy, cross-account and multi-component preservation tests, and
/// the init-seeded product sequence harness that exercises ghost
/// properties. Shapes that do not resolve stay recorded — emitted,
/// unsupported, or failed — never absent.
fn emit_product_module(
    out: &mut String,
    spec: &ParsedSpec,
    components: &[ProptestComponent],
    ghosts_liftable: bool,
    rec: &mut ObligationRecorder,
) -> Result<()> {
    // Field → owning component map. A field name declared by more than
    // one account cannot be routed and poisons any property that reads it.
    let mut product_fields: std::collections::BTreeMap<String, String> = Default::default();
    let mut ambiguous_fields: std::collections::BTreeSet<String> = Default::default();
    for comp in components {
        for (fname, _) in &comp.acct.fields {
            if product_fields
                .insert(fname.clone(), comp.mod_name.clone())
                .is_some()
            {
                ambiguous_fields.insert(fname.clone());
            }
        }
    }
    for f in &ambiguous_fields {
        product_fields.remove(f);
    }

    let component_of_handler = |name: &str| -> Option<usize> {
        components
            .iter()
            .position(|c| c.handler_names.iter().any(|h| h == name))
    };
    // Wrappable: routed to an emitted component, no record/sum-typed
    // params (those types live inside the account modules).
    let module_scoped_types: Vec<&str> = spec
        .records
        .iter()
        .map(|r| r.name.as_str())
        .chain(spec.sum_types.iter().map(|s| s.name.as_str()))
        .collect();
    let wrappable = |name: &str| -> Option<(usize, &ParsedHandler)> {
        let comp = component_of_handler(name)?;
        let op = spec.handlers.iter().find(|h| h.name == name)?;
        model_params(op)
            .all(|(_, t)| {
                map_type(t, spec).is_ok_and(|rust_ty| {
                    !module_scoped_types
                        .iter()
                        .any(|name| rust_type_mentions(&rust_ty, name))
                })
            })
            .then_some((comp, op))
    };

    // Classify every property with a body.
    let mut scopes: Vec<ProductPropScope> = Vec::new();
    for prop in &spec.properties {
        if prop.rust_expression.is_none() {
            continue;
        }
        let reads_ghost = spec
            .ghosts
            .iter()
            .any(|g| property_references_field(prop, &g.name));
        let comp_refs: Vec<usize> = components
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.acct
                    .fields
                    .iter()
                    .any(|(f, _)| property_references_field(prop, f))
            })
            .map(|(i, _)| i)
            .collect();
        let reads_ambiguous = ambiguous_fields
            .iter()
            .any(|f| property_references_field(prop, f));
        if reads_ghost {
            if ghosts_liftable && !reads_ambiguous {
                scopes.push(ProductPropScope::Ghost(prop));
            }
            // Non-liftable ghosts were already recorded unsupported.
            continue;
        }
        if reads_ambiguous {
            for op_name in &prop.preserved_by {
                if component_of_handler(op_name).is_some() {
                    rec.unsupported(
                        ObligationKind::PropertyPreservation,
                        op_name,
                        &prop.name,
                        UnsupportedReason::MultiAccountCrossAccountObligation,
                    );
                }
            }
            continue;
        }
        match comp_refs.as_slice() {
            [] => {}
            [single] => scopes.push(ProductPropScope::Component(*single, prop)),
            _ => scopes.push(ProductPropScope::MultiComponent(prop)),
        }
    }

    // Work lists.
    struct CrossPair<'a> {
        prop: &'a ParsedProperty,
        owner: usize,
        op: &'a ParsedHandler,
    }
    struct ProductPair<'a> {
        prop: &'a ParsedProperty,
        op: &'a ParsedHandler,
    }
    let mut cross_pairs: Vec<CrossPair> = Vec::new();
    let mut product_pairs: Vec<ProductPair> = Vec::new();
    let mut ghost_props: Vec<&ParsedProperty> = Vec::new();
    for scope in &scopes {
        match scope {
            ProductPropScope::Component(owner, prop) => {
                for op_name in &prop.preserved_by {
                    let Some(routed) = component_of_handler(op_name) else {
                        continue;
                    };
                    if routed == *owner {
                        continue; // tested inside the account module
                    }
                    match wrappable(op_name) {
                        Some((_, op)) => cross_pairs.push(CrossPair {
                            prop,
                            owner: *owner,
                            op,
                        }),
                        None => rec.unsupported(
                            ObligationKind::PropertyPreservation,
                            op_name,
                            &prop.name,
                            UnsupportedReason::MultiAccountCrossAccountObligation,
                        ),
                    }
                }
            }
            ProductPropScope::MultiComponent(prop) => {
                if prop.class == crate::check::PropertyClass::Binary
                    || rust_codegen_util::property_predicate_rust_product(prop, &product_fields)
                        .is_none()
                {
                    for op_name in &prop.preserved_by {
                        if component_of_handler(op_name).is_some() {
                            rec.unsupported(
                                ObligationKind::PropertyPreservation,
                                op_name,
                                &prop.name,
                                UnsupportedReason::MultiAccountCrossAccountObligation,
                            );
                        }
                    }
                    continue;
                }
                for op_name in &prop.preserved_by {
                    if component_of_handler(op_name).is_none() {
                        continue;
                    }
                    match wrappable(op_name) {
                        Some((_, op)) => product_pairs.push(ProductPair { prop, op }),
                        None => rec.unsupported(
                            ObligationKind::PropertyPreservation,
                            op_name,
                            &prop.name,
                            UnsupportedReason::MultiAccountCrossAccountObligation,
                        ),
                    }
                }
            }
            ProductPropScope::Ghost(prop) => ghost_props.push(prop),
        }
    }

    // The sequence harness needs every handler wrappable so the op
    // alphabet covers the whole spec.
    let all_wrappable: Vec<(usize, &ParsedHandler)> = spec
        .handlers
        .iter()
        .filter_map(|h| wrappable(&h.name))
        .collect();
    let want_sequence = !ghost_props.is_empty();
    let sequence_ok = want_sequence
        && all_wrappable.len() == spec.handlers.len()
        && ghost_props.iter().all(|p| {
            p.class == crate::check::PropertyClass::Unary
                && rust_codegen_util::property_predicate_rust_product(p, &product_fields).is_some()
        });

    // Record ghost pairs against the harness that will exercise them.
    for prop in &ghost_props {
        for op_name in &prop.preserved_by {
            if component_of_handler(op_name).is_none() {
                continue;
            }
            if sequence_ok {
                rec.emitted(
                    ObligationKind::PropertyPreservation,
                    op_name,
                    &prop.name,
                    "product_state_machine_sequence",
                );
            } else {
                rec.unsupported(
                    ObligationKind::PropertyPreservation,
                    op_name,
                    &prop.name,
                    UnsupportedReason::ProptestMultiAccountGhost,
                );
            }
        }
    }

    if cross_pairs.is_empty() && product_pairs.is_empty() && !sequence_ok {
        return Ok(());
    }

    // Which wrappers do the emitted harnesses actually call?
    let mut wrapper_names: std::collections::BTreeSet<&str> = Default::default();
    for pair in &cross_pairs {
        wrapper_names.insert(pair.op.name.as_str());
    }
    for pair in &product_pairs {
        wrapper_names.insert(pair.op.name.as_str());
    }
    if sequence_ok {
        for (_, op) in &all_wrappable {
            wrapper_names.insert(op.name.as_str());
        }
    }

    out.push_str(
        "// ============================================================================\n",
    );
    out.push_str("// Product state (#331) — one component per account module plus the\n");
    out.push_str("// spec-global ghosts; wrappers delegate to the account transitions and\n");
    out.push_str("// apply ghost updates atomically.\n");
    out.push_str(
        "// ============================================================================\n\n",
    );
    out.push_str("mod product {\n");
    out.push_str("    use super::*;\n\n");

    // ProductState + strategy.
    out.push_str("    #[derive(Debug, Clone, Copy)]\n");
    out.push_str("    struct ProductState {\n");
    for comp in components {
        out.push_str(&format!(
            "        {}: {}::State,\n",
            comp.mod_name, comp.mod_name
        ));
    }
    let liftable_ghosts: &[crate::check::ParsedGhost] =
        if ghosts_liftable { &spec.ghosts } else { &[] };
    for g in liftable_ghosts {
        out.push_str(&format!(
            "        {}: {},\n",
            g.name,
            map_type(&g.ty, spec)?
        ));
    }
    out.push_str("    }\n\n");

    out.push_str("    prop_compose! {\n");
    out.push_str("        fn arb_product_state()(\n");
    for comp in components {
        out.push_str(&format!(
            "            {} in {}::arb_state(),\n",
            comp.mod_name, comp.mod_name
        ));
    }
    for g in liftable_ghosts {
        let strategy = strategy_for_field(&g.ty, spec, StrategyMode::Full, None)?;
        out.push_str(&format!("            {} in {},\n", g.name, strategy));
    }
    out.push_str("        ) -> ProductState {\n");
    out.push_str("            ProductState {\n");
    for comp in components {
        out.push_str(&format!("                {},\n", comp.mod_name));
    }
    for g in liftable_ghosts {
        out.push_str(&format!("                {},\n", g.name));
    }
    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");

    // Transition wrappers.
    out.push_str("    // Transition wrappers — delegate to the owning account module and\n");
    out.push_str("    // apply ghost updates atomically with the account transition.\n");
    for name in &wrapper_names {
        let (comp_idx, op) = wrappable(name).expect("wrapper_names built from wrappable");
        let comp = &components[comp_idx];
        let mut params = String::new();
        let mut args = String::new();
        for (n, t) in model_params(op) {
            params.push_str(&format!(", {}: {}", n, map_type(t, spec)?));
            args.push_str(&format!(", {}", n));
        }
        let mut update_fields = product_fields.clone();
        for (field, _) in &comp.acct.fields {
            // The update is attached to this handler, so an otherwise
            // ambiguous field resolves to the handler's owning component.
            update_fields.insert(field.clone(), comp.mod_name.clone());
        }
        let ghost_updates: Vec<String> = liftable_ghosts
            .iter()
            .filter_map(|g| {
                g.updates.iter().find(|u| u.handler == *name).map(|u| {
                    let value = u
                        .value_tree
                        .as_ref()
                        .map(|tree| {
                            rust_codegen_util::tree_render::render_rust(
                                tree,
                                rust_codegen_util::tree_render::RustCx::native()
                                    .with_binder(rust_codegen_util::tree_render::Binder::SelfAcct(
                                        "pre",
                                    ))
                                    .with_product_fields(Some(&update_fields))
                                    // Ghost aggregate: saturate rather than
                                    // overflow-panic under the debug sequence
                                    // harness (see the single-account emit).
                                    .with_arith(
                                        rust_codegen_util::tree_render::ArithMode::SaturatingEffect,
                                    ),
                            )
                        })
                        .unwrap_or_else(|| u.value_rust.clone());
                    (g.name.clone(), value)
                })
            })
            .map(|(gname, value)| format!("            s.{} = {};\n", gname, value))
            .collect();
        out.push_str(&format!(
            "    fn {}(s: &mut ProductState{}) -> bool {{\n",
            op.name, params
        ));
        if ghost_updates.is_empty() {
            out.push_str(&format!(
                "        {}::{}(&mut s.{}{})\n",
                comp.mod_name, op.name, comp.mod_name, args
            ));
        } else {
            out.push_str("        let pre = s.clone();\n");
            out.push_str(&format!(
                "        if {}::{}(&mut s.{}{}) {{\n",
                comp.mod_name, op.name, comp.mod_name, args
            ));
            for u in &ghost_updates {
                out.push_str(u);
            }
            out.push_str("            true\n");
            out.push_str("        } else {\n");
            out.push_str("            false\n");
            out.push_str("        }\n");
        }
        out.push_str("    }\n\n");
    }

    // Product predicates for multi-component and ghost properties.
    let mut predicate_names: std::collections::BTreeSet<&str> = Default::default();
    for pair in &product_pairs {
        predicate_names.insert(pair.prop.name.as_str());
    }
    if sequence_ok {
        for prop in &ghost_props {
            predicate_names.insert(prop.name.as_str());
        }
    }
    for pname in &predicate_names {
        let prop = spec
            .properties
            .iter()
            .find(|p| p.name == *pname)
            .expect("predicate names come from spec properties");
        let body = rust_codegen_util::property_predicate_rust_product(prop, &product_fields)
            .expect("classification requires a renderable product body");
        let doc = prop.expression.as_deref().unwrap_or("");
        out.push_str(&format!("    /// {}: {}\n", prop.name, doc));
        out.push_str(&format!(
            "    fn {}(s: &ProductState) -> bool {{\n",
            prop.name
        ));
        out.push_str(&format!("        {}\n", body));
        out.push_str("    }\n\n");
    }

    // Cross-account preservation tests: component predicate, product
    // transition.
    for pair in &cross_pairs {
        let owner = &components[pair.owner];
        let harness = format!("{}_preserves_{}", pair.op.name, pair.prop.name);
        rec.emitted(
            ObligationKind::PropertyPreservation,
            &pair.op.name,
            &pair.prop.name,
            &harness,
        );
        emit_product_preservation_test(
            out,
            spec,
            pair.op,
            &harness,
            ProductPreservationPredicate {
                path: &format!("{}::{}", owner.mod_name, pair.prop.name),
                receiver: &format!(".{}", owner.mod_name),
                name: &pair.prop.name,
                binary: pair.prop.class == crate::check::PropertyClass::Binary,
            },
        )?;
    }

    // Multi-component preservation tests: product predicate, product
    // transition.
    for pair in &product_pairs {
        let harness = format!("{}_preserves_{}", pair.op.name, pair.prop.name);
        rec.emitted(
            ObligationKind::PropertyPreservation,
            &pair.op.name,
            &pair.prop.name,
            &harness,
        );
        emit_product_preservation_test(
            out,
            spec,
            pair.op,
            &harness,
            ProductPreservationPredicate {
                path: &pair.prop.name,
                receiver: "",
                name: &pair.prop.name,
                binary: false,
            },
        )?;
    }

    // Product sequence harness — the ghost-property gate.
    if sequence_ok {
        rec.emitted(
            ObligationKind::BackendExtra,
            "file",
            "product_state_machine_sequence",
            "product_state_machine_sequence",
        );
        emit_product_sequence_test(out, spec, components, liftable_ghosts, &ghost_props)?;
    }

    out.push_str("} // mod product\n\n");
    Ok(())
}

/// One product preservation test. `predicate_path` is the callable
/// predicate (`pool::pool_solvency` or a product predicate); `receiver`
/// narrows the asserted value (`.pool` for component predicates, empty
/// for product predicates).
struct ProductPreservationPredicate<'a> {
    path: &'a str,
    receiver: &'a str,
    name: &'a str,
    binary: bool,
}

fn emit_product_preservation_test(
    out: &mut String,
    spec: &ParsedSpec,
    op: &ParsedHandler,
    harness: &str,
    predicate: ProductPreservationPredicate<'_>,
) -> Result<()> {
    out.push_str("    proptest! {\n");
    out.push_str("        #![proptest_config(ProptestConfig { max_global_rejects: 65536, ..ProptestConfig::with_cases(256) })]\n");
    out.push_str("        #[test]\n");
    let mut params = vec!["s in arb_product_state()".to_string()];
    for (pname, ptype) in model_params(op) {
        let strategy = strategy_for_field(ptype, spec, StrategyMode::Full, None)?;
        params.push(format!("{} in {}", pname, strategy));
    }
    out.push_str(&format!(
        "        fn {}({}) {{\n",
        harness,
        params.join(", ")
    ));
    out.push_str("            let pre = s.clone();\n");
    out.push_str("            let mut post = s;\n");
    if !predicate.binary {
        out.push_str(&format!(
            "            prop_assume!({}(&pre{}));\n",
            predicate.path, predicate.receiver
        ));
    }
    let args: String = model_params(op).map(|(n, _)| format!(", {}", n)).collect();
    out.push_str(&format!(
        "            if {}(&mut post{}) {{\n",
        op.name, args
    ));
    let assertion = if predicate.binary {
        format!(
            "{}(&pre{}, &post{})",
            predicate.path, predicate.receiver, predicate.receiver
        )
    } else {
        format!("{}(&post{})", predicate.path, predicate.receiver)
    };
    out.push_str(&format!("                prop_assert!({},\n", assertion));
    out.push_str(&format!(
        "                    \"{} must hold after {}\");\n",
        predicate.name, op.name
    ));
    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
    Ok(())
}

/// Product sequence harness: seeds every component at its lifecycle
/// default, initializes each ghost ONCE from its declared init value,
/// applies random cross-account op sequences through the wrappers
/// (ghost updates ride the same call — atomic by construction), and
/// asserts every ghost property after each successful step.
fn emit_product_sequence_test(
    out: &mut String,
    spec: &ParsedSpec,
    components: &[ProptestComponent],
    liftable_ghosts: &[crate::check::ParsedGhost],
    ghost_props: &[&ParsedProperty],
) -> Result<()> {
    // Op alphabet over every handler.
    out.push_str("    #[derive(Debug, Clone)]\n");
    out.push_str("    enum Op {\n");
    for op in &spec.handlers {
        let params: Vec<_> = model_params(op).collect();
        let payload = if params.is_empty() {
            String::new()
        } else {
            let tys: Vec<String> = params
                .iter()
                .map(|(_, t)| map_type(t, spec))
                .collect::<Result<_>>()?;
            format!("({})", tys.join(", "))
        };
        out.push_str(&format!(
            "        {}{},\n",
            crate::codegen_shared::to_pascal_case(&op.name),
            payload
        ));
    }
    out.push_str("    }\n\n");

    out.push_str("    fn arb_op() -> impl Strategy<Value = Op> {\n");
    out.push_str("        prop_oneof![\n");
    for op in &spec.handlers {
        let variant = crate::codegen_shared::to_pascal_case(&op.name);
        let params: Vec<_> = model_params(op).collect();
        if params.is_empty() {
            out.push_str(&format!("            Just(Op::{}),\n", variant));
        } else if params.len() == 1 {
            let strategy = strategy_for_field(&params[0].1, spec, StrategyMode::Full, None)?;
            // Parens are load-bearing: `0u64..=u64::MAX.prop_map(…)`
            // parses as a range whose end is the method call.
            out.push_str(&format!(
                "            ({}).prop_map(Op::{}),\n",
                strategy, variant
            ));
        } else {
            let strategies: Vec<String> = params
                .iter()
                .map(|(_, t)| {
                    strategy_for_field(t, spec, StrategyMode::Full, None).map(|s| s.to_string())
                })
                .collect::<Result<_>>()?;
            let binders: Vec<String> = (0..params.len()).map(|i| format!("p{}", i)).collect();
            out.push_str(&format!(
                "            ({}).prop_map(|({})| Op::{}({})),\n",
                strategies.join(", "),
                binders.join(", "),
                variant,
                binders.join(", ")
            ));
        }
    }
    out.push_str("        ]\n");
    out.push_str("    }\n\n");

    out.push_str("    fn apply_op(s: &mut ProductState, op: &Op) -> bool {\n");
    out.push_str("        match op {\n");
    for op in &spec.handlers {
        let variant = crate::codegen_shared::to_pascal_case(&op.name);
        let params: Vec<_> = model_params(op).collect();
        if params.is_empty() {
            out.push_str(&format!("            Op::{} => {}(s),\n", variant, op.name));
        } else {
            let binders: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
            let args: Vec<String> = binders.iter().map(|n| format!("*{}", n)).collect();
            out.push_str(&format!(
                "            Op::{}({}) => {}(s, {}),\n",
                variant,
                binders.join(", "),
                op.name,
                args.join(", ")
            ));
        }
    }
    out.push_str("        }\n");
    out.push_str("    }\n\n");

    out.push_str("    proptest! {\n");
    out.push_str("        #![proptest_config(ProptestConfig::with_cases(256))]\n");
    out.push_str("        #[test]\n");
    out.push_str("        fn product_state_machine_sequence(ops in proptest::collection::vec(arb_op(), 1..20)) {\n");
    out.push_str("            let mut s = ProductState {\n");
    for comp in components {
        out.push_str(&format!(
            "                {}: {}::State {{\n",
            comp.mod_name, comp.mod_name
        ));
        for (fname, ftype) in &comp.acct.fields {
            if let Some(default) = spec.default_value_for_type(ftype) {
                out.push_str(&format!("                    {}: {},\n", fname, default));
            }
        }
        if comp.acct.lifecycle.len() >= 2 && !comp.acct.fields.iter().any(|(n, _)| n == "status") {
            out.push_str(&format!(
                "                    status: {}::Status::{},\n",
                comp.mod_name, comp.acct.lifecycle[0]
            ));
        }
        out.push_str("                },\n");
    }
    for g in liftable_ghosts {
        let init = g
            .init_tree
            .as_ref()
            .map(|tree| {
                rust_codegen_util::tree_render::render_rust(
                    tree,
                    rust_codegen_util::tree_render::RustCx::native(),
                )
            })
            .or_else(|| spec.default_value_for_type(&g.ty))
            .unwrap_or_else(|| "0".to_string());
        out.push_str(&format!("                {}: {},\n", g.name, init));
    }
    out.push_str("            };\n");
    out.push_str("            let mut initialized = false;\n");
    out.push_str("            for (i, op) in ops.iter().enumerate() {\n");
    out.push_str("                if apply_op(&mut s, op) {\n");
    out.push_str("                    if !initialized {\n");
    out.push_str("                        initialized = true;\n");
    out.push_str("                        continue;\n");
    out.push_str("                    }\n");
    for prop in ghost_props {
        out.push_str(&format!(
            "                    prop_assert!({}(&s),\n",
            prop.name
        ));
        out.push_str(&format!(
            "                        \"{} violated after op {{:?}} (step {{}})\", op, i);\n",
            prop.name
        ));
    }
    out.push_str("                }\n");
    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{ParsedRecordType, ParsedSumType, ParsedVariant};
    use crate::chumsky_adapter::parse_str;

    fn spec_with_record(name: &str, fields: &[(&str, &str)]) -> ParsedSpec {
        ParsedSpec {
            records: vec![ParsedRecordType {
                name: name.to_string(),
                fields: fields
                    .iter()
                    .map(|(n, t)| (n.to_string(), t.to_string()))
                    .collect(),
            }],
            ..ParsedSpec::default()
        }
    }

    fn spec_with_unit_sum(name: &str, variants: &[&str]) -> ParsedSpec {
        ParsedSpec {
            sum_types: vec![ParsedSumType {
                name: name.to_string(),
                variants: variants
                    .iter()
                    .map(|v| ParsedVariant {
                        name: v.to_string(),
                        fields: vec![],
                    })
                    .collect(),
            }],
            ..ParsedSpec::default()
        }
    }

    fn emit_full_harness(src: &str) -> (ParsedSpec, String) {
        let spec = parse_str(src).expect("parse");
        let mir = crate::mir::lower(&spec);
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("proptest.rs");
        let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
        generate_impl(&mir, &spec, Some(&output), &mut rec).expect("generate");
        let body = std::fs::read_to_string(output).expect("read harness");
        (spec, body)
    }

    #[test]
    fn product_harness_threads_abstract_binders_and_binary_component_properties() {
        let (_spec, body) = emit_full_harness(
            r#"
spec ProductEdges

type Pool
  | Idle
  | Active of { total : U64 }

type Loan
  | Empty
  | Open of { debt : U64 }

type Error
  | InvalidAmount

ghost lifetime_total : U64 {
  init { 0 }
  on deposit { lifetime_total += amount }
}

handler init_pool : Pool.Idle -> Pool.Active {
  effect { total := 0 }
}

handler deposit : Pool.Active -> Pool.Active {
  takes amount : U64
  abstract observed : U64
  requires amount > 0 else InvalidAmount
  requires observed > 0 else InvalidAmount
  effect { total += amount }
}

handler open_loan : Loan.Empty -> Loan.Open {
  takes amount : U64
  abstract oracle_debt : U64
  requires amount > 0 else InvalidAmount
  effect { debt := amount }
}

property ghost_tracks_total :
  state.lifetime_total >= state.total
  preserved_by [deposit]

property total_unchanged :
  state.total == old(state.total)
  preserved_by [open_loan]
"#,
        );

        assert!(
            body.contains("fn open_loan_preserves_total_unchanged(")
                && body.contains("oracle_debt in 0u64..=u64::MAX"),
            "product preservation must generate abstract strategies:\n{body}"
        );
        assert!(
            body.contains("prop_assert!(pool::total_unchanged(&pre.pool, &post.pool),"),
            "binary component property must receive pre and post:\n{body}"
        );
        assert!(
            body.contains("Deposit(u64, u64)")
                && body.contains("OpenLoan(u64, u64)")
                && body.contains("deposit(s, *amount, *observed)")
                && body.contains("open_loan(s, *amount, *oracle_debt)"),
            "product sequence must carry abstract binders in Op payloads:\n{body}"
        );
    }

    #[test]
    fn product_ghost_updates_use_pre_state_and_handler_owned_ambiguous_fields() {
        let (spec, body) = emit_full_harness(
            r#"
spec ProductGhostPre

type Pool
  | Idle
  | Active of { amount : U64 }

type Loan
  | Empty
  | Open of { amount : U64 }

ghost observed : U64 {
  init { 0 }
  on bump { observed := state.amount }
}

handler bump : Pool.Active -> Pool.Active {
  effect { amount += 1 }
}

handler open_loan : Loan.Empty -> Loan.Open {
  effect { amount := 1 }
}

property observed_nonnegative :
  state.observed >= 0
  preserved_by [bump]
"#,
        );

        assert!(
            body.contains(
                "let pre = s.clone();\n        if pool::bump(&mut s.pool) {\n            s.observed = pre.pool.amount;"
            ),
            "ghost update must read the owning component's pre-state:\n{body}"
        );
        let warnings = crate::check::check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "ghost_unsupported_state_shape"),
            "supported multi-account product ghosts must not trigger the old shape lint: {warnings:#?}"
        );
    }

    #[test]
    fn product_ghost_is_not_liftable_when_handler_effect_reads_it() {
        let spec = parse_str(
            r#"
spec ProductGhostEffectRead

type Pool
  | Idle
  | Active of { total : U64 }

type Loan
  | Empty
  | Open of { debt : U64 }

ghost observed : U64 {
  init { 0 }
}

handler copy_observed : Pool.Active -> Pool.Active {
  effect { total := state.observed }
}

handler open_loan : Loan.Empty -> Loan.Open {
  effect { debt := 1 }
}
"#,
        )
        .expect("parse");

        assert!(
            !rust_codegen_util::multi_account_ghosts_liftable(&spec),
            "effect RHS ghost read must retain per-account ghosts"
        );
    }

    #[test]
    fn product_rejects_aliased_nested_module_scoped_params() {
        let (_spec, body) = emit_full_harness(
            r#"
spec ProductRecordParam

type Payload = { value : U64 }
type MaybePayload = Option Payload

type Pool
  | Idle
  | Active of { total : U64 }

type Loan
  | Empty
  | Open of { debt : U64 }

handler apply_payload : Pool.Active -> Pool.Active {
  takes payload : MaybePayload
  effect { total := 1 }
}

handler open_loan : Loan.Empty -> Loan.Open {
  effect { debt := 1 }
}

property debt_nonnegative :
  state.debt >= 0
  preserved_by [apply_payload]
"#,
        );

        assert!(
            !body.contains("fn apply_payload(s: &mut ProductState"),
            "product wrapper must not name a record type owned by a sibling module:\n{body}"
        );
    }

    /// Overflow-test names must strip the variant prefix — `Active.balance`
    /// would otherwise yield the invalid fn identifier
    /// `deposit_no_overflow_on_Active.balance`.
    #[test]
    fn overflow_test_name_strips_variant_prefix_for_flat_state() {
        let src = r#"spec Vault
program_id "11111111111111111111111111111111"

type State
  | Uninitialized
  | Active of {
      owner   : Pubkey,
      balance : U64,
    }

type Error
  | MathOverflow

handler deposit (amount : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    vault : writable
  }
  requires amount > 0 else MathOverflow
  effect {
    Active.balance += amount
  }
}

property balance_nonneg :
  state.balance >= 0
  preserved_by all
"#;
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        let out_path = dir.path().join("tests/proptest.rs");
        std::fs::write(&spec_path, src).unwrap();
        let spec = crate::check::parse_spec_file(&spec_path).expect("parse");
        let mir = crate::mir::lower(&spec);
        let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
        generate_impl(&mir, &spec, Some(&out_path), &mut rec).unwrap();
        let body = std::fs::read_to_string(&out_path).unwrap();

        // Function names land as bare-field, not variant-prefixed.
        assert!(
            body.contains("fn deposit_no_overflow_on_balance"),
            "expected variant-prefix stripped from test name; got:\n{body}"
        );
        // No `Active.` substring anywhere in the proptest body —
        // every effect line should refer to bare `s.balance`.
        assert!(
            !body.contains("Active.balance"),
            "variant prefix leaked into proptest body:\n{body}"
        );
        assert!(
            !body.contains("Active.owner"),
            "variant prefix leaked into proptest body:\n{body}"
        );
    }

    /// #298 regression: the model state space allows count fields past a
    /// bounded container's capacity, so a guard like
    /// `i < s.member_count` passes for an index beyond `s.voted.len()`
    /// and the next conjunct panics the harness. Deployed code aborts
    /// the transaction there; the model must reject the transition.
    /// Synthesized bounds conjuncts lead the collected guard, and
    /// effect-only subscripts get transition pre-checks.
    #[test]
    fn subscript_bounds_lead_the_model_guard() {
        let src = r#"spec MiniBounds
const MAX_MEMBERS = 4

type State | Active of {
    voted : Map[MAX_MEMBERS] U8,
    tally : Map[MAX_MEMBERS] U64,
    member_count : U8,
  }

type Error
  | Unauthorized

handler vote (member_index : U8) : State.Active -> State.Active {
  accounts {
    voter : signer
    state : writable
  }
  requires member_index < member_count and voted[member_index] == 0 else Unauthorized
  effect {
    Active.voted[member_index] := 1
    Active.tally[member_index] += 1
  }
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("bounds.qedspec");
        let out_path = dir.path().join("tests/proptest.rs");
        std::fs::write(&spec_path, src).unwrap();
        let spec = crate::check::parse_spec_file(&spec_path).expect("parse");
        let mir = crate::mir::lower(&spec);
        let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
        generate_impl(&mir, &spec, Some(&out_path), &mut rec).unwrap();
        let body = std::fs::read_to_string(&out_path).unwrap();

        // Requires-derived bounds lead the transition guard: the bounds
        // term must appear before the indexing conjunct.
        let guard_pos = body
            .find("((member_index) as usize) < s.voted.len()")
            .expect("bounds term present");
        let index_read_pos = body
            .find("s.voted[(member_index) as usize] == 0")
            .expect("indexing conjunct present");
        assert!(
            guard_pos < index_read_pos,
            "bounds term must precede the indexing read:\n{body}"
        );
        // Effect-only subscript (`tally` never appears in requires) gets
        // a transition pre-check.
        assert!(
            body.contains("((member_index) as usize) < s.tally.len()"),
            "effect-only subscript gets a bounds pre-check:\n{body}"
        );
    }

    /// #296 regression: the proptest transition model must carry the
    /// spec's per-effect semantics. Default `+=` is checked — reject the
    /// transition on overflow, like the deployed
    /// `checked_add(..).ok_or(err)?` — while explicit `+=?` wraps and
    /// `+=!` saturates. The old `wrapping: true` forced default `+=` to
    /// wrap AND report success, so the generated
    /// `<op>_no_overflow_on_<field>` test failed on a correct spec.
    #[test]
    fn default_add_is_checked_in_transition_model() {
        let src = r#"spec MiniVault

type State | Active of {
    balance : U64,
    lifetime : U64,
    ticks : U64,
  }

type Error
  | MathOverflow

handler deposit (amount : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    vault : writable
  }
  requires amount > 0 else MathOverflow
  effect {
    Active.balance  += amount
    Active.lifetime +=! amount
    Active.ticks    +=? amount
  }
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        let out_path = dir.path().join("tests/proptest.rs");
        std::fs::write(&spec_path, src).unwrap();
        let spec = crate::check::parse_spec_file(&spec_path).expect("parse");
        let mir = crate::mir::lower(&spec);
        let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
        generate_impl(&mir, &spec, Some(&out_path), &mut rec).unwrap();
        let body = std::fs::read_to_string(&out_path).unwrap();

        // Default `+=` rejects on overflow instead of wrapping.
        assert!(
            body.contains("s.balance.checked_add(amount)"),
            "default += models checked_add:\n{body}"
        );
        assert!(
            !body.contains("s.balance.wrapping_add"),
            "default += must not wrap in the model:\n{body}"
        );
        // Explicit tiers keep their declared semantics.
        assert!(
            body.contains("s.lifetime.saturating_add(amount)"),
            "+=! models saturating_add:\n{body}"
        );
        assert!(
            body.contains("s.ticks.wrapping_add(amount)"),
            "+=? models wrapping_add:\n{body}"
        );
    }

    /// #295 regression: multisig-shaped specs (Pubkey handler params,
    /// bare-account guard terms, u8-indexed Map state) generated
    /// non-compiling proptests three ways:
    /// (a) `member_pubkey in 0[u8; 32]..=[u8; 32]::MAX` — Pubkey params
    ///     went through a numeric-range format string (syntax error);
    /// (b) `s.members[i] == approver` — bare account names survived the
    ///     pubkey-only guard suppression as free variables (E0425);
    /// (c) `s.voted[member_index] = 1` — effect-LHS subscripts kept the
    ///     param's u8 type (E0277; arrays index by usize).
    #[test]
    fn multisig_shapes_render_compiling_proptests() {
        let src = r#"spec MiniMultisig
const MAX_MEMBERS = 4

type State | Active of {
    members : Map[MAX_MEMBERS] Pubkey,
    voted : Map[MAX_MEMBERS] U8,
    member_count : U8,
  }

type Error
  | Unauthorized
  | AlreadyVoted

handler approve (member_index : U8) (member_pubkey : Pubkey) : State.Active -> State.Active {
  accounts {
    approver : signer
    state    : writable
  }
  requires member_index < member_count and members[member_index] == approver else Unauthorized
  requires voted[member_index] == 0 else AlreadyVoted
  effect {
    Active.voted[member_index] := 1
  }
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("multisig.qedspec");
        let out_path = dir.path().join("tests/proptest.rs");
        std::fs::write(&spec_path, src).unwrap();
        let spec = crate::check::parse_spec_file(&spec_path).expect("parse");
        let mir = crate::mir::lower(&spec);
        let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
        generate_impl(&mir, &spec, Some(&out_path), &mut rec).unwrap();
        let body = std::fs::read_to_string(&out_path).unwrap();

        // (a) Pubkey param strategy is type-dispatched, never a range.
        assert!(
            !body.contains("0[u8; 32]"),
            "Pubkey param must not render a numeric range strategy:\n{body}"
        );
        assert!(
            body.contains("member_pubkey in prop::array::uniform32"),
            "Pubkey param uses the array strategy:\n{body}"
        );
        // (b) The bare-account term is projected out; the adjacent
        // state/param terms survive.
        assert!(
            !body.contains("== approver"),
            "bare account name must not survive as a free variable:\n{body}"
        );
        assert!(
            body.contains("member_index < s.member_count"),
            "account-free conjunct survives the projection:\n{body}"
        );
        // (c) Effect-LHS subscript is cast to usize.
        assert!(
            body.contains("s.voted[(member_index) as usize] = 1"),
            "effect-LHS subscript casts to usize:\n{body}"
        );
    }

    #[test]
    fn strategy_for_field_primitive_routes_through_strategy_for_type() {
        let spec = ParsedSpec::default();
        let s = strategy_for_field("U64", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s.render(), "0u64..=u64::MAX");
        let s = strategy_for_field("U128", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s.render(), "0u128..=u128::MAX");
        let s = strategy_for_field("I128", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s.render(), "any::<i128>()");
    }

    #[test]
    fn strategy_for_field_map_of_primitive_emits_vec_with_try_into() {
        // Regression: `Map[4] U64` once fell through `strategy_for_type` and
        // emitted `0[u64; 4]..=u64::MAX[u64; 4]`; must route through
        // vec-with-prop_map.
        let spec = ParsedSpec {
            constants: vec![("N".to_string(), "4".to_string())],
            ..ParsedSpec::default()
        };
        let s = strategy_for_field("Map[N] U64", &spec, StrategyMode::Full, None).unwrap();
        let rendered = s.render();
        assert!(
            rendered.starts_with("prop::collection::vec(0u64..=u64::MAX, 4..=4)"),
            "unexpected Map-primitive strategy: {s}"
        );
        assert!(
            rendered.contains(".prop_map(|v| v.try_into().ok().unwrap())"),
            "missing try_into prop_map: {s}"
        );
    }

    #[test]
    fn strategy_for_field_record_routes_to_arb_name() {
        // `Map[N] Account` must route through arb_Account(), not
        // `0u64..=u64::MAX`.
        let src = r#"spec T
const N = 4
type Account = { active : U8, capital : U128 }
state { accounts : Map[N] Account }
handler noop { }
"#;
        let spec = parse_str(src).expect("parse");
        let s = strategy_for_field("Account", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s.render(), "arb_Account()");

        let s = strategy_for_field("Map[N] Account", &spec, StrategyMode::Full, None).unwrap();
        assert!(
            s.render()
                .starts_with("prop::collection::vec(arb_Account(), 4..=4)"),
            "Map-record strategy didn't call into arb_Account: {s}"
        );
    }

    #[test]
    fn strategy_for_field_unit_sum_routes_to_arb_name() {
        // ParsedSpec fixture: the adapter only populates `sum_types` for
        // `Map[N] <SumName>` references, so test the strategy in isolation.
        let spec = spec_with_unit_sum("Status", &["Open", "Closed", "Cancelled"]);
        let s = strategy_for_field("Status", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s.render(), "arb_Status()");
    }

    #[test]
    fn strategy_for_field_type_alias_resolves_transitively() {
        // `type AccountIdx = Fin[N]` — strategy routes through the Fin
        // handler and honors the DECLARED bound: exactly [0, N), never
        // the pre-#330 hard-coded 0..=1024.
        let src = r#"spec T
const N = 4
type AccountIdx = Fin[N]
state { i : AccountIdx }
handler noop { }
"#;
        let spec = parse_str(src).expect("parse");
        let s = strategy_for_field("AccountIdx", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(s.render(), "0usize..4usize");
    }

    #[test]
    fn strategy_for_fin_literal_bound_and_option_are_typed() {
        // `Fin[8]` (numeric-literal bound, #327 grammar) → [0, 8);
        // `Option U64` → prop::option::of; `Vec U64` → explicit error,
        // never a u64 fallback (#330).
        let src = r#"spec T
state { total : U64 }
handler noop { }
"#;
        let spec = parse_str(src).expect("parse");
        let fin = strategy_for_field("Fin[8]", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(fin.render(), "0usize..8usize");
        let opt = strategy_for_field("Option U64", &spec, StrategyMode::Full, None).unwrap();
        assert_eq!(opt.render(), "prop::option::of(0u64..=u64::MAX)");
        let vec_err = strategy_for_field("Vec U64", &spec, StrategyMode::Full, None)
            .unwrap_err()
            .to_string();
        assert!(vec_err.contains("no length-bound policy"), "{}", vec_err);
        let unknown_err = strategy_for_field("Mystery", &spec, StrategyMode::Full, None)
            .unwrap_err()
            .to_string();
        assert!(unknown_err.contains("unknown_type"), "{}", unknown_err);
    }

    #[test]
    fn emit_record_prop_composes_emits_block_per_record() {
        let spec = spec_with_record("Account", &[("active", "U8"), ("balance", "U128")]);
        let mut out = String::new();
        emit_record_prop_composes(&mut out, &spec).expect("emit");
        assert!(
            out.contains("prop_compose!"),
            "should emit prop_compose! block: {out}"
        );
        assert!(
            out.contains("fn arb_Account()"),
            "should define arb_Account: {out}"
        );
        assert!(
            out.contains("active in 0u8..=255u8"),
            "should strategy active field: {out}"
        );
        assert!(
            out.contains("balance in 0u128..=u128::MAX"),
            "should strategy balance field: {out}"
        );
    }

    #[test]
    fn emit_unit_sum_prop_oneofs_emits_fn_per_sum() {
        let spec = spec_with_unit_sum("Error", &["NotAdmin", "InsufficientFunds", "VaultOverflow"]);
        let mut out = String::new();
        emit_unit_sum_prop_oneofs(&mut out, &spec).expect("emit");
        assert!(
            out.contains("fn arb_Error() -> impl Strategy<Value = Error>"),
            "should define arb_Error: {out}"
        );
        assert!(out.contains("prop_oneof!"), "should use prop_oneof: {out}");
        assert!(
            out.contains("Just(Error::NotAdmin)"),
            "should include variant: {out}"
        );
        assert!(
            out.contains("Just(Error::InsufficientFunds)"),
            "should include variant: {out}"
        );
    }

    #[test]
    fn emit_unit_sum_skips_payload_variants() {
        // Payload-carrying sums aren't eligible for the unit-enum path —
        // they'd need a variant-aware strategy. Confirm the skip.
        let spec = ParsedSpec {
            sum_types: vec![ParsedSumType {
                name: "State".to_string(),
                variants: vec![
                    ParsedVariant {
                        name: "Active".to_string(),
                        fields: vec![("v".to_string(), "U64".to_string())],
                    },
                    ParsedVariant {
                        name: "Closed".to_string(),
                        fields: vec![],
                    },
                ],
            }],
            ..ParsedSpec::default()
        };
        let mut out = String::new();
        emit_unit_sum_prop_oneofs(&mut out, &spec).expect("emit");
        assert!(
            !out.contains("arb_State"),
            "payload-variant sum should not get unit-strategy: {out}"
        );
    }

    #[test]
    fn strategy_for_field_boundary_small_bound_avoids_underflow() {
        let spec = ParsedSpec::default();
        let s = strategy_for_field("U64", &spec, StrategyMode::Boundary, Some("2")).unwrap();
        let rendered = s.render();
        assert_eq!(rendered, "0u64..=2u64");
        assert!(
            !rendered.contains("- 3"),
            "must not emit `(b - 3)` for b < 3"
        );
    }

    // ========================================================================
    // Pre/post preservation harness shape
    // ========================================================================

    /// Parse a spec and emit its full account section to a string.
    fn emit_test_section(src: &str) -> String {
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = crate::mir::lower(&spec);
        let mutable_fields: Vec<&(String, String)> = spec.state_fields.iter().collect();
        let all_fields: Vec<(String, String)> = spec.state_fields.clone();
        let handlers: Vec<&ParsedHandler> = spec.handlers.iter().collect();
        let properties: Vec<&ParsedProperty> = spec.properties.iter().collect();
        let mut out = String::new();
        let mut rec = ObligationRecorder::new(ObligationBackend::Proptest);
        emit_account_section(
            &mut out,
            &mir,
            "TestAccount",
            &mutable_fields,
            &all_fields,
            &handlers,
            &properties,
            &spec.lifecycle_states,
            &spec,
            rust_codegen_util::VIS_PRIVATE,
            &mut rec,
        )
        .expect("emit");
        out
    }

    const BINARY_PROP_SPEC: &str = r#"
spec BinaryPropTest
program_id "11111111111111111111111111111111"

type State
  | Active of { balance : U64, settled : U64 }

type Error
  | E

handler bump (delta : U64) : State.Active -> State.Active {
  permissionless
  effect { balance := balance + delta }
}

property balance_nonneg :
  state.balance >= 0
  preserved_by all

property settled_monotonic :
  state.settled >= old(state.settled)
  preserved_by all
"#;

    #[test]
    fn binary_property_fn_has_pre_post_signature() {
        // Binary property must emit fn(pre: &State, post: &State) — the
        // single-state form was a structural tautology.
        let out = emit_test_section(BINARY_PROP_SPEC);
        assert!(
            out.contains("fn settled_monotonic(pre: &State, post: &State) -> bool"),
            "binary property must have (pre, post) signature; got:\n{}",
            out
        );
        assert!(
            !out.contains("fn settled_monotonic(s: &State)"),
            "binary property must not have single-state signature; got:\n{}",
            out
        );
    }

    #[test]
    fn binary_property_body_uses_post_and_pre_not_s() {
        // Body must reference `post.settled` and `pre.settled`, not the
        // `s.settled >= s.settled` tautology.
        let out = emit_test_section(BINARY_PROP_SPEC);
        let body_start = out.find("fn settled_monotonic(pre").unwrap_or(0);
        let body_end = out[body_start..]
            .find("}\n\n")
            .map(|i| body_start + i)
            .unwrap_or(out.len());
        let body = &out[body_start..body_end];
        assert!(
            body.contains("post.settled"),
            "binary body must reference post.settled; got: {}",
            body
        );
        assert!(
            body.contains("pre.settled"),
            "binary body must reference pre.settled; got: {}",
            body
        );
        // The tautology shape must NOT appear.
        assert!(
            !body.contains("s.settled >= s.settled")
                && !body.contains("post.settled >= post.settled"),
            "binary body must not be a structural tautology; got: {}",
            body
        );
    }

    #[test]
    fn unary_property_fn_keeps_single_state_signature() {
        // Unary properties stay `fn p(s: &State) -> bool`.
        let out = emit_test_section(BINARY_PROP_SPEC);
        assert!(
            out.contains("fn balance_nonneg(s: &State) -> bool"),
            "unary property must keep single-state signature; got:\n{}",
            out
        );
    }

    #[test]
    fn assert_all_properties_skips_binary() {
        // assert_all_properties is the wrong shape for binary properties.
        let out = emit_test_section(BINARY_PROP_SPEC);
        // assert_all_properties lives in emit_sequence_test_for, outside this
        // fragment — only pin that the section never calls settled_monotonic
        // with a single arg.
        assert!(
            !out.contains("settled_monotonic(s)"),
            "binary property must not be called with single state; got:\n{}",
            out
        );
    }

    #[test]
    fn preservation_test_captures_pre_state() {
        // Each preservation test must capture `let pre = s.clone();` before
        // the handler call so the post-assertion has both states in scope.
        let out = emit_test_section(BINARY_PROP_SPEC);
        let test_start = out
            .find("fn bump_preserves_settled_monotonic")
            .unwrap_or_else(|| panic!("missing bump_preserves_settled_monotonic; got:\n{}", out));
        let test_end = out[test_start..]
            .find("    }\n}")
            .map(|i| test_start + i)
            .unwrap_or(out.len());
        let body = &out[test_start..test_end];
        assert!(
            body.contains("let pre = s.clone();"),
            "preservation test must capture pre-state; got: {}",
            body
        );
        assert!(
            body.contains("let mut post = s;"),
            "preservation test must rename mutated state to `post`; got: {}",
            body
        );
        assert!(
            body.contains("bump(&mut post"),
            "handler must mutate `post`, not `s`; got: {}",
            body
        );
        assert!(
            body.contains("settled_monotonic(&pre, &post)"),
            "binary post-assert must use (&pre, &post); got: {}",
            body
        );
    }

    #[test]
    fn preservation_test_unary_assertion_uses_post() {
        // Unary post-assert must call `<prop>(&post)`, not `<prop>(&s)`.
        let out = emit_test_section(BINARY_PROP_SPEC);
        let test_start = out
            .find("fn bump_preserves_balance_nonneg")
            .unwrap_or_else(|| panic!("missing bump_preserves_balance_nonneg; got:\n{}", out));
        let test_end = out[test_start..]
            .find("    }\n}")
            .map(|i| test_start + i)
            .unwrap_or(out.len());
        let body = &out[test_start..test_end];
        assert!(
            body.contains("balance_nonneg(&post)"),
            "unary post-assert must use (&post); got: {}",
            body
        );
        assert!(
            !body.contains("balance_nonneg(&s)"),
            "unary post-assert must not still reference (&s); got: {}",
            body
        );
    }

    /// An init handler transitions from the implicit `Uninitialized`
    /// pre-state even when the State ADT does not declare it as a variant.
    /// The lifecycle enums must still carry the variant the transition fns
    /// reference (else the harness fails to compile, E0599), and the
    /// sequence must seed that pre-state (else it runs non-init handlers
    /// before init).
    #[test]
    fn init_from_undeclared_uninitialized_is_injected_into_lifecycle() {
        let (spec, body) = emit_full_harness(
            r#"
spec Vault

type Vault
  | Live of { total : U64 }
  | Closed

type Error
  | InvalidAmount

handler init : Vault.Uninitialized -> Vault.Live {
  effect { total := 0 }
}

handler deposit (amount : U64) : Vault.Live -> Vault.Live {
  requires amount > 0 else InvalidAmount
  effect { total += amount }
}

property total_nonneg :
  state.total >= 0
  preserved_by all
"#,
        );

        // The adapter injects the sentinel as the initial lifecycle state.
        assert_eq!(
            spec.lifecycle_states.first().map(String::as_str),
            Some("Uninitialized"),
            "undeclared init pre-state must be injected first"
        );
        // Both enums the transition fns reference must carry the variant.
        assert!(
            body.contains("Status::Uninitialized") && body.contains("Lifecycle::Uninitialized"),
            "Status/Lifecycle must include the injected Uninitialized:\n{body}"
        );
        // The transition table references it — with it declared, no E0599.
        assert!(
            body.contains("(Lifecycle::Uninitialized, Op::Init) => Some(Lifecycle::Live)"),
            "transition table should key off the injected pre-state:\n{body}"
        );
        // The sequence seeds the pre-existence state, not `Live`.
        assert!(
            body.contains("let mut lifecycle = Lifecycle::Uninitialized;")
                && body.contains("status: Status::Uninitialized,"),
            "sequence must seed the injected initial state:\n{body}"
        );
    }

    /// A `state.F <= CONST` property bounds `F`, but must not silently
    /// shrink an *unrelated* numeric field's domain: a type discriminant
    /// with no property of its own keeps its full range, so proptest can
    /// still reach every value (regression: a `version <= 1` bound once
    /// leaked onto every field, capping a `token_kind` discriminant to
    /// `0..=1`).
    #[test]
    fn unrelated_field_keeps_full_domain_when_another_field_is_bounded() {
        let (_spec, body) = emit_full_harness(
            r#"
spec Vault

state { token_kind : U8, version : U8 }

type Error
  | InvalidAmount

handler bump (v : U8) {
  requires v > 0 else InvalidAmount
  effect { version := v }
}

property version_bounded :
  state.version <= 1
  preserved_by all
"#,
        );

        // `version` co-occurs with its own bound, so it stays capped.
        assert!(
            body.contains("version in 0u8..=1u8"),
            "explicitly-bounded field should keep its bound:\n{body}"
        );
        // `token_kind` is in no bounded property — full domain, not `0..=1`.
        assert!(
            body.contains("token_kind in 0u8..=255u8"),
            "unrelated discriminant must keep the full u8 domain:\n{body}"
        );
    }

    /// A ghost accumulator has no reject path, so a full-domain `arb_op`
    /// amount would overflow-panic it in the debug sequence harness. The
    /// update renders SATURATING arithmetic — clamping at the type bound,
    /// never panicking — including a multiplicative term (`amount * 21`)
    /// that a `type_max/SEQ_LEN` input bound could not have tamed. No input
    /// bounding, so every param keeps its full domain.
    #[test]
    fn ghost_accumulator_saturates_and_params_stay_full_range() {
        let (_spec, body) = emit_full_harness(
            r#"
spec Vol

state { balance : U64 }

type Error
  | InvalidAmount

ghost volume : U64 {
  init { 0 }
  on deposit { volume := state.volume + amount * 21 }
}

handler deposit (amount : U64) {
  requires amount > 0 else InvalidAmount
  effect { balance := state.balance + amount }
}

handler noop {
  effect { balance := state.balance }
}

property vol_ge_balance :
  state.volume >= state.balance
  preserved_by all
"#,
        );

        // Nested saturating: `amount * 21` and the accumulation both clamp.
        assert!(
            body.contains("s.volume = (s.volume).saturating_add((amount).saturating_mul(21));"),
            "ghost accumulator must render nested saturating arithmetic:\n{body}"
        );
        // No input bounding — the param keeps its full domain (a pure
        // assignment `last := amount` would otherwise lose 95% of it).
        assert!(
            body.contains("(0u64..=u64::MAX).prop_map(|v| Op::Deposit(v))"),
            "arb_op param must keep the full domain (no accumulator bounding):\n{body}"
        );
    }

    /// Bound propagation is per connected-component, not one global
    /// minimum: `a <= 10; a >= b` and `c <= 1000; c >= d` are unrelated
    /// property groups, so `d` borrows `c`'s 1000 — not `a`'s tighter 10.
    #[test]
    fn bound_propagation_is_per_component_not_global_minimum() {
        let (_spec, body) = emit_full_harness(
            r#"
spec Groups

state { a : U64, b : U64, c : U64, d : U64 }

type Error
  | Bad

handler touch (x : U64) {
  requires x > 0 else Bad
  effect { a := x }
}

property a_cap : state.a <= 10 preserved_by all
property a_ge_b : state.a >= state.b preserved_by all
property c_cap : state.c <= 1000 preserved_by all
property c_ge_d : state.c >= state.d preserved_by all
"#,
        );

        // Grab the `arb_state` body so we assert on the full-domain strategy.
        let start = body.find("fn arb_state").expect("arb_state present");
        let arb = &body[start..start + body[start..].find("-> State").unwrap_or(400)];
        assert!(
            arb.contains("a in 0u64..=10u64") && arb.contains("b in 0u64..=10u64"),
            "component {{a,b}} must use its own bound 10:\n{arb}"
        );
        assert!(
            arb.contains("c in 0u64..=1000u64") && arb.contains("d in 0u64..=1000u64"),
            "component {{c,d}} must borrow c's 1000, not the global min 10:\n{arb}"
        );
    }

    /// `arb_state` repairs conservation invariants (`field >= sum`,
    /// `field == sum`) so preservation tests satisfy their `prop_assume`
    /// by construction instead of reject-exhausting. Stacked invariants
    /// repair in dependency order (a field feeding another's RHS first).
    #[test]
    fn arb_state_repairs_conservation_invariants_in_dependency_order() {
        let (_spec, body) = emit_full_harness(
            r#"
spec Cons

state { total : U64, reserved : U64, pending : U64, queued : U64 }

type Error | Bad

handler touch (x : U64) {
  requires x > 0 else Bad
  effect { total := state.total + x }
}

property p_total : state.total >= state.reserved + state.pending preserved_by [touch]
property p_pending : state.pending >= state.queued preserved_by [touch]
"#,
        );

        let start = body.find("fn arb_state").expect("arb_state present");
        let end = body[start..]
            .find("arb_boundary_state")
            .map(|i| start + i)
            .unwrap_or(body.len());
        let arb = &body[start..end];
        let ip = arb.find("let pending = pending.max(queued)");
        let it = arb.find("let total = total.max((reserved).saturating_add(pending))");
        assert!(
            ip.is_some() && it.is_some(),
            "both conservation repairs must be emitted:\n{arb}"
        );
        // `pending` feeds `total`'s RHS, so it must be repaired first.
        assert!(
            ip < it,
            "repairs must be in dependency order (pending before total):\n{arb}"
        );
    }

    /// The repair only fires for the recognized conservation shape; a
    /// subtraction on the RHS falls back to the `prop_assume` path (no
    /// `let` shadow), so nothing regresses for unsupported shapes.
    #[test]
    fn arb_state_repair_falls_back_on_unsupported_shape() {
        let (_spec, body) = emit_full_harness(
            r#"
spec Sub

state { a : U64, b : U64, c : U64 }

type Error | Bad

handler touch (x : U64) {
  requires x > 0 else Bad
  effect { a := state.a + x }
}

property rel : state.a >= state.b - state.c preserved_by [touch]
"#,
        );

        let start = body.find("fn arb_state").expect("arb_state present");
        let end = body[start..]
            .find("arb_boundary_state")
            .map(|i| start + i)
            .unwrap_or(body.len());
        let arb = &body[start..end];
        assert!(
            !arb.contains("let a ="),
            "a `-` RHS is unsupported and must not emit a repair:\n{arb}"
        );
    }
}
