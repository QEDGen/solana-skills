//! State-driven symbolic account construction (#162 phase 2).
//!
//! The qedspec's **State** is the construction layout — NOT the IDL. The IDL is
//! lossy (stale, Anchor-0.29 format, strips leading underscores); the State, now
//! able to mirror a real `#[account]` struct verbatim (`Option<T>` and
//! `Vec<Record>` fields landed with G9/G10 — #173/#174), is faithful and
//! checked. Given the State's fields + the spec's record types, emit
//! `symbolic_<state>()` whose every field is `kani::any()`: scalars direct,
//! `Pubkey` from a symbolic byte array, `Option` symbolic Some/None, `Vec`
//! bounded (≤ 3, matching the harness unwind budget), nested records recursed.
//!
//! The brownfield harness pairs this with
//! `kani::assume(state.invariant().is_ok())` so Kani explores only well-formed
//! instances. It replaces the `todo!("build a symbolic state account struct")`
//! agent-fill site: construction is now generated from the spec; only the
//! effect + validity-gate call remains agent-fill.
//!
//! CONTRACT: the real struct name comes from `pragma state_struct = <Name>`
//! (see `resolve_state_struct`) — a brownfield `#[account]` struct's name isn't
//! otherwise in the spec. A wrong name surfaces as a `crate::<Name>` not-found
//! compile error, not silent wrong behaviour.

use crate::check::{ParsedRecordType, ParsedSpec};
use crate::mir::{parse_ty, Ty};

/// Resolve the real on-chain struct this brownfield spec's State mirrors, as
/// `(struct_name, fields)`.
///
/// The struct NAME is declared by `pragma state_struct = <Name>` — a brownfield
/// program's `#[account]` struct (`Settings`, `SmartAccount`, …) has a specific
/// name that the spec's greenfield naming (`<Program>Account`) doesn't capture,
/// and the bare `state { … }` sugar defaults to a synthetic `"State"` that would
/// build a wrong `crate::State`. The pragma is the one thing only the user
/// knows; everything else (the field layout, incl. `Option<T>`/`Vec<Record>`
/// after #173/#174) is already in the spec's canonical `state_fields`.
///
/// Returns `None` when the pragma is absent (or the State has no fields) — the
/// caller keeps its construction `todo!()` rather than guess the struct name.
pub(crate) fn resolve_state_struct(spec: &ParsedSpec) -> Option<(&str, &[(String, String)])> {
    let name = spec.pragma_value("state_struct")?;
    if spec.state_fields.is_empty() {
        return None;
    }
    Some((name, spec.state_fields.as_slice()))
}

/// Emit `fn symbolic_<snake(struct_name)>() -> crate::<struct_name> { … }` with
/// every field constructed symbolically. Returns `None` when a field can't be
/// built without agent knowledge (a bare enum / imported type, or a `Map`
/// field) — the caller keeps the `todo!()` fallback rather than emit a
/// half-`todo!()` ctor that reads as "generated" but isn't.
pub(crate) fn emit_state_ctor(
    struct_name: &str,
    fields: &[(String, String)],
    records: &[ParsedRecordType],
) -> Option<String> {
    // Build every field first: bail on the FIRST unconstructible one so we
    // never emit a partially-`todo!()` constructor.
    let mut field_lines = Vec::with_capacity(fields.len());
    for (name, ty_str) in fields {
        let expr = emit_value(&parse_ty(ty_str), records, 0)?;
        field_lines.push(format!("        {name}: {expr},"));
    }

    let mut out = String::new();
    out.push_str(&format!(
        "/// Fully-symbolic `{struct_name}` — every field `kani::any()`; pair with\n\
         /// `kani::assume(state.invariant().is_ok())` to explore only valid states.\n",
    ));
    out.push_str(&format!(
        "fn symbolic_{}() -> crate::{struct_name} {{\n    crate::{struct_name} {{\n",
        pascal_to_snake(struct_name),
    ));
    for line in field_lines {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("    }\n}\n");
    Some(out)
}

/// The `symbolic_<name>` fn name for a struct — exposed so the harness emitter
/// can reference the ctor without re-deriving the mangling.
pub(crate) fn ctor_fn_name(struct_name: &str) -> String {
    format!("symbolic_{}", pascal_to_snake(struct_name))
}

/// Recursive `Ty` → symbolic-construction expression. `None` = unconstructible
/// without agent knowledge.
fn emit_value(ty: &Ty, records: &[ParsedRecordType], depth: usize) -> Option<String> {
    if depth > 8 {
        return None; // recursion guard (mutually-recursive record types)
    }
    Some(match ty {
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128 | Ty::I64 | Ty::I128 | Ty::Bool => {
            "kani::any()".to_string()
        }
        Ty::Pubkey => "anchor_lang::prelude::Pubkey::new_from_array(kani::any())".to_string(),
        Ty::Custom(s) => {
            // `Option T` / `Vec T` ride as `Ty::Custom("Option T")` / `"Vec T"`
            // (the MIR `Ty` enum has no first-class Option/Vec — see #173/#174);
            // the inner is a single named type (scalar or record).
            if let Some(inner) = s.strip_prefix("Option ") {
                let inner_expr = emit_value(&parse_ty(inner.trim()), records, depth + 1)?;
                format!("if kani::any() {{ Some({inner_expr}) }} else {{ None }}")
            } else if let Some(inner) = s.strip_prefix("Vec ") {
                let inner_expr = emit_value(&parse_ty(inner.trim()), records, depth + 1)?;
                // Bounded symbolic Vec (≤ 3, matching the harness unwind budget).
                format!(
                    "{{ let mut v = Vec::new(); let n: usize = kani::any(); \
                     kani::assume(n <= 3); let mut i = 0usize; \
                     while i < n {{ v.push({inner_expr}); i += 1; }} v }}"
                )
            } else if let Some(rec) = records.iter().find(|r| &r.name == s) {
                // Nested record — recurse into its fields.
                let inner: Option<Vec<String>> = rec
                    .fields
                    .iter()
                    .map(|(fname, fty)| {
                        let e = emit_value(&parse_ty(fty), records, depth + 1)?;
                        Some(format!("{fname}: {e}"))
                    })
                    .collect();
                format!("crate::{s} {{ {} }}", inner?.join(", "))
            } else {
                // Bare enum / imported / unresolved type — needs agent knowledge.
                return None;
            }
        }
        // A `Map[N] T` state field has no faithful symbolic default here (the
        // on-chain layout is a fixed array or a BTreeMap, spec-dependent).
        Ty::Map { .. } => return None,
    })
}

/// `Settings` → `settings`, `SmartAccount` → `smart_account`. Struct names are
/// PascalCase; field names are already snake_case (mirror the real struct) so
/// they're used verbatim.
fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, fields: &[(&str, &str)]) -> ParsedRecordType {
        ParsedRecordType {
            name: name.to_string(),
            fields: fields
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
        }
    }

    #[test]
    fn pascal_to_snake_cases() {
        assert_eq!(pascal_to_snake("Settings"), "settings");
        assert_eq!(pascal_to_snake("SmartAccount"), "smart_account");
        assert_eq!(ctor_fn_name("Settings"), "symbolic_settings");
    }

    #[test]
    fn scalars_pubkey_option_vec_and_nested() {
        let records = vec![
            rec(
                "SmartAccountSigner",
                &[("key", "Pubkey"), ("permissions", "Permissions")],
            ),
            rec("Permissions", &[("mask", "U8")]),
        ];
        // Scalars + Pubkey.
        assert_eq!(
            emit_value(&parse_ty("U64"), &records, 0).unwrap(),
            "kani::any()"
        );
        assert_eq!(
            emit_value(&parse_ty("Pubkey"), &records, 0).unwrap(),
            "anchor_lang::prelude::Pubkey::new_from_array(kani::any())"
        );
        // Option<Pubkey> → symbolic Some/None.
        let opt = emit_value(&parse_ty("Option Pubkey"), &records, 0).unwrap();
        assert!(opt.contains("if kani::any()") && opt.contains("Some(") && opt.contains("None"));
        // Vec<SmartAccountSigner> → bounded loop building the nested struct.
        let v = emit_value(&parse_ty("Vec SmartAccountSigner"), &records, 0).unwrap();
        assert!(v.contains("kani::assume(n <= 3)"), "bounded; got {v}");
        assert!(
            v.contains("crate::SmartAccountSigner {") && v.contains("crate::Permissions {"),
            "nested structs recursed; got {v}"
        );
        assert!(v.contains("mask:"), "leaf field present; got {v}");
    }

    #[test]
    fn full_settings_ctor_is_agent_fill_free() {
        let records = vec![
            rec(
                "SmartAccountSigner",
                &[("key", "Pubkey"), ("permissions", "Permissions")],
            ),
            rec("Permissions", &[("mask", "U8")]),
        ];
        let fields = vec![
            ("seed".into(), "U128".into()),
            ("settings_authority".into(), "Pubkey".into()),
            ("time_lock".into(), "U32".into()),
            ("archival_authority".into(), "Option Pubkey".into()),
            ("signers".into(), "Vec SmartAccountSigner".into()),
        ];
        let ctor = emit_state_ctor("Settings", &fields, &records).unwrap();
        assert!(ctor.contains("fn symbolic_settings() -> crate::Settings"));
        assert!(ctor.contains("settings_authority:") && ctor.contains("time_lock:"));
        assert!(ctor.contains("crate::SmartAccountSigner {"));
        assert!(!ctor.contains("todo!"), "no agent-fill; got:\n{ctor}");
    }

    #[test]
    fn unconstructible_field_bails_to_none() {
        // A bare enum / unresolved type (not in records) → whole ctor is None,
        // so the caller keeps its `todo!()` rather than emit a half-built struct.
        let fields = vec![
            ("ok".into(), "U64".into()),
            ("kind".into(), "SomeEnum".into()), // not a record → unconstructible
        ];
        assert!(emit_state_ctor("Thing", &fields, &[]).is_none());
        // A `Map` field is likewise unconstructible here.
        let map_fields = vec![("book".into(), "Map[8] U64".into())];
        assert!(emit_state_ctor("Thing", &map_fields, &[]).is_none());
    }
}
