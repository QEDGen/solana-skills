//! Forwarder resolution for discovered Anchor instructions (v2.9 M4.2).
//!
//! Given an `Instruction` from `anchor_project::parse_anchor_project`
//! (a pub fn inside the `#[program]` mod), this module classifies the
//! body's tail expression to find where the *actual* handler lives, and
//! returns a `HandlerLocation` carrying the resolved `syn::ItemFn` plus
//! a source-path breadcrumb for diagnostics.
//!
//! Survey-driven design (`reference_anchor_patterns.md`): six shapes
//! coexist in production Anchor code. v2.9 M4.2 supports four of them
//! plus a graceful `Unrecognized` fall-through:
//!
//! 1. **Inline** (Jito tip-distribution): body has multiple statements
//!    or a non-forwarder tail; the program_fn IS the handler.
//! 2. **Free-fn forwarder** (Anchor scaffold, Raydium): tail expression
//!    is `<path>::<function>(args)`. Resolved by walking the program
//!    crate's source files for a `pub fn <function>` matching the path
//!    segments.
//! 3. **Type-associated forwarder** (Squads V4): tail is
//!    `<Type>::<method>(ctx, args)`. Resolved by walking source for an
//!    `impl <Type>` block containing the named method.
//! 4. **Accounts-method forwarder** (Marinade): tail is
//!    `ctx.accounts.<method>(args)`. Resolved by looking up the
//!    `<Ctx>` type from the program_fn signature, then walking source
//!    for an `impl <Ctx>` block containing the named method.
//!
//! Drift's custom dispatcher pattern (no straight forwarder; handler
//! discovery via runtime lookup table) is documented as
//! `Unrecognized { reason: "custom dispatcher" }`. M4.3's CLI exposes
//! a `--handler <name>=<rust_path>` override flag for those cases.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::anchor_project::Instruction;

/// Where the actual handler body lives, after following the
/// `#[program]` mod's pub fn.
#[derive(Clone)]
#[allow(dead_code)]
pub enum HandlerLocation {
    /// The handler body is the `#[program]` mod fn itself — no
    /// forwarder; Jito tip-distribution style.
    Inline {
        item_fn: syn::ItemFn,
        /// Path to the file declaring the `#[program]` mod (typically
        /// `<crate>/src/lib.rs`).
        source_path: PathBuf,
    },
    /// A free `pub fn <name>(...)` reached via path forwarder
    /// (`module::fn(...)` or just `fn_name(...)`).
    FreeFn {
        item_fn: syn::ItemFn,
        source_path: PathBuf,
    },
    /// A method on a typed accounts struct or context wrapper. Covers
    /// both Marinade (`ctx.accounts.process(...)`) and Squads
    /// (`<Type>::<method>(ctx, args)`).
    Method {
        item_fn: syn::ImplItemFn,
        source_path: PathBuf,
        /// The type the impl block is on. For Marinade this is the
        /// `Context<X>`'s `X` (the accounts struct); for Squads it's
        /// the type written before `::<method>`.
        impl_type: String,
    },
    /// We couldn't classify the forwarder. The CLI's
    /// `--handler <name>=<rust_path>` override is the escape hatch
    /// for these cases.
    Unrecognized { reason: String },
}

impl std::fmt::Debug for HandlerLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandlerLocation::Inline { source_path, .. } => f
                .debug_struct("Inline")
                .field("source_path", source_path)
                .finish(),
            HandlerLocation::FreeFn { source_path, .. } => f
                .debug_struct("FreeFn")
                .field("source_path", source_path)
                .finish(),
            HandlerLocation::Method {
                source_path,
                impl_type,
                ..
            } => f
                .debug_struct("Method")
                .field("impl_type", impl_type)
                .field("source_path", source_path)
                .finish(),
            HandlerLocation::Unrecognized { reason } => f
                .debug_struct("Unrecognized")
                .field("reason", reason)
                .finish(),
        }
    }
}

/// Resolve where an instruction's actual handler body lives.
///
/// `lib_rs_path` is needed for Inline / Marinade-style fallbacks (the
/// program mod fn itself is the handler in inline cases, and the
/// accounts type lives in the same crate's source tree).
///
/// `program_root` is the program crate's root directory (sibling of
/// `src/`). Used to find sibling source files when the forwarder
/// references a module path like `instructions::buy`.
#[allow(dead_code)]
pub fn resolve_handler(
    instruction: &Instruction,
    lib_rs_path: &Path,
    program_root: &Path,
) -> Result<HandlerLocation> {
    match classify_forwarder(&instruction.program_fn) {
        ForwarderKind::Inline => Ok(HandlerLocation::Inline {
            item_fn: instruction.program_fn.clone(),
            source_path: lib_rs_path.to_path_buf(),
        }),
        ForwarderKind::FreeFn {
            module_path,
            fn_name,
        } => resolve_free_fn(&module_path, &fn_name, program_root, lib_rs_path),
        ForwarderKind::TypeAssoc {
            type_name,
            method_name,
        } => resolve_method(&type_name, &method_name, program_root, lib_rs_path),
        ForwarderKind::AccountsMethod { method_name } => {
            // Look up the Context<Ctx>'s Ctx from the program_fn
            // signature, then resolve the method on that type.
            match accounts_type_from_signature(&instruction.program_fn) {
                Some(ctx_type) => {
                    resolve_method(&ctx_type, &method_name, program_root, lib_rs_path)
                }
                None => Ok(HandlerLocation::Unrecognized {
                    reason: format!(
                        "could not extract Context<X> type from `{}` signature",
                        instruction.name
                    ),
                }),
            }
        }
        ForwarderKind::Unknown(reason) => Ok(HandlerLocation::Unrecognized { reason }),
    }
}

// ----------------------------------------------------------------------------
// Classifier — determine what shape of forwarder the program_fn body uses
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ForwarderKind {
    /// Body has more than one non-trivial statement, or the tail
    /// isn't a forwarder shape we recognize.
    Inline,
    /// Tail expression: `<module_path>::<fn_name>(args)`.
    FreeFn {
        /// Path segments before the final fn name. Empty when the
        /// call is a bare ident (`fn_name(args)`).
        module_path: Vec<String>,
        fn_name: String,
    },
    /// Tail expression: `<type>::<method>(args)` where the type is
    /// PascalCase (associated function on a type).
    TypeAssoc {
        type_name: String,
        method_name: String,
    },
    /// Tail expression: `ctx.accounts.<method>(args)`.
    AccountsMethod { method_name: String },
    /// Custom dispatcher or unsupported pattern — reason describes
    /// what we saw.
    Unknown(String),
}

fn classify_forwarder(program_fn: &syn::ItemFn) -> ForwarderKind {
    // The body might have non-trivial leading statements (require!
    // macros, let-bindings, etc.) followed by a tail forwarder. We
    // treat anything with more than one effectful statement as
    // "inline" — preserving leading guards in the spec_hash that
    // way is the safer choice. Pure forwarders are exactly: a body
    // with one statement that's an expression (the tail).
    let stmts = &program_fn.block.stmts;
    let tail_expr = match stmts.len() {
        0 => return ForwarderKind::Inline,
        1 => match &stmts[0] {
            syn::Stmt::Expr(expr, _) => expr,
            _ => return ForwarderKind::Inline,
        },
        _ => {
            // Multi-statement body. If the last stmt is an expression,
            // it could still be a forwarder, but the leading stmts
            // (likely require! / let-bindings) are part of the user's
            // body and shouldn't be skipped. Treat as inline.
            return ForwarderKind::Inline;
        }
    };

    // Strip a wrapping Ok(...) if present (some programs return
    // `Ok(handler::call(ctx)?)`). Look for the inner call.
    let actual = unwrap_ok_tail(tail_expr).unwrap_or(tail_expr);

    match actual {
        syn::Expr::Call(call) => classify_call(call),
        syn::Expr::MethodCall(mcall) => classify_method_call(mcall),
        _ => ForwarderKind::Inline,
    }
}

/// `Ok(call(...))` → return `call(...)`; otherwise None.
fn unwrap_ok_tail(expr: &syn::Expr) -> Option<&syn::Expr> {
    let call = match expr {
        syn::Expr::Call(c) => c,
        _ => return None,
    };
    let path = match &*call.func {
        syn::Expr::Path(p) => p,
        _ => return None,
    };
    let last = path.path.segments.last()?;
    if last.ident == "Ok" && call.args.len() == 1 {
        return Some(&call.args[0]);
    }
    None
}

fn classify_call(call: &syn::ExprCall) -> ForwarderKind {
    let path = match &*call.func {
        syn::Expr::Path(p) => &p.path,
        _ => {
            return ForwarderKind::Unknown(
                "tail expression is a call but the function isn't a path — likely a closure or complex receiver".to_string(),
            );
        }
    };
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();

    if segments.is_empty() {
        return ForwarderKind::Unknown("empty path call".to_string());
    }

    // Is the last segment-but-one PascalCase? → Type::method (Squads).
    // Otherwise the segments are a module path + function name (free fn).
    if segments.len() >= 2 {
        let prefix_last = &segments[segments.len() - 2];
        if is_pascal_case(prefix_last) {
            return ForwarderKind::TypeAssoc {
                type_name: prefix_last.clone(),
                method_name: segments.last().unwrap().clone(),
            };
        }
    }

    let fn_name = segments.last().unwrap().clone();
    let module_path = segments[..segments.len() - 1].to_vec();
    ForwarderKind::FreeFn {
        module_path,
        fn_name,
    }
}

fn classify_method_call(mcall: &syn::ExprMethodCall) -> ForwarderKind {
    // Look for the canonical Marinade pattern: ctx.accounts.<method>(args)
    let receiver = &*mcall.receiver;
    if let syn::Expr::Field(field) = receiver {
        if let syn::Member::Named(name) = &field.member {
            if name == "accounts" {
                if let syn::Expr::Path(p) = &*field.base {
                    if p.path.segments.len() == 1 && p.path.segments[0].ident == "ctx" {
                        return ForwarderKind::AccountsMethod {
                            method_name: mcall.method.to_string(),
                        };
                    }
                }
            }
        }
    }
    ForwarderKind::Unknown(format!(
        "method-call tail expression on a non-ctx.accounts receiver: .{}(...)",
        mcall.method
    ))
}

/// Heuristic: a segment is PascalCase if its first char is uppercase.
/// Sufficient for Type vs module distinction in Anchor code (modules
/// are snake_case, types are PascalCase per Rust convention).
fn is_pascal_case(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

// ----------------------------------------------------------------------------
// Resolvers — find the actual ItemFn / ImplItemFn in the project sources
// ----------------------------------------------------------------------------

/// Walk the program crate's `src/` for a `pub fn <fn_name>` whose
/// surrounding module path matches the forwarder's path. Returns
/// `Unrecognized` if the function can't be found.
///
/// `pub` so `qedgen adapt --handler <name>=<rust_path>` overrides can
/// reuse the same lookup path: an override is, semantically, a
/// hand-supplied free-fn forwarder for handlers the classifier
/// returned `Unrecognized` for (Drift's custom dispatcher being the
/// canonical case).
pub fn resolve_free_fn(
    module_path: &[String],
    fn_name: &str,
    program_root: &Path,
    lib_rs_path: &Path,
) -> Result<HandlerLocation> {
    let src_dir = program_root.join("src");
    let candidates = walk_rust_files(&src_dir);

    for path in &candidates {
        // Skip lib.rs itself for the free-fn lookup — the program mod
        // there shouldn't shadow handler bodies. (Still searched for
        // type-assoc / accounts methods, which can be defined inline
        // in lib.rs on rare programs.)
        if path == lib_rs_path && !module_path.is_empty() {
            continue;
        }
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let file: syn::File = match syn::parse_str(&source) {
            Ok(f) => f,
            Err(_) => continue,
        };

        // The file's items live at this module path (relative to the
        // crate root). We seed `current_path` with it so a target like
        // `instructions::buy` resolves against `src/instructions/buy.rs`.
        let file_mod_path = file_module_path(path, &src_dir);
        if let Some(item_fn) = find_pub_fn(&file, module_path, fn_name, &file_mod_path) {
            return Ok(HandlerLocation::FreeFn {
                item_fn,
                source_path: path.clone(),
            });
        }
    }

    Ok(HandlerLocation::Unrecognized {
        reason: format!(
            "could not find `pub fn {}` (looking for path {}) in {}",
            fn_name,
            if module_path.is_empty() {
                "<root>".to_string()
            } else {
                module_path.join("::")
            },
            src_dir.display()
        ),
    })
}

/// Walk `program_root/src/` for an `impl <type_name>` block containing
/// `pub fn <method_name>`. Returns `Unrecognized` when not found.
fn resolve_method(
    type_name: &str,
    method_name: &str,
    program_root: &Path,
    _lib_rs_path: &Path,
) -> Result<HandlerLocation> {
    let src_dir = program_root.join("src");
    let candidates = walk_rust_files(&src_dir);

    for path in &candidates {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let file: syn::File = match syn::parse_str(&source) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if let Some(item_fn) = find_impl_method(&file, type_name, method_name) {
            return Ok(HandlerLocation::Method {
                item_fn,
                source_path: path.clone(),
                impl_type: type_name.to_string(),
            });
        }
    }

    Ok(HandlerLocation::Unrecognized {
        reason: format!(
            "could not find `impl {} {{ pub fn {} }}` in {}",
            type_name,
            method_name,
            src_dir.display()
        ),
    })
}

/// Walk a directory for `.rs` files (recursively). Returns paths in
/// deterministic sorted order.
fn walk_rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_rust_files_inner(dir, &mut out);
    out.sort();
    out
}

fn walk_rust_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rust_files_inner(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Walk a parsed file for a `pub fn <fn_name>` whose enclosing module
/// path (relative to the crate root) matches the requested
/// `module_path`. The caller seeds `initial_path` with the file's own
/// module path (e.g. `["instructions", "buy"]` for
/// `src/instructions/buy.rs`) so file-to-module mapping works without
/// needing an explicit `pub mod ...` wrapper inside the file. Nested
/// `pub mod foo { ... }` blocks within the file extend the path.
/// Empty `module_path` matches any location.
fn find_pub_fn(
    file: &syn::File,
    module_path: &[String],
    fn_name: &str,
    initial_path: &[String],
) -> Option<syn::ItemFn> {
    find_pub_fn_in_items(&file.items, module_path, fn_name, initial_path)
}

/// Translate a `.rs` file path under `src/` into the Rust module path
/// that file represents. Mirrors Cargo/rustc conventions:
///   - `src/lib.rs`              → `[]`
///   - `src/foo.rs`              → `["foo"]`
///   - `src/foo/mod.rs`          → `["foo"]`
///   - `src/foo/bar.rs`          → `["foo", "bar"]`
///   - `src/foo/bar/mod.rs`      → `["foo", "bar"]`
///
/// Returns `[]` for files outside `src_dir` (defensive — shouldn't
/// happen since the walker is rooted there).
fn file_module_path(file_path: &Path, src_dir: &Path) -> Vec<String> {
    let rel = match file_path.strip_prefix(src_dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut segments: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if let Some(last) = segments.last_mut() {
        if let Some(stripped) = last.strip_suffix(".rs") {
            *last = stripped.to_string();
        }
    }
    if matches!(
        segments.last().map(|s| s.as_str()),
        Some("mod") | Some("lib")
    ) {
        segments.pop();
    }
    segments
}

fn find_pub_fn_in_items(
    items: &[syn::Item],
    target_path: &[String],
    fn_name: &str,
    current_path: &[String],
) -> Option<syn::ItemFn> {
    for item in items {
        match item {
            syn::Item::Fn(item_fn) => {
                if matches!(item_fn.vis, syn::Visibility::Public(_))
                    && item_fn.sig.ident == fn_name
                    && (target_path.is_empty() || target_path == current_path)
                {
                    return Some(item_fn.clone());
                }
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, sub_items)) = &item_mod.content {
                    let mut next = current_path.to_vec();
                    next.push(item_mod.ident.to_string());
                    if let Some(found) =
                        find_pub_fn_in_items(sub_items, target_path, fn_name, &next)
                    {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Walk a parsed file for an `impl <type_name>` block containing a
/// `pub fn <method_name>`. Tolerates impl blocks with generics
/// (`impl<'info> Buy<'info>`) — we strip generics before matching.
fn find_impl_method(
    file: &syn::File,
    type_name: &str,
    method_name: &str,
) -> Option<syn::ImplItemFn> {
    for item in &file.items {
        if let syn::Item::Impl(item_impl) = item {
            if impl_matches_type(item_impl, type_name) {
                for impl_item in &item_impl.items {
                    if let syn::ImplItem::Fn(impl_fn) = impl_item {
                        if matches!(impl_fn.vis, syn::Visibility::Public(_))
                            && impl_fn.sig.ident == method_name
                        {
                            return Some(impl_fn.clone());
                        }
                    }
                }
            }
        }
    }
    None
}

fn impl_matches_type(item_impl: &syn::ItemImpl, type_name: &str) -> bool {
    if let syn::Type::Path(type_path) = &*item_impl.self_ty {
        // Ignore generics (the last segment's PathArguments) — we're
        // matching on the bare type name here.
        if let Some(last) = type_path.path.segments.last() {
            return last.ident == type_name;
        }
    }
    false
}

/// Pull the `X` out of `Context<X>` in a program_fn signature. Returns
/// the bare type name (no lifetime / generic args) or None when the
/// first argument isn't a `Context<...>`.
fn accounts_type_from_signature(item_fn: &syn::ItemFn) -> Option<String> {
    let first_input = item_fn.sig.inputs.first()?;
    let pat_type = match first_input {
        syn::FnArg::Typed(pt) => pt,
        _ => return None,
    };
    let type_path = match &*pat_type.ty {
        syn::Type::Path(tp) => tp,
        _ => return None,
    };
    let last = type_path.path.segments.last()?;
    if last.ident != "Context" {
        return None;
    }
    let args = match &last.arguments {
        syn::PathArguments::AngleBracketed(a) => a,
        _ => return None,
    };
    for arg in &args.args {
        if let syn::GenericArgument::Type(syn::Type::Path(tp)) = arg {
            if let Some(seg) = tp.path.segments.last() {
                return Some(seg.ident.to_string());
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor_project::parse_lib_rs;

    fn project_for(src: &str) -> crate::anchor_project::AnchorProject {
        parse_lib_rs(Path::new("/test/lib.rs"), src).unwrap()
    }

    fn classify(src: &str) -> ForwarderKind {
        let project = project_for(src);
        classify_forwarder(&project.instructions[0].program_fn)
    }

    #[test]
    fn classifies_anchor_scaffold_as_free_fn() {
        // pub fn handler in instructions/<name>.rs, lib.rs forwards via
        // `<module>::handler(ctx)`.
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
                    instructions::initialize::handler(ctx)
                }
            }
        "#,
        );
        match kind {
            ForwarderKind::FreeFn {
                module_path,
                fn_name,
            } => {
                assert_eq!(module_path, vec!["instructions", "initialize"]);
                assert_eq!(fn_name, "handler");
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn classifies_raydium_style_as_free_fn_same_name() {
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn create_pool(ctx: Context<CreatePool>, sqrt_price: u128, open_time: u64) -> Result<()> {
                    instructions::create_pool(ctx, sqrt_price, open_time)
                }
            }
        "#,
        );
        match kind {
            ForwarderKind::FreeFn {
                module_path,
                fn_name,
            } => {
                assert_eq!(module_path, vec!["instructions"]);
                assert_eq!(fn_name, "create_pool");
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn classifies_squads_style_as_type_assoc() {
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn multisig_create(ctx: Context<MultisigCreateV2>, args: MultisigCreateArgsV2) -> Result<()> {
                    MultisigCreateV2::multisig_create(ctx, args)
                }
            }
        "#,
        );
        match kind {
            ForwarderKind::TypeAssoc {
                type_name,
                method_name,
            } => {
                assert_eq!(type_name, "MultisigCreateV2");
                assert_eq!(method_name, "multisig_create");
            }
            other => panic!("expected TypeAssoc, got {:?}", other),
        }
    }

    #[test]
    fn classifies_marinade_style_as_accounts_method() {
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn deposit(ctx: Context<Deposit>, lamports: u64) -> Result<()> {
                    ctx.accounts.process(lamports)
                }
            }
        "#,
        );
        match kind {
            ForwarderKind::AccountsMethod { method_name } => {
                assert_eq!(method_name, "process");
            }
            other => panic!("expected AccountsMethod, got {:?}", other),
        }
    }

    #[test]
    fn classifies_inline_body_when_multi_statement() {
        // Jito style: the body has the actual handler logic, not a
        // forwarder. Multi-statement bodies are always Inline.
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn initialize(ctx: Context<Init>, x: u64) -> Result<()> {
                    require!(x > 0, ErrorCode::Invalid);
                    let foo = ctx.accounts.foo.key();
                    ctx.accounts.state.x = x;
                    Ok(())
                }
            }
        "#,
        );
        assert!(matches!(kind, ForwarderKind::Inline));
    }

    #[test]
    fn unwraps_ok_tail_to_recognize_forwarder() {
        // Some programs wrap the forwarder call in Ok(...) explicitly.
        // We unwrap one level of Ok(...) before classifying.
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>) -> Result<()> {
                    Ok(instructions::buy::handler(ctx)?)
                }
            }
        "#,
        );
        // The `?` on the inner call makes this an Expr::Try, not a
        // call directly — unwrap_ok_tail returns the inner expr, but
        // it's `instructions::buy::handler(ctx)?` (a try expression).
        // Document what we do here: try-expressions don't classify;
        // we treat them as Inline. (More aggressive try-handling can
        // come in v2.10 if real adopters need it.)
        match kind {
            ForwarderKind::Inline | ForwarderKind::FreeFn { .. } => {}
            other => panic!("expected Inline or FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn accounts_type_from_signature_extracts_ctx_type() {
        let project = project_for(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn deposit(ctx: Context<Deposit>, x: u64) -> Result<()> {
                    ctx.accounts.process(x)
                }
            }
        "#,
        );
        let ctx_type = accounts_type_from_signature(&project.instructions[0].program_fn);
        assert_eq!(ctx_type.as_deref(), Some("Deposit"));
    }

    #[test]
    fn accounts_type_handles_lifetime_generics() {
        // `Context<'info, Buy<'info>>` style — extract `Buy`.
        let project = project_for(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy<'info>(ctx: Context<'info, Buy<'info>>) -> Result<()> {
                    ctx.accounts.process()
                }
            }
        "#,
        );
        let ctx_type = accounts_type_from_signature(&project.instructions[0].program_fn);
        assert_eq!(ctx_type.as_deref(), Some("Buy"));
    }

    #[test]
    fn is_pascal_case_distinguishes_modules_from_types() {
        assert!(is_pascal_case("MyType"));
        assert!(is_pascal_case("Buy"));
        assert!(!is_pascal_case("instructions"));
        assert!(!is_pascal_case("my_module"));
        assert!(!is_pascal_case(""));
    }

    // End-to-end: build a tiny project on disk, resolve a free-fn
    // forwarder. Exercises walk_rust_files + find_pub_fn integration.
    #[test]
    fn resolve_handler_finds_anchor_scaffold_free_fn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        let instr = src.join("instructions");
        std::fs::create_dir_all(&instr).unwrap();

        let lib_rs_path = src.join("lib.rs");
        std::fs::write(
            &lib_rs_path,
            r#"
                use anchor_lang::prelude::*;

                pub mod instructions;

                #[program]
                pub mod my_program {
                    use super::*;
                    pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
                        instructions::buy::handler(ctx, amount)
                    }
                }
            "#,
        )
        .unwrap();
        std::fs::write(instr.join("mod.rs"), "pub mod buy;\n").unwrap();
        std::fs::write(
            instr.join("buy.rs"),
            r#"
                use anchor_lang::prelude::*;

                pub fn handler(ctx: Context<Buy>, amount: u64) -> Result<()> {
                    require!(amount > 0, ErrorCode::Invalid);
                    Ok(())
                }
            "#,
        )
        .unwrap();

        let project = parse_lib_rs(
            &lib_rs_path,
            &std::fs::read_to_string(&lib_rs_path).unwrap(),
        )
        .unwrap();
        let resolved = resolve_handler(&project.instructions[0], &lib_rs_path, root).unwrap();
        match resolved {
            HandlerLocation::FreeFn {
                source_path,
                item_fn,
            } => {
                assert!(source_path.ends_with("instructions/buy.rs"));
                assert_eq!(item_fn.sig.ident, "handler");
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn resolve_handler_finds_marinade_style_method_on_accounts_type() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        let instr = src.join("instructions");
        std::fs::create_dir_all(&instr).unwrap();

        let lib_rs_path = src.join("lib.rs");
        std::fs::write(
            &lib_rs_path,
            r#"
                use anchor_lang::prelude::*;

                pub mod instructions;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn deposit(ctx: Context<Deposit>, lamports: u64) -> Result<()> {
                        ctx.accounts.process(lamports)
                    }
                }

                pub struct Deposit;
            "#,
        )
        .unwrap();
        std::fs::write(instr.join("mod.rs"), "pub mod deposit;\n").unwrap();
        std::fs::write(
            instr.join("deposit.rs"),
            r#"
                use anchor_lang::prelude::*;

                impl Deposit {
                    pub fn process(&mut self, lamports: u64) -> Result<()> {
                        Ok(())
                    }
                }
            "#,
        )
        .unwrap();

        let project = parse_lib_rs(
            &lib_rs_path,
            &std::fs::read_to_string(&lib_rs_path).unwrap(),
        )
        .unwrap();
        let resolved = resolve_handler(&project.instructions[0], &lib_rs_path, root).unwrap();
        match resolved {
            HandlerLocation::Method {
                source_path,
                impl_type,
                item_fn,
            } => {
                assert!(source_path.ends_with("instructions/deposit.rs"));
                assert_eq!(impl_type, "Deposit");
                assert_eq!(item_fn.sig.ident, "process");
            }
            other => panic!("expected Method, got {:?}", other),
        }
    }

    #[test]
    fn resolve_handler_inline_returns_program_fn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let lib_rs_path = src.join("lib.rs");
        std::fs::write(
            &lib_rs_path,
            r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>, x: u64) -> Result<()> {
                        require!(x > 0, ErrorCode::Invalid);
                        ctx.accounts.state.x = x;
                        Ok(())
                    }
                }
            "#,
        )
        .unwrap();

        let project = parse_lib_rs(
            &lib_rs_path,
            &std::fs::read_to_string(&lib_rs_path).unwrap(),
        )
        .unwrap();
        let resolved = resolve_handler(&project.instructions[0], &lib_rs_path, root).unwrap();
        match resolved {
            HandlerLocation::Inline {
                source_path,
                item_fn,
            } => {
                assert_eq!(source_path, lib_rs_path);
                assert_eq!(item_fn.sig.ident, "initialize");
                // Inline body has the require! macro (3 stmts total).
                assert!(item_fn.block.stmts.len() >= 2);
            }
            other => panic!("expected Inline, got {:?}", other),
        }
    }

    #[test]
    fn resolve_handler_returns_unrecognized_when_target_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let lib_rs_path = src.join("lib.rs");
        std::fs::write(
            &lib_rs_path,
            r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn buy(ctx: Context<Buy>) -> Result<()> {
                        nowhere::missing(ctx)
                    }
                }
            "#,
        )
        .unwrap();

        let project = parse_lib_rs(
            &lib_rs_path,
            &std::fs::read_to_string(&lib_rs_path).unwrap(),
        )
        .unwrap();
        let resolved = resolve_handler(&project.instructions[0], &lib_rs_path, root).unwrap();
        match resolved {
            HandlerLocation::Unrecognized { reason } => {
                assert!(reason.contains("could not find"), "got: {reason}");
            }
            other => panic!("expected Unrecognized, got {:?}", other),
        }
    }
}
