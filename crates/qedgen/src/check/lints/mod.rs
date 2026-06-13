//! Completeness-lint suite: the `check_completeness` orchestrator plus the
//! themed rule submodules it drives. Re-exports every rule and helper so
//! both `crate::check::<sym>` (via `check::mod`'s `pub use lints::*`) and
//! `crate::check::lints::<sym>` keep resolving after the split.

use super::*;
use regex::Regex;
use std::sync::LazyLock;

mod arithmetic;
mod auth;
mod cpi;
mod shared;
mod state;
mod structural;

pub(crate) use arithmetic::*;
pub(crate) use auth::*;
pub(crate) use cpi::*;
pub(crate) use shared::*;
pub(crate) use state::*;
pub(crate) use structural::*;

/// Check spec completeness — heuristic rules for under-specification.
/// Returns structured warnings with fix suggestions for agent consumption.
pub fn check_completeness(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    // Find a likely signer field name from state (first Pubkey field)
    let signer_hint = spec
        .state_fields
        .iter()
        .find(|(_, t)| t == "Pubkey")
        .map(|(n, _)| n.as_str())
        .unwrap_or("authority");

    // Variant index for `Variant.field` LHS normalization, shared by every
    // effect-LHS lint so the variant prefix is stripped before comparing
    // against bare field names. Maps variant name → its payload fields;
    // empty when no account type has variants.
    let mut variant_fields: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for acct in &spec.account_types {
        for variant in &acct.variants {
            let entry = variant_fields.entry(variant.name.clone()).or_default();
            for (fname, _) in &variant.fields {
                entry.insert(fname.clone());
            }
        }
    }
    // Strip a leading `Variant.` prefix when it names a known variant:
    // `Active.pool` → `pool`; `accounts[i].cap` / `pool` → unchanged.
    let normalize_lhs = |lhs: &str| -> String {
        if let Some(dot) = lhs.find('.') {
            let head = &lhs[..dot];
            if variant_fields.contains_key(head) {
                return lhs[dot + 1..].to_string();
            }
        }
        lhs.to_string()
    };

    // ADT-state transitions return `Err(WrongState)` on a variant-mismatch
    // fallthrough; without that error variant declared the emitted Rust
    // fails to compile. The failure is loud at `cargo check` — this lint
    // just surfaces it at spec-check time with a clear fix.
    if spec.state_repr_is_adt()
        && spec
            .account_types
            .first()
            .map(|a| a.variants.len() > 1)
            .unwrap_or(false)
        && !spec.error_codes.iter().any(|c| c == "WrongState")
    {
        warnings.push(CompletenessWarning {
            rule: "adt_state_missing_wrong_state".to_string(),
            severity: Severity::Warning,
            priority: 2,
            message: "`pragma state_repr = adt` is set but no `WrongState` error is declared — the inductive transitions return `Err(WrongState)` on a variant-mismatch fallthrough, which won't compile".to_string(),
            subject: None,
            fix: "Add `WrongState` to `type Error`, or drop `pragma state_repr = adt` to use the flat State representation.".to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Ghost-variable validation.
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
                warnings.push(CompletenessWarning {
                    rule: "ghost_non_scalar_type".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "ghost '{}' has non-scalar type '{}' — ghosts must be a scalar (U8…U128 / I8…I128 / Bool)",
                        g.name, g.ty
                    ),
                    subject: Some(g.name.clone()),
                    fix: "Use a scalar ghost type. Aggregate quantities over collections belong in a `property` via `sum i : Idx, …`, not a ghost.".to_string(),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
            for u in &g.updates {
                if !handler_names.contains(u.handler.as_str()) {
                    warnings.push(CompletenessWarning {
                        rule: "ghost_update_unknown_handler".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: format!(
                            "ghost '{}' has an `on {}` clause, but no handler named '{}' exists",
                            g.name, u.handler, u.handler
                        ),
                        subject: Some(g.name.clone()),
                        fix: "Name an existing handler in the `on` clause, or remove the clause."
                            .to_string(),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
            if unsupported_shape {
                warnings.push(CompletenessWarning {
                    rule: "ghost_unsupported_state_shape".to_string(),
                    severity: Severity::Warning,
                    priority: 2,
                    message: format!(
                        "ghost '{}' is declared with an indexed / multi-account / ADT state — ghost fields are only wired into the flat single-account verification State today",
                        g.name
                    ),
                    subject: Some(g.name.clone()),
                    fix: "Move the ghost to a flat single-account spec, or track the quantity in a `property` (e.g. `sum i : Idx, accounts[i].x`) until ghost support lands for this shape.".to_string(),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Hook validation.
    if !spec.hooks.is_empty() {
        // Lean enforcement is deferred (lands with qedsvm); hooks are
        // currently checked only in the Kani / proptest harnesses.
        warnings.push(CompletenessWarning {
            rule: "hook_lean_unsupported".to_string(),
            severity: Severity::Info,
            priority: 3,
            message: "hooks are enforced in the Kani / proptest harnesses; Lean enforcement is deferred (lands with qedsvm)".to_string(),
            subject: None,
            fix: "No action needed — `qedgen verify --kani` / `--proptest` exercise the hook assertions.".to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
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
                        warnings.push(CompletenessWarning {
                            rule: "hook_unknown_field".to_string(),
                            severity: Severity::Warning,
                            priority: 2,
                            message: format!(
                                "hook `after_store({})` names '{}', which is not a state field",
                                field, field
                            ),
                            subject: None,
                            fix: "Name a declared state field in `after_store(<field>)`."
                                .to_string(),
                            example: None,
                            counterexample: None,
                            fix_options: vec![],
                        });
                    }
                    if unsupported_shape {
                        warnings.push(CompletenessWarning {
                            rule: "hook_unsupported_state_shape".to_string(),
                            severity: Severity::Warning,
                            priority: 2,
                            message: format!(
                                "hook `after_store({})` is declared with an indexed / multi-account state — `after_store` is wired into the flat single-account transition only",
                                field
                            ),
                            subject: None,
                            fix: "Use a flat single-account spec, or assert the post-store condition in a `property`.".to_string(),
                            example: None,
                            counterexample: None,
                            fix_options: vec![],
                        });
                    }
                }
                ParsedHookKind::BeforeCpi(_) => {
                    warnings.push(CompletenessWarning {
                        rule: "hook_before_cpi_unsupported".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: "`hook before_cpi` enforcement is deferred — the runtime state model has no CPI to anchor to, and the Lean CPI-theorem precondition path lands with qedsvm".to_string(),
                        subject: None,
                        fix: "Encode the precondition as a `requires` on the calling handler for now, or assert it via `after_store` on the field the CPI consumes.".to_string(),
                        example: None,
                        counterexample: None,
                        fix_options: vec![],
                    });
                }
            }
            if hook.asserts.is_empty() {
                warnings.push(CompletenessWarning {
                    rule: "hook_no_assert".to_string(),
                    severity: Severity::Info,
                    priority: 3,
                    message: "hook has no `assert` clause — it checks nothing".to_string(),
                    subject: None,
                    fix: "Add at least one `assert <expr>` to the hook body.".to_string(),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    for op in &spec.handlers {
        // `auth X` and `permissionless` are contradictory; surface as P1
        // rather than silently letting one take precedence.
        if op.permissionless && op.who.is_some() {
            warnings.push(CompletenessWarning {
                rule: "contradictory_auth".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{}' declares both `auth {}` and `permissionless` — pick one",
                    op.name,
                    op.who.as_deref().unwrap_or("?"),
                ),
                subject: Some(op.name.clone()),
                fix: "Remove one: `permissionless` for deliberately-open handlers, `auth X` for access-controlled ones.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 1: handler without `auth`. Skipped for `permissionless` —
        // an explicit opt-in, not a missing declaration.
        if op.who.is_none() && !op.permissionless {
            warnings.push(CompletenessWarning {
                rule: "no_access_control".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!("handler '{}' has no `auth` — anyone can call it", op.name),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add `auth {}` to restrict who can execute this handler, or `permissionless` if this handler is deliberately open",
                    signer_hint
                ),
                example: Some(format!("  handler {}\n    auth {}", op.name, signer_hint)),
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 2: handler not covered by any property
        let covered = spec
            .properties
            .iter()
            .any(|p| p.preserved_by.contains(&op.name));
        if !covered && !spec.properties.is_empty() {
            let prop_names: Vec<&str> = spec.properties.iter().map(|p| p.name.as_str()).collect();
            warnings.push(CompletenessWarning {
                rule: "uncovered_operation".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' is not in any property's `preserved_by`",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: format!(
                    "Add '{}' to an existing property's `preserved_by` list, or confirm it doesn't need property coverage",
                    op.name
                ),
                example: Some(format!(
                    "  property {} \"...\"\n    preserved_by: ..., {}",
                    prop_names.first().unwrap_or(&"my_property"),
                    op.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }

        // Rule 3: add effect without explicit overflow bound (type-aware),
        // per-field. Sub effects get auto-guarded for underflow by codegen,
        // so only add overflow warns here.
        {
            // Collect all guard text for substring matching
            let all_guards: String = {
                let mut g = op.guard_str.clone().unwrap_or_default();
                for req in &op.requires {
                    g.push(' ');
                    g.push_str(&req.lean_expr);
                }
                g
            };

            for (field, kind, val) in &op.effects {
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
                warnings.push(CompletenessWarning {
                    rule: "unguarded_arithmetic".to_string(),
                    severity: Severity::Info,
                    priority: 2,
                    message: format!(
                        "handler '{}' adds to {} field '{}' without an explicit bound — codegen auto-inserts a {} guard, but an explicit `requires` with a tighter domain bound produces stronger proofs",
                        op.name, type_label, field, type_label
                    ),
                    subject: Some(op.name.clone()),
                    fix: format!(
                        "Add `requires state.{} + {} <= MY_BOUND` for a tighter bound than {} max",
                        field, val, type_label
                    ),
                    example: Some(format!(
                        "  handler {}\n    requires state.{} + {} <= {}",
                        op.name, field, val, type_max
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }

        // Rule 6: handler has no when/then lifecycle
        if op.pre_status.is_none() && op.post_status.is_none() {
            warnings.push(CompletenessWarning {
                rule: "no_lifecycle".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{}' has no `when`/`then` — no state machine enforcement",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add `when` and `then` clauses to enforce handler ordering".to_string(),
                example: Some(format!(
                    "  handler {}\n    when Active\n    then Active",
                    op.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 4: state fields never modified (excluding Pubkey)
    for (fname, ftype) in &spec.state_fields {
        if ftype == "Pubkey" {
            continue;
        }
        // A Map / record field counts as modified when written through
        // indexing or nested access (`accounts[i].active := 1`,
        // `pool.balance += amount`) — matching only whole-field LHS gave
        // false-positive `unused_field` on every Map field.
        let modified = spec.handlers.iter().any(|op| {
            op.effects.iter().any(|(f, _, _)| {
                let lhs = normalize_lhs(f);
                if lhs == *fname {
                    return true;
                }
                // Match `<fname>.` (record-nested) or `<fname>[` (Map-indexed)
                // as effective writes of the named field.
                lhs.starts_with(&format!("{}.", fname)) || lhs.starts_with(&format!("{}[", fname))
            })
        });
        if !modified {
            let mutating_ops: Vec<&str> = spec
                .handlers
                .iter()
                .filter(|op| op.has_effect())
                .map(|op| op.name.as_str())
                .collect();
            let op_hint = mutating_ops.first().copied().unwrap_or("some_handler");
            warnings.push(CompletenessWarning {
                rule: "unused_field".to_string(),
                severity: Severity::Info,
                priority: 4,
                message: format!("state field '{}' is never modified by any effect", fname),
                subject: Some(fname.clone()),
                fix: format!(
                    "Add an `effect: {} set <value>` or `effect: {} add <value>` to an operation, or remove the field if it's not needed",
                    fname, fname
                ),
                example: Some(format!(
                    "  operation {}\n    effect: {} set new_value",
                    op_hint, fname
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 5: property references nonexistent handler
    let op_names: Vec<&str> = spec.handlers.iter().map(|o| o.name.as_str()).collect();
    for prop in &spec.properties {
        for op_name in &prop.preserved_by {
            if !op_names.contains(&op_name.as_str()) {
                warnings.push(CompletenessWarning {
                    rule: "dangling_preserved_by".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "property '{}' references nonexistent handler '{}'",
                        prop.name, op_name
                    ),
                    subject: Some(format!("{}.preserved_by.{}", prop.name, op_name)),
                    fix: format!(
                        "Check the spelling of '{}' — available handlers: {}",
                        op_name,
                        op_names.join(", ")
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule: quantifier over a type that can't be exhausted at test time.
    // Two distinct shapes:
    //   - `forall s : <StateType>` — universal over states (e.g. `Pool.Active`).
    //     Always Lean territory; the whole quantifier is redundant since
    //     `state.x` already refers to the current state. Advice: drop it.
    //   - `forall i : <BinderType>` — bounded value quantifier over a primitive
    //     (U16+, AccountIdx, etc.). U8/I8 fit in proptest; wider types emit a
    //     stub `true`. Advice: narrow the binder.
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
                    rule: "unchecked_quantifier".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "property '{}' uses a quantifier over a type that proptest/Kani \
                         cannot exhaust — the harness emits `true` and skips the check ({})",
                        prop.name, detail
                    ),
                    subject: Some(prop.name.clone()),
                    fix,
                    example,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // P5: quantifier shape unsupported by codegen. The chumsky_adapter
    // records a precise reason (nested forall, exists, unbounded binder);
    // surfacing it here shows the exact breaking construct instead of a
    // silent `true` stub later. Supersedes `unchecked_quantifier` for the
    // shapes it covers (that lint skips when quantifier_lint is Some).
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
        warnings.push(CompletenessWarning {
            rule: "unsupported_quantifier_shape".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "property '{}' has a quantifier shape qedgen v2.20 can't lower to a \
                 non-vacuous harness — {} (bytes {}..{})",
                prop.name, qlint.message, qlint.span_start, qlint.span_end,
            ),
            subject: Some(prop.name.clone()),
            fix: workaround.to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }

    // P6: informational note that `Pubkey` state fields lower structurally
    // to `[u8; 32]` in the verification State (proptest generates them via
    // the 32-byte-array strategy).
    //
    // Scope — every place a Pubkey field can land as state:
    // `account_types[*].fields`, `sum_types[*].variants[*].fields`, and
    // `records[*].fields`. `state_fields` is a flat mirror of the first
    // account type's fields and is intentionally not scanned (double-firing).
    {
        let push_p6 = |warnings: &mut Vec<CompletenessWarning>, holder: &str, field: &str| {
            warnings.push(CompletenessWarning {
                rule: "pubkey_state_field_unsupported".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "P6: Pubkey field '{}' in {} is lowered to `[u8; 32]` in \
                     the generated proptest / Kani harness. The user-facing \
                     Anchor program target keeps the `Pubkey` type.",
                    field, holder,
                ),
                subject: Some(format!("{}.{}", holder, field)),
                fix: format!(
                    "No action required. To compare against an Anchor `Pubkey` \
                     param, convert at the call site: `s.{} == pk.to_bytes()`.",
                    field,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
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
    }

    // P7: effect references an undeclared state field. Codegen emits the
    // access verbatim and Rust fails deep inside the generated harness with
    // `no field "foo" on type "State"`; P7 catches it at `qedgen check` with
    // a precise spec-side message. Two paths:
    //   (a) LHS — `effect { undeclared := ... }`: split on `.`/`[` and check
    //       the root only; nested fields under a declared record-typed field
    //       elaborate fine downstream.
    //   (b) RHS — `effect { x := state.undeclared }`: scan the rendered Lean
    //       form for `state.<word>` and check each captured word.
    {
        // All field names declared anywhere as state. This is permissive
        // (a field that exists in any account variant clears P7 even if
        // the handler's specific lifecycle transition doesn't carry it)
        // — false negatives are preferable to a noisy lint that fires
        // on legitimate cross-variant references at this stage.
        let mut declared: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for acct in &spec.account_types {
            for (fname, _) in &acct.fields {
                declared.insert(fname.clone());
            }
        }
        for sum in &spec.sum_types {
            for variant in &sum.variants {
                for (fname, _) in &variant.fields {
                    declared.insert(fname.clone());
                }
            }
        }
        for rec in &spec.records {
            for (fname, _) in &rec.fields {
                declared.insert(fname.clone());
            }
        }
        for (fname, _) in &spec.state_fields {
            declared.insert(fname.clone());
        }

        let push_p7 =
            |warnings: &mut Vec<CompletenessWarning>, handler: &str, side: &str, name: &str| {
                warnings.push(CompletenessWarning {
                    rule: "undeclared_state_field_in_effect".to_string(),
                    severity: Severity::Warning,
                    priority: 1,
                    message: format!(
                        "P7: handler '{}' references undeclared state field \
                         '{}' on the {} of an effect — codegen will emit the \
                         reference verbatim and `cargo test` will fail with \
                         'no field' downstream",
                        handler, name, side,
                    ),
                    subject: Some(format!("{}.{}", handler, name)),
                    fix: format!(
                        "Declare `{}` in your state schema (an account_type \
                         field, a sum-variant payload field, or a record \
                         field), or rename the effect reference to match an \
                         existing field.",
                        name
                    ),
                    example: Some(format!(
                        "  type State\n    | Active of {{ {} : U64, ... }}\n",
                        name
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            };

        let strip_root = |path: &str| -> String {
            // Take the segment before the first `.` or `[`. Handles bare
            // (`foo`), nested (`foo.bar`), and indexed (`foo[i]`) forms.
            let mut end = path.len();
            for (i, c) in path.char_indices() {
                if c == '.' || c == '[' {
                    end = i;
                    break;
                }
            }
            path[..end].to_string()
        };

        // `Variant.field` LHS forms (`Active.pool := …`) bind the root to a
        // state ADT variant name, not a field; `variant_fields` (built at
        // the top of this fn) keeps the variant index consistent across
        // every effect-LHS lint.
        let second_seg = |path: &str| -> Option<String> {
            // Read the segment between the first and second separator.
            // `Active.pool` → Some("pool"); `Active.x[i]` → Some("x");
            // `Active` (no separator) → None.
            let bytes = path.as_bytes();
            let first = bytes.iter().position(|c| *c == b'.' || *c == b'[')?;
            // Only `.<ident>` is the form we care about for variant lookup.
            if bytes[first] != b'.' {
                return None;
            }
            let rest = &path[first + 1..];
            let mut end = rest.len();
            for (i, c) in rest.char_indices() {
                if c == '.' || c == '[' {
                    end = i;
                    break;
                }
            }
            Some(rest[..end].to_string())
        };

        // (a) LHS check
        for op in &spec.handlers {
            for (lhs, _kind, _rhs) in &op.effects {
                let root = strip_root(lhs);
                if root.is_empty() || declared.contains(&root) {
                    continue;
                }
                // `state := <expr>` is the variant-promotion /
                // whole-record-assignment form (`state := .Active { … }`):
                // `state` is a binder, not a field. The RHS check below
                // still scrutinizes field references in the payload.
                if root == "state" {
                    continue;
                }
                // Synthetic handlers (`_case_N`, `_otherwise`) inherit
                // their parent's effects; flagging twice would be noisy.
                if op.name.contains("_case_") || op.name.ends_with("_otherwise") {
                    continue;
                }
                // `Variant.field` LHS: a variant name as the path root is
                // legal in a multi-variant ADT state — re-target P7 at the
                // actual field, checked against that variant's payload.
                if let Some(variant_payload) = variant_fields.get(&root) {
                    if let Some(field) = second_seg(lhs) {
                        if !variant_payload.contains(&field) && !declared.contains(&field) {
                            push_p7(
                                &mut warnings,
                                &op.name,
                                "LHS",
                                &format!("{}.{}", root, field),
                            );
                        }
                    }
                    // Path root is a known variant — never push the
                    // variant name itself as "undeclared field".
                    continue;
                }
                push_p7(&mut warnings, &op.name, "LHS", &root);
            }
        }

        // (b) RHS check — scan rendered Lean form for state-path
        // references. `expr_to_lean` renders `state.X` as `s.X` (the
        // standard Lean binder for the current state), so we match that
        // form. The leading `\b` keeps `xs.foo` / `as.bar` from
        // triggering — only bare `s.` token boundaries match.
        let state_path_re =
            regex::Regex::new(r"\bs\.([A-Za-z_][A-Za-z0-9_]*)").expect("static regex");
        for op in &spec.handlers {
            let mut seen_rhs: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            for (_lhs, _kind, rhs) in &op.effects {
                for caps in state_path_re.captures_iter(rhs) {
                    let name = caps.get(1).unwrap().as_str().to_string();
                    if declared.contains(&name) || !seen_rhs.insert(name.clone()) {
                        continue;
                    }
                    if op.name.contains("_case_") || op.name.ends_with("_otherwise") {
                        continue;
                    }
                    push_p7(&mut warnings, &op.name, "RHS", &name);
                }
            }
        }
    }

    // Rule 7: takes params (U64) with no guard — suggest input validation
    for op in &spec.handlers {
        if op.has_guard() {
            continue;
        }
        // Skip if rule 3 (unguarded_arithmetic) already fired for this op
        let already_flagged = warnings
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
            warnings.push(CompletenessWarning {
                rule: "missing_guard_from_takes".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{}' takes U64 params but has no guard — no input validation",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add input validation for takes parameters".to_string(),
                example: Some(format!("  handler {}\n    guard {}", op.name, guard_expr)),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 8: takes params + lifecycle transition but no effect
    for op in &spec.handlers {
        if op.has_effect() {
            continue;
        }
        // ensures-only handlers are deliberate — the author pinned frame
        // conditions (`ensures state.x == old(state.x)`) instead of an
        // effect. Legitimate shape, not a gap.
        if !op.ensures.is_empty() {
            continue;
        }
        // A `call X.handler(...)` (CPI), `transfers` block, or declared
        // `modifies [...]` IS the handler's effect — firing here on
        // CPI-only handlers would force fictional state writes.
        if !op.calls.is_empty() || !op.transfers.is_empty() || op.modifies.is_some() {
            continue;
        }
        // Synthetic per-arm handlers (`<parent>_case_<N>`, `_otherwise`)
        // from `match` expansion have no effect by construction; mirror the
        // codegen's name convention so the lint doesn't fire on them.
        if op.name.contains("_case_") || op.name.ends_with("_otherwise") {
            continue;
        }
        // Top-level abort handlers carry `aborts_if` / `aborts_total` and
        // also have no effect by construction.
        if !op.aborts_if.is_empty() || op.aborts_total {
            continue;
        }
        let has_lifecycle = op.pre_status.is_some() || op.post_status.is_some();
        let is_init_like = op.name.contains("init") || op.name.contains("create");
        if !op.takes_params.is_empty() && (has_lifecycle || is_init_like) {
            let effect_lines = suggested_effect_lines(spec, op, is_init_like);
            warnings.push(CompletenessWarning {
                rule: "missing_effect".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "handler '{}' takes params and transitions state but has no effect",
                    op.name
                ),
                subject: Some(op.name.clone()),
                fix: "Add an effect block to describe state changes".to_string(),
                example: Some(format!(
                    "  handler {}\n  effect {{\n{}\n  }}",
                    op.name,
                    effect_lines.join("\n")
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 9: handlers with effects but zero properties
    let has_effects = spec.handlers.iter().any(|op| op.has_effect());
    if has_effects && spec.properties.is_empty() && spec.invariants.is_empty() {
        // Suggest conservation if paired add/sub exist on same field
        let mut modified_fields: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            for (field, kind, _) in &op.effects {
                modified_fields
                    .entry(field.as_str())
                    .or_default()
                    .push(kind.as_str());
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

        warnings.push(CompletenessWarning {
            rule: "no_properties".to_string(),
            severity: Severity::Warning,
            priority: 3,
            message: "spec has effects but no properties — verification has nothing to prove"
                .to_string(),
            subject: None,
            fix: "Add at least one property to define what the verification should prove"
                .to_string(),
            example: Some(example),
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Rule 10: handler has token program in accounts but no transfers.
    //
    // Suppressed on lifecycle-init handlers that create a token account:
    // Anchor's `#[account(init, token::… / associated_token::…)]` handles
    // the SPL Token CPI implicitly — no explicit `transfers` / `call
    // Token.*` needed. Init detection is a shape predicate (pre-state
    // variant carries no payload fields = freshly-created account), not a
    // hardcoded name list, which over-fired on specs naming the pre-state
    // `Uninit` / `Created` / etc. Unit variants come from both
    // `account_types[*].variants` and `sum_types`.
    let unit_variant_names: std::collections::HashSet<&str> = spec
        .account_types
        .iter()
        .flat_map(|a| a.variants.iter())
        .chain(spec.sum_types.iter().flat_map(|s| s.variants.iter()))
        .filter(|v| v.fields.is_empty())
        .map(|v| v.name.as_str())
        .collect();
    for handler in &spec.handlers {
        if !handler.has_token_program() {
            continue;
        }
        if !handler.has_calls() {
            let is_lifecycle_init = handler
                .pre_status
                .as_deref()
                .map(|s| unit_variant_names.contains(s))
                .unwrap_or(false);
            // No writable-token-account sub-condition: real specs often
            // leave token accounts bare-typed and let Anchor resolve via
            // init constraints; `is_lifecycle_init && !has_calls()` already
            // captures the shape Anchor's init macro covers implicitly.
            if is_lifecycle_init {
                continue;
            }
            let writable_tokens: Vec<&str> = handler
                .accounts
                .iter()
                .filter(|a| {
                    a.is_writable && a.account_type.as_deref() == Some("token") && !a.is_program
                })
                .map(|a| a.name.as_str())
                .collect();
            let signer_name = handler
                .signer_account()
                .map(|a| a.name.as_str())
                .unwrap_or("authority");
            let accounts_str = if writable_tokens.len() >= 2 {
                format!(
                    "from {} to {} authority {}",
                    writable_tokens[0], writable_tokens[1], signer_name
                )
            } else if writable_tokens.len() == 1 {
                format!(
                    "from {} to dest authority {}",
                    writable_tokens[0], signer_name
                )
            } else {
                format!("from source to dest authority {}", signer_name)
            };
            warnings.push(CompletenessWarning {
                rule: "missing_cpi_for_token_context".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "handler '{}' has token_program in accounts but no `transfers` block",
                    handler.name
                ),
                subject: Some(handler.name.clone()),
                fix: "Add a `transfers` block to specify token movements".to_string(),
                example: Some(format!(
                    "  handler {}\n    transfers {{\n      {} amount <expr>\n    }}",
                    handler.name, accounts_str
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 11: no errors block but handlers have guards
    let any_guards = spec.handlers.iter().any(|op| op.has_guard());
    if any_guards && spec.error_codes.is_empty() {
        warnings.push(CompletenessWarning {
            rule: "no_errors_block".to_string(),
            severity: Severity::Info,
            priority: 4,
            message: "spec has guards but no `errors` block — codegen can't generate error types"
                .to_string(),
            subject: None,
            fix: "Add an errors block listing all failure modes".to_string(),
            example: Some("  errors [InvalidAmount, Unauthorized, AlreadyClosed]".to_string()),
            counterexample: None,
            fix_options: vec![],
        });
    }

    // Rule 12: lifecycle states unreachable by any operation transition
    if spec.lifecycle_states.len() > 1 {
        let reachable = reachable_lifecycle_states(spec);
        for state in &spec.lifecycle_states {
            if !reachable.contains(state) {
                warnings.push(CompletenessWarning {
                    rule: "lifecycle_unreachable_state".to_string(),
                    severity: Severity::Info,
                    priority: 2,
                    message: format!(
                        "lifecycle state '{}' cannot be reached from any initial state via operation transitions",
                        state
                    ),
                    subject: Some(state.clone()),
                    fix: format!(
                        "Add a `when: {}` or `then: {}` clause to an operation, or remove '{}' from the lifecycle",
                        state, state, state
                    ),
                    example: None,
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 13: write_without_read — state field written in effects but never read in guards/properties
    {
        // Normalize variant-prefixed LHS (`Active.pool` → `pool`) so the
        // read-match finds bare references, and emit leaf names for nested
        // paths: `accounts[i].fee_credits` writes both `accounts` and
        // `fee_credits` for bare-leaf reads in properties/requires.
        let mut written_fields: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for op in &spec.handlers {
            for (field, _, _) in &op.effects {
                let normalized = normalize_lhs(field);
                written_fields.insert(normalized.clone());
                // Also seed every dotted segment / index root so
                // nested-path writes count for the read-side bare-
                // leaf search. `accounts[i].fee_credits` →
                // `accounts`, `fee_credits`. Pure ident segments only;
                // skip the `[…]` indexing form.
                for seg in normalized
                    .split(['.', '[', ']'])
                    .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
                {
                    written_fields.insert(seg.to_string());
                }
            }
        }
        // Gather every text that might mention a state field — all
        // requires / ensures / property bodies / invariants, not just the
        // legacy `guard_str` slot (which modern specs leave `None`,
        // making reads invisible and the lint false-positive-heavy).
        let mut texts: Vec<&str> = Vec::new();
        for op in &spec.handlers {
            if let Some(ref guard) = op.guard_str {
                texts.push(guard.as_str());
            }
            for req in &op.requires {
                texts.push(req.lean_expr.as_str());
                texts.push(req.rust_expr.as_str());
            }
            for ens in &op.ensures {
                texts.push(ens.lean_expr.as_str());
            }
        }
        for prop in &spec.properties {
            if let Some(ref expr) = prop.expression {
                texts.push(expr.as_str());
            }
        }
        for inv in &spec.invariants {
            if let Some(ref e) = inv.lean_expr {
                texts.push(e.as_str());
            }
        }
        let mut read_fields: std::collections::HashSet<String> = std::collections::HashSet::new();
        for text in &texts {
            for field in &written_fields {
                if text.contains(&format!("s.{}", field))
                    || text.contains(&format!("state.{}", field))
                    || contains_word(text, field)
                {
                    read_fields.insert(field.clone());
                }
            }
        }
        for field in &written_fields {
            if !read_fields.contains(field) {
                warnings.push(CompletenessWarning {
                    rule: "write_without_read".to_string(),
                    severity: Severity::Info,
                    priority: 3,
                    message: format!(
                        "state field '{}' is written in effects but never referenced in any guard or property",
                        field
                    ),
                    subject: Some(field.clone()),
                    fix: format!(
                        "Add '{}' to a property expression or guard, or verify that writing it without reading is intentional",
                        field
                    ),
                    example: Some(format!(
                        "  property my_invariant {{\n    expr state.{} >= 0\n    preserved_by all\n  }}",
                        field
                    )),
                    counterexample: None,
                    fix_options: vec![],
                });
            }
        }
    }

    // Rule 14: dead_guard — a guard conjunct subsumed by another on the same operation
    {
        static CMP_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?:s\.|state\.)?(\w+)\s*(>=|<=|>|<|=)\s*(\d+)$").unwrap()
        });
        let cmp_re = &*CMP_RE;
        for op in &spec.handlers {
            if let Some(ref guard) = op.guard_str {
                // Split on ∧ and "and" to get individual conjuncts
                let conjuncts: Vec<&str> = guard
                    .split('\u{2227}')
                    .flat_map(|s| s.split(" and "))
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();

                let parsed: Vec<(usize, &str, &str, i64)> = conjuncts
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| {
                        cmp_re.captures(c).and_then(|caps| {
                            let field = caps.get(1)?.as_str();
                            let cmp = caps.get(2)?.as_str();
                            let val: i64 = caps.get(3)?.as_str().parse().ok()?;
                            Some((i, field, cmp, val))
                        })
                    })
                    .collect();

                for &(i, field_a, cmp_a, val_a) in &parsed {
                    for &(j, field_b, cmp_b, val_b) in &parsed {
                        if i == j || field_a != field_b {
                            continue;
                        }
                        // Check if conjunct j implies conjunct i (making i redundant)
                        let subsumed = match (cmp_a, cmp_b) {
                            (">=", ">=") => val_b >= val_a, // x >= 5 implies x >= 3
                            (">", ">") => val_b >= val_a,   // x > 5 implies x > 3
                            (">=", ">") => val_b >= val_a,  // x > 5 implies x >= 5
                            ("<=", "<=") => val_b <= val_a, // x <= 3 implies x <= 5
                            ("<", "<") => val_b <= val_a,
                            ("<=", "<") => val_b <= val_a,
                            _ => false,
                        };
                        if subsumed && i != j {
                            warnings.push(CompletenessWarning {
                                rule: "dead_guard".to_string(),
                                severity: Severity::Info,
                                priority: 4,
                                message: format!(
                                    "guard conjunct '{}' on operation '{}' is subsumed by '{}'",
                                    conjuncts[i], op.name, conjuncts[j]
                                ),
                                subject: Some(op.name.clone()),
                                fix: format!("Remove the redundant conjunct '{}'", conjuncts[i]),
                                example: None,
                                counterexample: None,
                                fix_options: vec![],
                            });
                            break; // Only report once per subsumed conjunct
                        }
                    }
                }
            }
        }
    }

    // Rule 15: circular_lifecycle_no_terminal — lifecycle where every state has outgoing transitions
    if spec.lifecycle_states.len() > 1 {
        let mut outgoing: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
            std::collections::HashMap::new();
        for op in &spec.handlers {
            if let (Some(ref pre), Some(ref post)) = (&op.pre_status, &op.post_status) {
                if pre != post {
                    outgoing
                        .entry(pre.as_str())
                        .or_default()
                        .insert(post.as_str());
                }
            }
        }
        // A terminal state has no outgoing transitions to a different state
        let terminal_exists = spec
            .lifecycle_states
            .iter()
            .any(|s| !outgoing.contains_key(s.as_str()) || outgoing[s.as_str()].is_empty());
        if !terminal_exists {
            warnings.push(CompletenessWarning {
                rule: "circular_lifecycle_no_terminal".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: "lifecycle has no terminal state — every state has outgoing transitions"
                    .to_string(),
                subject: None,
                fix: "Consider whether the cycle is intentional. If not, designate a terminal state by removing its outgoing transitions.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Rule 16: excluded_op_modifies_property — handler NOT in preserved_by modifies fields
    // referenced by the property. The inductive theorem will need a manual proof (not sorry).
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
                        .filter(|(f, _, _)| prop_fields.contains(&f.as_str()))
                        .map(|(f, _, _)| f.as_str())
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
                                warnings.push(CompletenessWarning {
                                    rule: "preserved_by_all_potential_violation".to_string(),
                                    severity: Severity::Warning,
                                    priority: 1,
                                    message: format!(
                                        "handler '{}' is in `preserved_by` for property '{}' but effect analysis suggests a violation",
                                        op.name, prop.name
                                    ),
                                    subject: Some(op.name.clone()),
                                    fix: format!(
                                        "Add a guard to '{}' ensuring the invariant holds after the effect, or remove it from `preserved_by`",
                                        op.name
                                    ),
                                    example: None,
                                    counterexample: Some(ce),
                                    fix_options: vec![],
                                });
                            }
                        }
                    }
                    continue;
                }
                // Check if this excluded op modifies any field in the property expression
                let modified_prop_fields: Vec<&str> = op
                    .effects
                    .iter()
                    .filter(|(f, _, _)| prop_fields.contains(&f.as_str()))
                    .map(|(f, _, _)| f.as_str())
                    .collect();

                if !modified_prop_fields.is_empty() {
                    // Skip if ALL effects on property fields are monotonically safe.
                    // e.g., sub on LHS of ≤ can only decrease the LHS → invariant still holds.
                    if let Some((lhs, op_sym, _rhs)) = parse_property_relation(expr, &prop_fields) {
                        let all_safe = op
                            .effects
                            .iter()
                            .filter(|(f, _, _)| modified_prop_fields.contains(&f.as_str()))
                            .all(|(f, kind, _)| {
                                let on_lhs = f.as_str() == lhs;
                                match (kind.as_str(), op_sym, on_lhs) {
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
                        rule: "excluded_op_modifies_property".to_string(),
                        severity: Severity::Warning,
                        priority: 2,
                        message: format!(
                            "handler '{}' modifies field(s) [{}] used in property '{}' but is excluded from `preserved_by` — no inductive arm is generated for this handler, so the per-arm proof obligation is silently dropped. Either add the handler to `preserved_by` (and discharge the proof) or refactor the property so this handler doesn't need to preserve it.",
                            op.name,
                            modified_prop_fields.join(", "),
                            prop.name
                        ),
                        subject: Some(op.name.clone()),
                        fix,
                        example: None,
                        counterexample,
                        fix_options,
                    });
                }
            }
        }
    }

    // Rule 17: invariant_no_body — doc-string-only invariant. Lean codegen
    // would lower it to `theorem <name> : True := trivial` (vacuous, banned
    // by the no-tautological-proofs policy); surface at check time.
    for inv in &spec.invariants {
        if inv.lean_expr.is_none() {
            warnings.push(CompletenessWarning {
                rule: "invariant_no_body".to_string(),
                severity: Severity::Error,
                priority: 1,
                message: format!(
                    "invariant '{}' has only a description string, no `expr` body — \
                     codegen would emit `theorem {} : True := trivial` (vacuous proof)",
                    inv.name, inv.name
                ),
                subject: Some(inv.name.clone()),
                fix: format!(
                    "Add an `expr` body to invariant '{}': \
                     `invariant {} {{ expr <predicate-over-state> preserved_by all }}`",
                    inv.name, inv.name
                ),
                example: Some(format!(
                    "  invariant {} {{\n    expr state.total_in == state.total_out\n    preserved_by all\n  }}",
                    inv.name
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    // Validate new-DSL constructs: Map[N] T fields, subscripted effect LHS.
    warnings.extend(check_map_and_subscript(spec));

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

    // Spec-authoring lints for post-codegen-audit security shapes. See
    // `docs/prds/SPEC-AUTHORING-LINTS-v2.10.md` for the auditor-finding mapping.
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
