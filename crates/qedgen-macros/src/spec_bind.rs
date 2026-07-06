//! Spec-binding extension for the `#[qed(verified, ...)]` attribute.
//!
//! At compile time, reads the `.qedspec` file referenced by the attribute,
//! extracts the raw text of `handler <name> { ... }` via balanced-brace
//! scanning, hashes that text (same SHA-256-hex16 algorithm used for
//! function-body drift), and compares to the declared `spec_hash`.
//!
//! Pure compile-time check: the wrapped function is emitted byte-identical
//! to what the user wrote.

use proc_macro2::TokenStream;

// Canonical hashing shared with the qedgen CLI (`qedgen::spec_hash`
// re-exports the same functions) — agreement by construction, not by
// hand-kept mirroring. `extract_handler_block` / `normalize_spec_block` /
// `spec_context_digest` are pulled in so the test module (and future
// callers) can reach them via `super::*`.
#[allow(unused_imports)]
use qedgen_hash_core::{
    extract_handler_block, normalize_spec_block, spec_context_digest, spec_hash_for_handler,
    token_stream_hash,
};

use crate::verified::FnLike;

/// Parsed attribute arguments from `#[qed(verified, spec=..., handler=..., ...)]`.
pub(crate) struct Args {
    pub spec: Option<String>,
    pub handler: Option<String>,
    pub hash: Option<String>,
    pub spec_hash: Option<String>,
    /// Optional `#[derive(Accounts)]` struct name (e.g. `Buy`,
    /// `Initialize`). Present when the user wants the macro to also
    /// hash-check the accounts struct alongside the body + spec.
    pub accounts: Option<String>,
    /// Path to the file declaring the accounts struct, relative to
    /// `CARGO_MANIFEST_DIR`. Anchor scaffold typically puts it in
    /// `src/lib.rs`; some programs split into `src/accounts.rs` or
    /// `src/instructions/<name>.rs`.
    pub accounts_file: Option<String>,
    /// Sealed hash of the canonicalized accounts struct (sha256-hex16
    /// of the syn ItemStruct's tokens after attribute stripping).
    pub accounts_hash: Option<String>,
}

/// v2.29 — recursively read every `*.qedspec` file under `dir`, sort
/// by path, and concatenate (with newline separation). Mirrors
/// `check::read_spec_source`'s directory branch so the spec_hash
/// the macro recomputes matches the codegen-time hash byte-for-byte.
fn read_spec_dir(dir: &std::path::Path) -> std::io::Result<String> {
    let mut files = Vec::new();
    collect_qedspec_files(dir, &mut files)?;
    files.sort();
    let mut out = String::new();
    for f in &files {
        let src = std::fs::read_to_string(f)?;
        out.push_str(&src);
        if !src.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

fn collect_qedspec_files(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_qedspec_files(&path, out)?;
        } else if file_type.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("qedspec")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse all `key = "value"` pairs from the attribute stream.
pub(crate) fn parse_args(attr: &TokenStream) -> Result<Args, syn::Error> {
    let tokens: Vec<proc_macro2::TokenTree> = attr.clone().into_iter().collect();

    let mut spec = None;
    let mut handler = None;
    let mut hash = None;
    let mut spec_hash = None;
    let mut accounts = None;
    let mut accounts_file = None;
    let mut accounts_hash = None;

    let mut i = 0;
    while i < tokens.len() {
        if let proc_macro2::TokenTree::Ident(ref ident) = tokens[i] {
            let name = ident.to_string();
            if matches!(
                name.as_str(),
                "spec"
                    | "handler"
                    | "hash"
                    | "spec_hash"
                    | "accounts"
                    | "accounts_file"
                    | "accounts_hash"
            ) {
                // Expect `=` then literal
                if i + 2 >= tokens.len() {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): expected `{} = \"...\"`", name),
                    ));
                }
                let eq_ok = matches!(&tokens[i + 1], proc_macro2::TokenTree::Punct(p) if p.as_char() == '=');
                if !eq_ok {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): expected `=` after `{}`", name),
                    ));
                }
                let value = if let proc_macro2::TokenTree::Literal(ref lit) = tokens[i + 2] {
                    let lit_str = lit.to_string();
                    lit_str.trim_matches('"').to_string()
                } else {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): expected string literal for `{}`", name),
                    ));
                };
                if value.is_empty() {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("qed(verified): `{}` value cannot be empty", name),
                    ));
                }
                match name.as_str() {
                    "spec" => spec = Some(value),
                    "handler" => handler = Some(value),
                    "hash" => hash = Some(value),
                    "spec_hash" => spec_hash = Some(value),
                    "accounts" => accounts = Some(value),
                    "accounts_file" => accounts_file = Some(value),
                    "accounts_hash" => accounts_hash = Some(value),
                    _ => unreachable!(),
                }
                i += 3;
                continue;
            }
        }
        i += 1;
    }

    Ok(Args {
        spec,
        handler,
        hash,
        spec_hash,
        accounts,
        accounts_file,
        accounts_hash,
    })
}

/// Parse `source` as Rust, find a `pub struct <name>` (top-level or
/// inside an inline `pub mod`), and return the canonical hash of its
/// tokens (after outer-attribute stripping). Mirrors
/// `qedgen::spec_hash::accounts_struct_hash` so the proc-macro and
/// the qedgen-side computation produce identical values; any
/// divergence yields a spurious accounts-hash drift.
///
/// Returns `None` when:
///   - the file isn't valid Rust source
///   - no `struct <name>` is declared anywhere in the file
pub(crate) fn accounts_struct_hash_in(source: &str, struct_name: &str) -> Option<String> {
    let file: syn::File = syn::parse_str(source).ok()?;
    accounts_struct_hash_in_items(&file.items, struct_name)
}

fn accounts_struct_hash_in_items(items: &[syn::Item], struct_name: &str) -> Option<String> {
    use quote::ToTokens;
    for item in items {
        match item {
            syn::Item::Struct(s) if s.ident == struct_name => {
                let mut stripped = s.clone();
                stripped.attrs.clear();
                // v2.15: token_stream_hash canonicalizes Spacing on
                // every Punct so this computation agrees byte-for-byte
                // with `qedgen::spec_hash::accounts_struct_hash_in_items`
                // — see qedgen_hash_core::canonical_token_string.
                return Some(token_stream_hash(&stripped.to_token_stream()));
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, sub_items)) = &item_mod.content {
                    if let Some(h) = accounts_struct_hash_in_items(sub_items, struct_name) {
                        return Some(h);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Main expansion for `#[qed(verified, spec=..., handler=..., hash=..., spec_hash=...)]`.
///
/// If `spec` and `handler` are both absent, falls back to the legacy
/// body-only flow in `verified::expand`. Otherwise performs both the
/// body-hash check AND the spec-hash check at compile time.
///
/// The macro NEVER injects runtime code — expansion is the original function
/// alone (or compile_error + original function on drift).
pub fn expand_bound(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the item — accepts free fns and impl methods alike.
    let func = match FnLike::from_tokens(item.clone()) {
        Ok(f) => f,
        Err(_) => {
            return syn::Error::new_spanned(
                &item,
                "qed(verified): can only be applied to free functions or impl methods",
            )
            .to_compile_error();
        }
    };

    let args = match parse_args(&attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error(),
    };

    // Accounts metadata is all-or-nothing. Partial config (e.g.
    // `accounts` + `accounts_file` without `accounts_hash`) used to
    // silently disable accounts-struct sealing while looking like a
    // proper config — teams believed accounts-level drift was enforced
    // when it was not. (GH issue #29.) Either provide all three, or
    // provide none.
    let acct_provided = [
        args.accounts.is_some(),
        args.accounts_file.is_some(),
        args.accounts_hash.is_some(),
    ];
    if acct_provided.iter().any(|p| *p) && !acct_provided.iter().all(|p| *p) {
        let func_span = match FnLike::from_tokens(item.clone()) {
            Ok(f) => f.name_span(),
            Err(_) => proc_macro2::Span::call_site(),
        };
        let missing: Vec<&str> = ["accounts", "accounts_file", "accounts_hash"]
            .iter()
            .copied()
            .zip(acct_provided.iter())
            .filter_map(|(name, provided)| if *provided { None } else { Some(name) })
            .collect();
        let msg = format!(
            "qed(verified): partial accounts metadata — missing `{}`. Either \
             provide all of `accounts`, `accounts_file`, `accounts_hash` or \
             omit all three. Partial configs silently disable accounts-struct \
             sealing.",
            missing.join("`, `")
        );
        return syn::Error::new(func_span, msg).to_compile_error();
    }

    // If spec/handler not both present, fall back to the body-only path.
    let (spec_path, handler_name) = match (&args.spec, &args.handler) {
        (Some(s), Some(h)) => (s.clone(), h.clone()),
        _ => return crate::verified::expand(attr, item),
    };

    let fn_name = func.ident().to_string();
    let actual_body_hash = func.content_hash();
    let func_span = func.name_span();
    let func_tokens = func.to_token_stream();

    // Locate the spec file relative to CARGO_MANIFEST_DIR.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let full_path = std::path::Path::new(&manifest_dir).join(&spec_path);
    // v2.29 — accept directory `spec = "..."` paths so multi-file
    // specs (handlers/, properties/ subdirs alongside the root
    // spec) work without forcing users to point the macro at a
    // single "primary" file. Mirrors `check::read_spec_source`:
    // walk the dir, collect `*.qedspec`, sort, concatenate. spec_hash
    // recomputation then operates on the same merged content the
    // codegen-time hash was computed against.
    let source = if full_path.is_dir() {
        match read_spec_dir(&full_path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!(
                    "qed(verified): could not read spec dir `{}` (resolved to `{}`): {}",
                    spec_path,
                    full_path.display(),
                    e
                );
                let err = syn::Error::new(func_span, msg).to_compile_error();
                return quote::quote! { #err #func_tokens };
            }
        }
    } else {
        match std::fs::read_to_string(&full_path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!(
                    "qed(verified): could not read spec file `{}` (resolved to `{}`): {}",
                    spec_path,
                    full_path.display(),
                    e
                );
                let err = syn::Error::new(func_span, msg).to_compile_error();
                return quote::quote! { #err #func_tokens };
            }
        }
    };

    let actual_spec_hash = match spec_hash_for_handler(&source, &handler_name) {
        Some(h) => h,
        None => {
            let msg = format!(
                "qed(verified): handler `{}` not found in spec `{}`",
                handler_name, spec_path,
            );
            let err = syn::Error::new(func_span, msg).to_compile_error();
            return quote::quote! { #err #func_tokens };
        }
    };

    // Optional accounts-struct check. When all three of `accounts`,
    // `accounts_file`, and `accounts_hash` are present, hash the
    // referenced struct and validate. Absent → backward-compat
    // body+spec-only check.
    let actual_accounts_hash = match (
        args.accounts.as_deref(),
        args.accounts_file.as_deref(),
        args.accounts_hash.as_deref(),
    ) {
        (Some(name), Some(file), _) => {
            let acct_path = std::path::Path::new(&manifest_dir).join(file);
            match std::fs::read_to_string(&acct_path) {
                Ok(src) => match accounts_struct_hash_in(&src, name) {
                    Some(h) => Some(h),
                    None => {
                        let msg = format!(
                            "qed(verified): accounts struct `{}` not found in `{}` (resolved to `{}`)",
                            name, file, acct_path.display(),
                        );
                        let err = syn::Error::new(func_span, msg).to_compile_error();
                        return quote::quote! { #err #func_tokens };
                    }
                },
                Err(e) => {
                    let msg = format!(
                        "qed(verified): could not read accounts file `{}` (resolved to `{}`): {}",
                        file,
                        acct_path.display(),
                        e
                    );
                    let err = syn::Error::new(func_span, msg).to_compile_error();
                    return quote::quote! { #err #func_tokens };
                }
            }
        }
        _ => None,
    };

    // Both core hashes must be provided for a sealed attribute. The
    // accounts-hash leg is optional (legacy attributes still work).
    match (args.hash.as_deref(), args.spec_hash.as_deref()) {
        (Some(body_expected), Some(spec_expected)) => {
            if body_expected != actual_body_hash {
                let msg = format!(
                    "qed: verified function `{}` has changed since verification \
                     — re-verify or update hash.\n\
                     Expected: {}\n\
                     Actual:   {}",
                    fn_name, body_expected, actual_body_hash
                );
                let err = syn::Error::new(func_span, msg).to_compile_error();
                return quote::quote! { #err #func_tokens };
            }
            if spec_expected != actual_spec_hash {
                let msg = format!(
                    "qed: handler `{}` spec contract changed.\n\
                     Expected: {}\n\
                     Actual:   {}\n\
                     Re-run `qedgen check` for a diff.",
                    handler_name, spec_expected, actual_spec_hash
                );
                let err = syn::Error::new(func_span, msg).to_compile_error();
                return quote::quote! { #err #func_tokens };
            }
            // If accounts metadata supplied, validate that leg too.
            if let (Some(acct_name), Some(acct_actual), Some(acct_expected)) = (
                args.accounts.as_deref(),
                actual_accounts_hash.as_deref(),
                args.accounts_hash.as_deref(),
            ) {
                if acct_expected != acct_actual {
                    let msg = format!(
                        "qed: accounts struct `{}` changed since verification \
                         (handler `{}`).\n\
                         Expected: {}\n\
                         Actual:   {}\n\
                         Re-run `qedgen adapt --spec ...` to refresh.",
                        acct_name, handler_name, acct_expected, acct_actual
                    );
                    let err = syn::Error::new(func_span, msg).to_compile_error();
                    return quote::quote! { #err #func_tokens };
                }
            }
            // All match — pass through.
            item
        }
        _ => {
            // Setup mode: emit the computed hashes for copy-paste.
            let accounts_line = match (
                args.accounts.as_deref(),
                args.accounts_file.as_deref(),
                actual_accounts_hash.as_deref(),
            ) {
                (Some(name), Some(file), Some(h)) => format!(
                    ", accounts = \"{}\", accounts_file = \"{}\", accounts_hash = \"{}\"",
                    name, file, h
                ),
                _ => String::new(),
            };
            let msg = format!(
                "qed(verified): no hash(es) provided for `{}`.\n\
                 Computed body hash: {}\n\
                 Computed spec hash: {}\n\
                 Usage: #[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\"{})]",
                fn_name,
                actual_body_hash,
                actual_spec_hash,
                spec_path,
                handler_name,
                actual_body_hash,
                actual_spec_hash,
                accounts_line,
            );
            let err = syn::Error::new(func_span, msg).to_compile_error();
            quote::quote! { #err #func_tokens }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SPEC: &str = r#"
spec Vault

handler deposit (i : AccountIdx) (amount : U128) : State.Active -> State.Active {
  requires state.accounts[i].active == 1 else SlotInactive
  effect {
    V += amount
    accounts[i].capital += amount
  }
}

handler withdraw (i : AccountIdx) (amount : U128) : State.Active -> State.Active {
  requires state.accounts[i].capital >= amount else InsufficientFunds
  effect {
    V -= amount
    accounts[i].capital -= amount
  }
}
"#;

    #[test]
    fn extract_deposit() {
        let block = extract_handler_block(SAMPLE_SPEC, "deposit").unwrap();
        assert!(block.starts_with('{'));
        assert!(block.ends_with('}'));
        assert!(block.contains("active == 1"));
        assert!(block.contains("V += amount"));
        // Should NOT pull in withdraw.
        assert!(!block.contains("withdraw"));
        assert!(!block.contains("capital >= amount"));
    }

    #[test]
    fn extract_withdraw() {
        let block = extract_handler_block(SAMPLE_SPEC, "withdraw").unwrap();
        assert!(block.contains("capital >= amount"));
        assert!(!block.contains("active == 1"));
    }

    #[test]
    fn missing_handler() {
        assert!(extract_handler_block(SAMPLE_SPEC, "nonexistent").is_none());
    }

    #[test]
    fn spec_hash_stable() {
        let h1 = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let h2 = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn spec_hash_different_for_different_handlers() {
        let h_dep = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let h_wit = spec_hash_for_handler(SAMPLE_SPEC, "withdraw").unwrap();
        assert_ne!(h_dep, h_wit);
    }

    #[test]
    fn spec_hash_changes_on_edit() {
        let h_orig = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let edited = SAMPLE_SPEC.replace("V += amount", "V += amount + 1");
        let h_edit = spec_hash_for_handler(&edited, "deposit").unwrap();
        assert_ne!(h_orig, h_edit);
    }

    #[test]
    fn spec_hash_tolerates_cosmetic_whitespace() {
        // v2.9 second-pass: cosmetic reformats (extra spaces, blank
        // lines, indentation changes) don't fire drift. Real edits
        // still do (covered by `spec_hash_changes_on_edit`).
        let h_orig = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let reformatted = SAMPLE_SPEC.replace("active == 1", "active   ==   1");
        let h_reformatted = spec_hash_for_handler(&reformatted, "deposit").unwrap();
        assert_eq!(h_orig, h_reformatted);
    }

    #[test]
    fn spec_hash_tolerates_added_line_comments() {
        let with_comment = SAMPLE_SPEC.replace("effect {", "// new comment, harmless\n  effect {");
        let h_orig = spec_hash_for_handler(SAMPLE_SPEC, "deposit").unwrap();
        let h_commented = spec_hash_for_handler(&with_comment, "deposit").unwrap();
        assert_eq!(h_orig, h_commented);
    }

    #[test]
    fn parse_all_args() {
        let attr: TokenStream = quote::quote! {
            verified,
            spec = "vault.qedspec",
            handler = "deposit",
            hash = "aaaaaaaaaaaaaaaa",
            spec_hash = "bbbbbbbbbbbbbbbb"
        };
        let args = parse_args(&attr).unwrap();
        assert_eq!(args.spec.as_deref(), Some("vault.qedspec"));
        assert_eq!(args.handler.as_deref(), Some("deposit"));
        assert_eq!(args.hash.as_deref(), Some("aaaaaaaaaaaaaaaa"));
        assert_eq!(args.spec_hash.as_deref(), Some("bbbbbbbbbbbbbbbb"));
    }

    #[test]
    fn parse_legacy_no_spec() {
        let attr: TokenStream = quote::quote! { verified, hash = "abc123def456789a" };
        let args = parse_args(&attr).unwrap();
        assert_eq!(args.spec, None);
        assert_eq!(args.handler, None);
        assert_eq!(args.hash.as_deref(), Some("abc123def456789a"));
        assert_eq!(args.spec_hash, None);
    }

    #[test]
    fn extract_handles_nested_braces() {
        let src = r#"
handler complex : State.Active -> State.Active {
  effect {
    if cond {
      x := 1
    }
  }
}
"#;
        let block = extract_handler_block(src, "complex").unwrap();
        assert!(block.contains("x := 1"));
        // Must include both inner closing braces.
        assert_eq!(block.matches('{').count(), block.matches('}').count());
    }

    #[test]
    fn extract_ignores_braces_in_comments() {
        let src = r#"
handler commented : State.Active -> State.Active {
  // This has a { brace in a comment
  effect {
    x := 1
  }
}
"#;
        let block = extract_handler_block(src, "commented").unwrap();
        assert!(block.contains("x := 1"));
        // The '{' inside the `// …` line should not unbalance the scanner.
        assert!(block.ends_with('}'));
    }
}
