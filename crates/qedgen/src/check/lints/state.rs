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
pub(super) fn check_old_in_single_state_context(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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

pub(super) fn check_unconstrained_modifies(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for h in &spec.handlers {
        let Some(modifies) = h.modifies.as_ref() else {
            continue;
        };
        // Set of bare field names written by the effect block.
        let mut effect_fields: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for eff in &h.effects {
            let lhs = &eff.field;
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
            warnings.push(
                warn(
                    "unconstrained_modifies",
                    Severity::Error,
                    0,
                    format!(
                        "handler '{}' lists '{}' in `modifies` but no `effect` writes \
                     it and no `ensures` clause references it — the field is \
                     completely unconstrained. Verification harnesses have no \
                     contract to check against and the Lean frame conditions \
                     allow any post-value.",
                        h.name, field
                    ),
                )
                .subject(h.name.clone())
                .fix(format!(
                    "Either add an `ensures` clause that constrains `{}` against \
                     its pre-state value (so Kani / proptest can verify the impl \
                     satisfies the contract), or remove `{}` from `modifies` if \
                     it isn't really being modified.",
                    field, field
                ))
                .example(format!(
                    "  ensures {}_grew : state.{} >= old(state.{})",
                    field, field, field
                )),
            );
        }
    }
    warnings
}

/// `[unguarded_terminal_transition]` — handler transitions to a terminal
/// lifecycle state (a state that's not the post of any other handler,
/// or matches the heuristic terminal-name list) with no `requires`
/// clauses AND no R25-eligible auth binding. Catches the
/// lending::liquidate HIGH (anyone-can-liquidate).
pub(super) fn check_unguarded_terminal_transition(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
        warnings.push(warn("unguarded_terminal_transition", Severity::Warning, 1, format!(
                "handler '{handler}' transitions to terminal state `{post}` with no `requires` clauses. Terminal transitions usually need a guard — anyone with the right account shape can otherwise trigger the transition.",
                handler = handler.name,
                post = post,
            )).subject(handler.name.clone()).fix("Add a `requires` clause that gates the transition. For liquidation: a health threshold (`requires state.amount > state.collateral else AccountHealthy`). For closing: an empty-balance check (`requires state.balance == 0`). For settlement: a finality predicate."));
    }
    warnings
}

/// `[scalar_counter_no_dedup]` — handler increments a scalar counter
/// (e.g. `approval_count += 1`) bounded by another scalar
/// (e.g. `approval_count + rejection_count < member_count`), but the
/// spec has no per-actor tracking field that prevents the same actor
/// from voting multiple times. Catches the dedup arm of the multisig
/// approve/reject HIGH.
pub(super) fn check_scalar_counter_no_dedup(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
        for eff in &handler.effects {
            let lhs = &eff.field;
            if eff.op != "add" {
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
            warnings.push(warn("scalar_counter_no_dedup", Severity::Info, 2, format!(
                    "handler '{handler}' increments scalar counter `{lhs}` toward an existing bound, but the spec has no per-actor record (e.g. `voted : Map[N] U8`) preventing the same actor from incrementing across different signer pubkeys.",
                    handler = handler.name,
                    lhs = lhs,
                )).subject(handler.name.clone()).fix(format!(
                    "Add a per-actor tracking field and a corresponding requires clause:\n\n    state.Active of {{ ... voted : Map[N] U8 ... }}\n\n    handler {handler} (i : U8) ... {{\n      requires state.voted[i] == 0 else AlreadyVoted\n      effect {{\n        {lhs} += 1\n        voted[i] := 1\n      }}\n    }}",
                    handler = handler.name,
                    lhs = lhs,
                )));
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
pub(super) fn check_unguarded_indexed_mutation(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
        for eff in &handler.effects {
            let lhs = &eff.field;
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
        warnings.push(warn("unguarded_indexed_mutation", Severity::Warning, 1, format!(
                "handler '{handler}' takes index `{idx} : <int>` and mutates `state.<map>[{idx}]`, but no `requires` clause binds `{idx}` to the signer `{who}`. As written, any signer can drive the indexed mutation against any slot — the only existing check is the bounds (`{idx} < bound`), which rules out out-of-range but not unauthorized writes.",
                handler = handler.name,
                idx = idx_param,
                who = who,
            )).subject(handler.name.clone()).fix(format!(
                "Add a `requires` clause that ties `{idx}` to `{who}`, e.g.:\n\n    requires state.members[{idx}] == {who} else NotAMember\n\nWithout it, `{idx}` is just a number the caller picks.",
                idx = idx_param,
                who = who,
            )));
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
pub(super) fn check_cross_adt_field_ambiguity(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
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
            warnings.push(warn("cross_adt_field_ambiguity", Severity::Warning, 2, format!(
                    "property '{}' references field `{}` which is declared in multiple account types ({}); codegen will emit the predicate inside every matching module",
                    prop.name, field, adt_list,
                )).subject(prop.name.clone()).fix(format!(
                    "Qualify the reference with the owning account type (e.g. `{}.{}`), or split the property into one per account type.",
                    first_adt_lower, field,
                )).example(format!(
                    "  property {} \"...\"\n    {}.{} >= 0",
                    prop.name, first_adt_lower, field,
                )));
        }
    }
    warnings
}

/// ADT-state transitions return `Err(WrongState)` on a variant-mismatch
/// fallthrough; without that error variant declared the emitted Rust
/// fails to compile. The failure is loud at `cargo check` — this lint
/// just surfaces it at spec-check time with a clear fix.
pub(super) fn check_adt_state_missing_wrong_state(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    if spec.state_repr_is_adt()
        && spec
            .account_types
            .first()
            .map(|a| a.variants.len() > 1)
            .unwrap_or(false)
        && !spec.error_codes.iter().any(|c| c == "WrongState")
    {
        warnings.push(warn("adt_state_missing_wrong_state", Severity::Warning, 2, "`pragma state_repr = adt` is set but no `WrongState` error is declared — the inductive transitions return `Err(WrongState)` on a variant-mismatch fallthrough, which won't compile").fix("Add `WrongState` to `type Error`, or drop `pragma state_repr = adt` to use the flat State representation."));
    }
    warnings
}

/// Rule 6: handler has no when/then lifecycle.
pub(super) fn check_no_lifecycle(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    for op in &spec.handlers {
        if op.pre_status.is_none() && op.post_status.is_none() {
            warnings.push(
                warn(
                    "no_lifecycle",
                    Severity::Info,
                    2,
                    format!(
                        "handler '{}' has no `when`/`then` — no state machine enforcement",
                        op.name
                    ),
                )
                .subject(op.name.clone())
                .fix("Add `when` and `then` clauses to enforce handler ordering")
                .example(format!(
                    "  handler {}\n    when Active\n    then Active",
                    op.name
                )),
            );
        }
    }
    warnings
}

/// Rule 4: state fields never modified (excluding Pubkey).
pub(super) fn check_unused_field(ctx: &LintCtx) -> Vec<CompletenessWarning> {
    let spec = ctx.spec;
    let mut warnings = Vec::new();
    for (fname, ftype) in &spec.state_fields {
        if ftype == "Pubkey" {
            continue;
        }
        // A Map / record field counts as modified when written through
        // indexing or nested access (`accounts[i].active := 1`,
        // `pool.balance += amount`) — matching only whole-field LHS gave
        // false-positive `unused_field` on every Map field.
        let modified = spec.handlers.iter().any(|op| {
            op.effects.iter().any(|e| {
                let lhs = ctx.normalize_lhs(&e.field);
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
            warnings.push(warn("unused_field", Severity::Info, 4, format!("state field '{}' is never modified by any effect", fname)).subject(fname.clone()).fix(format!(
                    "Add an `effect: {} set <value>` or `effect: {} add <value>` to an operation, or remove the field if it's not needed",
                    fname, fname
                )).example(format!(
                    "  operation {}\n    effect: {} set new_value",
                    op_hint, fname
                )));
        }
    }
    warnings
}

/// P7: effect references an undeclared state field. Codegen emits the
/// access verbatim and Rust fails deep inside the generated harness with
/// `no field "foo" on type "State"`; P7 catches it at `qedgen check` with
/// a precise spec-side message. Two paths:
///   (a) LHS — `effect { undeclared := ... }`: split on `.`/`[` and check
///       the root only; nested fields under a declared record-typed field
///       elaborate fine downstream.
///   (b) RHS — `effect { x := state.undeclared }`: scan the rendered Lean
///       form for `state.<word>` and check each captured word.
pub(super) fn check_undeclared_state_field_in_effect(ctx: &LintCtx) -> Vec<CompletenessWarning> {
    let spec = ctx.spec;
    let variant_fields = &ctx.variant_fields;
    let mut warnings = Vec::new();

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
            warnings.push(
                warn(
                    "undeclared_state_field_in_effect",
                    Severity::Warning,
                    1,
                    format!(
                        "P7: handler '{}' references undeclared state field \
                         '{}' on the {} of an effect — codegen will emit the \
                         reference verbatim and `cargo test` will fail with \
                         'no field' downstream",
                        handler, name, side,
                    ),
                )
                .subject(format!("{}.{}", handler, name))
                .fix(format!(
                    "Declare `{}` in your state schema (an account_type \
                         field, a sum-variant payload field, or a record \
                         field), or rename the effect reference to match an \
                         existing field.",
                    name
                ))
                .example(format!(
                    "  type State\n    | Active of {{ {} : U64, ... }}\n",
                    name
                )),
            );
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
    // state ADT variant name, not a field; `variant_fields` (on the
    // shared `LintCtx`) keeps the variant index consistent across
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
        for eff in &op.effects {
            let lhs = &eff.field;
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
    let state_path_re = regex::Regex::new(r"\bs\.([A-Za-z_][A-Za-z0-9_]*)").expect("static regex");
    for op in &spec.handlers {
        let mut seen_rhs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for eff in &op.effects {
            for caps in state_path_re.captures_iter(&eff.value) {
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

    warnings
}

/// Rule 8: takes params + lifecycle transition but no effect.
pub(super) fn check_missing_effect(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
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
        // Top-level abort handlers carry `aborts_total` and also have no
        // effect by construction.
        if op.aborts_total {
            continue;
        }
        let has_lifecycle = op.pre_status.is_some() || op.post_status.is_some();
        let is_init_like = op.name.contains("init") || op.name.contains("create");
        if !op.takes_params.is_empty() && (has_lifecycle || is_init_like) {
            let effect_lines = suggested_effect_lines(spec, op, is_init_like);
            warnings.push(
                warn(
                    "missing_effect",
                    Severity::Warning,
                    2,
                    format!(
                        "handler '{}' takes params and transitions state but has no effect",
                        op.name
                    ),
                )
                .subject(op.name.clone())
                .fix("Add an effect block to describe state changes")
                .example(format!(
                    "  handler {}\n  effect {{\n{}\n  }}",
                    op.name,
                    effect_lines.join("\n")
                )),
            );
        }
    }
    warnings
}

/// Rule 12: lifecycle states unreachable by any operation transition.
pub(super) fn check_lifecycle_unreachable_state(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    if spec.lifecycle_states.len() > 1 {
        let reachable = reachable_lifecycle_states(spec);
        for state in &spec.lifecycle_states {
            if !reachable.contains(state) {
                warnings.push(warn("lifecycle_unreachable_state", Severity::Info, 2, format!(
                        "lifecycle state '{}' cannot be reached from any initial state via operation transitions",
                        state
                    )).subject(state.clone()).fix(format!(
                        "Add a `when: {}` or `then: {}` clause to an operation, or remove '{}' from the lifecycle",
                        state, state, state
                    )));
            }
        }
    }
    warnings
}

/// Rule 13: write_without_read — state field written in effects but never
/// read in guards/properties.
pub(super) fn check_write_without_read(ctx: &LintCtx) -> Vec<CompletenessWarning> {
    let spec = ctx.spec;
    let mut warnings = Vec::new();
    // Normalize variant-prefixed LHS (`Active.pool` → `pool`) so the
    // read-match finds bare references, and emit leaf names for nested
    // paths: `accounts[i].fee_credits` writes both `accounts` and
    // `fee_credits` for bare-leaf reads in properties/requires.
    let mut written_fields: std::collections::HashSet<String> = std::collections::HashSet::new();
    for op in &spec.handlers {
        for eff in &op.effects {
            let normalized = ctx.normalize_lhs(&eff.field);
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
    // requires / ensures / property bodies / invariants.
    let mut texts: Vec<&str> = Vec::new();
    for op in &spec.handlers {
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
            warnings.push(warn("write_without_read", Severity::Info, 3, format!(
                    "state field '{}' is written in effects but never referenced in any guard or property",
                    field
                )).subject(field.clone()).fix(format!(
                    "Add '{}' to a property expression or guard, or verify that writing it without reading is intentional",
                    field
                )).example(format!(
                    "  property my_invariant {{\n    expr state.{} >= 0\n    preserved_by all\n  }}",
                    field
                )));
        }
    }
    warnings
}

/// Rule 15: circular_lifecycle_no_terminal — lifecycle where every state
/// has outgoing transitions.
pub(super) fn check_circular_lifecycle_no_terminal(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
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
            warnings.push(warn("circular_lifecycle_no_terminal", Severity::Info, 3, "lifecycle has no terminal state — every state has outgoing transitions").fix("Consider whether the cycle is intentional. If not, designate a terminal state by removing its outgoing transitions."));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::test_support::*;

    // `modifies [X]` + no effect write + no ensures referencing X =
    // completely unconstrained field; fires P0.
    #[test]
    fn unconstrained_modifies_lint_fires_on_uncovered_field() {
        let src = r#"
    spec Probe
    state { pool_balance : U64, lp_supply : U64 }
    type Error
      | InvalidAmount
      | MathOverflow
    handler deposit (amount : U64) {
      requires amount > 0 else InvalidAmount
      modifies [pool_balance, lp_supply]
      effect { pool_balance += amount }
    }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_unconstrained_modifies(&spec);
        let hit = warnings
            .iter()
            .find(|w| w.rule == "unconstrained_modifies")
            .expect("unconstrained_modifies fires for lp_supply");
        assert_eq!(hit.severity, Severity::Error);
        assert!(
            hit.message.contains("'lp_supply'"),
            "message names the field, got: {}",
            hit.message
        );
        // pool_balance is in modifies AND in effect — no warning for it.
        assert!(
            !warnings
                .iter()
                .any(|w| w.message.contains("'pool_balance'")),
            "pool_balance must not fire — it's written by the effect"
        );
    }

    // Inverse: when an `ensures` clause references the field, the
    // lint stays silent. The field is constrained even if the effect
    // block doesn't write it (the "Kani checks impl" pattern).
    #[test]
    fn unconstrained_modifies_lint_silent_when_ensures_references_field() {
        let src = r#"
    spec Probe
    state { pool_balance : U64, lp_supply : U64 }
    type Error
      | InvalidAmount
      | MathOverflow
    handler deposit (amount : U64) {
      requires amount > 0 else InvalidAmount
      modifies [pool_balance, lp_supply]
      effect { pool_balance += amount }
      ensures lp_supply >= old(state.lp_supply)
    }
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        let warnings = check_unconstrained_modifies(&spec);
        assert!(
            warnings.is_empty(),
            "lint must stay silent when ensures references the field, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// Fixture mirroring the multisig::approve/reject HIGH: handler
    /// takes `member_index` and mutates `state.voted[member_index]` but
    /// no `requires` binds the index to the signer.
    const UNGUARDED_INDEXED_FIXTURE: &str = r#"
    spec Voting

    const N = 8

    type State
      | Uninitialized
      | Active of {
          voted : Map[N] U8,
          count : U8,
        }

    type Error | OutOfRange | MathOverflow

    handler vote (member_index : U8) : State.Active -> State.Active {
      auth voter
      accounts {
        voter : signer
        vault : writable
      }
      requires member_index < 8 else OutOfRange
      effect {
        count += 1
        voted[member_index] := 1
      }
    }
    "#;

    #[test]
    fn lint_unguarded_indexed_mutation_fires() {
        let spec = crate::chumsky_adapter::parse_str(UNGUARDED_INDEXED_FIXTURE)
            .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_indexed_mutation")
            .collect();
        assert!(
                !hits.is_empty(),
                "expected unguarded_indexed_mutation to fire on a vote-by-index handler with no signer↔index binding; got: {:?}",
                warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
            );
    }

    /// Fixture mirroring the lending::liquidate HIGH: handler
    /// transitions to a terminal state with no `requires`.
    const UNGUARDED_TERMINAL_FIXTURE: &str = r#"
    spec Loan

    type State
      | Empty
      | Active of {
          borrower : Pubkey,
          amount   : U64,
        }
      | Liquidated

    type Error | NotFound

    handler liquidate : State.Active -> State.Liquidated {
      auth liquidator
      accounts {
        liquidator : signer
        loan       : writable
      }
      effect { amount := 0 }
    }
    "#;

    #[test]
    fn lint_unguarded_terminal_transition_fires() {
        let spec = crate::chumsky_adapter::parse_str(UNGUARDED_TERMINAL_FIXTURE)
            .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_terminal_transition")
            .collect();
        assert!(
                !hits.is_empty(),
                "expected unguarded_terminal_transition to fire on a Liquidated transition with no requires; got: {:?}",
                warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
            );
    }

    /// Inverse: when the transition IS gated by an explicit `requires`,
    /// the lint should NOT fire (audit-fixed lending::liquidate shape).
    const GATED_TERMINAL_FIXTURE: &str = r#"
    spec Loan

    type State
      | Empty
      | Active of {
          borrower   : Pubkey,
          amount     : U64,
          collateral : U64,
        }
      | Liquidated

    type Error | AccountHealthy

    handler liquidate : State.Active -> State.Liquidated {
      auth liquidator
      accounts {
        liquidator : signer
        loan       : writable
      }
      requires state.amount > state.collateral else AccountHealthy
      effect { amount := 0 }
    }
    "#;

    #[test]
    fn lint_gated_terminal_transition_does_not_fire() {
        let spec = crate::chumsky_adapter::parse_str(GATED_TERMINAL_FIXTURE)
            .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<&str> = warnings
            .iter()
            .filter(|w| w.rule == "unguarded_terminal_transition")
            .map(|w| w.rule.as_str())
            .collect();
        assert!(
            hits.is_empty(),
            "unguarded_terminal_transition should not fire on health-gated liquidate; got: {:?}",
            hits
        );
    }

    // Cross-ADT field-ambiguity lint. Three cases:
    //   (a) two ADTs share a field name AND a property references the bare
    //       name → lint fires.
    //   (b) single-ADT spec → never fires (lint short-circuits).
    //   (c) explicit `<adt>.<field>` qualification → does not fire.
    #[test]
    fn cross_adt_field_ambiguity_fires_on_bare_reference() {
        let src = r#"spec Pair

    type Distribution
      | Empty
      | Active of {
          authority : Pubkey,
          balance   : U64,
        }

    type Claim
      | Empty
      | Active of {
          claimant : Pubkey,
          balance  : U64,
        }

    property positive_balance :
      state.balance >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_cross_adt_field_ambiguity(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "cross_adt_field_ambiguity"),
            "expected cross_adt_field_ambiguity to fire on bare `state.balance` ref, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
        );
        // The message names both ADTs so the user can pick.
        let msg = &warnings
            .iter()
            .find(|w| w.rule == "cross_adt_field_ambiguity")
            .unwrap()
            .message;
        assert!(
            msg.contains("Distribution"),
            "message must name Distribution: {}",
            msg
        );
        assert!(msg.contains("Claim"), "message must name Claim: {}", msg);
    }

    #[test]
    fn cross_adt_field_ambiguity_silent_on_single_adt() {
        // Lending's exact shape: two ADTs but no overlapping field names.
        // Cross-ADT lint must stay silent. (We don't try lending itself
        // because the parser needs proper headers; use a synthetic two-ADT
        // spec with disjoint fields.)
        let src = r#"spec Lending

    type Pool
      | Uninitialized
      | Active of {
          authority      : Pubkey,
          total_deposits : U64,
        }

    type Loan
      | Empty
      | Active of {
          borrower : Pubkey,
          amount   : U64,
        }

    property pool_nonneg :
      state.total_deposits >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_cross_adt_field_ambiguity(&spec);
        assert!(
            warnings.is_empty(),
            "no overlapping fields → no lint, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn cross_adt_field_ambiguity_silent_when_qualified() {
        // Same shape as the positive-case fixture, but the property
        // qualifies the reference as `distribution.balance`. The lint
        // must NOT fire — the user has already disambiguated.
        let src = r#"spec Pair

    type Distribution
      | Empty
      | Active of {
          authority : Pubkey,
          balance   : U64,
        }

    type Claim
      | Empty
      | Active of {
          claimant : Pubkey,
          balance  : U64,
        }

    property positive_balance :
      distribution.balance >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_cross_adt_field_ambiguity(&spec);
        assert!(
            warnings.is_empty(),
            "qualified `distribution.balance` should clear the ambiguity, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    // ========================================================================
    // old_in_single_state_context lint
    // ========================================================================

    const OLD_SSC_SPEC_HEAD: &str = r#"
    spec OldSscTest
    program_id "11111111111111111111111111111111"

    type State
      | Active of { balance : U64 }

    type Error
      | E
      | BadGuard
    "#;

    #[test]
    fn old_ssc_lint_fires_on_old_in_requires() {
        // `old(...)` inside a `requires` body — category error, P1.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
    handler tweak (delta : U64) : State.Active -> State.Active {
      permissionless
      requires state.balance >= old(state.balance) else BadGuard
      effect { balance := balance + delta }
    }
    "#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "old_in_single_state_context"),
            "expected lint to fire on old() inside requires; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>(),
        );
        let w = &warnings[0];
        assert_eq!(w.severity, Severity::Warning);
        assert_eq!(w.priority, 1);
        assert!(w.message.contains("requires"), "msg: {}", w.message);
    }

    #[test]
    fn old_ssc_lint_fires_on_old_in_invariant() {
        // `old(...)` inside an `invariant` body — category error, P1.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
    invariant balance_nondec : state.balance >= old(state.balance)

    handler tweak (delta : U64) : State.Active -> State.Active {
      permissionless
      effect { balance := balance + delta }
    }
    "#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "old_in_single_state_context" && w.message.contains("invariant")),
            "expected lint to fire on old() inside invariant; got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.message))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn old_ssc_lint_silent_on_clean_requires() {
        // `requires` without `old(...)` — silent, no false positive.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
    handler tweak (delta : U64) : State.Active -> State.Active {
      permissionless
      requires delta > 0 else BadGuard
      effect { balance := balance + delta }
    }
    "#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings.is_empty(),
            "clean requires must not fire the lint; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn old_ssc_lint_silent_on_old_in_ensures() {
        // `old(...)` inside `ensures` — the right context, must NOT fire.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
    handler tweak (delta : U64) : State.Active -> State.Active {
      permissionless
      effect { balance := balance + delta }
      ensures state.balance >= old(state.balance)
    }
    "#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings.is_empty(),
            "old() in ensures must not fire the lint; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn old_ssc_lint_silent_on_old_in_property() {
        // `old(...)` inside a `property` body — the right context, must
        // NOT fire.
        let src = format!(
            "{}{}",
            OLD_SSC_SPEC_HEAD,
            r#"
    handler tweak (delta : U64) : State.Active -> State.Active {
      permissionless
      effect { balance := balance + delta }
    }

    property balance_monotonic :
      state.balance >= old(state.balance)
      preserved_by all
    "#
        );
        let spec = crate::chumsky_adapter::parse_str(&src).expect("parse");
        let warnings = check_old_in_single_state_context(&spec);
        assert!(
            warnings.is_empty(),
            "old() in property body must not fire the single-state lint; got: {:?}",
            warnings.iter().map(|w| &w.message).collect::<Vec<_>>(),
        );
    }

    // ========================================================================
    // adt_state_missing_wrong_state lint
    // ========================================================================

    /// `pragma state_repr = adt` selects the inductive representation,
    /// whose variant-mismatch fallthrough returns `Err(WrongState)`.
    /// Declaring the pragma without the error variant would emit
    /// non-compiling Rust, so `check` surfaces it. Without the pragma the
    /// same spec lowers flat and the lint stays silent.
    #[test]
    fn adt_state_pragma_without_wrong_state_fires() {
        let body = r#"
    program_id "11111111111111111111111111111111"

    type State
      | Uninitialized
      | Active of { balance : U64 }
      | Closed

    type Error
      | InvalidAmount

    handler open (amount : U64) : State.Uninitialized -> State.Active {
      auth owner
      accounts { owner : signer, writable }
      requires amount > 0 else InvalidAmount
    }"#;

        // pragma set, no WrongState → fires
        let adt = crate::chumsky_adapter::parse_str(&format!(
            "spec Adt\npragma state_repr = adt\n{body}"
        ))
        .expect("parse adt");
        let w = check_completeness(&adt);
        let hit = w
            .iter()
            .find(|w| w.rule == "adt_state_missing_wrong_state")
            .unwrap_or_else(|| {
                panic!(
                    "lint must fire; got: {:?}",
                    w.iter().map(|w| &w.rule).collect::<Vec<_>>()
                )
            });
        assert_eq!(hit.severity, Severity::Warning);
        assert_eq!(hit.priority, 2);

        // no pragma (flat) → silent even without WrongState
        let flat =
            crate::chumsky_adapter::parse_str(&format!("spec Flat\n{body}")).expect("parse flat");
        assert!(
            !check_completeness(&flat)
                .iter()
                .any(|w| w.rule == "adt_state_missing_wrong_state"),
            "flat specs don't need WrongState; lint must stay silent"
        );
    }

    #[test]
    fn test_missing_effect_fires() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "amount > 0".to_string(),
            ..Default::default()
        });
        // has lifecycle (pre/post set via make_handler) but no effect
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| w.rule == "missing_effect"),
            "expected missing_effect, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// `call X.handler(...)`, `transfers { … }`, or `modifies [...]` all
    /// count as effect-satisfying — the lint must not fire on CPI-only
    /// handlers where state writes are the wrong abstraction.
    #[test]
    fn test_missing_effect_skips_when_handler_has_only_calls() {
        let mut h = make_handler("init_mint");
        h.takes_params = vec![("decimals".to_string(), "U64".to_string())];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "decimals > 0".to_string(),
            ..Default::default()
        });
        h.calls = vec![ParsedCall {
            target_interface: "Token".to_string(),
            target_handler: "initialize_mint".to_string(),
            args: vec![],
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
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "missing_effect should not fire when handler has CPI calls; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    /// `modifies [field, ...]` is the frame-condition shape for handlers
    /// whose writes the spec doesn't model further — must satisfy the lint.
    #[test]
    fn test_missing_effect_skips_when_handler_has_modifies() {
        let mut h = make_handler("opaque_update");
        h.takes_params = vec![("payload".to_string(), "U64".to_string())];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "payload > 0".to_string(),
            ..Default::default()
        });
        h.modifies = Some(vec!["balance".to_string()]);
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "missing_effect should not fire when handler declares `modifies`; got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_missing_effect_skips_when_effect_exists() {
        let mut h = make_handler("deposit");
        h.takes_params = vec![("amount".to_string(), "U64".to_string())];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "amount > 0".to_string(),
            ..Default::default()
        });
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings.iter().any(|w| w.rule == "missing_effect"),
            "should not fire when effect exists"
        );
    }

    #[test]
    fn test_missing_effect_uses_on_account_fields() {
        let mut h = make_handler("borrow");
        h.on_account = Some("Loan".to_string());
        h.takes_params = vec![("loan_amount".to_string(), "U64".to_string())];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "loan_amount > 0".to_string(),
            ..Default::default()
        });
        h.pre_status = Some("Empty".to_string());
        h.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            handlers: vec![h],
            account_types: vec![
                ParsedAccountType {
                    name: "Pool".to_string(),
                    fields: vec![("total_deposits".to_string(), "U64".to_string())],
                    lifecycle: vec!["Active".to_string()],
                    pda_ref: None,
                    variants: vec![],
                },
                ParsedAccountType {
                    name: "Loan".to_string(),
                    fields: vec![("loan_amount".to_string(), "U64".to_string())],
                    lifecycle: vec!["Empty".to_string(), "Active".to_string()],
                    pda_ref: None,
                    variants: vec![],
                },
            ],
            state_fields: vec![("total_deposits".to_string(), "U64".to_string())],
            lifecycle_states: vec!["Empty".to_string(), "Active".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        let warning = warnings
            .iter()
            .find(|w| w.rule == "missing_effect")
            .expect("expected missing_effect warning");
        let example = warning
            .example
            .as_deref()
            .expect("missing_effect should include example");
        assert!(
            example.contains("loan_amount += loan_amount"),
            "expected account-aware suggestion, got: {}",
            example
        );
        assert!(
            !example.contains("total_deposits"),
            "should not use fields from a different account type: {}",
            example
        );
    }

    #[test]
    fn test_lifecycle_unreachable_state() {
        let mut h = make_handler("initialize");
        h.pre_status = Some("Uninitialized".to_string());
        h.post_status = Some("Active".to_string());
        let spec = ParsedSpec {
            handlers: vec![h],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Closed".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "lifecycle_unreachable_state"
                    && w.subject.as_deref() == Some("Closed")),
            "expected lifecycle_unreachable_state for Closed, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_lifecycle_disconnected_subgraph_is_unreachable() {
        let mut init = make_handler("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let mut close = make_handler("close");
        close.pre_status = Some("Frozen".to_string());
        close.post_status = Some("Closed".to_string());

        let spec = ParsedSpec {
            handlers: vec![init, close],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Frozen".to_string(),
                "Closed".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Frozen")
            }),
            "expected disconnected state Frozen to be unreachable, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
        assert!(
            warnings.iter().any(|w| {
                w.rule == "lifecycle_unreachable_state" && w.subject.as_deref() == Some("Closed")
            }),
            "expected downstream state Closed to be unreachable, got: {:?}",
            warnings
                .iter()
                .map(|w| (&w.rule, &w.subject))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_global_initial_state_seeded_when_account_lifecycle_differs() {
        // Account lifecycle starts at "Active", but the global initial state
        // is "Uninitialized". Without always seeding the global initial state,
        // "Uninitialized" would be flagged as unreachable even though it is
        // the entry point of the lifecycle.
        let mut init = make_handler("initialize");
        init.pre_status = Some("Uninitialized".to_string());
        init.post_status = Some("Active".to_string());

        let spec = ParsedSpec {
            handlers: vec![init],
            account_types: vec![ParsedAccountType {
                name: "Pool".to_string(),
                fields: vec![],
                lifecycle: vec!["Active".to_string(), "Frozen".to_string()],
                pda_ref: None,
                variants: vec![],
            }],
            lifecycle_states: vec![
                "Uninitialized".to_string(),
                "Active".to_string(),
                "Frozen".to_string(),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
                !warnings.iter().any(|w| {
                    w.rule == "lifecycle_unreachable_state"
                        && w.subject.as_deref() == Some("Uninitialized")
                }),
                "Uninitialized is the global initial state and should NOT be flagged as unreachable, got: {:?}",
                warnings
                    .iter()
                    .filter(|w| w.rule == "lifecycle_unreachable_state")
                    .map(|w| &w.subject)
                    .collect::<Vec<_>>()
            );
    }

    #[test]
    fn test_write_without_read_lint() {
        let mut h = make_handler("deposit");
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "amount > 0".to_string(),
            ..Default::default()
        });
        h.effects = vec![
            ParsedEffect::from_triple("balance", "add", "amount"),
            ParsedEffect::from_triple("counter", "add", "1"),
        ];
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("authority".into(), "Pubkey".into()),
                ("balance".into(), "U64".into()),
                ("counter".into(), "U64".into()),
            ],
            properties: vec![ParsedProperty {
                name: "conservation".to_string(),
                expression: Some("s.balance >= 0".to_string()),
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
        // "counter" is written but never read in any guard or property
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("counter")),
            "expected write_without_read for 'counter', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .map(|w| &w.subject)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_circular_lifecycle_no_terminal() {
        let mut h1 = make_handler("advance");
        h1.pre_status = Some("A".to_string());
        h1.post_status = Some("B".to_string());
        let mut h2 = make_handler("retreat");
        h2.pre_status = Some("B".to_string());
        h2.post_status = Some("A".to_string());
        let spec = ParsedSpec {
            handlers: vec![h1, h2],
            lifecycle_states: vec!["A".to_string(), "B".to_string()],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            warnings
                .iter()
                .any(|w| w.rule == "circular_lifecycle_no_terminal"),
            "expected circular_lifecycle_no_terminal, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
    }

    // ---- write_without_read word-boundary tests ----

    #[test]
    fn test_write_without_read_no_substring_match() {
        // Field "id" written in effects, guard only has "valid" — should NOT count as read
        let mut h = make_handler("update");
        h.effects = vec![ParsedEffect::from_triple("id", "set", "1")];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "valid > 0".to_string(),
            ..Default::default()
        });
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![
                ("id".to_string(), "U64".to_string()),
                ("valid".to_string(), "U64".to_string()),
            ],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
                warnings
                    .iter()
                    .any(|w| w.rule == "write_without_read"
                        && w.subject.as_deref() == Some("id")),
                "field 'id' should be flagged as write_without_read when guard only contains 'valid', got: {:?}",
                warnings.iter().filter(|w| w.rule == "write_without_read").collect::<Vec<_>>()
            );
    }

    #[test]
    fn test_write_without_read_bare_word_match() {
        // Field "balance" written in effects, guard has "balance > 0" — should count as read
        let mut h = make_handler("deposit");
        h.effects = vec![ParsedEffect::from_triple("balance", "add", "amount")];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "balance > 0".to_string(),
            ..Default::default()
        });
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("balance".to_string(), "U64".to_string())],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("balance")),
            "field 'balance' should NOT be flagged when guard contains bare word 'balance', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_write_without_read_prefixed_match() {
        // Field "id" written, guard has "state.id > 0" — should count as read
        let mut h = make_handler("update");
        h.effects = vec![ParsedEffect::from_triple("id", "set", "1")];
        h.requires.push(crate::check::ParsedRequires {
            lean_expr: "state.id > 0".to_string(),
            ..Default::default()
        });
        let spec = ParsedSpec {
            handlers: vec![h],
            state_fields: vec![("id".to_string(), "U64".to_string())],
            ..empty_spec()
        };
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "write_without_read" && w.subject.as_deref() == Some("id")),
            "field 'id' should NOT be flagged when guard contains 'state.id', got: {:?}",
            warnings
                .iter()
                .filter(|w| w.rule == "write_without_read")
                .collect::<Vec<_>>()
        );
    }

    // ── P7: undeclared_state_field_in_effect ──────────────────────────────

    #[test]
    fn p7_fires_on_lhs_undeclared_field() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec P7Lhs
    type State | Active of { balance : U64 }
    handler bump : State.Active -> State.Active {
      permissionless
      effect { undeclared += 1 }
    }
    "#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "undeclared_state_field_in_effect")
            .collect();
        assert!(
            hits.iter()
                .any(|w| w.message.contains("LHS") && w.message.contains("'undeclared'")),
            "expected LHS hit naming `undeclared`; got: {hits:#?}"
        );
    }

    #[test]
    fn p7_fires_on_rhs_undeclared_state_reference() {
        // RHS check catches `state.<field>` references inside complex
        // expressions. A bare `state.X` RHS goes through render_effect's
        // path-stripping shortcut (it ends up as just `X`), which is
        // indistinguishable from a param reference at lint time — that
        // case is caught downstream by codegen unless the user wrote
        // any composition. We pin the composition case here.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec P7Rhs
    type State | Active of { balance : U64 }
    handler bump : State.Active -> State.Active {
      permissionless
      effect { balance := state.missing + 1 }
    }
    "#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        let hits: Vec<_> = warnings
            .iter()
            .filter(|w| w.rule == "undeclared_state_field_in_effect")
            .collect();
        assert!(
            hits.iter()
                .any(|w| w.message.contains("RHS") && w.message.contains("'missing'")),
            "expected RHS hit naming `missing`; got: {hits:#?}"
        );
    }

    #[test]
    fn p7_silent_when_all_fields_declared() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec P7Clean
    type State | Active of { balance : U64, total : U64 }
    handler add : State.Active -> State.Active {
      permissionless
      effect { total := state.balance }
    }
    "#,
        )
        .expect("fixture should parse");
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "undeclared_state_field_in_effect"),
            "clean spec must not fire P7, got: {warnings:#?}"
        );
    }

    #[test]
    fn p7_does_not_fire_on_state_variant_promotion() {
        // `state := .Variant { ... }` is the documented variant-promotion /
        // whole-state-assignment form; P7 must not strip the LHS root and
        // flag `state` as an undeclared field.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Lifecycle
    program_id "11111111111111111111111111111111"
    type State
      | Setup of { x : U64 }
      | Active of { x : U64 }
    type Error | E

    handler activate : State.Setup -> State.Active {
      permissionless
      effect {
        state := .Active { x := 0 }
      }
    }
    "#,
        )
        .expect("variant-promotion spec must parse");
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "undeclared_state_field_in_effect"),
            "P7 must not fire on `state := .Variant {{...}}`; got: {warnings:#?}"
        );
    }

    #[test]
    fn p7_ignores_synthetic_match_arm_handlers() {
        // `_case_N` / `_otherwise` synthetic handlers inherit their
        // parent's effects — they don't get a second P7 hit because
        // the parent already covers it.
        let mut spec = ParsedSpec::default();
        spec.account_types.push(ParsedAccountType {
            name: "State".into(),
            fields: vec![("balance".into(), "U64".into())],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        });
        spec.handlers.push(ParsedHandler {
            name: "outer_case_0".into(),
            permissionless: true,
            effects: vec![ParsedEffect::from_triple("undeclared", "set", "0")],
            ..synthetic_handler_default("outer_case_0")
        });
        let warnings = check_completeness(&spec);
        assert!(
            !warnings
                .iter()
                .any(|w| w.rule == "undeclared_state_field_in_effect"),
            "P7 must not fire on `_case_N` synthetic handlers: {warnings:#?}"
        );
    }

    fn synthetic_handler_default(name: &str) -> ParsedHandler {
        ParsedHandler {
            name: name.into(),
            ..Default::default()
        }
    }

    // ========================================================================
    // ParsedAccountType.variants populated for multi-variant ADTs
    // ========================================================================

    #[test]
    fn multi_variant_adt_populates_account_variants() {
        // Two-variant state ADT. Flat `fields` view stays the union (first
        // occurrence wins); `variants` carries the per-variant shape so
        // codegen can emit `pub enum State { Setup{...}, Active{...} }`.
        let src = r#"spec Multi
    program_id "11111111111111111111111111111111"

    type State
      | Setup of { owner : Pubkey }
      | Active of {
          owner : Pubkey,
          pool  : U64,
        }

    property pool_nonneg :
      state.pool >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let state = spec
            .account_types
            .iter()
            .find(|a| a.name == "State")
            .expect("state account type present");

        assert_eq!(
            state.variants.len(),
            2,
            "two-variant ADT should produce two ParsedVariant entries"
        );
        assert_eq!(state.variants[0].name, "Setup");
        assert_eq!(state.variants[1].name, "Active");
        assert_eq!(state.variants[0].fields.len(), 1);
        assert_eq!(state.variants[1].fields.len(), 2);
        // Flat view stays populated as the union (back-compat).
        assert!(state.fields.iter().any(|(n, _)| n == "owner"));
        assert!(state.fields.iter().any(|(n, _)| n == "pool"));
    }

    #[test]
    fn no_payload_variant_keeps_empty_field_list() {
        // A unit-style variant (no payload) should still appear in
        // `variants` with an empty field list so codegen can emit
        // `pub enum State { Inactive, Active{...} }`.
        let src = r#"spec NoPayload
    program_id "11111111111111111111111111111111"

    type State
      | Inactive
      | Active of { pool : U64 }

    property pool_nonneg :
      state.pool >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let state = spec
            .account_types
            .iter()
            .find(|a| a.name == "State")
            .expect("state account type present");
        assert_eq!(state.variants.len(), 2);
        let inactive = state
            .variants
            .iter()
            .find(|v| v.name == "Inactive")
            .expect("unit variant retained");
        assert!(
            inactive.fields.is_empty(),
            "no-payload variant has zero fields"
        );
    }

    // ========================================================================
    // Variant-prefixed effect LHS doesn't false-positive lints
    // ========================================================================

    #[test]
    fn variant_prefixed_lhs_passes_all_effect_lints() {
        // `Active.pool := amount` on a multi-variant ADT state must NOT
        // trigger undeclared_state_field_in_effect (P7 LHS),
        // write_without_read (Rule 13), or unused_field (Rule 4) — all
        // three walk the LHS string and must not treat the variant prefix
        // as a field name.
        let src = r#"spec MultiVar
    program_id "11111111111111111111111111111111"

    type State
      | Setup of { owner : Pubkey }
      | Active of {
          owner : Pubkey,
          pool  : U64,
        }

    type Error
      | MathOverflow

    handler activate (amount : U64) : State.Setup -> State.Active {
      auth owner
      requires amount > 0
      effect {
        Active.pool := amount
      }
    }

    property pool_nonneg :
      state.pool >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_completeness(&spec);
        let rules: Vec<&str> = warnings.iter().map(|w| w.rule.as_str()).collect();

        assert!(
                !rules.contains(&"undeclared_state_field_in_effect"),
                "P7 should not fire on `Active.pool := amount` (Active is a variant, pool is its field) — got: {:?}",
                rules
            );
        assert!(
                !rules.contains(&"write_without_read"),
                "write_without_read should match `pool` (read by property) to `Active.pool` (written) — got: {:?}",
                rules
            );
        assert!(
            !rules.contains(&"unused_field"),
            "unused_field should see `pool` as modified via `Active.pool := amount` — got: {:?}",
            rules
        );
    }

    #[test]
    fn variant_prefixed_lhs_still_catches_unknown_field() {
        // A real bug: `Active.poool := amount` (typo). P7 should fire
        // with subject `activate.Active.poool` — the variant prefix is
        // legal, the field name behind it isn't declared anywhere.
        let src = r#"spec MultiVarTypo
    program_id "11111111111111111111111111111111"

    type State
      | Setup of { owner : Pubkey }
      | Active of {
          owner : Pubkey,
          pool  : U64,
        }

    type Error
      | MathOverflow

    handler activate (amount : U64) : State.Setup -> State.Active {
      auth owner
      requires amount > 0
      effect {
        Active.poool := amount
      }
    }

    property pool_nonneg :
      state.pool >= 0
      preserved_by all
    "#;
        let spec = crate::chumsky_adapter::parse_str(src).expect("parse");
        let warnings = check_completeness(&spec);
        let p7s: Vec<&CompletenessWarning> = warnings
            .iter()
            .filter(|w| w.rule == "undeclared_state_field_in_effect")
            .collect();
        assert_eq!(
            p7s.len(),
            1,
            "expected exactly one P7 hit on the misspelled `poool`, got: {:?}",
            warnings.iter().map(|w| &w.rule).collect::<Vec<_>>()
        );
        assert!(
            p7s[0].subject.as_deref().unwrap_or("").contains("poool"),
            "P7 subject should name the misspelled field, got: {:?}",
            p7s[0].subject
        );
    }
}
