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
    warnings.push(
        warn(
            "error_declared_as_record",
            Severity::Error,
            0,
            "`type Error = { ... }` (record brace form) does not declare error \
                  variants — the parser treats it as a struct named `Error` and \
                  `spec.error_codes` ends up empty. Downstream lowering then \
                  misbehaves silently (CPI error refs unresolved, `WrongState` / \
                  `MathOverflow` gates don't fire).",
        )
        .subject("Error".to_string())
        .fix(
            "Use the pipe form instead of `= { ... }`. Each variant goes on its \
              own line with a leading `|`.",
        )
        .example(format!("  type Error\n{}", fields_hint)),
    );
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
            warnings.push(warn("unknown_error_variant", Severity::Warning, 2, format!(
                    "`pragma {} = {}` references a variant absent from `type Error | …`. Generated Rust references `{}Error::{}` and won't compile.",
                    key,
                    value,
                    crate::codegen_shared::to_pascal_case(&spec.program_name),
                    value,
                )).subject(value.clone()).fix(format!(
                    "Add `{}` to your `type Error | …` block, drop the pragma, or replace it with a declared variant name.",
                    value,
                )));
        }
    }

    // Per-site `or X` references.
    for h in &spec.handlers {
        for on_error in h.effects.iter().filter_map(|e| e.on_error.as_ref()) {
            if !has_decl(on_error) {
                warnings.push(warn("unknown_error_variant", Severity::Warning, 2, format!(
                        "handler '{}' has an effect with `else {}` referencing a variant absent from `type Error | …`. Generated Rust references `{}Error::{}` and won't compile.",
                        h.name,
                        on_error,
                        crate::codegen_shared::to_pascal_case(&spec.program_name),
                        on_error,
                    )).subject(h.name.clone()).fix(format!(
                        "Add `{}` to your `type Error | …` block, drop the `else {}` suffix to fall back to the default, or use a declared variant.",
                        on_error, on_error,
                    )));
            }
        }
    }
    warnings
}

/// `duplicate_effect_target`: an effect block writes the same target
/// field twice. Under parallel effect semantics the Lean model and the
/// generated Rust disagree on the result (last-write vs accumulated), so
/// codegen refuses; this surfaces it at check time with a fix. Mirrors
/// the codegen-time bail in `check_effect_targets`.
pub(super) fn check_duplicate_effect_target(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for h in &spec.handlers {
        // Per effect block — arms are mutually exclusive, so a target
        // written in two different arms does NOT conflict.
        let blocks: Vec<&[crate::check::ParsedEffect]> = match &h.effect_branches {
            Some(branches) => branches.arms.iter().map(|a| a.effects.as_slice()).collect(),
            None => vec![h.effects.as_slice()],
        };
        for block in blocks {
            for dup in crate::rust_codegen_util::duplicate_effect_targets(block, spec) {
                warnings.push(warn("duplicate_effect_target", Severity::Error, 1, format!(
                    "handler '{}' writes effect target `{}` more than once in one effect block. Under parallel effect semantics every RHS reads the pre-state, so the two writes diverge — the Lean model keeps the last write while the generated Rust accumulates. Codegen refuses such specs.",
                    h.name, dup,
                )).subject(h.name.clone()).fix(format!(
                    "Combine the writes into a single effect on `{}` (e.g. `{} += <total>`), or move them into mutually-exclusive `match` arms.",
                    dup, dup,
                )));
            }
        }
    }
    warnings
}

/// `effect_type_mismatch`: an effect assigns an account address
/// (`field := <acct>` / `<acct>.pubkey`) into a field that isn't declared
/// `Pubkey`. The RHS lowers to `<acct>.key()` (a `[u8; 32]`), so a scalar
/// destination (`pool : U64; pool := new_admin`) is an E0308 type
/// mismatch. Codegen keeps such sites on the `todo!()` fill path rather
/// than miscompile; this surfaces the mismatch at check time so the
/// author fixes the spec instead of hitting an opaque fill site.
pub(super) fn check_effect_account_key_type_mismatch(
    spec: &ParsedSpec,
) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    // Resolve a bare field name's declared type across every site it can
    // live: flat `state_fields`, per-account fields, and ADT variant
    // payloads. `None` when undeclared (a separate lint owns that).
    let field_type = |base: &str| -> Option<&str> {
        if let Some((_, t)) = spec.state_fields.iter().find(|(n, _)| n == base) {
            return Some(t.as_str());
        }
        for a in &spec.account_types {
            if let Some((_, t)) = a.fields.iter().find(|(n, _)| n == base) {
                return Some(t.as_str());
            }
            for v in &a.variants {
                if let Some((_, t)) = v.fields.iter().find(|(n, _)| n == base) {
                    return Some(t.as_str());
                }
            }
        }
        None
    };

    let ctx = LintCtx::new(spec);
    for h in &spec.handlers {
        for eff in &h.effects {
            // Binding-resolved account-address RHS (`<acct>` / `<acct>.pubkey`).
            let Some(tree) = eff.tree.as_ref() else {
                continue;
            };
            let Some(acct) = crate::codegen_shared::account_key_rhs(tree) else {
                continue;
            };
            // The RHS means "this account's address" — only coherent when
            // assigned (`:=`) into a `Pubkey` slot.
            let base = {
                let normalized = ctx.normalize_lhs(&eff.field);
                normalized
                    .split(['[', '.'])
                    .next()
                    .unwrap_or(&normalized)
                    .to_string()
            };
            match field_type(&base) {
                Some("Pubkey") => {} // well-typed
                Some(t) => {
                    warnings.push(warn("effect_type_mismatch", Severity::Error, 1, format!(
                        "handler '{}' assigns account `{}`'s address into field `{}` (declared `{}`, not `Pubkey`). The address lowers to `{}.key()` (a 32-byte key), so the assignment is a type mismatch — codegen leaves it as a `todo!()` fill site rather than emit non-compiling Rust.",
                        h.name, acct, base, t, acct,
                    )).subject(h.name.clone()).fix(format!(
                        "Assign `{}` into a `Pubkey` field, or if `{}` is meant to hold a numeric derived from the account, compute it explicitly (an account address can't be coerced to `{}`).",
                        acct, base, t,
                    )));
                }
                None => {} // undeclared target — check_effect_targets / other lints own it
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
                warnings.push(warn("pda_seed_collision", Severity::Warning, 1, format!(
                        "PDA '{}' and PDA '{}' have identical seed tuples [{}] — they will always resolve to the same on-chain address",
                        a.name, b.name, a.seeds.join(", ")
                    )).subject(a.name.clone()).fix(format!(
                        "Add a distinguishing seed to '{}' or '{}' (e.g., a discriminator byte or unique program-specific tag)",
                        a.name, b.name
                    )).example(format!(
                        "  pda {} [\"{}_tag\", {}]\n  pda {} [\"{}_tag\", {}]",
                        a.name,
                        a.name.to_lowercase(),
                        a.seeds.join(", "),
                        b.name,
                        b.name.to_lowercase(),
                        b.seeds.join(", ")
                    )));
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
                warnings.push(warn("pda_seed_possible_collision", Severity::Warning, 2, format!(
                        "PDA '{}' and PDA '{}' share all literal seeds [{}] and differ only in variable positions — they can collide at runtime when variables hold the same values",
                        a.name, b.name, a_literals.join(", ")
                    )).subject(a.name.clone()).fix(format!(
                        "Add a unique literal discriminator seed to '{}' or '{}' so their namespaces cannot overlap",
                        a.name, b.name
                    )).example(format!(
                        "  pda {} [\"{}\", ...]\n  pda {} [\"{}\", ...]",
                        a.name,
                        a.name.to_lowercase(),
                        b.name,
                        b.name.to_lowercase()
                    )));
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
            warnings.push(
                warn(
                    "vacuous_property_lowering",
                    Severity::Warning,
                    1,
                    format!(
                        "property '{}' lowered Rust contains \
                     QEDGEN_UNSUPPORTED_QUANTIFIER — the harness emits a `true` \
                     body and skips the real check",
                        prop.name
                    ),
                )
                .subject(prop.name.clone())
                .fix(
                    "Rewrite the quantifier in a shape qedgen can lower \
                      (see docs/limitations.md#unsupported-quantifier-shapes) \
                      or split the property into per-element guards.",
                ),
            );
            continue;
        }

        // Rule 2b — unconditional: unbounded-sum marker present. The Rust
        // lowering is a bare sentinel comment, so every backend skips the
        // property entirely.
        if rs.contains(crate::check::QEDGEN_UNSUPPORTED_SUM_MARKER) {
            warnings.push(
                warn(
                    "vacuous_property_lowering",
                    Severity::Warning,
                    1,
                    format!(
                        "property '{}' contains a sum without a finite domain — \
                     Kani/proptest/Crucible skip this property entirely",
                        prop.name
                    ),
                )
                .subject(prop.name.clone())
                .fix(
                    "Give the sum binder a finite domain: declare `type Idx = \
                      Fin[N]` (or use Fin[N] directly) so the sum lowers to a \
                      bounded fold.",
                ),
            );
            continue;
        }

        // Rule 3 — unconditional: bare `true` body.
        if trimmed == "true" {
            warnings.push(
                warn(
                    "vacuous_property_lowering",
                    Severity::Warning,
                    1,
                    format!(
                        "property '{}' lowered to the literal `true` — the harness \
                     can never fail. Check the spec body and re-run check.",
                        prop.name
                    ),
                )
                .subject(prop.name.clone())
                .fix(
                    "Inspect the property body for a spec construct that \
                      lowered to a constant. If the property is genuinely \
                      trivial, remove it; otherwise file a codegen bug.",
                ),
            );
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
            warnings.push(
                warn(
                    "vacuous_property_lowering",
                    Severity::Warning,
                    1,
                    format!(
                        "property '{}' uses `old(...)` but lowered Rust collapses to a \
                     structural tautology (`{} {} {}`). The temporal marker was \
                     dropped during lowering — this indicates a codegen regression.",
                        prop.name, lhs, _op, rhs
                    ),
                )
                .subject(prop.name.clone())
                .fix(
                    "File a qedgen issue with the spec snippet. Pre-v2.23 this \
                      was the default behavior for `old(...)` in proptest/Kani; \
                      post-Slices 2-4 it should be unreachable.",
                ),
            );
        }
    }
    warnings
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
        known.extend(h.let_bindings.iter().map(|b| b.name.as_str()));
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
                warnings.push(
                    warn(
                        "unknown_guard_identifier",
                        Severity::Error,
                        0,
                        format!(
                            "handler '{}' references `{}` in a `requires` clause, but it \
                         resolves to nothing — not a state field, parameter, account, \
                         const, or binding. Generated code carries the name verbatim \
                         and won't compile in any backend.",
                            h.name, display
                        ),
                    )
                    .subject(h.name.clone())
                    .fix(format!(
                        "Declare `{name}` (state field, param, or const) or fix the \
                         reference to an existing name."
                    )),
                );
            }
        }
    }
    warnings
}

/// Ghost-variable validation: non-scalar types, `on <handler>` clauses
/// naming unknown handlers, and state shapes ghosts aren't wired into yet.
pub(super) fn check_ghost_declarations(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    if !spec.ghosts.is_empty() {
        let scalar = |t: &str| {
            matches!(
                t.trim(),
                "U8" | "U16"
                    | "U32"
                    | "U64"
                    | "U128"
                    | "I8"
                    | "I16"
                    | "I32"
                    | "I64"
                    | "I128"
                    | "Bool"
            )
        };
        // Ghosts are only wired into the flat single-account verification
        // State today. Indexed (`Map[N]`), multi-account, and explicit
        // ADT-state shapes don't yet thread ghost fields through their
        // renderers, so flag rather than silently drop them.
        let is_indexed = spec
            .state_fields
            .iter()
            .any(|(_, t)| t.trim_start().starts_with("Map"));
        let is_multi_account = spec.account_types.len() > 1;
        let is_adt = spec.state_repr_is_adt();
        let unsupported_shape = is_indexed || is_multi_account || is_adt;
        let handler_names: std::collections::BTreeSet<&str> =
            spec.handlers.iter().map(|h| h.name.as_str()).collect();
        for g in &spec.ghosts {
            if !scalar(&g.ty) {
                warnings.push(warn("ghost_non_scalar_type", Severity::Warning, 2, format!(
                        "ghost '{}' has non-scalar type '{}' — ghosts must be a scalar (U8…U128 / I8…I128 / Bool)",
                        g.name, g.ty
                    )).subject(g.name.clone()).fix("Use a scalar ghost type. Aggregate quantities over collections belong in a `property` via `sum i : Idx, …`, not a ghost."));
            }
            for u in &g.updates {
                if !handler_names.contains(u.handler.as_str()) {
                    warnings.push(
                        warn(
                            "ghost_update_unknown_handler",
                            Severity::Warning,
                            2,
                            format!(
                            "ghost '{}' has an `on {}` clause, but no handler named '{}' exists",
                            g.name, u.handler, u.handler
                        ),
                        )
                        .subject(g.name.clone())
                        .fix("Name an existing handler in the `on` clause, or remove the clause."),
                    );
                }
            }
            if unsupported_shape {
                warnings.push(warn("ghost_unsupported_state_shape", Severity::Warning, 2, format!(
                        "ghost '{}' is declared with an indexed / multi-account / ADT state — ghost fields are only wired into the flat single-account verification State today",
                        g.name
                    )).subject(g.name.clone()).fix("Move the ghost to a flat single-account spec, or track the quantity in a `property` (e.g. `sum i : Idx, accounts[i].x`) until ghost support lands for this shape."));
            }
        }
    }
    warnings
}

/// Hook validation: deferred-Lean note, unknown `after_store` fields,
/// unsupported state shapes, deferred `before_cpi`, and assert-less hooks.
pub(super) fn check_hook_declarations(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    if !spec.hooks.is_empty() {
        // Lean enforcement is deferred (lands with qedsvm); hooks are
        // currently checked only in the Kani / proptest harnesses.
        warnings.push(warn("hook_lean_unsupported", Severity::Info, 3, "hooks are enforced in the Kani / proptest harnesses; Lean enforcement is deferred (lands with qedsvm)").fix("No action needed — `qedgen verify --kani` / `--proptest` exercise the hook assertions."));
        let state_field_names: std::collections::BTreeSet<&str> =
            spec.state_fields.iter().map(|(n, _)| n.as_str()).collect();
        let is_indexed = spec
            .state_fields
            .iter()
            .any(|(_, t)| t.trim_start().starts_with("Map"));
        let unsupported_shape = is_indexed || spec.account_types.len() > 1;
        for hook in &spec.hooks {
            match &hook.kind {
                ParsedHookKind::AfterStore(field) => {
                    if !state_field_names.contains(field.as_str()) {
                        warnings.push(
                            warn(
                                "hook_unknown_field",
                                Severity::Warning,
                                2,
                                format!(
                                    "hook `after_store({})` names '{}', which is not a state field",
                                    field, field
                                ),
                            )
                            .fix("Name a declared state field in `after_store(<field>)`."),
                        );
                    }
                    if unsupported_shape {
                        warnings.push(warn("hook_unsupported_state_shape", Severity::Warning, 2, format!(
                                "hook `after_store({})` is declared with an indexed / multi-account state — `after_store` is wired into the flat single-account transition only",
                                field
                            )).fix("Use a flat single-account spec, or assert the post-store condition in a `property`."));
                    }
                }
                ParsedHookKind::BeforeCpi(_) => {
                    warnings.push(warn("hook_before_cpi_unsupported", Severity::Warning, 2, "`hook before_cpi` enforcement is deferred — the runtime state model has no CPI to anchor to, and the Lean CPI-theorem precondition path lands with qedsvm").fix("Encode the precondition as a `requires` on the calling handler for now, or assert it via `after_store` on the field the CPI consumes."));
                }
            }
            if hook.asserts.is_empty() {
                warnings.push(
                    warn(
                        "hook_no_assert",
                        Severity::Info,
                        3,
                        "hook has no `assert` clause — it checks nothing",
                    )
                    .fix("Add at least one `assert <expr>` to the hook body."),
                );
            }
        }
    }
    warnings
}

/// Rule 2: handler not covered by any property.
pub(super) fn check_uncovered_operation(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        let covered = spec
            .properties
            .iter()
            .any(|p| p.preserved_by.contains(&op.name));
        if !covered && !spec.properties.is_empty() {
            let prop_names: Vec<&str> = spec.properties.iter().map(|p| p.name.as_str()).collect();
            warnings.push(warn("uncovered_operation", Severity::Info, 3, format!(
                    "handler '{}' is not in any property's `preserved_by`",
                    op.name
                )).subject(op.name.clone()).fix(format!(
                    "Add '{}' to an existing property's `preserved_by` list, or confirm it doesn't need property coverage",
                    op.name
                )).example(format!(
                    "  property {} \"...\"\n    preserved_by: ..., {}",
                    prop_names.first().unwrap_or(&"my_property"),
                    op.name
                )));
        }
    }
    warnings
}

/// Rule 5: property references nonexistent handler.
pub(super) fn check_dangling_preserved_by(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let op_names: Vec<&str> = spec.handlers.iter().map(|o| o.name.as_str()).collect();
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            if !op_names.contains(&op_name.as_str()) {
                warnings.push(
                    warn(
                        "dangling_preserved_by",
                        Severity::Warning,
                        1,
                        format!(
                            "property '{}' references nonexistent handler '{}'",
                            prop.name, op_name
                        ),
                    )
                    .subject(format!("{}.preserved_by.{}", prop.name, op_name))
                    .fix(format!(
                        "Check the spelling of '{}' — available handlers: {}",
                        op_name,
                        op_names.join(", ")
                    )),
                );
            }
        }
    }
    warnings
}

/// Quantifier over a type that can't be exhausted at test time.
/// Two distinct shapes:
///   - `forall s : <StateType>` — universal over states (e.g. `Pool.Active`).
///     Always Lean territory; the whole quantifier is redundant since
///     `state.x` already refers to the current state. Advice: drop it.
///   - `forall i : <BinderType>` — bounded value quantifier over a primitive
///     (U16+, AccountIdx, etc.). U8/I8 fit in proptest; wider types emit a
///     stub `true`. Advice: narrow the binder.
pub(super) fn check_unchecked_quantifier(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let state_type_names: std::collections::HashSet<String> = spec
        .account_types
        .iter()
        .flat_map(|at| {
            // Both the bare type name (e.g. `Pool`) and `Pool.<Variant>` for
            // each lifecycle variant — qedspec quantifiers use the qualified
            // form `Pool.Active` to range over a specific lifecycle state.
            let qualified = at
                .lifecycle
                .iter()
                .map(move |v| format!("{}.{}", at.name, v));
            std::iter::once(at.name.clone()).chain(qualified)
        })
        .collect();
    for prop in &spec.properties {
        // Per-slot lowering already provides a proptest-checkable form for
        // wide-binder forall properties (see ParsedProperty::per_slot).
        // The lint's "harness emits true" warning isn't accurate for these:
        // the per-slot `{prop}_at` predicate is generated and called at the
        // modified slot in each handler's preservation test.
        if prop.per_slot.is_some() {
            continue;
        }
        // When P5 `unsupported_quantifier_shape` fires, skip the legacy
        // `unchecked_quantifier` — P5 carries strictly more precise
        // information (kind + span); double-reporting clutters.
        if prop.quantifier_lint.is_some() {
            continue;
        }
        if let Some(ref rust_expr) = prop.rust_expression {
            if rust_expr_is_unsupported(rust_expr) {
                // Extract the quantifier kind and binder type from the sentinel
                // comment so the message is specific.
                let detail = rust_expr
                    .trim_start_matches("/*")
                    .trim_end_matches("*/")
                    .trim()
                    .trim_start_matches(QEDGEN_UNSUPPORTED_MARKER)
                    .trim_start_matches(':')
                    .trim()
                    .to_string();
                // Pull the binder type out of `forall <var> : <Type>` so we
                // can pick the right advice. Detail looks like
                // `forall s : Pool.Active — lower at harness level`.
                let binder_type: Option<String> = detail
                    .split_once(':')
                    .and_then(|(_, rest)| rest.split('—').next())
                    .map(|s| s.trim().to_string());
                let is_state_quantifier = binder_type
                    .as_ref()
                    .map(|t| state_type_names.contains(t))
                    .unwrap_or(false);
                let (fix, example) = if is_state_quantifier {
                    (
                        "Drop the `forall s : <State>` wrapper — properties are \
                         implicitly evaluated against the current state. Use \
                         `state.<field>` directly."
                            .to_string(),
                        Some(format!(
                            "  // instead of: forall s : <State>, s.x >= s.y\n  \
                             property {} :\n    state.x >= state.y",
                            prop.name
                        )),
                    )
                } else {
                    (
                        "Use U8 or I8 as the quantifier binder type (≤256 values, \
                         exhausted automatically), or split the property into a \
                         per-element guard."
                            .to_string(),
                        Some(format!(
                            "  // instead of: forall v : U64, …\n  \
                             property {} :\n    forall v : U8, …",
                            prop.name
                        )),
                    )
                };
                warnings.push(CompletenessWarning {
                    subject: Some(prop.name.clone()),
                    fix,
                    example,
                    ..warn(
                        "unchecked_quantifier",
                        Severity::Warning,
                        1,
                        format!(
                            "property '{}' uses a quantifier over a type that proptest/Kani \
                         cannot exhaust — the harness emits `true` and skips the check ({})",
                            prop.name, detail
                        ),
                    )
                });
            }
        }
    }
    warnings
}

/// P5: quantifier shape unsupported by codegen. The chumsky_adapter
/// records a precise reason (nested forall, exists, unbounded binder);
/// surfacing it here shows the exact breaking construct instead of a
/// silent `true` stub later. Supersedes `unchecked_quantifier` for the
/// shapes it covers (that lint skips when quantifier_lint is Some).
pub(super) fn check_unsupported_quantifier_shape(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for prop in &spec.properties {
        let Some(qlint) = &prop.quantifier_lint else {
            continue;
        };
        let workaround = match qlint.kind.as_str() {
            "nested_quantifier" => {
                "Split into two single-binder properties — one per quantifier — \
                 so each lowers to a bool-valued harness independently."
            }
            "unbounded_binder" => {
                "Use a primitive (U8…U128) or a declared record type as the binder. \
                 `Vec<T>` / `List<T>` aren't enumerable by Kani / proptest in v2.20."
            }
            "exists_quantifier" => {
                "A bounded `exists` (binder typed `Fin[N]`, e.g. via an index \
                 alias like `MemberIdx = Fin[MAX_MEMBERS]`) lowers to \
                 `(0..N).any(…)`. This `exists` ranges over an unbounded domain \
                 (e.g. `U64`); bound the binder with a `Fin[N]` index type so it \
                 can be enumerated."
            }
            _ => "See docs/limitations.md#unsupported-quantifier-shapes for the workaround.",
        };
        warnings.push(
            warn(
                "unsupported_quantifier_shape",
                Severity::Warning,
                1,
                format!(
                    "property '{}' has a quantifier shape qedgen v2.20 can't lower to a \
                 non-vacuous harness — {} (bytes {}..{})",
                    prop.name, qlint.message, qlint.span_start, qlint.span_end,
                ),
            )
            .subject(prop.name.clone())
            .fix(workaround.to_string()),
        );
    }
    warnings
}

/// P6: informational note that `Pubkey` state fields lower structurally
/// to `[u8; 32]` in the verification State (proptest generates them via
/// the 32-byte-array strategy).
///
/// Scope — every place a Pubkey field can land as state:
/// `account_types[*].fields`, `sum_types[*].variants[*].fields`, and
/// `records[*].fields`. `state_fields` is a flat mirror of the first
/// account type's fields and is intentionally not scanned (double-firing).
pub(super) fn check_pubkey_state_field_unsupported(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let push_p6 = |warnings: &mut Vec<CompletenessWarning>, holder: &str, field: &str| {
        warnings.push(
            warn(
                "pubkey_state_field_unsupported",
                Severity::Info,
                3,
                format!(
                    "P6: Pubkey field '{}' in {} is lowered to `[u8; 32]` in \
                     the generated proptest / Kani harness. The user-facing \
                     Anchor program target keeps the `Pubkey` type.",
                    field, holder,
                ),
            )
            .subject(format!("{}.{}", holder, field))
            .fix(format!(
                "No action required. To compare against an Anchor `Pubkey` \
                     param, convert at the call site: `s.{} == pk.to_bytes()`.",
                field,
            )),
        );
    };

    for acct in &spec.account_types {
        for (fname, ftype) in &acct.fields {
            if ftype == "Pubkey" {
                push_p6(&mut warnings, &acct.name, fname);
            }
        }
    }
    for sum in &spec.sum_types {
        for variant in &sum.variants {
            for (fname, ftype) in &variant.fields {
                if ftype == "Pubkey" {
                    let holder = format!("{}.{}", sum.name, variant.name);
                    push_p6(&mut warnings, &holder, fname);
                }
            }
        }
    }
    for rec in &spec.records {
        for (fname, ftype) in &rec.fields {
            if ftype == "Pubkey" {
                push_p6(&mut warnings, &rec.name, fname);
            }
        }
    }
    warnings
}

/// Rule 9: handlers with effects but zero properties. CPI call sites are
/// proof surface too — every `call Iface.handler(...)` emits a
/// per-call-site ensures theorem in Spec.lean — so a spec whose point is
/// CPI composition (the bundled-stdlib-demo shape) has something to prove
/// without a standalone `property`, and the lint stays silent there.
/// `transfers { ... }` sugar lowers to the same per-call-site CPI build
/// theorems (`build_<handler>_transfer…`), so it counts as proof surface
/// too (the bundled escrow shape).
pub(super) fn check_no_properties(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let has_effects = spec.handlers.iter().any(|op| op.has_effect());
    let has_cpi_theorems = spec
        .handlers
        .iter()
        .any(|op| !op.calls.is_empty() || !op.transfers.is_empty());
    if has_effects && !has_cpi_theorems && spec.properties.is_empty() && spec.invariants.is_empty()
    {
        // Suggest conservation if paired add/sub exist on same field
        let mut modified_fields: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            for eff in &op.effects {
                modified_fields
                    .entry(eff.field.as_str())
                    .or_default()
                    .push(eff.op.as_str());
            }
        }
        let conservation_candidates: Vec<&str> = modified_fields
            .iter()
            .filter(|(_, kinds)| kinds.contains(&"add") && kinds.contains(&"sub"))
            .map(|(f, _)| *f)
            .collect();

        let op_list: Vec<&str> = spec
            .handlers
            .iter()
            .filter(|op| op.has_effect())
            .map(|op| op.name.as_str())
            .collect();
        let preserved_by = if op_list.len() <= 4 {
            format!("[{}]", op_list.join(", "))
        } else {
            "all".to_string()
        };

        let example = if !conservation_candidates.is_empty() {
            let field = conservation_candidates[0];
            format!(
                "  property conservation {{\n    expr state.{} >= 0\n    preserved_by {}\n  }}",
                field, preserved_by
            )
        } else {
            format!(
                "  property my_invariant {{\n    expr <your invariant expression>\n    preserved_by {}\n  }}",
                preserved_by
            )
        };

        warnings.push(
            warn(
                "no_properties",
                Severity::Warning,
                3,
                "spec has effects but no properties — verification has nothing to prove",
            )
            .fix("Add at least one property to define what the verification should prove")
            .example(example),
        );
    }
    warnings
}

/// Rule 11: no errors block but handlers have guards.
pub(super) fn check_no_errors_block(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let any_guards = spec.handlers.iter().any(|op| op.has_guard());
    if any_guards && spec.error_codes.is_empty() {
        warnings.push(
            warn(
                "no_errors_block",
                Severity::Info,
                4,
                "spec has guards but no `errors` block — codegen can't generate error types",
            )
            .fix("Add an errors block listing all failure modes")
            .example("  errors [InvalidAmount, Unauthorized, AlreadyClosed]".to_string()),
        );
    }
    warnings
}

/// Rule 16: excluded_op_modifies_property — handler NOT in preserved_by
/// modifies fields referenced by the property. The inductive theorem will
/// need a manual proof (not sorry).
pub(super) fn check_excluded_op_modifies_property(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for prop in &spec.properties {
        if let Some(ref expr) = prop.expression {
            // Extract field names from the property expression.
            // The expression is in Lean form (s.field_name) from the parser.
            let prop_fields: Vec<&str> = {
                let mut fields = Vec::new();
                // Check both "s." (Lean form) and "state." (DSL form) patterns
                for prefix in &["s.", "state."] {
                    for (i, _) in expr.match_indices(prefix) {
                        let rest = &expr[i + prefix.len()..];
                        let end = rest
                            .find(|c: char| !c.is_alphanumeric() && c != '_')
                            .unwrap_or(rest.len());
                        if end > 0 {
                            let field = &rest[..end];
                            if !fields.contains(&field) {
                                fields.push(field);
                            }
                        }
                    }
                }
                fields
            };

            let uses_all = prop.preserved_by.iter().any(|p| p == "all");
            if uses_all {
                continue; // all ops are in preserved_by, no exclusion
            }

            for op in &spec.handlers {
                if prop.preserved_by.contains(&op.name) {
                    // Handler is claimed to preserve the property — verify via
                    // effect analysis. Warn when the effect demonstrably violates
                    // the bound (covers preserved_by all expansion and explicit lists).
                    let covered_modified: Vec<&str> = op
                        .effects
                        .iter()
                        .filter(|e| prop_fields.contains(&e.field.as_str()))
                        .map(|e| e.field.as_str())
                        .collect();
                    if !covered_modified.is_empty() {
                        // Skip when any `requires` references a property
                        // field: the boundary `build_counterexample` picks is
                        // often unreachable because of guards the local
                        // analyzer doesn't model (dedup bitmaps, lifecycle
                        // gates). Trust the author's bound; preserved_by
                        // claims with NO constraining guard still fire.
                        if requires_constrains_prop_fields(op, &prop_fields) {
                            continue;
                        }
                        if let Some(ce) = build_counterexample(
                            expr,
                            &prop.name,
                            &prop_fields,
                            op,
                            &covered_modified,
                            &spec.constants,
                        ) {
                            if !ce.invariant_holds {
                                warnings.push(warn("preserved_by_all_potential_violation", Severity::Warning, 1, format!(
                                        "handler '{}' is in `preserved_by` for property '{}' but effect analysis suggests a violation",
                                        op.name, prop.name
                                    )).subject(op.name.clone()).fix(format!(
                                        "Add a guard to '{}' ensuring the invariant holds after the effect, or remove it from `preserved_by`",
                                        op.name
                                    )).counterexample(ce));
                            }
                        }
                    }
                    continue;
                }
                // Check if this excluded op modifies any field in the property expression
                let modified_prop_fields: Vec<&str> = op
                    .effects
                    .iter()
                    .filter(|e| prop_fields.contains(&e.field.as_str()))
                    .map(|e| e.field.as_str())
                    .collect();

                if !modified_prop_fields.is_empty() {
                    // Skip if ALL effects on property fields are monotonically safe.
                    // e.g., sub on LHS of ≤ can only decrease the LHS → invariant still holds.
                    if let Some((lhs, op_sym, _rhs)) = parse_property_relation(expr, &prop_fields) {
                        let all_safe = op
                            .effects
                            .iter()
                            .filter(|e| modified_prop_fields.contains(&e.field.as_str()))
                            .all(|e| {
                                let on_lhs = e.field.as_str() == lhs;
                                match (e.op.as_str(), op_sym, on_lhs) {
                                    ("sub", "≤", true) | ("sub", "<=", true) => true, // decreasing LHS of ≤
                                    ("add", "≥", true) | ("add", ">=", true) => true, // increasing LHS of ≥
                                    ("sub", "≥", false) | ("sub", ">=", false) => true, // decreasing RHS of ≥
                                    ("add", "≤", false) | ("add", "<=", false) => true, // increasing RHS of ≤
                                    _ => false,
                                }
                            });
                        if all_safe {
                            continue; // monotonically preserves the invariant
                        }
                    }

                    let counterexample = build_counterexample(
                        expr,
                        &prop.name,
                        &prop_fields,
                        op,
                        &modified_prop_fields,
                        &spec.constants,
                    );

                    let fix_options = build_fix_suggestions(
                        expr,
                        &prop.name,
                        op,
                        &prop_fields,
                        &modified_prop_fields,
                    );

                    let fix = fix_options.first().map_or_else(
                        || format!(
                            "Add '{}' to property '{}' `preserved_by` with a guard, or restructure the property",
                            op.name, prop.name
                        ),
                        |f| f.snippet.clone(),
                    );

                    warnings.push(CompletenessWarning {
                        subject: Some(op.name.clone()),
                        fix,
                        counterexample,
                        fix_options,
                        ..warn(
                            "excluded_op_modifies_property",
                            Severity::Warning,
                            2,
                            format!(
                                "handler '{}' modifies field(s) [{}] used in property '{}' but is excluded from `preserved_by` — no inductive arm is generated for this handler, so the per-arm proof obligation is silently dropped. Either add the handler to `preserved_by` (and discharge the proof) or refactor the property so this handler doesn't need to preserve it.",
                                op.name,
                                modified_prop_fields.join(", "),
                                prop.name
                            ),
                        )
                    });
                }
            }
        }
    }
    warnings
}

/// Rule 17: invariant_no_body — doc-string-only invariant. Lean codegen
/// would lower it to `theorem <name> : True := trivial` (vacuous, banned
/// by the no-tautological-proofs policy); surface at check time.
pub(super) fn check_invariant_no_body(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for inv in &spec.invariants {
        if inv.lean_expr.is_none() {
            warnings.push(warn("invariant_no_body", Severity::Error, 1, format!(
                    "invariant '{}' has only a description string, no `expr` body — \
                     codegen would emit `theorem {} : True := trivial` (vacuous proof)",
                    inv.name, inv.name
                )).subject(inv.name.clone()).fix(format!(
                    "Add an `expr` body to invariant '{}': \
                     `invariant {} {{ expr <predicate-over-state> preserved_by all }}`",
                    inv.name, inv.name
                )).example(format!(
                    "  invariant {} {{\n    expr state.total_in == state.total_out\n    preserved_by all\n  }}",
                    inv.name
                )));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::test_support::*;

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

    // ----- duplicate_effect_target -----

    #[test]
    fn duplicate_effect_target_lint_fires_and_suggests_combining() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec T
state { b : U64 }
handler bump {
  effect { b += 1
           b += 2 }
}"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "duplicate_effect_target")
            .expect("duplicate_effect_target fires");
        assert_eq!(hit.severity, Severity::Error);
        assert_eq!(hit.subject.as_deref(), Some("bump"));
        assert!(
            hit.fix.contains("+= <total>"),
            "fix should suggest combining; got: {:?}",
            hit.fix
        );
    }

    #[test]
    fn duplicate_effect_target_lint_silent_for_distinct_match_arms() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec T
state { b : U64 }
handler pick (mode : U8) {
  effect {
    match mode {
      0 => b += 1,
      _ => b += 2,
    }
  }
}"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "duplicate_effect_target"),
            "same field in mutually-exclusive arms must not fire; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ----- effect_type_mismatch: account address into non-Pubkey field -----

    #[test]
    fn effect_type_mismatch_fires_when_account_key_assigned_to_scalar() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec V
type State | Active of { admin : Pubkey, pool : U64 }
type Error | Bad
handler bad_set : State.Active -> State.Active {
  auth admin
  accounts { admin : signer, new_admin : signer, state : writable }
  effect { pool := new_admin }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "effect_type_mismatch")
            .expect("effect_type_mismatch fires for U64 := account");
        assert_eq!(hit.severity, Severity::Error);
        assert_eq!(hit.subject.as_deref(), Some("bad_set"));
    }

    #[test]
    fn effect_type_mismatch_silent_when_dest_is_pubkey() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec V
type State | Active of { admin : Pubkey, pool : U64 }
type Error | Bad
handler good_set : State.Active -> State.Active {
  auth admin
  accounts { admin : signer, new_admin : signer, state : writable }
  effect { admin := new_admin }
}
"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "effect_type_mismatch"),
            "well-typed `admin : Pubkey := new_admin` must not fire; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
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
            tree: None,
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
            tree: None,
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

    #[test]
    fn test_no_properties_fires() {
        let mut h = make_handler("deposit");
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "amount > 0".to_string(),
            ..Default::default()
        });
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "no_properties"),
            "expected no_properties, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_properties_skips_with_property() {
        let mut h = make_handler("deposit");
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "amount > 0".to_string(),
            ..Default::default()
        });
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("state.balance >= 0".to_string()),
                rust_expression: Some("s.balance >= 0".to_string()),
                rust_expression_pod: Some("s.balance >= 0".to_string()),
                rust_expression_math: None,
                preserved_by: vec!["deposit".to_string()],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
                tree: None,
            }],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "no_properties"),
            "should not fire when properties exist"
        );
    }

    #[test]
    fn test_no_properties_skips_with_cpi_call_sites() {
        // A CPI-composition spec proves per-call-site ensures theorems —
        // "verification has nothing to prove" is false there even with no
        // standalone `property` block.
        let mut h = make_handler("deposit");
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        h.calls = vec![crate::check::ParsedCall {
            target_interface: "Token".to_string(),
            target_handler: "transfer".to_string(),
            args: Vec::new(),
            result_binding: None,
            state_binders: Vec::new(),
        }];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "no_properties"),
            "CPI call-site theorems are proof surface; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_properties_skips_with_transfers_sugar() {
        // `transfers { ... }` lowers to the same per-call-site CPI build
        // theorems as explicit `call` — the bundled escrow shape must not
        // fire no_properties (#260 baseline fix).
        let mut h = make_handler("cancel");
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        h.transfers = vec![crate::check::ParsedTransfer {
            from: "escrow_ta".to_string(),
            to: "initializer_ta".to_string(),
            amount: Some("initializer_amount".to_string()),
            amount_tree: None,
            authority: Some("escrow".to_string()),
        }];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "no_properties"),
            "transfers-sugar CPI theorems are proof surface; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_errors_block_fires() {
        let mut h = make_handler("deposit");
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
            warnings.iter().any(|w| w.rule == "no_errors_block"),
            "expected no_errors_block, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unchecked_quantifier_lint_fires_for_large_type() {
        // U64 quantifier can't be exhausted — check.rs must warn so the user
        // knows the property is being silently skipped in proptest/Kani.
        let spec = ParsedSpec {
            properties: vec![ParsedProperty {
                name: "all_balances_positive".to_string(),
                expression: Some("∀ v : Nat, v ≥ 0".to_string()),
                rust_expression: Some(
                    "/* QEDGEN_UNSUPPORTED_QUANTIFIER: forall v : U64 \
                         — lower at harness level */"
                        .to_string(),
                ),
                rust_expression_pod: None,
                rust_expression_math: None,
                preserved_by: vec![],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
                tree: None,
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "unchecked_quantifier"),
            "expected unchecked_quantifier lint for U64 forall, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
        let w = warnings
            .iter()
            .find(|w| w.rule == "unchecked_quantifier")
            .unwrap();
        assert_eq!(w.priority, 1, "unchecked_quantifier must be P1");
        assert!(
            w.message.contains("all_balances_positive"),
            "message must name the property"
        );
    }

    #[test]
    fn unchecked_quantifier_lint_does_not_fire_for_u8() {
        // U8 forall lowers to a real iterator — no lint should fire.
        let spec = ParsedSpec {
            properties: vec![ParsedProperty {
                name: "bytes_nonneg".to_string(),
                expression: Some("∀ v : Nat, v ≥ 0".to_string()),
                rust_expression: Some("(u8::MIN..=u8::MAX).all(|v| v >= 0)".to_string()),
                rust_expression_pod: None,
                rust_expression_math: None,
                preserved_by: vec![],
                per_slot: None,
                quantifier_lint: None,
                class: PropertyClass::Unary,
                ast_body: None,
                tree: None,
            }],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "unchecked_quantifier"),
            "U8 forall must not fire unchecked_quantifier"
        );
    }

    #[test]
    fn build_counterexample_resolves_named_const_in_effect() {
        let handler = ParsedHandler {
            name: "reset".to_string(),
            effects: vec![ParsedEffect::from_triple("counter", "set", "ZERO")],
            ..make_handler("reset")
        };
        let constants = vec![("ZERO".to_string(), "0".to_string())];
        let ce = build_counterexample(
            "s.counter \u{2264} 5",
            "bounded",
            &["counter"],
            &handler,
            &["counter"],
            &constants,
        )
        .expect("should produce a counterexample");
        let post = ce
            .post_state
            .iter()
            .find(|(f, _)| f == "counter")
            .unwrap()
            .1;
        assert_eq!(post, 0, "ZERO should resolve to 0, not fall back to 1");
    }

    #[test]
    fn preserved_by_all_potential_violation_fires_for_named_const_effect() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Test
    program_id "11111111111111111111111111111111"
    const STEP = 5
    type State | Active of { counter : U64 }
    type Error | E
    property counter_small :
      state.counter <= 3
      preserved_by all
    handler tick : State.Active -> State.Active {
      permissionless
      effect { counter := STEP }
    }"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "preserved_by_all_potential_violation"),
            "must warn when preserved_by all handler demonstrably violates the property"
        );
    }

    /// Transition property `counter >= old(counter)` preserved by an `add`
    /// handler must NOT fire — guards against the counterexample builder
    /// misreading `s'.counter` as a constant and applying the effect to the
    /// `old(...)` side (inverting the relation into a bogus violation).
    #[test]
    fn preserved_by_transition_property_silent_when_add_preserves_monotonicity() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Test
    program_id "11111111111111111111111111111111"
    type State | Active of { counter : U64 }
    type Error | E
    property counter_monotonic :
      state.counter >= old(state.counter)
      preserved_by all
    handler grow (delta : U64) : State.Active -> State.Active {
      permissionless
      effect { counter += delta }
    }"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "preserved_by_all_potential_violation"),
            "add preserves `counter >= old(counter)` — must not flag a violation"
        );
    }

    /// The same transition property `counter >= old(counter)` claimed-
    /// preserved by a `sub` handler MUST still fire — decreasing the post
    /// side genuinely breaks monotonicity.
    #[test]
    fn preserved_by_transition_property_fires_when_sub_breaks_monotonicity() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Test
    program_id "11111111111111111111111111111111"
    type State | Active of { counter : U64 }
    type Error | E
    property counter_monotonic :
      state.counter >= old(state.counter)
      preserved_by all
    handler shrink : State.Active -> State.Active {
      permissionless
      effect { counter -= 1 }
    }"#,
        )
        .unwrap();
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "preserved_by_all_potential_violation"),
            "sub breaks `counter >= old(counter)` — must flag the violation"
        );
    }

    /// `build_fix_suggestions` must not emit a nonsensical
    /// `requires state.counter > state.counter` guard for a transition
    /// property (same field on both sides). Fix A is suppressed; Fix B
    /// (add to preserved_by) still applies.
    #[test]
    fn build_fix_suggestions_skips_self_guard_for_transition_property() {
        let handler = ParsedHandler {
            name: "shrink".to_string(),
            effects: vec![ParsedEffect::from_triple("counter", "sub", "1")],
            ..make_handler("shrink")
        };
        let fixes = build_fix_suggestions(
            "s'.counter \u{2265} s.counter",
            "counter_monotonic",
            &handler,
            &["counter"],
            &["counter"],
        );
        assert!(
            !fixes
                .iter()
                .any(|f| f.snippet.contains("state.counter > state.counter")
                    || f.snippet.contains("state.counter < state.counter")),
            "must not suggest a self-comparison guard; got: {:?}",
            fixes.iter().map(|f| &f.snippet).collect::<Vec<_>>()
        );
        assert!(
            fixes.iter().any(|f| f.label == "Add to preserved_by"),
            "the preserved_by fix should still be offered"
        );
    }

    // ── P6: pubkey_state_field_unsupported ────────────────────────────────
    //
    // Guards the structural lowering note: a State carrying
    // `authority : Pubkey` lowers to `[u8; 32]` in the verification State;
    // P6 surfaces the lowering at check time.

    #[test]
    fn pubkey_state_field_lint_fires_on_account_type() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec PubkeyState
    type State
      | Active of {
          authority : Pubkey,
          balance : U64,
        }
    handler h : State.Active -> State.Active {
      permissionless
      effect { balance += 1 }
    }
    "#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "pubkey_state_field_unsupported")
            .collect();
        assert_eq!(hits.len(), 1, "expected exactly one P6 hit: {hits:#?}");
        let w = hits[0];
        assert!(
            w.message.contains("P6:") && w.message.contains("'authority'"),
            "message must cite P6 and name the field: {}",
            w.message
        );
        // P6 is Info-only: Pubkey state fields lower to `[u8; 32]`
        // automatically; the lint just documents the lowering.
        assert!(
            w.message.contains("lowered to `[u8; 32]`"),
            "message must describe the lowering: {}",
            w.message
        );
        assert_eq!(w.priority, 3, "P6 is now a P3 informational");
        assert_eq!(w.severity, Severity::Info);
    }

    #[test]
    fn pubkey_state_field_lint_silent_without_pubkey_field() {
        // Control: no Pubkey field in state → no P6.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec NoPubkey
    type State | Active of { balance : U64 }
    handler bump : State.Active -> State.Active {
      permissionless
      effect { balance += 1 }
    }
    "#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "pubkey_state_field_unsupported"),
            "no Pubkey field → no P6, got: {warnings:#?}"
        );
    }

    #[test]
    fn pubkey_state_field_lint_fires_per_field() {
        // Two Pubkey fields → two P6 lints, each naming its specific
        // field. The non-Pubkey `balance` must not appear in any hit's
        // subject. This pins field-scoped reporting (mirrors how
        // `wrapping_arithmetic` fires per-op).
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec PubkeyMulti
    type State
      | Active of {
          authority : Pubkey,
          mint : Pubkey,
          balance : U64,
        }
    handler h : State.Active -> State.Active {
      permissionless
      effect { balance += 1 }
    }
    "#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "pubkey_state_field_unsupported")
            .collect();
        assert_eq!(hits.len(), 2, "expected two P6 hits: {hits:#?}");
        let subjects: Vec<&str> = hits
            .iter()
            .map(|w| w.subject.as_deref().unwrap_or(""))
            .collect();
        assert!(
            subjects.iter().any(|s| s.ends_with(".authority")),
            "must name authority: {subjects:?}"
        );
        assert!(
            subjects.iter().any(|s| s.ends_with(".mint")),
            "must name mint: {subjects:?}"
        );
        assert!(
            !subjects.iter().any(|s| s.ends_with(".balance")),
            "must NOT name balance: {subjects:?}"
        );
    }
}
