//! State-machine and state-field lints: `old(...)` misuse, unconstrained
//! `modifies`, terminal-transition / indexed-mutation / dedup gaps, and
//! cross-ADT field ambiguity.

use super::*;

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
