use super::*;

/// Lean literal for the type's default value, used when a variant doesn't
/// carry a field referenced through an accessor.
pub(super) fn ty_default_literal(ty: &crate::mir::Ty) -> &'static str {
    use crate::mir::Ty;
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => "0",
        Ty::I64 | Ty::I128 => "0",
        Ty::Bool => "false",
        _ => "default",
    }
}

/// Lean literal for the type's maximum value, used to synthesize overflow
/// bound checks on `Stmt::CheckedAdd` sites. `None` for non-numeric /
/// signed types.
pub(super) fn ty_max_const(ty: &crate::mir::Ty) -> Option<&'static str> {
    use crate::mir::Ty;
    match ty {
        Ty::U8 => Some("255"),
        Ty::U16 => Some("65535"),
        Ty::U32 => Some("4294967295"),
        Ty::U64 => Some("18446744073709551615"),
        Ty::U128 => Some("340282366920938463463374607431768211455"),
        _ => None,
    }
}

/// Strip a leading `Variant.` prefix from a `Path` so an effect like
/// `Open.pool_balance := initial` resolves to the bare field name for
/// variant-arm binding.
pub(super) fn strip_variant_prefix(path: &crate::mir::Path, mir: &Mir) -> String {
    if path.segments.len() >= 2 {
        let head = &path.segments[0];
        let is_variant = mir.state.variants.iter().any(|v| &v.tag == head);
        if is_variant {
            return path.segments[1..].join(".");
        }
    }
    path.segments.join(".")
}

/// Render an effect RHS for the indexed-state transition body. Bare
/// numeric literals and bare param refs pass through; pre-rendered Lean
/// compounds (starting with `s.`, `(`, `match`, etc.) get subscript
/// rewriting only; bare field names take an `s.` prefix + rewriting.
pub(super) fn effect_value_to_lean_mir(
    value: &str,
    params: &[(crate::mir::Symbol, crate::mir::Ty)],
) -> String {
    let trimmed = value.trim();
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '_' || c == '-')
    {
        return trimmed.replace('_', "");
    }
    let is_bare_ident = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if is_bare_ident && params.iter().any(|(n, _)| n == trimmed) {
        return trimmed.to_string();
    }
    let looks_prerendered = trimmed.starts_with("s.")
        || trimmed.starts_with("s'.")
        || trimmed.starts_with('(')
        || trimmed.contains("match ")
        || trimmed.contains("=> ")
        || trimmed.contains(" with ")
        || trimmed.contains(".{");
    if looks_prerendered {
        return rewrite_subscripts_lean(trimmed);
    }
    let first = trimmed.chars().next().unwrap_or('_');
    let prefixed = if first.is_ascii_alphabetic() || first == '_' {
        format!("s.{}", trimmed)
    } else {
        trimmed.to_string()
    };
    rewrite_subscripts_lean(&prefixed)
}

/// Map a DSL type-string to its Lean form (scalar cases only). Unknown
/// forms — including compounds like `Map[N] T`, `Fin[N]` — pass through
/// unchanged; add compound support when a fixture demands it.
pub(super) fn map_dsl_ty(s: &str) -> String {
    match s.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat".to_string(),
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int".to_string(),
        other => other.to_string(),
    }
}

/// Union of (field-name, type) pairs across every state variant —
/// matches the flat-state `emit_state_struct` projection. Used by
/// `emit_handler_transition` to look up field types for the auth
/// gate and the auto overflow/underflow guards.
pub(super) fn flat_state_fields(mir: &Mir) -> Vec<(String, crate::mir::Ty)> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<(String, crate::mir::Ty)> = Vec::new();
    for v in &mir.state.variants {
        for f in &v.fields {
            if seen.insert(f.name.clone()) {
                out.push((f.name.clone(), f.ty.clone()));
            }
        }
    }
    out
}

/// Scan `s.<ident>` occurrences and return each unique field name.
pub(super) fn fields_referenced_in_expr_owned(expr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, _) in expr.match_indices("s.") {
        let rest = &expr[i + 2..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if end == 0 {
            continue;
        }
        let field = rest[..end].to_string();
        if !out.contains(&field) {
            out.push(field);
        }
    }
    out
}

/// If `expr` starts with `∀ s : T,` or `forall s : T,`, strip the
/// quantifier prefix and return the body — the surrounding `def
/// <prop> (s : State)` already binds `s`. Other quantified bodies
/// (value binders) pass through unchanged.
pub(super) fn strip_state_forall(expr: &str) -> String {
    let trimmed = expr.trim();
    let rest = trimmed
        .strip_prefix('\u{2200}')
        .or_else(|| trimmed.strip_prefix("forall"));
    if let Some(rest) = rest {
        let rest_trim = rest.trim_start();
        // Only strip if the quantified binder is literally `s`.
        if rest_trim.starts_with("s ") || rest_trim.starts_with("s:") {
            if let Some(comma_pos) = rest.find(',') {
                return rest[comma_pos + 1..].trim().to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Prefix every state-field identifier in `expr` with `s.`. Word-boundary
/// regex avoids touching substrings of other identifiers (e.g., `amount`
/// shouldn't become `s.amount` inside `taker_amount`). Skips fields
/// already prefixed.
pub(super) fn prefix_state_fields(
    expr: &str,
    fields: &std::collections::HashSet<String>,
) -> String {
    let mut out = expr.to_string();
    for field in fields {
        let pattern = format!(r"\b{}\b", regex::escape(field));
        let re = regex::Regex::new(&pattern).expect("regex compiles for state-field name");
        let replacement = format!("s.{}", field);
        // Re-passes can't double-prefix: `\b` doesn't match after `.`.
        out = re
            .replace_all(&out, regex::NoExpand(&replacement))
            .into_owned();
    }
    out
}

/// Count top-level `∧` conjuncts in a Lean expression, respecting
/// parenthesis nesting (`(a ∧ b) ∧ c` returns 2, not 3).
pub(super) fn count_top_level_conjuncts(expr: &str) -> usize {
    let mut depth: i32 = 0;
    let mut count = 0usize;
    for ch in expr.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            '\u{2227}' if depth == 0 => count += 1, // ∧
            _ => {}
        }
    }
    count + 1
}

/// Projection path into a right-associative `∧` chain.
pub(super) fn conjunction_projection(flat_index: usize, total_atoms: usize) -> String {
    let mut path = String::from("hg");
    for _ in 0..flat_index {
        path.push_str(".2");
    }
    if flat_index < total_atoms - 1 {
        path.push_str(".1");
    }
    path
}

/// Detect whether an expression references a handler-account's `.pubkey`
/// or `.key()`. Account-binding pubkey refs aren't in Lean scope; theorems
/// mentioning them would carry free identifiers.
pub(super) fn mentions_handler_account_pubkey(
    expr: &str,
    accounts: &[crate::mir::AccountBindingShape],
) -> bool {
    accounts.iter().any(|a| {
        let needle_pubkey = format!("{}.pubkey", a.name);
        let needle_key = format!("{}.key()", a.name);
        expr.contains(&needle_pubkey) || expr.contains(&needle_key)
    })
}

/// Build the call-side argument string for transition function
/// invocations: `" p1 p2 ..."`. Empty when `params` is empty.
pub(super) fn param_args_str(params: &[(crate::mir::Symbol, crate::mir::Ty)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    format!(
        " {}",
        params
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// Render a MIR `Ty` to its Lean form — unsigned numerics widen to `Nat`
/// (proofs run in Nat), signed to `Int`; Pubkey is an opaque abbreviation.
pub(super) fn render_ty(ty: &crate::mir::Ty) -> String {
    use crate::mir::Ty;
    match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 => "Nat".to_string(),
        Ty::I64 | Ty::I128 => "Int".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Pubkey => "Pubkey".to_string(),
        Ty::Custom(name) => name.clone(),
        Ty::Map { capacity: _, value } => {
            // Indexed-state has its own renderer; this codepath shouldn't
            // fire for single-account specs.
            format!("Map /* {} */", render_ty(value))
        }
    }
}

/// Build a parameter signature string for transition function
/// declarations: `" (p1 : T1) (p2 : T2) ..."`; empty when `params` is.
pub(super) fn param_sig_str(params: &[(crate::mir::Symbol, crate::mir::Ty)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(|(n, t)| format!(" ({} : {})", n, render_ty(t)))
        .collect::<Vec<_>>()
        .join("")
}

/// Extract the auth-account name for the alias-let, if any. `None` for
/// permissionless handlers and dotted-auth shapes (desugared upstream
/// into a synthetic `requires` clause — no separate alias needed).
pub(super) fn handler_auth_name(h: &crate::mir::HandlerMir) -> Option<crate::mir::Symbol> {
    use crate::mir::{AccountOrField, AccountRef};
    match &h.auth {
        Some(AccountOrField::Account(AccountRef::ByBinding(name))) => Some(name.clone()),
        _ => None,
    }
}

/// Trailing segment of a Path — dotted paths collapse to the last
/// segment on the flat-state path.
pub(super) fn path_field_name(path: &crate::mir::Path) -> String {
    path.segments
        .last()
        .cloned()
        .unwrap_or_else(|| "?".to_string())
}

/// True iff the RHS is exactly `<identifier>.pubkey` — account-binding
/// pubkey refs have no Lean scope and are dropped from record updates.
pub(super) fn is_account_pubkey_ref(rust: &str) -> bool {
    let trimmed = rust.trim();
    trimmed
        .strip_suffix(".pubkey")
        .map(|head| !head.is_empty() && head.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .unwrap_or(false)
}

/// Wrap an expression in parens if it contains low-precedence operators
/// that would re-group when joined under `∧` — defensive parens at concat
/// sites (divergence class C3 in `docs/design/codegen-divergence.md`).
pub(super) fn paren_low_prec(expr: &str) -> String {
    let trimmed = expr.trim();
    // Already-parenthesized at the top level: leave alone.
    if trimmed.starts_with('(') && trimmed.ends_with(')') {
        // Check the parens actually match (could be `(a) ∧ (b)`).
        let mut depth = 0i32;
        let mut top_level_seen = false;
        for c in trimmed.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {
                    if depth == 0 {
                        top_level_seen = true;
                        break;
                    }
                }
            }
        }
        if !top_level_seen {
            return trimmed.to_string();
        }
    }
    // Look for low-precedence ops (or / and) at the top level.
    if has_top_level_op(trimmed, &[" or ", " ∨ ", " || "]) {
        format!("({})", trimmed)
    } else {
        trimmed.to_string()
    }
}

pub(super) fn has_top_level_op(expr: &str, ops: &[&str]) -> bool {
    let mut depth = 0i32;
    for (i, c) in expr.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth == 0 => {
                for op in ops {
                    if expr[i..].starts_with(op) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Quote Lean reserved names as `«name»`. Extend as fixtures surface
/// collisions; keep in sync with `lean_sidecars::safe_name`.
pub(super) fn safe_name(name: &str) -> String {
    // Reserved words that collide with common qedspec identifiers
    // (notably `initialize`).
    const LEAN_RESERVED: &[&str] = &[
        "open",
        "close",
        "initialize",
        "import",
        "namespace",
        "end",
        "where",
        "with",
        "do",
        "let",
        "if",
        "then",
        "else",
        "match",
        "return",
        "in",
        "for",
    ];
    if LEAN_RESERVED.contains(&name) {
        format!("\u{00AB}{}\u{00BB}", name)
    } else {
        name.to_string()
    }
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------
