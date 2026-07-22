//! `unknown_type` — validate every type-bearing string in the spec
//! against the canonical type IR (#327).
//!
//! Before this lint, an undeclared or malformed type spelling passed
//! `check` silently: `mir::parse_ty` absorbed it into `Ty::Custom`, the
//! Lean backend printed it verbatim (invalid Lean, failing only at `lake
//! build`), and proptest fell back to a `u64` strategy. Now `Custom`
//! means "declared nominal type" by contract, and this lint is the gate
//! that enforces it: every `Custom` leaf must resolve to a declared
//! record, sum type, or alias, and every `Fin`/`Map` bound must be a
//! numeric literal, a declared constant, or a unit-only sum type.
//!
//! Struct-mirror specs (`pragma state_struct` / `state_module`) are
//! exempt: they legitimately name external Rust types the spec does not
//! declare.

use super::*;

pub(super) fn check_known_types(spec: &ParsedSpec) -> Vec<CompletenessWarning> {
    if spec.is_struct_mirror() {
        return Vec::new();
    }

    // Declared nominal names. Account-type names are included because the
    // adapter routes `type X | A | B` to `account_types` unless X is used
    // as a Map value — a field typed by such a name is declared, whatever
    // its downstream semantics.
    let registry: std::collections::BTreeSet<&str> = spec
        .records
        .iter()
        .map(|r| r.name.as_str())
        .chain(spec.sum_types.iter().map(|s| s.name.as_str()))
        .chain(spec.type_aliases.iter().map(|(n, _)| n.as_str()))
        .chain(spec.account_types.iter().map(|a| a.name.as_str()))
        .collect();

    let mut warnings = Vec::new();
    let mut lint = |site: String, ty_str: &str| {
        if let Err(problem) = validate_type_str(ty_str, &registry, spec) {
            warnings.push(
                warn(
                    "unknown_type",
                    Severity::Error,
                    1,
                    format!("{} has type `{}` — {}", site, ty_str, problem),
                )
                .subject(ty_str.to_string())
                .fix(
                    "Declare a record / sum type / alias with this name, or use a \
                     built-in type (see references/qedspec-dsl.md).",
                ),
            );
        }
    };

    // `state_fields` mirrors the primary account type and `account_types
    // [i].fields` is the flattened union of that account's variant
    // payloads — lint each declaration once, at its owning account when
    // one exists.
    if spec.account_types.is_empty() {
        for (name, ty) in &spec.state_fields {
            lint(format!("state field `{}`", name), ty);
        }
    }
    for acct in &spec.account_types {
        for (name, ty) in &acct.fields {
            lint(format!("account `{}` field `{}`", acct.name, name), ty);
        }
    }
    for record in &spec.records {
        // The flat-state `State` record mirrors `state_fields` /
        // `account_types` — those sites own the diagnostics.
        if record.name == "State" {
            continue;
        }
        for (name, ty) in &record.fields {
            lint(format!("record `{}` field `{}`", record.name, name), ty);
        }
    }
    for sum in &spec.sum_types {
        for variant in &sum.variants {
            for (name, ty) in &variant.fields {
                lint(
                    format!(
                        "sum `{}` variant `{}` field `{}`",
                        sum.name, variant.name, name
                    ),
                    ty,
                );
            }
        }
    }
    for handler in &spec.handlers {
        for (name, ty) in &handler.takes_params {
            lint(format!("handler `{}` param `{}`", handler.name, name), ty);
        }
    }
    for ghost in &spec.ghosts {
        lint(format!("ghost `{}`", ghost.name), &ghost.ty);
    }
    for env in &spec.environments {
        for (name, ty) in &env.mutates {
            lint(format!("environment `{}` mutates `{}`", env.name, name), ty);
        }
        for (object, field, ty) in &env.external_fields {
            lint(
                format!("environment `{}` external `{}.{}`", env.name, object, field),
                ty,
            );
        }
    }

    warnings
}

/// Parse via the canonical IR, then walk the structure: every `Custom`
/// leaf must be declared; every `Fin`/`Map` bound must resolve.
fn validate_type_str(
    ty_str: &str,
    registry: &std::collections::BTreeSet<&str>,
    spec: &ParsedSpec,
) -> Result<(), String> {
    // Resolve top-level aliases first (transitively, cycle-guarded) so an
    // alias to a structured form validates the target's structure.
    let mut resolved = ty_str.trim();
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert(resolved.to_string()) {
        match spec.type_aliases.iter().find(|(n, _)| n == resolved) {
            Some((_, rhs)) => resolved = rhs.trim(),
            None => break,
        }
    }
    validate_ty(&crate::mir::parse_ty(resolved), registry, spec)
}

fn validate_ty(
    ty: &crate::mir::Ty,
    registry: &std::collections::BTreeSet<&str>,
    spec: &ParsedSpec,
) -> Result<(), String> {
    use crate::mir::Ty;
    match ty {
        Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::U128
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::I128
        | Ty::Bool
        | Ty::Pubkey
        | Ty::Bytes32
        | Ty::Bytes64 => Ok(()),
        Ty::Fin { bound } => validate_bound(bound, spec),
        Ty::Vec { value } | Ty::Option { value } => validate_ty(value, registry, spec),
        Ty::Map { capacity, value } => {
            validate_bound(capacity, spec)?;
            validate_ty(value, registry, spec)
        }
        Ty::Custom(name) => {
            // A structured spelling that failed to parse cleanly lands
            // here with its punctuation intact — report it as malformed
            // rather than undeclared.
            if name.contains(' ') || name.contains('[') {
                return Err(format!("`{}` is not a recognized type form", name));
            }
            if registry.contains(name.as_str()) {
                Ok(())
            } else {
                Err(format!("`{}` is not declared in this spec", name))
            }
        }
    }
}

/// `Fin[N]` / `Map[N] T` bound: numeric literal, declared constant, or
/// unit-only sum type (variant count) — `resolve_map_bound` semantics.
fn validate_bound(bound: &str, spec: &ParsedSpec) -> Result<(), String> {
    if bound.chars().all(|c| c.is_ascii_digit()) && !bound.is_empty() {
        return Ok(());
    }
    if spec.constants.iter().any(|(n, _)| n == bound) {
        return Ok(());
    }
    if spec
        .sum_types
        .iter()
        .any(|s| s.name == bound && s.variants.iter().all(|v| v.fields.is_empty()))
    {
        return Ok(());
    }
    Err(format!(
        "bound `{}` is not a numeric literal, declared constant, or unit-only sum type",
        bound
    ))
}

#[cfg(test)]
mod tests {
    fn unknown_type_findings(src: &str) -> Vec<String> {
        let spec = crate::chumsky_adapter::parse_str(src).expect("spec parses");
        super::check_known_types(&spec)
            .into_iter()
            .map(|w| w.message)
            .collect()
    }

    #[test]
    fn structured_and_declared_types_pass() {
        let findings = unknown_type_findings(
            r#"spec T
const MAX = 4
type Entry = { who : Pubkey, amount : U64 }
type Mode | Fast | Slow
type Idx = Fin[MAX]
state { total : U64, idx : Fin[8], note : Option U64, log : Vec U64, entries : Map[MAX] Entry, mode : Mode, i : Idx }
handler noop { }
"#,
        );
        assert!(findings.is_empty(), "{:#?}", findings);
    }

    #[test]
    fn undeclared_nominal_and_bad_bound_fail() {
        let findings = unknown_type_findings(
            r#"spec T
state { owner : Ghostly, tag : Fin[NOPE] }
handler noop { }
"#,
        );
        assert_eq!(findings.len(), 2, "{:#?}", findings);
        assert!(findings[0].contains("`Ghostly` is not declared"));
        assert!(findings[1].contains("bound `NOPE`"));
    }
}
