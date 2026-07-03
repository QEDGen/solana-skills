use super::*;

fn fields_for_handler<'a>(spec: &'a ParsedSpec, handler: &ParsedHandler) -> &'a [(String, String)] {
    if let Some(account_name) = handler.on_account.as_deref() {
        if let Some(account) = spec
            .account_types
            .iter()
            .find(|acct| acct.name == account_name)
        {
            return &account.fields;
        }
    }
    &spec.state_fields
}

pub(crate) fn suggested_effect_lines(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
    is_init_like: bool,
) -> Vec<String> {
    handler
        .takes_params
        .iter()
        .map(|(name, _)| name.as_str())
        .take(3)
        .map(|param| {
            let matching_field = fields_for_handler(spec, handler)
                .iter()
                .find(|(field, _)| field.contains(param) || param.contains(field.as_str()));
            if let Some((field, _)) = matching_field {
                if is_init_like {
                    format!("    {} = {}", field, param)
                } else {
                    format!("    {} += {}", field, param)
                }
            } else if is_init_like {
                format!("    <field> = {}", param)
            } else {
                format!("    <field> += {}", param)
            }
        })
        .collect()
}

pub(crate) fn reachable_lifecycle_states(spec: &ParsedSpec) -> std::collections::HashSet<String> {
    let mut reachable: std::collections::HashSet<String> = spec
        .account_types
        .iter()
        .filter_map(|acct| acct.lifecycle.first().cloned())
        .collect();
    // Always include the global initial state — account-level lifecycles
    // may start at a later state (e.g. "Active") while the true entry
    // state (e.g. "Uninitialized") is only declared globally.
    if let Some(initial) = spec.lifecycle_states.first() {
        reachable.insert(initial.clone());
    }

    let mut changed = true;
    while changed {
        changed = false;
        for op in &spec.handlers {
            let next_state = match op.post_status.as_ref() {
                Some(post) => post,
                None => continue,
            };
            let can_reach = match op.pre_status.as_ref() {
                Some(pre) => reachable.contains(pre),
                None => true,
            };
            if can_reach && reachable.insert(next_state.clone()) {
                changed = true;
            }
        }
    }

    reachable
}

/// Look up the declared type of a field, checking the handler's target account
/// first, then falling back to the global state_fields.
pub(crate) fn find_field_type(
    spec: &ParsedSpec,
    op: &ParsedHandler,
    field: &str,
) -> Option<String> {
    if let Some(ref acct_name) = op.on_account {
        if let Some(acct) = spec.account_types.iter().find(|a| a.name == *acct_name) {
            if let Some((_, t)) = acct.fields.iter().find(|(n, _)| n == field) {
                return Some(t.clone());
            }
        }
    }
    spec.state_fields
        .iter()
        .find(|(n, _)| n == field)
        .map(|(_, t)| t.clone())
}

/// Detect the comparison operator and LHS/RHS in a property expression.
/// Returns (lhs_field, operator, rhs_ref) where rhs_ref is either a field name
/// or "__const" for constant comparisons (e.g., `s.V ≤ 10000`).
pub(crate) fn parse_property_relation<'a>(
    expr: &'a str,
    prop_fields: &[&'a str],
) -> Option<(&'a str, &'a str, &'a str)> {
    for op in &[" ≤ ", " ≥ ", " < ", " > ", " = "] {
        if let Some(pos) = expr.find(op) {
            let lhs = &expr[..pos];
            let rhs = &expr[pos + op.len()..];
            // Find which prop field is on each side. A transition property
            // (one referencing `old(...)`) renders the post-state as
            // `s'.<field>` and the `old(...)` side as `s.<field>`; match
            // both so the post side isn't misread as a constant.
            let side_field = |side: &str| {
                prop_fields.iter().find(|f| {
                    side.contains(&format!("s.{}", f)) || side.contains(&format!("s'.{}", f))
                })
            };
            let lhs_field = side_field(lhs);
            let rhs_field = side_field(rhs);
            match (lhs_field, rhs_field) {
                (Some(lf), Some(rf)) => return Some((lf, op.trim(), rf)),
                // Single field vs constant (e.g., s.V ≤ 10000000)
                (Some(lf), None) => return Some((lf, op.trim(), "__const")),
                (None, Some(rf)) => return Some(("__const", op.trim(), rf)),
                _ => {}
            }
        }
    }
    None
}

/// True iff any of the handler's `requires` clauses textually reference any
/// of the named property fields (as `state.<f>` or `s.<f>` with a word
/// boundary on the trailing side, so `state.x` doesn't match `state.xyz`).
///
/// Used by `preserved_by_all_potential_violation` to suppress boundary-only
/// false positives — when the spec author has bounded the relevant fields,
/// trust their claim of inductive preservation rather than firing a warning
/// the local effect-analyzer can't refute.
pub(crate) fn requires_constrains_prop_fields(op: &ParsedHandler, prop_fields: &[&str]) -> bool {
    for req in &op.requires {
        for expr in [&req.rust_expr, &req.lean_expr] {
            for field in prop_fields {
                for prefix in ["state.", "s."] {
                    let needle = format!("{}{}", prefix, field);
                    let mut search = expr.as_str();
                    while let Some(pos) = search.find(&needle) {
                        let after = search[pos + needle.len()..]
                            .chars()
                            .next()
                            .map(|c| !c.is_alphanumeric() && c != '_')
                            .unwrap_or(true);
                        if after {
                            return true;
                        }
                        search = &search[pos + needle.len()..];
                    }
                }
            }
        }
    }
    false
}

pub(crate) fn build_counterexample(
    expr: &str,
    prop_name: &str,
    prop_fields: &[&str],
    op: &ParsedHandler,
    modified_fields: &[&str],
    constants: &[(String, String)],
) -> Option<Counterexample> {
    let relation = parse_property_relation(expr, prop_fields);

    let effect_triples: Vec<(&str, &str, &str)> = op
        .effects
        .iter()
        .filter(|e| modified_fields.contains(&e.field.as_str()))
        .map(|e| (e.field.as_str(), e.op.as_str(), e.value.as_str()))
        .collect();

    if effect_triples.is_empty() {
        return None;
    }

    let (lhs, op_sym, rhs) = relation?;

    // Transition property: the post side renders `s'.<field>`, the `old(...)`
    // side `s.<field>` (frozen at the pre-state snapshot). Without per-side
    // frozen-ness detection the effect would land on whichever side's field
    // name matches first — inverting `counter ≥ old(counter)` into a bogus
    // violation.
    let is_transition = expr.contains("s'.");
    let (lhs_frozen, rhs_frozen) = if is_transition {
        let mut split = (false, false);
        for opv in &[" ≤ ", " ≥ ", " < ", " > ", " = "] {
            if let Some(pos) = expr.find(opv) {
                let lhs_raw = &expr[..pos];
                let rhs_raw = &expr[pos + opv.len()..];
                let frozen = |raw: &str, field: &str| {
                    field != "__const"
                        && raw.contains(&format!("s.{}", field))
                        && !raw.contains(&format!("s'.{}", field))
                };
                split = (frozen(lhs_raw, lhs), frozen(rhs_raw, rhs));
                break;
            }
        }
        split
    } else {
        (false, false)
    };
    // Display label: a frozen side is the `old(...)` snapshot.
    let label = |field: &str, frozen: bool| {
        if frozen {
            format!("old({})", field)
        } else {
            field.to_string()
        }
    };

    // Build a boundary pre-state where the invariant barely holds
    let (lhs_val, rhs_val): (i64, i64) = match op_sym {
        "≤" | "<=" => (3, 3),
        "≥" | ">=" => (3, 3),
        "<" => (2, 3),
        ">" => (3, 2),
        _ => (3, 3),
    };

    let mut pre_state = Vec::new();
    if lhs != "__const" {
        pre_state.push((label(lhs, lhs_frozen), lhs_val));
    }
    if rhs != "__const" {
        pre_state.push((label(rhs, rhs_frozen), rhs_val));
    }

    let pre_check = format!("{} {} {}", lhs_val, op_sym, rhs_val);

    let mut post_lhs = lhs_val;
    let mut post_rhs = rhs_val;
    let mut effects = Vec::new();
    for (field, kind, value) in &effect_triples {
        let v: i64 = value.parse().unwrap_or_else(|_| {
            constants
                .iter()
                .find(|(n, _)| n == value)
                .and_then(|(_, val)| val.parse().ok())
                .unwrap_or(1)
        });
        let desc = match *kind {
            "add" => format!("{} += {}", field, value),
            "sub" => format!("{} -= {}", field, value),
            "set" => format!("{} = {}", field, value),
            _ => continue,
        };
        effects.push(desc);
        // Effects mutate only the live (non-frozen) side; an `old(...)`
        // reference stays at its pre-state snapshot.
        if *field == lhs && !lhs_frozen {
            match *kind {
                "add" => post_lhs += v,
                "sub" => post_lhs -= v,
                "set" => post_lhs = v,
                _ => {}
            }
        }
        if *field == rhs && !rhs_frozen {
            match *kind {
                "add" => post_rhs += v,
                "sub" => post_rhs -= v,
                "set" => post_rhs = v,
                _ => {}
            }
        }
    }

    let mut post_state = Vec::new();
    if lhs != "__const" {
        post_state.push((label(lhs, lhs_frozen), post_lhs));
    }
    if rhs != "__const" {
        post_state.push((label(rhs, rhs_frozen), post_rhs));
    }

    let holds = match op_sym {
        "≤" | "<=" => post_lhs <= post_rhs,
        "≥" | ">=" => post_lhs >= post_rhs,
        "<" => post_lhs < post_rhs,
        ">" => post_lhs > post_rhs,
        _ => false,
    };

    let post_check = format!("{} {} {}", post_lhs, op_sym, post_rhs);

    Some(Counterexample {
        property: prop_name.to_string(),
        handler: op.name.clone(),
        pre_state,
        pre_check,
        effects,
        post_state,
        post_check,
        invariant_holds: holds,
    })
}

/// Build structured fix suggestions for a property preservation conflict.
pub(crate) fn build_fix_suggestions(
    expr: &str,
    prop_name: &str,
    op: &ParsedHandler,
    prop_fields: &[&str],
    modified_fields: &[&str],
) -> Vec<FixOption> {
    let relation = parse_property_relation(expr, prop_fields);
    let unmodified: Vec<&&str> = prop_fields
        .iter()
        .filter(|f| !modified_fields.contains(f))
        .collect();

    let mut fixes = Vec::new();

    // Fix A: add a guard that ensures the invariant holds after the effect.
    // Only meaningful when the two sides are distinct fields — a transition
    // property (`counter ≥ old(counter)`) has the same field on both sides,
    // where a `requires state.counter > state.counter` guard is nonsensical.
    if let Some((lhs, op_sym, rhs)) = relation.filter(|&(l, _, r)| l != r) {
        for eff in &op.effects {
            let (field, kind) = (&eff.field, &eff.op);
            if !modified_fields.contains(&field.as_str()) {
                continue;
            }
            if kind == "sub" {
                if field.as_str() == rhs && (op_sym == "≤" || op_sym == "<=") {
                    fixes.push(FixOption {
                        label: "Add guard".to_string(),
                        rationale: format!(
                            "{} subtracts from {} (RHS of ≤). A strict inequality guard ensures the invariant survives.",
                            op.name, rhs
                        ),
                        snippet: format!(
                            "handler {}\n  requires state.{} < state.{}",
                            op.name, lhs, rhs
                        ),
                    });
                } else if field.as_str() == lhs && (op_sym == "≥" || op_sym == ">=") {
                    fixes.push(FixOption {
                        label: "Add guard".to_string(),
                        rationale: format!(
                            "{} subtracts from {} (LHS of ≥). A strict inequality guard ensures the invariant survives.",
                            op.name, lhs
                        ),
                        snippet: format!(
                            "handler {}\n  requires state.{} > state.{}",
                            op.name, lhs, rhs
                        ),
                    });
                }
            }
        }
    }

    // Fix B: add the handler to preserved_by
    fixes.push(FixOption {
        label: "Add to preserved_by".to_string(),
        rationale: format!(
            "Include '{}' in the property's preserved_by list. Requires a guard (option above) to make the proof go through.",
            op.name
        ),
        snippet: format!(
            "property {} {{\n  preserved_by [..., {}]\n}}",
            prop_name, op.name
        ),
    });

    // Fix C: add a compensating effect
    if let Some(unmod) = unmodified.first() {
        fixes.push(FixOption {
            label: "Add compensating effect".to_string(),
            rationale: format!(
                "Adjust '{}' alongside the modified field(s) to maintain the invariant.",
                unmod
            ),
            snippet: format!(
                "handler {}\n  effect {{ {} = <adjusted_value> }}",
                op.name, unmod
            ),
        });
    }

    fixes
}
