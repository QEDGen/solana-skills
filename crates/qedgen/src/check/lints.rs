use super::*;
use regex::Regex;
use std::sync::LazyLock;

/// Whole-word match: boundaries are start/end of string or any non-alphanumeric, non-underscore byte.
pub(crate) fn contains_word(haystack: &str, needle: &str) -> bool {
    for (i, _) in haystack.match_indices(needle) {
        let before_ok = i == 0 || {
            let b = haystack.as_bytes()[i - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after = i + needle.len();
        let after_ok = after >= haystack.len() || {
            let b = haystack.as_bytes()[after];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

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

/// Split a rendered Rust comparison `<lhs> <op> <rhs>` at the top-level
/// comparison operator (string-level, no AST). Top-level = not inside
/// parens, generic args (`Vec<...>`), or `[...]` indices; first depth-0
/// comparison wins, with `==`/`!=`/`<=`/`>=` matched before `<`/`>`.
/// `None` if the expression isn't a top-level comparison.
pub(crate) fn parse_top_level_cmp(expr: &str) -> Option<(&str, &str, &str)> {
    let bytes = expr.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'(' | b'[' | b'<' => {
                // `<` could be the comparison or the start of a generic.
                // Heuristic: if the next char is `=`, it's `<=` — handle
                // below. Otherwise treat `<` as depth-increment only when
                // preceded by an alphanumeric (generic) or whitespace
                // around a punctuation form is the comparison case.
                if b == b'<' {
                    let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                    let next = if i + 1 < bytes.len() {
                        bytes[i + 1]
                    } else {
                        b' '
                    };
                    // `<=` — comparison
                    if next == b'=' && depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 2..].trim();
                        return Some((lhs, "<=", rhs));
                    }
                    // bare `<` at depth 0 after an identifier could be a
                    // generic-list start (e.g. `Vec<u8>`). Treat as depth
                    // increment in that case.
                    if prev.is_ascii_alphanumeric() || prev == b'_' {
                        depth += 1;
                    } else if depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 1..].trim();
                        return Some((lhs, "<", rhs));
                    }
                } else {
                    depth += 1;
                }
            }
            b')' | b']' | b'>' => {
                if b == b'>' {
                    let next = if i + 1 < bytes.len() {
                        bytes[i + 1]
                    } else {
                        b' '
                    };
                    if next == b'=' && depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 2..].trim();
                        return Some((lhs, ">=", rhs));
                    }
                    if depth > 0 {
                        depth -= 1;
                    } else if depth == 0 {
                        let lhs = expr[..i].trim();
                        let rhs = expr[i + 1..].trim();
                        return Some((lhs, ">", rhs));
                    }
                } else if depth > 0 {
                    depth -= 1;
                }
            }
            b'=' => {
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' && depth == 0 {
                    let lhs = expr[..i].trim();
                    let rhs = expr[i + 2..].trim();
                    return Some((lhs, "==", rhs));
                }
            }
            b'!' => {
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' && depth == 0 {
                    let lhs = expr[..i].trim();
                    let rhs = expr[i + 2..].trim();
                    return Some((lhs, "!=", rhs));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `old_in_single_state_context`: P1 when `Expr::Old(_)` appears in a
/// `requires` clause or `invariant` body. Both describe a single state —
/// no transition has happened, so there is no "old" value; the right
/// constructs are `ensures` / `property … preserved_by …`. Left alone,
/// Lean renders guillemet-quoted `«old(...)»` (type-fails downstream) and
/// Rust silently drops the marker. Synthetic requires (match-arm
/// desugaring) carry `ast_body: None` and are skipped — no source to fix.
pub(crate) fn check_old_in_single_state_context(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        for req in &op.requires {
            let Some(ast) = &req.ast_body else { continue };
            if crate::chumsky_adapter::expr_contains_old(ast) {
                warnings.push(make_old_in_single_state_warning(
                    &op.name,
                    "requires",
                    &req.rust_expr,
                ));
            }
        }
    }
    for inv in &spec.invariants {
        let Some(ast) = &inv.ast_body else { continue };
        if crate::chumsky_adapter::expr_contains_old(ast) {
            let body_display = inv.lean_expr.as_deref().unwrap_or("(body)");
            warnings.push(make_old_in_single_state_warning(
                &inv.name,
                "invariant",
                body_display,
            ));
        }
    }
    warnings
}

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

/// Predicate shared with `kani_impl::spec_triggers_impl_harness`: true iff
/// a ref_impl carries arithmetic that could overflow on bounded Rust types
/// (the Lean lowering on `Nat`/`Int` cannot). Used as both a lint trigger
/// and the impl-targeted Kani auto-trigger so ref_impl-bearing specs always
/// get the bit-width-bounded verification surface.
pub fn ref_impl_has_overflow_risk(r: &ParsedRefImpl) -> bool {
    let has_numeric_io = std::iter::once(&r.return_type)
        .chain(r.params.iter().map(|(_, t)| t))
        .any(|t| {
            matches!(
                t.trim(),
                "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128"
            )
        });
    if !has_numeric_io {
        return false;
    }
    // Pure-expression bodies — `*` is always multiplication, `<<` is always
    // left-shift, `+`/`-` are always add/sub (no pointer arithmetic, no
    // unary `-` ambiguity in our DSL emission). A simple substring check
    // is sufficient and the lint's false-positive cost is "user is told
    // to run Kani" — tolerable.
    let body = &r.rust_body;
    body.contains('*') || body.contains("<<") || body.contains('+') || body.contains('-')
}

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

pub(crate) fn check_unconstrained_modifies(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for h in &spec.handlers {
        let Some(modifies) = h.modifies.as_ref() else {
            continue;
        };
        // Set of bare field names written by the effect block.
        let mut effect_fields: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (lhs, _, _) in &h.effects {
            // Strip a leading `Variant.` prefix (multi-variant ADT specs
            // use Variant-qualified LHS) and any `[idx]` subscript so the
            // bare field name lines up with the modifies list.
            let stripped = lhs
                .split_once('.')
                .map(|(_, rest)| rest)
                .unwrap_or(lhs.as_str());
            let bare = stripped.split('[').next().unwrap_or(stripped);
            effect_fields.insert(bare);
        }
        for field in modifies {
            if effect_fields.contains(field.as_str()) {
                continue;
            }
            // Does any ensures clause reference this field by name?
            // Conservative textual scan — `rust_expr` carries `post.<field>`
            // / `pre.<field>` / `s.<field>` depending on opts. Substring
            // match is fine because field names are user-declared and
            // bounded; false positives (`field` substring of another
            // field) are caught by the codegen lint when emitting the
            // fill site.
            let referenced = h
                .ensures
                .iter()
                .any(|e| e.rust_expr.contains(field.as_str()));
            if referenced {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "unconstrained_modifies".to_string(),
                severity: Severity::Error,
                priority: 0,
                message: format!(
                    "handler '{}' lists '{}' in `modifies` but no `effect` writes \
                     it and no `ensures` clause references it — the field is \
                     completely unconstrained. Verification harnesses have no \
                     contract to check against and the Lean frame conditions \
                     allow any post-value.",
                    h.name, field
                ),
                subject: Some(h.name.clone()),
                fix: format!(
                    "Either add an `ensures` clause that constrains `{}` against \
                     its pre-state value (so Kani / proptest can verify the impl \
                     satisfies the contract), or remove `{}` from `modifies` if \
                     it isn't really being modified.",
                    field, field
                ),
                example: Some(format!(
                    "  ensures {}_grew : state.{} >= old(state.{})",
                    field, field, field
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// Extract `pre.<field>` / `post.<field>` references from a
/// `rust_expr_binary`-rendered expression. The binary-mode renderer is the
/// only source of these tokens, so a static regex is sufficient and stable.
/// `pre.X` and `post.X` both normalize to `X` — the Kani impl harness reads
/// both from the same snapshot pair, so either binds the same locals.
pub fn extract_pre_post_field_refs(expr: &str) -> std::collections::BTreeSet<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        // Word-boundary at the start ensures `xpre.foo` doesn't match.
        Regex::new(r"\b(?:pre|post)\.([A-Za-z_][A-Za-z0-9_]*)").expect("static regex")
    });
    let mut fields = std::collections::BTreeSet::new();
    for cap in RE.captures_iter(expr) {
        fields.insert(cap[1].to_string());
    }
    fields
}

/// Per-handler predicate shared by `check.rs` (lint) and `kani_impl.rs`
/// (breadcrumb comment). For each unordered call pair whose callees resolve
/// in `spec.interfaces`, runs the same substitution as
/// `emit_cpi_ensures_as_assume` and reports `pre.X` / `post.X` references
/// appearing in both callees' substituted ensures. Tier-0 callees are
/// silent. Returns `(call_i_label, call_j_label, shared_field)` triples;
/// label format `Iface.handler` mirrors the harness CPI-block comment.
pub fn multi_cpi_shared_fields(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
) -> Vec<(String, String, String)> {
    // Resolve every call's substituted-ensures field set up front. Tier-0
    // / unresolved callees get an empty set and effectively drop out of the
    // pairwise compare.
    let resolved: Vec<(String, std::collections::BTreeSet<String>)> = handler
        .calls
        .iter()
        .map(|call| {
            let label = format!("{}.{}", call.target_interface, call.target_handler);
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                return (label, std::collections::BTreeSet::new());
            };
            let Some(callee) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                return (label, std::collections::BTreeSet::new());
            };
            let mut fields = std::collections::BTreeSet::new();
            for ens in &callee.ensures {
                let substituted = crate::cpi_substitute::substitute_callee_ensures_rust_binary(
                    &ens.rust_expr_binary,
                    call,
                    &callee.params,
                    callee.result_binder.as_deref(),
                );
                fields.extend(extract_pre_post_field_refs(&substituted));
            }
            (label, fields)
        })
        .collect();

    let mut findings = Vec::new();
    for i in 0..resolved.len() {
        if resolved[i].1.is_empty() {
            continue;
        }
        for j in (i + 1)..resolved.len() {
            if resolved[j].1.is_empty() {
                continue;
            }
            if disjoint_token_transfer_resources(&handler.calls[i], &handler.calls[j]) {
                continue;
            }
            // Set intersection ordered by BTreeSet iteration (stable
            // alphabetical for deterministic lint output).
            for field in resolved[i].1.intersection(&resolved[j].1) {
                findings.push((resolved[i].0.clone(), resolved[j].0.clone(), field.clone()));
            }
        }
    }
    findings
}

pub(crate) fn disjoint_token_transfer_resources(left: &ParsedCall, right: &ParsedCall) -> bool {
    fn token_transfer_resources(call: &ParsedCall) -> Option<std::collections::BTreeSet<String>> {
        if call.target_interface != "Token" || call.target_handler != "transfer" {
            return None;
        }

        let mut resources = std::collections::BTreeSet::new();
        for arg_name in ["from", "to"] {
            let arg = call.args.iter().find(|arg| arg.name == arg_name)?;
            resources.insert(arg.rust_expr.trim().to_string());
        }
        Some(resources)
    }

    let Some(left_resources) = token_transfer_resources(left) else {
        return false;
    };
    let Some(right_resources) = token_transfer_resources(right) else {
        return false;
    };
    left_resources.is_disjoint(&right_resources)
}

/// P2 informational lint for the multi-CPI ordering gap; one warning per
/// shared field per call pair.
pub(crate) fn check_multi_cpi_same_field(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        let findings = multi_cpi_shared_fields(spec, handler);
        for (call_i_label, call_j_label, field) in findings {
            warnings.push(CompletenessWarning {
                rule: "multi_cpi_same_field".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{}' makes multiple CPI calls ({} and {}) whose \
                     substituted ensures both reference '{}'. Kani's impl-targeted \
                     harness has only one (pre_{}, post_{}) snapshot pair captured \
                     at handler boundary; both assumes will fire at the same splice \
                     point, which can over-constrain.",
                    handler.name, call_i_label, call_j_label, field, field, field
                ),
                subject: Some(handler.name.clone()),
                fix: "Until per-call snapshot frames land (v3.0), either: (1) \
                      merge the CPI calls into a single helper handler whose \
                      ensures captures the combined effect; (2) tighten each \
                      callee's ensures so they reference disjoint fields; or \
                      (3) split the multi-CPI handler into separate handlers \
                      (one per CPI) so each gets its own (pre, post) snapshot."
                    .to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

pub(crate) fn make_old_in_single_state_warning(
    holder: &str,
    kind: &str,
    body_snippet: &str,
) -> CompletenessWarning {
    CompletenessWarning {
        rule: "old_in_single_state_context".to_string(),
        severity: Severity::Warning,
        priority: 1,
        message: format!(
            "'{}' uses `old(...)` inside a `{}` body ({}) — only meaningful in \
             `ensures` or `property` bodies (a binary transition context). \
             `requires` and `invariant` describe a single state and have no \
             \"old\" value to reference.",
            holder, kind, body_snippet
        ),
        subject: Some(holder.to_string()),
        fix: "If you meant a precondition on the pre-state, drop `old(...)` \
              and reference `state.x` directly. If you meant a property across \
              the transition, lift the clause into a `property X : ... \
              preserved_by Y`."
            .to_string(),
        example: None,
        counterexample: None,
        fix_options: vec![],
    }
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

/// True iff this handler's `auth X` will be lowered to `has_one = X` by
/// R25 — that is, `X` is a field on a state account this handler
/// touches. Used by terminal-transition and value-transfer lints to
/// avoid false positives on auth-bound handlers (the signer identity
/// IS the gate).
pub(crate) fn r25_will_bind_auth(handler: &ParsedHandler, spec: &ParsedSpec) -> bool {
    let Some(ref who) = handler.who else {
        return false;
    };
    if spec.account_types.is_empty() {
        return spec.state_fields.iter().any(|(n, _)| n == who);
    }
    spec.account_types
        .iter()
        .any(|at| at.fields.iter().any(|(n, _)| n == who))
}

// ============================================================================
// Spec-authoring lints (audit follow-up)
//
// Complement codegen fixes R25–R28 by surfacing the *spec shapes* behind
// under-specified auth, value transfer, and lifecycle transitions. Each
// lint maps 1:1 to a post-codegen audit finding; catching them at check
// time means routine spec gaps don't wait for an auditor invocation.
// ============================================================================

/// `[cross_adt_field_ambiguity]` — multi-ADT spec has a property whose
/// expression mentions a bare field name that's declared in 2+ account
/// types, and the reference isn't qualified by an account prefix. Codegen
/// then assigns the property to every ADT module whose field set the
/// expression substring-matches, which silently produces duplicate (and
/// usually wrong) predicates.
///
/// Lint, don't auto-qualify: auto-qualification would silently pick the
/// first-matching ADT and can wedge invariants against the wrong State.
pub(crate) fn check_cross_adt_field_ambiguity(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    if spec.account_types.len() < 2 {
        return warnings;
    }

    // Build field_name → Vec<account_name>. Keep only fields declared on
    // 2+ account types (the ambiguous set).
    let mut field_to_adts: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for acct in &spec.account_types {
        for (fname, _) in &acct.fields {
            field_to_adts
                .entry(fname.as_str())
                .or_default()
                .push(acct.name.as_str());
        }
    }
    field_to_adts.retain(|_, adts| adts.len() >= 2);
    if field_to_adts.is_empty() {
        return warnings;
    }

    let adt_prefixes: Vec<String> = spec
        .account_types
        .iter()
        .map(|a| format!("{}.", a.name.to_lowercase()))
        .collect();

    // Walk every property's expression. For each ambiguous field, check
    // for word-boundary references that are NOT already qualified by an
    // ADT-name prefix or by `state.` (state.X means "the implicit single
    // State", which is itself ambiguous in multi-ADT mode — flag it too).
    for prop in &spec.properties {
        let Some(ref expr) = prop.expression else {
            continue;
        };
        for (&field, adts) in &field_to_adts {
            // Quick reject: no occurrence of the field name anywhere.
            if !expr.contains(field) {
                continue;
            }
            // Walk every word-boundary position where `field` appears.
            // A reference is "qualified" if the immediately-preceding
            // character is a `.` AND the preceding identifier matches
            // one of the lowercase ADT names (`<adt>.<field>`).
            let bytes = expr.as_bytes();
            let needle = field.as_bytes();
            let mut idx = 0;
            let mut any_unqualified = false;
            while let Some(rel) = expr[idx..].find(field) {
                let start = idx + rel;
                let end = start + needle.len();
                // Word-boundary check: not preceded/followed by identifier chars.
                let pre_is_ident = start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
                let post_is_ident =
                    end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
                if !pre_is_ident && !post_is_ident {
                    // Is this an `<adt>.<field>` reference?
                    let qualified = adt_prefixes.iter().any(|p| {
                        let p_bytes = p.as_bytes();
                        start >= p_bytes.len()
                            && bytes[start - p_bytes.len()..start].eq_ignore_ascii_case(p_bytes)
                    });
                    if !qualified {
                        any_unqualified = true;
                        break;
                    }
                }
                idx = end;
            }
            if !any_unqualified {
                continue;
            }
            let adt_list = adts.join(", ");
            let first_adt_lower = adts[0].to_lowercase();
            warnings.push(CompletenessWarning {
                rule: "cross_adt_field_ambiguity".to_string(),
                severity: Severity::Warning,
                priority: 2,
                message: format!(
                    "property '{}' references field `{}` which is declared in multiple account types ({}); codegen will emit the predicate inside every matching module",
                    prop.name, field, adt_list,
                ),
                subject: Some(prop.name.clone()),
                fix: format!(
                    "Qualify the reference with the owning account type (e.g. `{}.{}`), or split the property into one per account type.",
                    first_adt_lower, field,
                ),
                example: Some(format!(
                    "  property {} \"...\"\n    {}.{} >= 0",
                    prop.name, first_adt_lower, field,
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// `[unbound_auth]` — `auth X` doesn't match a state field, so codegen's
/// `auth → has_one` lowering (R25) can't fire. The signer check verifies
/// "someone signed," not "the right someone."
///
/// Closed by R25 when `X` IS a state field. Catches the percolator-CRIT
/// shape — auth name without a state-side anchor.
pub(crate) fn check_unbound_auth(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        if handler.permissionless {
            continue;
        }
        let Some(ref who) = handler.who else {
            // `no_access_control` already covers the no-auth case; don't
            // double-flag.
            continue;
        };
        // Skip handlers without a discoverable state account — single-
        // signer admin handlers without state aren't this lint's target.
        if handler.accounts.is_empty() {
            continue;
        }
        // The state-bearing account in this handler — same logic as
        // codegen.rs::find_state_account, but we only need to know
        // *whether* one exists for field lookup. A handler with multiple
        // state candidates falls back to single-state field set.
        let has_who_field = if spec.account_types.is_empty() {
            spec.state_fields.iter().any(|(n, _)| n == who)
        } else {
            spec.account_types
                .iter()
                .any(|at| at.fields.iter().any(|(n, _)| n == who))
        };
        if has_who_field {
            continue;
        }
        // The auth name might still have a state-side binding via an
        // explicit `requires` clause. If any `requires` references both
        // `who` and a state field, treat the spec as deliberately
        // self-binding and skip the warning.
        let manually_bound = handler
            .requires
            .iter()
            .any(|r| r.lean_expr.contains(who) && r.lean_expr.contains("s."));
        if manually_bound {
            continue;
        }
        // Also accept the dotted-auth desugar / cross-program shape, where
        // the binding clause reads an imported-account field: pattern
        // `<acct>.<field> = <who>.pubkey` (Lean form) with `<acct>` a
        // non-signer account. Covers both the `auth <acct>.<field>` sugar
        // (adapt() rewrites it to this synthesized clause) and the
        // hand-written `requires` longhand.
        let who_pubkey = format!("{who}.pubkey");
        let auth_bound_via_account = handler.requires.iter().any(|r| {
            if !r.lean_expr.contains(&who_pubkey) {
                return false;
            }
            handler
                .accounts
                .iter()
                .any(|a| !a.is_signer && r.lean_expr.contains(&format!("{}.", a.name)))
        });
        if auth_bound_via_account {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unbound_auth".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' declares `auth {who}` but no state field is named `{who}`. R25's `auth → has_one` lowering only fires when the auth name matches a state field — as written, any signer can call this handler against any program-owned account.",
                handler = handler.name,
                who = who,
            ),
            subject: Some(handler.name.clone()),
            fix: format!(
                "Either (a) add `{who} : Pubkey` to the state account so codegen emits `has_one = {who}`, (b) add an explicit `requires state.<field> == {who} else Unauthorized` clause that binds the signer to a stored value, or (c) mark the handler `permissionless` if it's deliberately open.",
                who = who,
            ),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[unguarded_indexed_mutation]` — handler takes an index parameter
/// and mutates `state.<map>[i]`, but no `requires` binds the index to
/// the signer. Catches the multisig::approve/reject shape — anyone can
/// vote with any `member_index` because the spec doesn't tie the index
/// to the signer's pubkey.
pub(crate) fn check_unguarded_indexed_mutation(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        if handler.permissionless {
            continue;
        }
        let Some(ref who) = handler.who else {
            continue;
        };
        // Index-shaped params (Fin[N], U8/U16/U32 used for indexing).
        // We accept any unsigned int as a candidate; the trigger is
        // whether the param actually appears as an index in an effect's
        // LHS.
        let index_params: Vec<&str> = handler
            .takes_params
            .iter()
            .filter(|(_, t)| {
                let tt = t.trim();
                tt.starts_with("Fin") || matches!(tt, "U8" | "U16" | "U32" | "U64")
            })
            .map(|(n, _)| n.as_str())
            .collect();
        if index_params.is_empty() {
            continue;
        }
        // Does any effect LHS use one of the index params?
        let mut indexed_effect_param: Option<&str> = None;
        for (lhs, _, _) in &handler.effects {
            for p in &index_params {
                let needle = format!("[{}]", p);
                if lhs.contains(&needle) {
                    indexed_effect_param = Some(p);
                    break;
                }
            }
            if indexed_effect_param.is_some() {
                break;
            }
        }
        let Some(idx_param) = indexed_effect_param else {
            continue;
        };
        // Is there a requires that binds `who` to `state.<map>[<idx_param>]`?
        let has_binding = handler.requires.iter().any(|r| {
            let e = r.lean_expr.as_str();
            e.contains(who) && e.contains(&format!("[{}]", idx_param))
        });
        if has_binding {
            continue;
        }
        // R25 has_one binding counts as a gate too. When the auth name
        // matches a state field, only that pubkey can drive the
        // handler — so the indexed mutation IS gated, just by signer
        // identity rather than by the index itself. Multisig::add_member
        // is the canonical shape: the creator sets `members[i]`,
        // `auth creator` + `has_one = creator` binds the writer.
        if r25_will_bind_auth(handler, spec) {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unguarded_indexed_mutation".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' takes index `{idx} : <int>` and mutates `state.<map>[{idx}]`, but no `requires` clause binds `{idx}` to the signer `{who}`. As written, any signer can drive the indexed mutation against any slot — the only existing check is the bounds (`{idx} < bound`), which rules out out-of-range but not unauthorized writes.",
                handler = handler.name,
                idx = idx_param,
                who = who,
            ),
            subject: Some(handler.name.clone()),
            fix: format!(
                "Add a `requires` clause that ties `{idx}` to `{who}`, e.g.:\n\n    requires state.members[{idx}] == {who} else NotAMember\n\nWithout it, `{idx}` is just a number the caller picks.",
                idx = idx_param,
                who = who,
            ),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[scalar_counter_no_dedup]` — handler increments a scalar counter
/// (e.g. `approval_count += 1`) bounded by another scalar
/// (e.g. `approval_count + rejection_count < member_count`), but the
/// spec has no per-actor tracking field that prevents the same actor
/// from voting multiple times. Catches the dedup arm of the multisig
/// approve/reject HIGH.
pub(crate) fn check_scalar_counter_no_dedup(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    // Map field names whose type starts with Bool/U8 + "Map[" — the kinds
    // of fields users add for per-actor dedup (`voted : Map[N] U8`,
    // `processed : Map[N] Bool`).
    let has_dedup_shaped_field = |spec: &ParsedSpec| -> bool {
        let by_state = spec.state_fields.iter();
        let by_account = spec.account_types.iter().flat_map(|at| at.fields.iter());
        by_state.chain(by_account).any(|(_, t)| {
            let tt = t.trim();
            tt.starts_with("Map[") && (tt.ends_with("Bool") || tt.ends_with("U8"))
        })
    };
    if has_dedup_shaped_field(spec) {
        // Spec already has at least one dedup-shaped field — assume the
        // user has thought about this and skip. (If they have one but
        // forgot to use it, that's a separate concern.)
        return warnings;
    }
    for handler in &spec.handlers {
        for (lhs, op_kind, _) in &handler.effects {
            if op_kind != "add" {
                continue;
            }
            // Scalar increment — no subscript on the LHS.
            if lhs.contains('[') {
                continue;
            }
            // Is the incremented field bounded by ANOTHER STATE FIELD
            // in any requires clause? Const-bounded scalars (TVL caps,
            // overflow guards) don't fit this lint's shape — the
            // multisig pattern is specifically "this counter ceiling
            // is itself a state field" (`approval_count + ... <
            // member_count`), where the ceiling is per-vault dynamic
            // data and per-actor dedup is the missing piece.
            let bounded_by_state = handler.requires.iter().any(|r| {
                let e = &r.lean_expr;
                if !e.contains(lhs.as_str()) {
                    return false;
                }
                if !e.contains('<') && !e.contains('≤') {
                    return false;
                }
                // At least two distinct state-field references
                // (ours + at least one other on the bound side).
                e.matches("s.").count() >= 2 || e.matches("state.").count() >= 2
            });
            if !bounded_by_state {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "scalar_counter_no_dedup".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "handler '{handler}' increments scalar counter `{lhs}` toward an existing bound, but the spec has no per-actor record (e.g. `voted : Map[N] U8`) preventing the same actor from incrementing across different signer pubkeys.",
                    handler = handler.name,
                    lhs = lhs,
                ),
                subject: Some(handler.name.clone()),
                fix: format!(
                    "Add a per-actor tracking field and a corresponding requires clause:\n\n    state.Active of {{ ... voted : Map[N] U8 ... }}\n\n    handler {handler} (i : U8) ... {{\n      requires state.voted[i] == 0 else AlreadyVoted\n      effect {{\n        {lhs} += 1\n        voted[i] := 1\n      }}\n    }}",
                    handler = handler.name,
                    lhs = lhs,
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            // Only one warning per handler.
            break;
        }
    }
    warnings
}

/// `[unguarded_terminal_transition]` — handler transitions to a terminal
/// lifecycle state (a state that's not the post of any other handler,
/// or matches the heuristic terminal-name list) with no `requires`
/// clauses AND no R25-eligible auth binding. Catches the
/// lending::liquidate HIGH (anyone-can-liquidate).
pub(crate) fn check_unguarded_terminal_transition(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let terminal_name_heuristic: &[&str] = &[
        "Liquidated",
        "Closed",
        "Drained",
        "Cancelled",
        "Burned",
        "Settled",
        "Redeemed",
        "Finalized",
    ];
    for handler in &spec.handlers {
        let Some(ref post) = handler.post_status else {
            continue;
        };
        let is_named_terminal = terminal_name_heuristic.iter().any(|t| t == post);
        let is_structurally_terminal = !spec
            .handlers
            .iter()
            .any(|h| h.pre_status.as_deref() == Some(post.as_str()));
        if !is_named_terminal && !is_structurally_terminal {
            continue;
        }
        // Init handlers (Uninitialized → Active) aren't this lint's target —
        // a fresh-account creation transition with no requires is fine.
        let pre = handler.pre_status.as_deref().unwrap_or("");
        if matches!(pre, "Uninitialized" | "Empty") {
            continue;
        }
        if !handler.requires.is_empty() {
            continue;
        }
        // R25 has_one binding counts as a gate. If the handler's `auth X`
        // matches a state field, R25 emits `has_one = X` and only the
        // matching pubkey can trigger the transition. This is the
        // escrow::cancel / escrow::exchange shape — gated by signer
        // identity, no data precondition needed.
        if r25_will_bind_auth(handler, spec) {
            continue;
        }
        warnings.push(CompletenessWarning {
            rule: "unguarded_terminal_transition".to_string(),
            severity: Severity::Warning,
            priority: 1,
            message: format!(
                "handler '{handler}' transitions to terminal state `{post}` with no `requires` clauses. Terminal transitions usually need a guard — anyone with the right account shape can otherwise trigger the transition.",
                handler = handler.name,
                post = post,
            ),
            subject: Some(handler.name.clone()),
            fix: "Add a `requires` clause that gates the transition. For liquidation: a health threshold (`requires state.amount > state.collateral else AccountHealthy`). For closing: an empty-balance check (`requires state.balance == 0`). For settlement: a finality predicate.".to_string(),
            example: None,
            counterexample: None,
            fix_options: vec![],
        });
    }
    warnings
}

/// `[unconditional_value_transfer]` — handler has a `transfers` clause
/// where the source account is owned by program state (i.e. has
/// `authority X` with X being a handler-bound account that's program-
/// derived), AND the handler has no `requires` clause that constrains
/// who can call it. Catches the lending::liquidate vault-drain shape.
pub(crate) fn check_unconditional_value_transfer(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        for transfer in &handler.transfers {
            // Look up the `from` account in the handler's accounts list.
            // If it has a token authority that points at a writable
            // PDA-typed account in this handler, the source is program-
            // owned.
            let Some(from_acct) = handler.accounts.iter().find(|a| a.name == transfer.from) else {
                continue;
            };
            let Some(ref auth_name) = from_acct.authority else {
                continue;
            };
            let auth_is_program_owned = handler
                .accounts
                .iter()
                .any(|a| &a.name == auth_name && a.is_writable && a.pda_seeds.is_some());
            if !auth_is_program_owned {
                continue;
            }
            // Does the handler have a constraining requires beyond
            // amount-validity? We treat "amount > 0" / "amount < ..." as
            // not constraining caller identity.
            let has_caller_requires = handler.requires.iter().any(|r| {
                let e = &r.lean_expr;
                // Heuristic: caller-binding requires reference state.<field>
                // rather than just the amount param.
                e.contains("s.") || e.contains("state.")
            });
            if has_caller_requires {
                continue;
            }
            // R25 has_one binding counts as a caller gate — escrow::exchange
            // and ::cancel are both auth-bound (`auth taker` / `auth
            // initializer` matching state fields), so the transfer is
            // already gated by signer identity even without an explicit
            // `requires`.
            if r25_will_bind_auth(handler, spec) {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "unconditional_value_transfer".to_string(),
                severity: Severity::Warning,
                priority: 1,
                message: format!(
                    "handler '{handler}' transfers from program-owned `{from}` (authority `{auth}`) with no `requires` clauses constraining who can call it. Value-extracting handlers usually need an authority binding or a precondition that gates the transfer.",
                    handler = handler.name,
                    from = transfer.from,
                    auth = auth_name,
                ),
                subject: Some(handler.name.clone()),
                fix: "Either bind the auth to a state field (so R25 emits `has_one = X`) or add a precondition that gates the transfer (e.g. health check, redemption ratio, allowance). Without one, any signer can extract value from the program-owned account.".to_string(),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
            break; // one warning per handler
        }
    }
    warnings
}

/// `cpi_no_callee_ensures`: flags a call site whose interface handler has
/// no `ensures` — the caller's Lean proof carries `by sorry` (Tier-0
/// axiomatization) with no post-condition to discharge. Distinct from
/// `shape_only_cpi` (missing interface/handler declarations): this fires
/// on declared handlers that simply have no post-condition shape.
pub(crate) fn check_cpi_no_callee_ensures(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue; // shape_only_cpi handles undeclared interfaces.
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue; // shape_only_cpi handles undeclared handlers.
            };
            if !ih.ensures.is_empty() {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "cpi_no_callee_ensures".to_string(),
                severity: Severity::Info,
                priority: 1,
                message: format!(
                    "handler '{}' calls `{}.{}` — callee has no `ensures` clauses; \
                     caller's Lean theorem carries `by sorry` (Tier-0 axiomatization)",
                    handler.name, call.target_interface, call.target_handler,
                ),
                subject: Some(handler.name.clone()),
                fix: format!(
                    "Add at least one `ensures <expr>` inside `interface {} {{ handler {} {{ ... }} }}`, \
                     or commit to an `upstream {{ binary_hash = ... }}` pin on the interface so the \
                     caller can discharge via the bundled axiom module.",
                    call.target_interface, call.target_handler,
                ),
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* observable post-condition */\n    }}\n  }}",
                    call.target_interface, call.target_handler,
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// `cpi_unverified_callee`: callee has `ensures` but no imported proof
/// package. The caller still gets discharge via the bundled axiom (Stance
/// 1), but the trust anchor is "binary matches a pinned hash" rather than
/// "we have a proof against the callee's spec." Fires on bundled-stdlib
/// builtins (no proofs shipped) and external imports without
/// `<source>/.qed/proofs/<Iface>.lean` + `lakefile.lean`; suppressed when
/// `spec.verified_callees` has the interface. P2 advisory — `qedgen verify
/// --require-verified` escalates.
pub(crate) fn check_cpi_unverified_callee(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    // Only walk imports — in-spec interfaces declared inline by the
    // author aren't "callees" from a composition standpoint; they're
    // contracts the same author is committing to.
    let import_iface_names: std::collections::HashSet<&str> = spec
        .imports
        .iter()
        .map(|i| i.as_name.as_deref().unwrap_or(i.name.as_str()))
        .collect();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            if !import_iface_names.contains(call.target_interface.as_str()) {
                continue;
            }
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue;
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue;
            };
            if ih.ensures.is_empty() {
                // cpi_no_callee_ensures (P1) owns this case.
                continue;
            }
            if spec.verified_callees.contains_key(&iface.name) {
                continue;
            }
            // One warning per (interface, handler) pair — same call
            // site referenced from multiple handlers shouldn't fire N
            // times.
            let key = format!("{}.{}", iface.name, ih.name);
            if !seen.insert(key) {
                continue;
            }
            warnings.push(CompletenessWarning {
                rule: "cpi_unverified_callee".to_string(),
                severity: Severity::Info,
                priority: 2,
                message: format!(
                    "import `{}` is unverified — `{}.{}` discharges via Stance-1 axiom (binary_hash pin) instead of an imported proof",
                    iface.name, iface.name, ih.name,
                ),
                subject: Some(iface.name.clone()),
                fix: format!(
                    "Ship a Lake-buildable proof package alongside the provider's qedspec at \
                     `<source>/.qed/proofs/{}.lean` (with a sibling `lakefile.lean` declaring \
                     `package {}`). The consumer's codegen will auto-detect the package and \
                     swap the caller's theorem from Stance 1 (axiom) to Stance 2 (imported proof).",
                    iface.name,
                    crate::lean_sidecars::proof_pkg_name(&iface.name),
                ),
                example: None,
                counterexample: None,
                fix_options: vec![],
            });
        }
    }
    warnings
}

/// One finding per imported interface that `qedgen verify
/// --require-verified` would reject; carries enough context for main.rs to
/// render a CRIT line and exit non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct UnverifiedCallee {
    pub interface_name: String,
    pub fix_hint: String,
}

/// `qedgen verify --require-verified` predicate. Yields one
/// [`UnverifiedCallee`] per imported interface that: was reached via
/// `import` (not declared inline); has at least one handler with non-empty
/// `ensures` (Tier-0 shape-only imports are exempt — `cpi_no_callee_ensures`
/// covers them); is absent from `spec.verified_callees`; and is NOT
/// sentinel-pinned (`sha256:00…00`). Sentinel-pinned native programs
/// (System) are documented runtime trust boundaries — their `ensures` are
/// discharged by the validator itself, so counting them "unverified" would
/// fail every spec that imports them. Empty vec = dep graph fully proven
/// from a Stance-2 standpoint; mirrors `check_cpi_unverified_callee`.
#[allow(dead_code)]
pub fn collect_require_verified_findings(spec: &ParsedSpec) -> Vec<UnverifiedCallee> {
    let import_iface_names: std::collections::HashSet<&str> = spec
        .imports
        .iter()
        .map(|i| i.as_name.as_deref().unwrap_or(i.name.as_str()))
        .collect();

    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for iface in &spec.interfaces {
        if !import_iface_names.contains(iface.name.as_str()) {
            continue;
        }
        let has_ensures = iface.handlers.iter().any(|h| !h.ensures.is_empty());
        if !has_ensures {
            continue;
        }
        if spec.verified_callees.contains_key(&iface.name) {
            continue;
        }
        if iface
            .upstream
            .as_ref()
            .and_then(|u| u.binary_hash.as_deref())
            .map(crate::upstream_check::is_sentinel_hash)
            .unwrap_or(false)
        {
            continue;
        }
        if !seen.insert(iface.name.clone()) {
            continue;
        }
        let proof_pkg = crate::lean_sidecars::proof_pkg_name(&iface.name);
        results.push(UnverifiedCallee {
            interface_name: iface.name.clone(),
            fix_hint: format!(
                "provider must ship `<source>/.qed/proofs/{}.lean` + a sibling `lakefile.lean` \
                 declaring `package {}`. Run without --require-verified to accept Stance-1 \
                 axiom discharge instead.",
                iface.name, proof_pkg
            ),
        });
    }
    results
}

pub(crate) fn check_shape_only_cpi(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();

    for handler in &spec.handlers {
        for call in &handler.calls {
            let iface = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface);
            let target_handler =
                iface.and_then(|i| i.handlers.iter().find(|h| h.name == call.target_handler));

            let (reason, fix) = match (iface, target_handler) {
                (None, _) => (
                    format!(
                        "interface `{}` is not declared in this spec — the call compiles but has no contract",
                        call.target_interface
                    ),
                    format!(
                        "Declare `interface {} {{ ... }}` at the top level, or `qedgen interface --idl <path>` to scaffold one.",
                        call.target_interface
                    ),
                ),
                (Some(_), None) => (
                    format!(
                        "interface `{}` has no handler named `{}` — check for a typo or add the handler",
                        call.target_interface, call.target_handler
                    ),
                    format!(
                        "Add `handler {}` inside `interface {} {{ ... }}`, or update the call site to match a real handler.",
                        call.target_handler, call.target_interface
                    ),
                ),
                // Declared interface + declared handler: skip, even with no
                // `ensures`. Firing here pressured authors into `ensures
                // true` on shapes with no meaningful post-condition (Token
                // init / metadata-create / close); the import-level Tier
                // 0/1/2 signal already covers it.
                _ => continue,
            };

            warnings.push(CompletenessWarning {
                rule: "shape_only_cpi".to_string(),
                severity: Severity::Info,
                priority: 3,
                message: format!(
                    "handler '{}' calls `{}.{}` — {}",
                    handler.name, call.target_interface, call.target_handler, reason
                ),
                subject: Some(handler.name.clone()),
                fix,
                example: Some(format!(
                    "  interface {} {{\n    handler {} (...) {{\n      ensures /* what the callee guarantees */\n    }}\n  }}",
                    call.target_interface, call.target_handler
                )),
                counterexample: None,
                fix_options: vec![],
            });
        }
    }

    warnings
}

/// Parsed form of a field type string. Captures the distinction between a
/// plain type (e.g. `U128`, `Account`) and a bounded map (`Map[N] T`).
///
/// Only `Map { .. }` is inspected by the current consumer; `Simple` carries
/// the trimmed type string for future linting passes (e.g., primitive-type
/// checks, alias resolution) and intentionally remains exhaustive.
#[derive(Debug)]
pub(crate) enum FieldTypeShape<'a> {
    Simple(#[allow(dead_code)] &'a str),
    Map { bound: &'a str, inner: &'a str },
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

/// Parse a field-type source string into a structured view.
/// Returns `Simple` for `U128`, `Account`, `Vec U64` and `Map { ... }` for
/// `Map[CONST] T` (bound and inner trimmed).
pub(crate) fn classify_field_type(s: &str) -> FieldTypeShape<'_> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound = rest[..close].trim();
                let inner = rest[close + 1..].trim();
                return FieldTypeShape::Map { bound, inner };
            }
        }
    }
    FieldTypeShape::Simple(trimmed)
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
