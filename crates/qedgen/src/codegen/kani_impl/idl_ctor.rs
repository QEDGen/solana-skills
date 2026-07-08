//! IDL-driven symbolic account construction (#162 phase 2). Given an Anchor
//! account struct's field layout (from the qedspec's IDL) plus the map of
//! defined types for nested resolution, emit a `symbolic_<struct>()`
//! constructor whose every field is `kani::any()` — scalars/`Pubkey` direct,
//! `Option` symbolic, `Vec` bounded, nested `defined` structs recursed.
//!
//! The harness pairs this with `kani::assume(<account>.invariant().is_ok())`
//! (or the relevant precondition), so Kani explores only well-formed
//! instances. This replaces the `todo!("build a symbolic state account
//! struct")` agent-fill site in the brownfield emitter — construction becomes
//! fully generated; only the effect + validity-gate call remains agent-fill.
//!
//! Requires a CURRENT IDL: a stale one (fields renamed/added since it was
//! generated) emits a non-compiling constructor. The caller is responsible
//! for freshness (regenerate on build / drift-check).
//!
//! STAGING: the emitter is complete and unit-tested here; wiring it into
//! `emit_brownfield_handler_harness` (replacing the construction `todo!()`)
//! lands with the IDL-loading plumbing in the next #162-p2 increment.
#![allow(dead_code)]

use serde_json::Value;
use std::collections::BTreeMap;

use crate::spec::idl::IdlField;

/// Map of defined-type name → its fields, for resolving `{"defined": …}`.
pub(crate) type TypeMap<'a> = BTreeMap<String, &'a [IdlField]>;

/// Emit `fn symbolic_<snake(struct_name)>() -> crate::<struct_name> { … }`
/// with every field constructed symbolically.
pub(crate) fn emit_account_ctor(struct_name: &str, fields: &[IdlField], types: &TypeMap) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "/// Fully-symbolic `{0}` — every field `kani::any()`; pair with\n\
         /// `kani::assume(state.invariant().is_ok())` to explore only valid states.\n",
        struct_name
    ));
    out.push_str(&format!(
        "fn symbolic_{}() -> crate::{} {{\n",
        idl_to_snake(struct_name),
        struct_name
    ));
    out.push_str(&format!("    crate::{} {{\n", struct_name));
    for f in fields {
        out.push_str(&format!(
            "        {}: {},\n",
            idl_to_snake(&f.name),
            emit_value(&f.ty, types, 0)
        ));
    }
    out.push_str("    }\n}\n");
    out
}

/// Recursive type → symbolic-construction expression.
pub(crate) fn emit_value(ty: &Value, types: &TypeMap, depth: usize) -> String {
    if depth > 8 {
        return "todo!(\"IDL recursion limit\")".to_string();
    }
    match ty {
        Value::String(s) => match s.as_str() {
            "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64" | "i128"
            | "usize" | "isize" | "bool" => "kani::any()".to_string(),
            "publicKey" | "pubkey" => {
                "anchor_lang::prelude::Pubkey::new_from_array(kani::any())".to_string()
            }
            other => emit_defined(other, types, depth),
        },
        Value::Object(map) => {
            if let Some(inner) = map.get("option") {
                format!(
                    "if kani::any() {{ Some({}) }} else {{ None }}",
                    emit_value(inner, types, depth + 1)
                )
            } else if let Some(inner) = map.get("vec") {
                // Bounded symbolic Vec (≤ 3, matching the harness unwind budget).
                format!(
                    "{{ let mut v = Vec::new(); let n: usize = kani::any(); \
                     kani::assume(n <= 3); let mut i = 0usize; \
                     while i < n {{ v.push({}); i += 1; }} v }}",
                    emit_value(inner, types, depth + 1)
                )
            } else if let Some(d) = map.get("defined") {
                emit_defined(&defined_name(d), types, depth)
            } else if let Some(arr) = map.get("array").and_then(|a| a.as_array()) {
                // `["T", N]`
                let elem = arr.first().map(|e| emit_value(e, types, depth + 1));
                let n = arr.get(1).and_then(|n| n.as_u64());
                match (elem, n) {
                    (Some(e), Some(n)) => format!("[(); {}].map(|_| {})", n, e),
                    _ => format!("todo!(\"unsupported IDL array: {}\")", ty),
                }
            } else {
                format!("todo!(\"unsupported IDL type: {}\")", ty)
            }
        }
        _ => format!("todo!(\"unsupported IDL type: {}\")", ty),
    }
}

/// Construct a `defined` struct inline by resolving its fields in `types`.
fn emit_defined(name: &str, types: &TypeMap, depth: usize) -> String {
    match types.get(name) {
        Some(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}",
                        idl_to_snake(&f.name),
                        emit_value(&f.ty, types, depth + 1)
                    )
                })
                .collect();
            format!("crate::{} {{ {} }}", name, inner.join(", "))
        }
        // A defined type not in the map (e.g. an enum, or an unresolved import)
        // — surface as agent-fill rather than emit something that won't compile.
        None => format!(
            "todo!(\"construct crate::{} (not a struct in the IDL types)\")",
            name
        ),
    }
}

/// Anchor 0.30 wraps `defined` as `{"name": "Foo"}`; 0.29 uses a bare string.
fn defined_name(d: &Value) -> String {
    match d {
        Value::String(s) => s.clone(),
        Value::Object(m) => m
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// Anchor IDL field names are camelCase; the Rust struct fields are snake_case.
/// `settingsAuthority` → `settings_authority`, `timeLock` → `time_lock`.
fn idl_to_snake(name: &str) -> String {
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

    fn field(name: &str, ty: Value) -> IdlField {
        IdlField {
            name: name.to_string(),
            ty,
        }
    }

    #[test]
    fn snake_case_from_camel() {
        assert_eq!(idl_to_snake("settingsAuthority"), "settings_authority");
        assert_eq!(idl_to_snake("timeLock"), "time_lock");
        assert_eq!(idl_to_snake("seed"), "seed");
    }

    #[test]
    fn scalars_and_pubkey_are_symbolic() {
        let types = TypeMap::new();
        assert_eq!(emit_value(&Value::from("u64"), &types, 0), "kani::any()");
        assert_eq!(emit_value(&Value::from("bool"), &types, 0), "kani::any()");
        assert_eq!(
            emit_value(&Value::from("publicKey"), &types, 0),
            "anchor_lang::prelude::Pubkey::new_from_array(kani::any())"
        );
    }

    #[test]
    fn option_and_vec_and_nested_struct() {
        // Nested: SmartAccountSigner { key: publicKey, permissions: Permissions }
        //         Permissions { mask: u8 }
        let signer_fields = vec![
            field("key", Value::from("publicKey")),
            field(
                "permissions",
                serde_json::json!({ "defined": "Permissions" }),
            ),
        ];
        let perm_fields = vec![field("mask", Value::from("u8"))];
        let mut types: TypeMap = TypeMap::new();
        types.insert("SmartAccountSigner".to_string(), &signer_fields);
        types.insert("Permissions".to_string(), &perm_fields);

        // Option<Pubkey> → symbolic Some/None
        let opt = emit_value(&serde_json::json!({ "option": "publicKey" }), &types, 0);
        assert!(opt.contains("if kani::any()") && opt.contains("Some(") && opt.contains("None"));

        // Vec<SmartAccountSigner> → bounded loop building the nested struct
        let vecty = emit_value(
            &serde_json::json!({ "vec": { "defined": "SmartAccountSigner" } }),
            &types,
            0,
        );
        assert!(
            vecty.contains("kani::assume(n <= 3)"),
            "bounded vec; got {vecty}"
        );
        assert!(
            vecty.contains("crate::SmartAccountSigner {") && vecty.contains("crate::Permissions {"),
            "nested structs recursed; got {vecty}"
        );
        assert!(vecty.contains("mask:"), "leaf field present; got {vecty}");
    }

    #[test]
    fn full_account_ctor_snake_cases_and_recurses() {
        let signer_fields = vec![
            field("key", Value::from("publicKey")),
            field(
                "permissions",
                serde_json::json!({ "defined": "Permissions" }),
            ),
        ];
        let perm_fields = vec![field("mask", Value::from("u8"))];
        let mut types: TypeMap = TypeMap::new();
        types.insert("SmartAccountSigner".to_string(), &signer_fields);
        types.insert("Permissions".to_string(), &perm_fields);

        let settings_fields = vec![
            field("seed", Value::from("u128")),
            field("settingsAuthority", Value::from("publicKey")),
            field("timeLock", Value::from("u32")),
            field(
                "archivalAuthority",
                serde_json::json!({ "option": "publicKey" }),
            ),
            field(
                "signers",
                serde_json::json!({ "vec": { "defined": "SmartAccountSigner" } }),
            ),
        ];
        let ctor = emit_account_ctor("Settings", &settings_fields, &types);
        assert!(ctor.contains("fn symbolic_settings() -> crate::Settings"));
        assert!(ctor.contains("settings_authority:") && ctor.contains("time_lock:"));
        assert!(ctor.contains("crate::SmartAccountSigner {"));
        assert!(
            !ctor.contains("todo!"),
            "no unresolved agent-fill; got:\n{ctor}"
        );
    }
}
