//! State-driven symbolic account construction (#162 phase 2).
//!
//! The qedspec's **State** is the construction layout — NOT the IDL. The IDL is
//! lossy (stale, Anchor-0.29 format, strips leading underscores); the State, now
//! able to mirror a real `#[account]` struct verbatim (`Option<T>` and
//! `Vec<Record>` fields landed with G9/G10 — #173/#174), is faithful and
//! checked. Given the State's fields + the spec's record types, emit
//! `symbolic_<state>()` whose every field is `kani::any()`: scalars direct,
//! `Pubkey` from a symbolic byte array, `Option` symbolic Some/None, `Vec` as a
//! fixed-length-K `vec![…]` of symbolic elements (`pragma kani_vec_bound`,
//! default 1 — a symbolic *length* OOMs CBMC; see `emit_value`), nested records
//! recursed, and enum (sum-type) fields via symbolic variant selection (#177).
//!
//! The brownfield harness pairs this with
//! `kani::assume(state.invariant().is_ok())` so Kani explores only well-formed
//! instances. It replaces the `todo!("build a symbolic state account struct")`
//! agent-fill site: construction is now generated from the spec; only the
//! effect + validity-gate call remains agent-fill.
//!
//! CONTRACT: the real struct name comes from `pragma state_struct = <Name>`
//! (see `resolve_state_struct`) — a brownfield `#[account]` struct's name isn't
//! otherwise in the spec. A wrong name surfaces as a `crate::<Name>` not-found
//! compile error, not silent wrong behaviour.

use crate::check::{ParsedRecordType, ParsedSpec, ParsedSumType};
use crate::mir::{parse_ty, Ty};

/// Everything `emit_value` needs to recurse: the spec's record + sum-type
/// tables (for nested-struct / enum construction) and the fixed `Vec` length.
/// Bundled so the recursion carries one ref, not five positional args.
pub(crate) struct CtorCtx<'a> {
    pub records: &'a [ParsedRecordType],
    /// Owned because sum types come from TWO places: `spec.sum_types` (only
    /// Map-value sum types land there) and `spec.account_types` entries that
    /// carry variants (a `type X | V of {…}` used as a plain field type routes
    /// there). `from_spec` merges them so field-typed enums resolve.
    pub sum_types: Vec<ParsedSumType>,
    pub vec_bound: usize,
    /// Prefix for every constructed type name. `"crate::"` (default) when the
    /// harness sits at the crate root and the types are re-exported there;
    /// `""` (bare) when `pragma state_module` places the harness INSIDE the
    /// module that defines them (a private `mod` can't be reached via
    /// `crate::<Type>` — see #180/G17), so the harness uses `use super::*`.
    pub type_path: String,
}

impl<'a> CtorCtx<'a> {
    /// Build from a spec: records + merged sum types, `Vec` length from
    /// `pragma kani_vec_bound` (default 1 — see `vec_bound_of`), and the type
    /// prefix from `pragma state_module` (see `type_path`).
    pub(crate) fn from_spec(spec: &'a ParsedSpec) -> Self {
        let mut sum_types = spec.sum_types.clone();
        for at in &spec.account_types {
            if !at.variants.is_empty() {
                sum_types.push(ParsedSumType {
                    name: at.name.clone(),
                    variants: at.variants.clone(),
                });
            }
        }
        CtorCtx {
            records: &spec.records,
            sum_types,
            vec_bound: vec_bound_of(spec),
            type_path: type_path_of(spec),
        }
    }
}

/// `""` (bare, via `use super::*`) when `pragma state_module` is set — the
/// in-module placement for private-module types; else `"crate::"`.
pub(crate) fn type_path_of(spec: &ParsedSpec) -> String {
    if spec.pragma_value("state_module").is_some() {
        String::new()
    } else {
        "crate::".to_string()
    }
}

/// True when the harness must be placed inside the target module (bare type
/// names + `use super::*`) rather than at the crate root — driven by
/// `pragma state_module`.
pub(crate) fn is_in_module(spec: &ParsedSpec) -> bool {
    spec.pragma_value("state_module").is_some()
}

/// The pre-state validity method the harness assumes: `pragma state_invariant`
/// (default `invariant`); `= none` returns `None` (skip the assume — the struct
/// has no validity method, or its `invariant()` panics under fully-symbolic
/// input for a property that doesn't need it).
pub(crate) fn invariant_method(spec: &ParsedSpec) -> Option<String> {
    match spec.pragma_value("state_invariant") {
        Some("none") => None,
        Some(m) => Some(m.to_string()),
        None => Some("invariant".to_string()),
    }
}

/// `pragma kani_stub_clock = <anything>` → the agent-fill effect calls a method
/// that reads `Clock::get()`; the harness must stub it (`-Z stubbing`) + emit
/// the stub fn (G14, #178). Without it, `Clock::get()` aborts under Kani. Any
/// value signals it (`true` is a reserved word, so callers write e.g.
/// `= clock`).
pub(crate) fn wants_clock_stub(spec: &ParsedSpec) -> bool {
    spec.pragma_value("kani_stub_clock").is_some()
}

/// `true` when the harness touches `Pubkey` (a State/record field or a param,
/// directly or inside `Option`/`Vec`) → the harness stubs `Pubkey`'s derived
/// `==` with an abstract wide-integer compare (#182 Tier 1), so CBMC doesn't
/// bit-blast a 32-byte `memcmp` loop (unwind 2 vs ≥34). The stub is proven
/// bit-for-bit equivalent to `==`, so it can't change any result. Opt out with
/// `pragma kani_abstract_pubkey = off`.
pub(crate) fn wants_pubkey_abstraction(spec: &ParsedSpec) -> bool {
    if spec.pragma_value("kani_abstract_pubkey") == Some("off") {
        return false;
    }
    let mentions_pubkey = |t: &str| {
        t.split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "Pubkey")
    };
    spec.state_fields.iter().any(|(_, t)| mentions_pubkey(t))
        || spec
            .records
            .iter()
            .any(|r| r.fields.iter().any(|(_, t)| mentions_pubkey(t)))
        || spec
            .handlers
            .iter()
            .any(|h| h.takes_params.iter().any(|(_, t)| mentions_pubkey(t)))
}

/// The abstract-`Pubkey`-equality support fn (emitted once when
/// `wants_pubkey_abstraction`). Reinterprets the 32 bytes as two `u128` halves
/// and compares — 2 word-comparisons, not a 32-byte `memcmp` loop. Sound
/// (machine-checked equivalent to `derive(PartialEq)`, #182); verification-only
/// (`#[cfg(kani)]`), so the transmute never runs on-chain.
pub(crate) fn pubkey_eq_abstract_fn() -> String {
    "// Abstract Pubkey equality + ordering (#182 Tier 1): the 32 bytes as two\n\
     // u128 halves — word-comparisons, NOT a 32-byte memcmp/lex loop (Kani unwind\n\
     // 2 vs >= 34). Both PROVEN bit-for-bit equivalent to derive(PartialEq/Ord);\n\
     // the proofs stub `==` and `cmp` to these. Verification-only, so the\n\
     // transmute never runs on-chain.\n\
     #[allow(clippy::missing_transmute_annotations)]\n\
     fn pk_eq_abstract(a: &anchor_lang::prelude::Pubkey, b: &anchor_lang::prelude::Pubkey) -> bool {\n\
     \x20   let a: [u128; 2] = unsafe { core::mem::transmute(a.to_bytes()) };\n\
     \x20   let b: [u128; 2] = unsafe { core::mem::transmute(b.to_bytes()) };\n\
     \x20   a[0] == b[0] && a[1] == b[1]\n\
     }\n\
     // Pubkey `Ord` is byte-lexicographic (index 0 most significant) = big-endian\n\
     // u256; compare two big-endian u128 halves lexicographically (`binary_search`\n\
     // / `sort` use this).\n\
     #[allow(clippy::missing_transmute_annotations)]\n\
     fn pk_cmp_abstract(a: &anchor_lang::prelude::Pubkey, b: &anchor_lang::prelude::Pubkey) -> core::cmp::Ordering {\n\
     \x20   let a: [u128; 2] = unsafe { core::mem::transmute(a.to_bytes()) };\n\
     \x20   let b: [u128; 2] = unsafe { core::mem::transmute(b.to_bytes()) };\n\
     \x20   (a[0].swap_bytes(), a[1].swap_bytes()).cmp(&(b[0].swap_bytes(), b[1].swap_bytes()))\n\
     }\n"
        .to_string()
}

/// The `#[kani::stub]` attributes that redirect `Pubkey`'s derived `==` and
/// `cmp` to the abstract versions (needs `-Z stubbing`).
pub(crate) fn pubkey_stub_attr() -> &'static str {
    "#[kani::stub(<anchor_lang::prelude::Pubkey as core::cmp::PartialEq>::eq, pk_eq_abstract)]\n\
     #[kani::stub(<anchor_lang::prelude::Pubkey as core::cmp::Ord>::cmp, pk_cmp_abstract)]\n"
}

/// `pragma kani_stub_pda = <anything>` → the agent-fill effect calls code that
/// derives a PDA (`find_program_address` = sha256 + a bump search, which
/// bit-blasts catastrophically). Opt-in (the derivation is inside called
/// methods, not visible from the spec). #182 Tier 2.
pub(crate) fn wants_pda_abstraction(spec: &ParsedSpec) -> bool {
    spec.pragma_value("kani_stub_pda").is_some()
}

/// The abstract PDA-derivation support fn (emitted once when
/// `wants_pda_abstraction`). Returns an OPAQUE deterministic address, not the
/// real sha256. Sound over-approximation for safety — the program must hold for
/// ANY derived address; we never re-verify that sha256 is sha256.
pub(crate) fn pda_stub_fn() -> String {
    "// Abstract PDA derivation (#182 Tier 2): an OPAQUE address, NOT the real\n\
     // sha256 + bump search (which bit-blasts catastrophically — the sha2\n\
     // GenericArray fold doesn't even unwind). Sound over-approximation for\n\
     // safety properties; verification-only.\n\
     fn find_pda_abstract(\n\
     \x20   _seeds: &[&[u8]],\n\
     \x20   _program_id: &anchor_lang::prelude::Pubkey,\n\
     ) -> (anchor_lang::prelude::Pubkey, u8) {\n\
     \x20   (anchor_lang::prelude::Pubkey::new_from_array(kani::any()), kani::any())\n\
     }\n"
    .to_string()
}

/// The `#[kani::stub]` attribute redirecting `find_program_address` to the
/// abstract (needs `-Z stubbing`).
pub(crate) fn pda_stub_attr() -> &'static str {
    "#[kani::stub(solana_program::pubkey::Pubkey::find_program_address, find_pda_abstract)]\n"
}

/// The `Clock::get` stub fn (emitted once when `wants_clock_stub`). Fixed,
/// plausible fields — `approve`/`cancel` only read `unix_timestamp` into the
/// status, which the membership/threshold properties don't constrain.
pub(crate) fn clock_stub_fn() -> String {
    "// G14: symbolic runs read `Clock::get()`; Kani stubs it (`-Z stubbing`).\n\
     fn stub_clock_get(\n\
     ) -> core::result::Result<anchor_lang::solana_program::clock::Clock, anchor_lang::solana_program::program_error::ProgramError>\n\
     {\n\
    \x20   Ok(anchor_lang::solana_program::clock::Clock {\n\
    \x20       slot: 1,\n\
    \x20       epoch_start_timestamp: 0,\n\
    \x20       epoch: 0,\n\
    \x20       leader_schedule_epoch: 0,\n\
    \x20       unix_timestamp: 1_700_000_000,\n\
    \x20   })\n\
     }\n"
        .to_string()
}

/// Resolve the real on-chain struct this brownfield spec's State mirrors, as
/// `(struct_name, fields)`.
///
/// The struct NAME is declared by `pragma state_struct = <Name>` — a brownfield
/// program's `#[account]` struct (`Settings`, `SmartAccount`, …) has a specific
/// name that the spec's greenfield naming (`<Program>Account`) doesn't capture,
/// and the bare `state { … }` sugar defaults to a synthetic `"State"` that would
/// build a wrong `crate::State`. The pragma is the one thing only the user
/// knows; everything else (the field layout, incl. `Option<T>`/`Vec<Record>`
/// after #173/#174) is already in the spec's canonical `state_fields`.
///
/// Returns `None` when the pragma is absent (or the State has no fields) — the
/// caller keeps its construction `todo!()` rather than guess the struct name.
pub(crate) fn resolve_state_struct(spec: &ParsedSpec) -> Option<(&str, &[(String, String)])> {
    let name = spec.pragma_value("state_struct")?;
    // The `state { … }` block lands in `records` as "State". Prefer it over
    // `spec.state_fields`, which the adapter derives from `account_types.first()`
    // — a field-typed enum (`type Status | …`, also an account-type ADT) can
    // sort ahead of the state there and shadow it (its variant fields become
    // `state_fields`). `records["State"]` is unaffected by that ordering.
    let fields = spec
        .records
        .iter()
        .find(|r| r.name == "State")
        .map(|r| r.fields.as_slice())
        .unwrap_or(spec.state_fields.as_slice());
    if fields.is_empty() {
        return None;
    }
    Some((name, fields))
}

/// Emit `fn symbolic_<snake(struct_name)>() -> crate::<struct_name> { … }` with
/// every field constructed symbolically. Returns `None` when a field can't be
/// built without agent knowledge (an imported/unresolved type, or a `Map`
/// field) — the caller keeps the `todo!()` fallback rather than emit a
/// half-`todo!()` ctor that reads as "generated" but isn't.
pub(crate) fn emit_state_ctor(
    struct_name: &str,
    fields: &[(String, String)],
    ctx: &CtorCtx,
) -> Option<String> {
    // Build every field first: bail on the FIRST unconstructible one so we
    // never emit a partially-`todo!()` constructor.
    let mut field_lines = Vec::with_capacity(fields.len());
    for (name, ty_str) in fields {
        let expr = emit_value(&parse_ty(ty_str), ctx, 0)?;
        field_lines.push(format!("        {name}: {expr},"));
    }

    let tp = &ctx.type_path;
    let mut out = String::new();
    out.push_str(&format!(
        "/// Fully-symbolic `{struct_name}` — every field `kani::any()`; pair with\n\
         /// `kani::assume(state.invariant().is_ok())` to explore only valid states.\n",
    ));
    out.push_str(&format!(
        "fn symbolic_{}() -> {tp}{struct_name} {{\n    {tp}{struct_name} {{\n",
        pascal_to_snake(struct_name),
    ));
    for line in field_lines {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("    }\n}\n");
    Some(out)
}

/// The `symbolic_<name>` fn name for a struct — exposed so the harness emitter
/// can reference the ctor without re-deriving the mangling.
pub(crate) fn ctor_fn_name(struct_name: &str) -> String {
    format!("symbolic_{}", pascal_to_snake(struct_name))
}

/// Recursive `Ty` → symbolic-construction expression. `None` = unconstructible
/// without agent knowledge.
fn emit_value(ty: &Ty, ctx: &CtorCtx, depth: usize) -> Option<String> {
    if depth > 8 {
        return None; // recursion guard (mutually-recursive record/sum types)
    }
    Some(match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 | Ty::I64 | Ty::I128 | Ty::Bool => {
            "kani::any()".to_string()
        }
        Ty::Pubkey => "anchor_lang::prelude::Pubkey::new_from_array(kani::any())".to_string(),
        Ty::Custom(s) => {
            // `Option T` / `Vec T` ride as `Ty::Custom("Option T")` / `"Vec T"`
            // (the MIR `Ty` enum has no first-class Option/Vec — see #173/#174);
            // the inner is a single named type (scalar or record).
            if let Some(inner) = s.strip_prefix("Option ") {
                let inner_expr = emit_value(&parse_ty(inner.trim()), ctx, depth + 1)?;
                format!("if kani::any() {{ Some({inner_expr}) }} else {{ None }}")
            } else if let Some(inner) = s.strip_prefix("Vec ") {
                let inner_expr = emit_value(&parse_ty(inner.trim()), ctx, depth + 1)?;
                // FIXED-LENGTH-K symbolic Vec — `vec![<elem>, …]` with K
                // independent symbolic elements, NOT a symbolic-length `while`
                // loop. A symbolic length forces CBMC to unwind the build loop
                // (and the real `invariant()`'s own iteration over the field) to
                // the harness `#[kani::unwind]` bound and to model Vec
                // growth/realloc — which dominates (OOMs) the proof even for a
                // property that never reads the collection. K = `pragma
                // kani_vec_bound` (default 1). Raise it for a property that DOES
                // read the collection; a bounded (BMC) length is the trade-off.
                let elems = std::iter::repeat_n(inner_expr, ctx.vec_bound)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("vec![{elems}]")
            } else if let Some(rec) = ctx.records.iter().find(|r| &r.name == s) {
                // Nested record — recurse into its fields.
                let inner: Option<Vec<String>> = rec
                    .fields
                    .iter()
                    .map(|(fname, fty)| {
                        let e = emit_value(&parse_ty(fty), ctx, depth + 1)?;
                        Some(format!("{fname}: {e}"))
                    })
                    .collect();
                format!("{}{s} {{ {} }}", ctx.type_path, inner?.join(", "))
            } else if let Some(sum) = ctx.sum_types.iter().find(|t| &t.name == s) {
                // Enum (sum type) — symbolic variant selection (#177/G13).
                emit_enum(s, sum, ctx, depth)?
            } else {
                // Imported / unresolved type — needs agent knowledge.
                return None;
            }
        }
        // A `Map[N] T` state field has no faithful symbolic default here (the
        // on-chain layout is a fixed array or a BTreeMap, spec-dependent).
        Ty::Map { .. } => return None,
    })
}

/// Symbolic enum construction: pick a variant with `kani::any::<usize>() % N`
/// and build its payload. Unit variants construct directly; named-payload
/// variants (`Active of { timestamp : I64 }`) recurse per field. A single
/// discriminant covers all N variants, so Kani explores every variant.
fn emit_enum(name: &str, sum: &ParsedSumType, ctx: &CtorCtx, depth: usize) -> Option<String> {
    let n = sum.variants.len();
    if n == 0 {
        return None;
    }
    let tp = &ctx.type_path;
    let mut arms = Vec::with_capacity(n);
    for (i, v) in sum.variants.iter().enumerate() {
        let ctor = if v.fields.is_empty() {
            format!("{tp}{name}::{}", v.name) // unit variant
        } else {
            // Named-payload struct variant — recurse per field.
            let fs: Option<Vec<String>> = v
                .fields
                .iter()
                .map(|(fname, fty)| {
                    let e = emit_value(&parse_ty(fty), ctx, depth + 1)?;
                    Some(format!("{fname}: {e}"))
                })
                .collect();
            format!("{tp}{name}::{} {{ {} }}", v.name, fs?.join(", "))
        };
        // Map the last variant to the `_` catch-all (`% n` yields `0..n-1`).
        if i + 1 == n {
            arms.push(format!("_ => {ctor}"));
        } else {
            arms.push(format!("{i} => {ctor}"));
        }
    }
    Some(format!(
        "match kani::any::<usize>() % {n} {{ {} }}",
        arms.join(", ")
    ))
}

/// The fixed length used for symbolic `Vec` state fields: `pragma
/// kani_vec_bound = <N>` if set (and parseable), else 1. Kept small by default
/// because the real `invariant()`'s iteration over the field unwinds per
/// element; raise it only for a property that reads into the collection.
pub(crate) fn vec_bound_of(spec: &ParsedSpec) -> usize {
    spec.pragma_value("kani_vec_bound")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
}

/// `Settings` → `settings`, `SmartAccount` → `smart_account`. Struct names are
/// PascalCase; field names are already snake_case (mirror the real struct) so
/// they're used verbatim.
fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, fields: &[(&str, &str)]) -> ParsedRecordType {
        ParsedRecordType {
            name: name.to_string(),
            fields: fields
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
        }
    }

    /// A sum type from `(variant, [(field, ty)])` pairs; empty fields = unit.
    fn sum(name: &str, variants: &[(&str, &[(&str, &str)])]) -> ParsedSumType {
        ParsedSumType {
            name: name.to_string(),
            variants: variants
                .iter()
                .map(|(vn, fs)| crate::check::ParsedVariant {
                    name: vn.to_string(),
                    fields: fs
                        .iter()
                        .map(|(f, t)| (f.to_string(), t.to_string()))
                        .collect(),
                })
                .collect(),
        }
    }

    fn ctx<'a>(
        records: &'a [ParsedRecordType],
        sum_types: &[ParsedSumType],
        vec_bound: usize,
    ) -> CtorCtx<'a> {
        CtorCtx {
            records,
            sum_types: sum_types.to_vec(),
            vec_bound,
            type_path: "crate::".to_string(),
        }
    }

    #[test]
    fn pascal_to_snake_cases() {
        assert_eq!(pascal_to_snake("Settings"), "settings");
        assert_eq!(pascal_to_snake("SmartAccount"), "smart_account");
        assert_eq!(ctor_fn_name("Settings"), "symbolic_settings");
    }

    #[test]
    fn scalars_pubkey_option_vec_and_nested() {
        let records = vec![
            rec(
                "SmartAccountSigner",
                &[("key", "Pubkey"), ("permissions", "Permissions")],
            ),
            rec("Permissions", &[("mask", "U8")]),
        ];
        // Scalars + Pubkey.
        assert_eq!(
            emit_value(&parse_ty("U64"), &ctx(&records, &[], 1), 0).unwrap(),
            "kani::any()"
        );
        assert_eq!(
            emit_value(&parse_ty("Pubkey"), &ctx(&records, &[], 1), 0).unwrap(),
            "anchor_lang::prelude::Pubkey::new_from_array(kani::any())"
        );
        // Option<Pubkey> → symbolic Some/None.
        let opt = emit_value(&parse_ty("Option Pubkey"), &ctx(&records, &[], 1), 0).unwrap();
        assert!(opt.contains("if kani::any()") && opt.contains("Some(") && opt.contains("None"));
        // Vec<SmartAccountSigner> → FIXED-LENGTH-K `vec![…]` (no symbolic-length
        // `while` loop — that OOMs CBMC), K nested symbolic structs.
        let v = emit_value(
            &parse_ty("Vec SmartAccountSigner"),
            &ctx(&records, &[], 2),
            0,
        )
        .unwrap();
        assert!(
            v.starts_with("vec![") && !v.contains("while") && !v.contains("kani::assume(n"),
            "fixed-length vec![], no symbolic-length loop; got {v}"
        );
        assert_eq!(
            v.matches("crate::SmartAccountSigner {").count(),
            2,
            "K=2 elements; got {v}"
        );
        assert!(
            v.contains("crate::Permissions {") && v.contains("mask:"),
            "nested; got {v}"
        );
        // K=1 (the default) → a single element.
        let v1 = emit_value(
            &parse_ty("Vec SmartAccountSigner"),
            &ctx(&records, &[], 1),
            0,
        )
        .unwrap();
        assert_eq!(
            v1.matches("crate::SmartAccountSigner {").count(),
            1,
            "K=1 element; got {v1}"
        );
    }

    #[test]
    fn enum_symbolic_variant_selection() {
        // ProposalStatus-shaped: all named-payload struct variants.
        let sums = vec![sum(
            "ProposalStatus",
            &[
                ("Draft", &[("timestamp", "I64")]),
                ("Active", &[("timestamp", "I64")]),
                ("Approved", &[("timestamp", "I64")]),
            ],
        )];
        let e = emit_value(&parse_ty("ProposalStatus"), &ctx(&[], &sums, 1), 0).unwrap();
        assert!(
            e.starts_with("match kani::any::<usize>() % 3 {"),
            "symbolic 3-way selection; got {e}"
        );
        assert!(
            e.contains("0 => crate::ProposalStatus::Draft { timestamp: kani::any() }")
                && e.contains("_ => crate::ProposalStatus::Approved { timestamp: kani::any() }"),
            "named-payload arms, last is `_`; got {e}"
        );
        // A unit + payload mix (PeriodV2 minus the tuple variant).
        let sums2 = vec![sum(
            "P",
            &[
                ("OneTime", &[]),
                ("Daily", &[]),
                ("Windowed", &[("secs", "U32")]),
            ],
        )];
        let e2 = emit_value(&parse_ty("P"), &ctx(&[], &sums2, 1), 0).unwrap();
        assert!(
            e2.contains("0 => crate::P::OneTime")
                && !e2.contains("OneTime {")
                && e2.contains("_ => crate::P::Windowed { secs: kani::any() }"),
            "unit variant has no payload braces; got {e2}"
        );
    }

    #[test]
    fn full_settings_ctor_is_agent_fill_free() {
        let records = vec![
            rec(
                "SmartAccountSigner",
                &[("key", "Pubkey"), ("permissions", "Permissions")],
            ),
            rec("Permissions", &[("mask", "U8")]),
        ];
        let fields = vec![
            ("seed".into(), "U128".into()),
            ("settings_authority".into(), "Pubkey".into()),
            ("time_lock".into(), "U32".into()),
            ("archival_authority".into(), "Option Pubkey".into()),
            ("signers".into(), "Vec SmartAccountSigner".into()),
        ];
        let ctor = emit_state_ctor("Settings", &fields, &ctx(&records, &[], 1)).unwrap();
        assert!(ctor.contains("fn symbolic_settings() -> crate::Settings"));
        assert!(ctor.contains("settings_authority:") && ctor.contains("time_lock:"));
        assert!(ctor.contains("signers: vec![crate::SmartAccountSigner {"));
        assert!(!ctor.contains("todo!"), "no agent-fill; got:\n{ctor}");
    }

    #[test]
    fn unconstructible_field_bails_to_none() {
        // An unresolved type (not a record or sum type) → whole ctor is None, so
        // the caller keeps its `todo!()` rather than emit a half-built struct.
        let fields = vec![
            ("ok".into(), "U64".into()),
            ("kind".into(), "MysteryType".into()), // unresolved → unconstructible
        ];
        assert!(emit_state_ctor("Thing", &fields, &ctx(&[], &[], 1)).is_none());
        // A `Map` field is likewise unconstructible here.
        let map_fields = vec![("book".into(), "Map[8] U64".into())];
        assert!(emit_state_ctor("Thing", &map_fields, &ctx(&[], &[], 1)).is_none());
    }
}
