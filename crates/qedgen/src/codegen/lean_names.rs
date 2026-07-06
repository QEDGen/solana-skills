//! Lean identifier / type-name helpers shared by the Lean-emitting
//! backends (`lean_gen_mir` and `lean_sidecars`). One source of truth —
//! these used to be hand-kept mirrors with a "keep in sync" comment.

/// Lean reserved words that collide with common qedspec identifiers
/// (notably `initialize`). Extend as fixtures surface collisions.
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

/// Quote Lean reserved names as `«name»` so they survive as identifiers.
pub(crate) fn safe_name(name: &str) -> String {
    if LEAN_RESERVED.contains(&name) {
        format!("\u{00AB}{}\u{00BB}", name)
    } else {
        name.to_string()
    }
}

/// Map a DSL type-string to its Lean form (scalar cases only). Unknown
/// forms — including compounds like `Map[N] T`, `Fin[N]` — pass through
/// unchanged; add compound support when a fixture demands it.
pub(crate) fn map_dsl_ty(s: &str) -> String {
    match s.trim() {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat".to_string(),
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int".to_string(),
        other => other.to_string(),
    }
}

/// Build a parameter signature string (` (n : T)` per param) for Lean
/// declaration sites, rendering each param type through `render_ty`.
/// Empty when `params` is empty.
pub(crate) fn param_sig_str_with<T>(
    params: &[(String, T)],
    render_ty: impl Fn(&T) -> String,
) -> String {
    if params.is_empty() {
        return String::new();
    }
    params
        .iter()
        .map(|(n, t)| format!(" ({} : {})", n, render_ty(t)))
        .collect::<Vec<_>>()
        .join("")
}

/// True iff an `upstream { binary_hash }` pin is present and non-empty —
/// half of the Tier-1/2 "pinned" predicate (the other half is the callee
/// declaring `ensures`), shared between the parse-layer and MIR-layer
/// `handler_is_pinned*` twins.
pub(crate) fn binary_hash_is_pinned(binary_hash: Option<&str>) -> bool {
    binary_hash.is_some_and(|h| !h.trim().is_empty())
}
