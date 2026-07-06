//! Cross-cutting helpers shared across the lint rules: word-boundary
//! matching, comparison parsing, field-type classification, and the
//! overflow-risk / `old(...)` predicates several rules build on.

use super::*;

/// Shorthand builder for lint warnings — see `CompletenessWarning::new`.
pub(crate) fn warn(
    rule: &str,
    severity: Severity,
    priority: u8,
    message: impl Into<String>,
) -> CompletenessWarning {
    CompletenessWarning::new(rule, severity, priority, message)
}

/// Shared per-spec context threaded through the completeness rules that
/// need more than the bare `ParsedSpec`: the signer hint for auth
/// suggestions and the variant index every effect-LHS lint uses to
/// normalize `Variant.field` paths.
pub(crate) struct LintCtx<'a> {
    pub(crate) spec: &'a ParsedSpec,
    /// A likely signer field name from state (first Pubkey field).
    pub(crate) signer_hint: &'a str,
    /// Variant index for `Variant.field` LHS normalization, shared by every
    /// effect-LHS lint so the variant prefix is stripped before comparing
    /// against bare field names. Maps variant name → its payload fields;
    /// empty when no account type has variants.
    pub(crate) variant_fields:
        std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

impl<'a> LintCtx<'a> {
    pub(crate) fn new(spec: &'a ParsedSpec) -> Self {
        let signer_hint = spec
            .state_fields
            .iter()
            .find(|(_, t)| t == "Pubkey")
            .map(|(n, _)| n.as_str())
            .unwrap_or("authority");

        let mut variant_fields: std::collections::BTreeMap<
            String,
            std::collections::BTreeSet<String>,
        > = std::collections::BTreeMap::new();
        for acct in &spec.account_types {
            for variant in &acct.variants {
                let entry = variant_fields.entry(variant.name.clone()).or_default();
                for (fname, _) in &variant.fields {
                    entry.insert(fname.clone());
                }
            }
        }

        LintCtx {
            spec,
            signer_hint,
            variant_fields,
        }
    }

    /// Strip a leading `Variant.` prefix when it names a known variant:
    /// `Active.pool` → `pool`; `accounts[i].cap` / `pool` → unchanged.
    pub(crate) fn normalize_lhs(&self, lhs: &str) -> String {
        if let Some(dot) = lhs.find('.') {
            let head = &lhs[..dot];
            if self.variant_fields.contains_key(head) {
                return lhs[dot + 1..].to_string();
            }
        }
        lhs.to_string()
    }
}

/// Whole-word match: boundaries are start/end of string or any non-alphanumeric, non-underscore byte.
pub(super) fn contains_word(haystack: &str, needle: &str) -> bool {
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

/// Split a rendered Rust comparison `<lhs> <op> <rhs>` at the top-level
/// comparison operator (string-level, no AST). Top-level = not inside
/// parens, generic args (`Vec<...>`), or `[...]` indices; first depth-0
/// comparison wins, with `==`/`!=`/`<=`/`>=` matched before `<`/`>`.
/// `None` if the expression isn't a top-level comparison.
pub(super) fn parse_top_level_cmp(expr: &str) -> Option<(&str, &str, &str)> {
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

/// Parsed form of a field type string. Captures the distinction between a
/// plain type (e.g. `U128`, `Account`) and a bounded map (`Map[N] T`).
///
/// Only `Map { .. }` is inspected by the current consumer; `Simple` carries
/// the trimmed type string for future linting passes (e.g., primitive-type
/// checks, alias resolution) and intentionally remains exhaustive.
#[derive(Debug)]
pub(super) enum FieldTypeShape<'a> {
    Simple(#[allow(dead_code)] &'a str),
    Map { bound: &'a str, inner: &'a str },
}

/// Parse a field-type source string into a structured view.
/// Returns `Simple` for `U128`, `Account`, `Vec U64` and `Map { ... }` for
/// `Map[CONST] T` (bound and inner trimmed).
pub(super) fn classify_field_type(s: &str) -> FieldTypeShape<'_> {
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

pub(super) fn make_old_in_single_state_warning(
    holder: &str,
    kind: &str,
    body_snippet: &str,
) -> CompletenessWarning {
    warn(
        "old_in_single_state_context",
        Severity::Warning,
        1,
        format!(
            "'{}' uses `old(...)` inside a `{}` body ({}) — only meaningful in \
             `ensures` or `property` bodies (a binary transition context). \
             `requires` and `invariant` describe a single state and have no \
             \"old\" value to reference.",
            holder, kind, body_snippet
        ),
    )
    .subject(holder.to_string())
    .fix(
        "If you meant a precondition on the pre-state, drop `old(...)` \
              and reference `state.x` directly. If you meant a property across \
              the transition, lift the clause into a `property X : ... \
              preserved_by Y`.",
    )
}

/// Predicate shared with `kani_impl::spec_triggers_impl_harness`: true iff
/// a ref_impl carries arithmetic that could overflow on bounded Rust types
/// (the Lean lowering on `Nat`/`Int` cannot). Used as both a lint trigger
/// and the impl-targeted Kani auto-trigger so ref_impl-bearing specs always
/// get the bit-width-bounded verification surface.
pub(crate) fn ref_impl_has_overflow_risk(r: &ParsedRefImpl) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- contains_word unit tests ----

    #[test]
    fn test_contains_word_basic() {
        assert!(contains_word("balance > 0", "balance"));
        assert!(contains_word("check balance here", "balance"));
        assert!(!contains_word("imbalance > 0", "balance"));
        assert!(!contains_word("rebalance_flag", "balance"));
        assert!(!contains_word("my_balance_v2", "balance"));
    }

    #[test]
    fn test_contains_word_short_field() {
        // Field "id" must not match inside "valid", "provide", "identity"
        assert!(!contains_word("valid > 0", "id"));
        assert!(!contains_word("provide_service", "id"));
        assert!(!contains_word("identity = true", "id"));
        // But should match when standalone
        assert!(contains_word("id > 0", "id"));
        assert!(contains_word("state.id > 0", "id"));
        assert!(contains_word("check id here", "id"));
    }

    #[test]
    fn test_contains_word_at_boundaries() {
        assert!(contains_word("id", "id"));
        assert!(contains_word("id ", "id"));
        assert!(contains_word(" id", "id"));
        assert!(contains_word("(id)", "id"));
        assert!(contains_word("id+1", "id"));
        assert!(!contains_word("kid", "id"));
        assert!(!contains_word("ids", "id"));
    }

    #[test]
    fn parse_top_level_cmp_handles_simple_comparison() {
        let r = parse_top_level_cmp("s.balance >= s.balance");
        assert_eq!(r, Some(("s.balance", ">=", "s.balance")));
    }

    #[test]
    fn parse_top_level_cmp_handles_equality() {
        let r = parse_top_level_cmp("s.admin == s.admin");
        assert_eq!(r, Some(("s.admin", "==", "s.admin")));
    }

    #[test]
    fn parse_top_level_cmp_returns_none_on_non_comparison() {
        let r = parse_top_level_cmp("s.x + 1");
        assert!(r.is_none(), "expected None on non-comparison; got: {:?}", r);
    }
}
