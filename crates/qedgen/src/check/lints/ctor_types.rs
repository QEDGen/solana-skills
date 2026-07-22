//! `unresolved_constructor_type` — every `.Variant` constructor, record
//! literal, and record update in an expression position must carry its
//! resolved nominal type (#325).
//!
//! The types are resolved at tree-build time (`chumsky_adapter::tree`):
//! constructors by unique-variant search, record literals by unique
//! field-set match, record updates from the base path's type. When
//! resolution fails (ambiguous variant name, partial or ambiguous field
//! set, non-path update base), the Rust renderer would have to emit a
//! placeholder — so the gap fails `check` here instead, and the renderer's
//! poison identifier (`__QEDGEN_UNRESOLVED_TYPE`) is only a backstop for
//! codegen runs that skipped check.
//!
//! Lean is unaffected: it renders these forms anonymously and lets the
//! elaborator infer the type.

use super::*;
use crate::mir::ExprTree;

pub(super) fn check_ctor_types(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    let mut warnings = Vec::new();
    let mut lint = |site: &str, tree: &ExprTree| {
        let mut problems: Vec<String> = Vec::new();
        tree.for_each_node(&mut |node| match node {
            ExprTree::Ctor {
                variant, ty: None, ..
            } => problems.push(format!(
                "constructor `.{}` does not resolve to a unique sum type",
                variant
            )),
            ExprTree::RecordLit { fields, ty: None } => problems.push(format!(
                "record literal {{ {} }} does not match a unique declared record's field set",
                fields
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            ExprTree::RecordUpdate { ty: None, .. } => {
                problems.push("record update base has no resolvable record type".to_string())
            }
            _ => {}
        });
        for problem in problems {
            warnings.push(
                warn(
                    "unresolved_constructor_type",
                    Severity::Error,
                    1,
                    format!("{}: {}", site, problem),
                )
                .fix(
                    "Rust codegen needs a concrete type name here. Disambiguate the \
                     variant / field set, or declare the record the literal constructs.",
                ),
            );
        }
    };

    for prop in &spec.properties {
        if let Some(tree) = &prop.tree {
            lint(&format!("property `{}`", prop.name), tree);
        }
    }
    for inv in &spec.invariants {
        if let Some(tree) = &inv.tree {
            lint(&format!("invariant `{}`", inv.name), tree);
        }
    }
    for handler in &spec.handlers {
        for (idx, req) in handler.requires.iter().enumerate() {
            if let Some(tree) = &req.tree {
                lint(
                    &format!("handler `{}` requires #{}", handler.name, idx),
                    tree,
                );
            }
        }
        for (idx, ens) in handler.ensures.iter().enumerate() {
            if let Some(tree) = &ens.tree {
                lint(
                    &format!("handler `{}` ensures #{}", handler.name, idx),
                    tree,
                );
            }
        }
        for effect in &handler.effects {
            if let Some(tree) = &effect.tree {
                lint(
                    &format!("handler `{}` effect `{}`", handler.name, effect.field),
                    tree,
                );
            }
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    fn findings(src: &str) -> Vec<String> {
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        super::check_ctor_types(&spec)
            .into_iter()
            .map(|w| w.message)
            .collect()
    }

    #[test]
    fn resolved_ctor_and_record_lit_pass() {
        let findings = findings(
            r#"spec T
type Entry = { who : Pubkey, amount : U64 }
state { total : U64, best : Entry }
handler set_best (who : Pubkey) (amount : U64) {
  permissionless
  accounts {
    caller : signer
  }
  effect {
    best := { who := who, amount := amount }
  }
}
"#,
        );
        assert!(findings.is_empty(), "{:#?}", findings);
    }

    #[test]
    fn ambiguous_record_literal_fails() {
        // Two records with the same field set: the literal cannot resolve.
        let findings = findings(
            r#"spec T
type EntryA = { who : Pubkey, amount : U64 }
type EntryB = { who : Pubkey, amount : U64 }
state { total : U64, best : EntryA }
handler set_best (who : Pubkey) (amount : U64) {
  permissionless
  accounts {
    caller : signer
  }
  effect {
    best := { who := who, amount := amount }
  }
}
"#,
        );
        assert_eq!(findings.len(), 1, "{:#?}", findings);
        assert!(findings[0].contains("does not match a unique declared record"));
    }
}
