//! Completeness-lint suite: the `check_completeness` orchestrator plus the
//! themed rule submodules it drives. Re-exports every rule and helper so
//! both `crate::check::<sym>` (via `check::mod`'s `pub use lints::*`) and
//! `crate::check::lints::<sym>` keep resolving after the split.

use super::*;

mod arithmetic;
mod auth;
mod cpi;
mod ctor_types;
mod known_types;
mod shared;
mod state;
mod structural;

pub(in crate::check::lints) use arithmetic::*;
pub(in crate::check::lints) use auth::*;
pub(crate) use cpi::*;
pub(crate) use shared::*;
pub(in crate::check::lints) use state::*;
pub(in crate::check::lints) use structural::*;

/// Check spec completeness — heuristic rules for under-specification.
/// Returns structured warnings with fix suggestions for agent consumption.
///
/// Pure orchestration: every rule lives in its family submodule
/// (`arithmetic` / `auth` / `cpi` / `state` / `structural`) and returns its
/// own warnings; this function fixes the run sequence and applies the final
/// priority sort. Rule 7 is the one ordering-sensitive call — it reads the
/// warnings accumulated so far.
pub fn check_completeness(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    // Shared per-spec context: the signer hint plus the `Variant.field`
    // index every effect-LHS rule normalizes against.
    let ctx = LintCtx::new(spec);

    let mut warnings = Vec::new();

    // Every type-bearing string must resolve through the canonical type
    // IR (#327) — undeclared / malformed spellings fail check here
    // instead of surfacing as invalid Lean or a silent u64 proptest
    // strategy at codegen time.
    warnings.extend(known_types::check_known_types(spec));

    // Constructor / record-literal / record-update expressions must carry
    // a resolved nominal type for Rust rendering (#325).
    warnings.extend(ctor_types::check_ctor_types(spec));

    // ADT-state `WrongState` compile gate.
    warnings.extend(check_adt_state_missing_wrong_state(spec));

    // Ghost-variable validation.
    warnings.extend(check_ghost_declarations(spec));

    // Hook validation.
    warnings.extend(check_hook_declarations(spec));

    // `auth X` + `permissionless` contradiction.
    warnings.extend(check_contradictory_auth(spec));

    // Rule 1: handler without `auth`.
    warnings.extend(check_no_access_control(&ctx));

    // Rule 2: handler not covered by any property.
    warnings.extend(check_uncovered_operation(spec));

    // Rule 3: add effect without explicit overflow bound.
    warnings.extend(check_unguarded_arithmetic(spec));

    // Account-address RHS assigned into a non-Pubkey field.
    warnings.extend(check_effect_account_key_type_mismatch(spec));

    // Rule 6: handler has no when/then lifecycle.
    warnings.extend(check_no_lifecycle(spec));

    // Rule 4: state fields never modified (excluding Pubkey).
    warnings.extend(check_unused_field(&ctx));

    // Rule 5: property references nonexistent handler.
    warnings.extend(check_dangling_preserved_by(spec));

    // Quantifier over a type that can't be exhausted at test time.
    warnings.extend(check_unchecked_quantifier(spec));

    // P5: quantifier shape unsupported by codegen.
    warnings.extend(check_unsupported_quantifier_shape(spec));

    // P6: Pubkey state fields lower structurally to `[u8; 32]`.
    warnings.extend(check_pubkey_state_field_unsupported(spec));

    // P7: effect references an undeclared state field.
    warnings.extend(check_undeclared_state_field_in_effect(&ctx));

    // Rule 7: takes params (U64) with no guard. Runs after Rule 3 — it
    // reads the accumulated warnings to skip handlers `unguarded_arithmetic`
    // already flagged.
    let rule7 = check_missing_guard_from_takes(spec, &warnings);
    warnings.extend(rule7);

    // Rule 8: takes params + lifecycle transition but no effect.
    warnings.extend(check_missing_effect(spec));

    // Rule 9: handlers with effects but zero properties.
    warnings.extend(check_no_properties(spec));

    // Rule 10: token program in accounts but no `transfers` block.
    warnings.extend(check_missing_cpi_for_token_context(spec));

    // Rule 11: guards but no `errors` block.
    warnings.extend(check_no_errors_block(spec));

    // Rule 12: lifecycle states unreachable by any operation transition.
    warnings.extend(check_lifecycle_unreachable_state(spec));

    // Rule 13: state field written in effects but never read in
    // guards/properties.
    warnings.extend(check_write_without_read(&ctx));

    // Rule 14: guard conjunct subsumed by another on the same operation.

    // Rule 15: lifecycle where every state has outgoing transitions.
    warnings.extend(check_circular_lifecycle_no_terminal(spec));

    // Rule 16: handler outside `preserved_by` modifies property fields.
    warnings.extend(check_excluded_op_modifies_property(spec));

    // Rule 17: doc-string-only invariant (would lower to a vacuous proof).
    warnings.extend(check_invariant_no_body(spec));

    // Validate new-DSL constructs: Map[N] T fields, subscripted effect LHS.
    warnings.extend(check_map_and_subscript(spec));

    // Duplicate effect target in one block: diverges under parallel
    // effect semantics (last-write vs accumulated); codegen refuses.
    warnings.extend(check_duplicate_effect_target(spec));

    // CPI tier lint: call sites whose target is Tier 0 (no ensures declared)
    // get flagged so users see the gap between "my Rust compiles" and "my
    // program is verified." See docs/design/spec-composition.md §2.
    warnings.extend(check_shape_only_cpi(spec));

    // Complement to shape_only_cpi: declared handlers with no `ensures`
    // leave the caller's Lean theorem carrying `by sorry`.
    warnings.extend(check_cpi_no_callee_ensures(spec));

    // Trust-anchor advisory: imported interfaces discharging via Stance-1
    // axiom because the provider shipped no proof package. P2 advisory;
    // the caller still gets discharge.
    warnings.extend(check_cpi_unverified_callee(spec));

    // PDA seed collision: two PDA declarations with identical seed tuples resolve
    // to the same on-chain address — a common source of account confusion bugs.
    warnings.extend(check_pda_collisions(spec));

    // Checked-arithmetic effects (`+=` / `-=`) make the generated Rust
    // reference `<ProgramName>Error::MathOverflow`; without that variant
    // declared, cargo build fails — surface it at check time instead.
    warnings.extend(check_checked_arith_needs_math_overflow(spec));

    // Per-site `or X` overrides or checked_overflow/underflow pragmas
    // referencing undeclared Error variants would also fail cargo build.
    warnings.extend(check_unknown_error_variant(spec));

    // Opt-in non-default arithmetic (`+=?`/`-=?` wrapping, `+=!`/`-=!`
    // saturating) needs surfacing but isn't reproducible from the spec
    // alone — lives in check, not probe (reproducer-only probe contract).
    warnings.extend(check_wrapping_arithmetic_opt_in(spec));

    // Spec-authoring lints for post-codegen-audit security shapes. Keep the
    // current mapping aligned with the auditor category catalog and DSL reference.
    warnings.extend(check_unbound_auth(spec));
    warnings.extend(check_unguarded_indexed_mutation(spec));
    warnings.extend(check_scalar_counter_no_dedup(spec));
    warnings.extend(check_unguarded_terminal_transition(spec));
    warnings.extend(check_unconditional_value_transfer(spec));

    // Flag bare same-named field references in multi-ADT specs.
    // Lint-only; user qualifies or splits the property.
    warnings.extend(check_cross_adt_field_ambiguity(spec));

    // vacuous_property_lowering: codegen-induced tautologies, the
    // unsupported-quantifier marker, and literal `true` bodies.
    // Author-written tautologies are silently accepted.
    warnings.extend(check_vacuous_property_lowering(spec));

    // `old(...)` inside `requires` / `invariant` is a category error —
    // both describe a single state with no "old" value. P1 with fix-it.
    warnings.extend(check_old_in_single_state_context(spec));

    // `type Error = { … }` (record brace form) parses cleanly but yields no
    // error variants, silently breaking every `error_codes` consumer. P0.
    warnings.extend(check_error_declared_as_record(spec));

    // A `requires` name that resolves to nothing renders verbatim into
    // every backend and fails to compile there — fail at check instead. P0.
    warnings.extend(check_unknown_guard_identifier(spec));

    // `modifies [X]` with no effect write and no `ensures` reference: the
    // field is completely unconstrained — Lean frame proofs allow any
    // post-value, the impl-fill site has nothing to verify against. P0.
    warnings.extend(check_unconstrained_modifies(spec));

    // ref_impl bodies with potentially-overflowing arithmetic over bounded
    // numerics: Lean proves on unbounded `Nat`; Rust runs on `u64`/`i64`
    // where the same expression can wrap or panic. Bounded-arith
    // verification lives in Kani; the same predicate drives the
    // impl-targeted Kani auto-trigger.
    warnings.extend(check_ref_impl_unbounded_arith(spec));

    // ≥2 CPI calls whose substituted ensures reference the SAME caller-state
    // field: both `kani::assume` lines fire at one splice point against one
    // (pre, post) snapshot pair, which can over-constrain. Per-call snapshot
    // frames is v3.0-class.
    warnings.extend(check_multi_cpi_same_field(spec));

    // Sort by priority (ascending), then by rule name for stability.
    warnings.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.rule.cmp(&b.rule)));

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::test_support::*;

    // Orchestration-level tests: the final priority sort and the
    // whole-suite "clean spec" gate. Per-rule tests live next to their
    // rules in the family submodules.

    #[test]
    fn test_priority_ordering() {
        // Build a spec that triggers multiple rules at different priorities
        let mut h = make_handler("deposit");
        h.who = None; // priority 1: no_access_control
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        // no guard → priority 1: unguarded_arithmetic + missing_guard_from_takes
        // no properties → priority 3: no_properties
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("authority".to_string(), "Pubkey".to_string()),
                ("balance".to_string(), "U64".to_string()),
            ],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        // Verify sorted ascending by priority
        for window in warnings.windows(2) {
            assert!(
                window[0].priority <= window[1].priority,
                "warnings not sorted by priority: {} ({}) should come before {} ({})",
                window[0].rule,
                window[0].priority,
                window[1].rule,
                window[1].priority
            );
        }
    }

    #[test]
    fn test_complete_spec_clean() {
        let spec_content = include_str!("../../../../../examples/rust/escrow/escrow.qedspec");
        let spec =
            crate::chumsky_adapter::parse_str(spec_content).expect("escrow.qedspec should parse");
        let warnings = check_completeness(&spec);
        // A well-formed spec should have zero `Warning`-severity findings.
        // (P6 on Pubkey state fields is Info-only, so it never appears here.)
        let warning_rules: Vec<&str> = warnings
            .iter()
            .filter(|w| w.severity == Severity::Warning)
            .map(|w| w.rule.as_str())
            .collect();
        assert!(
            warning_rules.is_empty(),
            "escrow.qedspec should be Warning-clean but got: {:?}",
            warning_rules
        );
    }
}
