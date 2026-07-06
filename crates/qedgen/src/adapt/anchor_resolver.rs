//! Forwarder resolution: classify a `#[program]` pub fn's tail expression to
//! find where the actual handler body lives. Production shapes: Inline (the
//! program fn IS the handler), free-fn `<path>::<fn>(args)` (incl. `<call>?;
//! Ok(())` and `?`-tail plumbing), type-assoc `<Type>::<method>(ctx, args)`,
//! and accounts-method `ctx.accounts.<method>(args)`. Custom dispatchers fall
//! to `Unrecognized`; `--handler <name>=<rust_path>` is the CLI override.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::anchor_project::Instruction;

/// Where the actual handler body lives.
#[derive(Clone)]
pub enum HandlerLocation {
    /// The `#[program]` mod fn itself is the handler — no forwarder.
    Inline {
        item_fn: syn::ItemFn,
        /// File declaring the `#[program]` mod (typically `src/lib.rs`).
        source_path: PathBuf,
    },
    /// A free `pub fn` reached via path forwarder (`module::fn(...)`).
    FreeFn {
        item_fn: syn::ItemFn,
        source_path: PathBuf,
    },
    /// Method reached via `ctx.accounts.<m>(...)` or `<Type>::<m>(ctx, ...)`.
    Method {
        item_fn: syn::ImplItemFn,
        source_path: PathBuf,
        /// Type the impl block is on: the `Context<X>`'s `X` for
        /// accounts-method, the written type for type-assoc.
        impl_type: String,
    },
    /// Unclassifiable forwarder; the `--handler` override is the escape hatch.
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

/// Resolve where an instruction's handler body lives. `lib_rs_path` backs
/// the Inline case; `program_root` roots the source walk for forwarders.
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
            // Resolve the method on the signature's Context<Ctx> type.
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
    // Accept only forwarder plumbing (Ok(...) wrappers, `?` propagation,
    // `<call>?; Ok(())`). Any leading user logic — require!, msg!, lets —
    // keeps the body Inline so its bytes flow into the spec hash.
    let stmts = &program_fn.block.stmts;
    let tail_expr = match extract_forwarder_tail(stmts) {
        Some(expr) => expr,
        None => return ForwarderKind::Inline,
    };

    // Strip a wrapping `Ok(...)` (`Ok(handler(ctx)?)`) then a trailing `?` —
    // both are forwarder plumbing.
    let unwrapped = unwrap_ok_tail(tail_expr).unwrap_or(tail_expr);
    let actual = unwrap_try(unwrapped).unwrap_or(unwrapped);

    match actual {
        syn::Expr::Call(call) => classify_call(call),
        syn::Expr::MethodCall(mcall) => classify_method_call(mcall),
        _ => ForwarderKind::Inline,
    }
}

/// Tail call expression of a pure-forwarder body: a single-expression body,
/// or `<call>?;` followed by `Ok(())` / `return Ok(())`. Anything else (lets,
/// require!, extra stmts) returns None → Inline, so user code keeps flowing
/// into the body hash.
fn extract_forwarder_tail(stmts: &[syn::Stmt]) -> Option<&syn::Expr> {
    match stmts.len() {
        0 => None,
        1 => match &stmts[0] {
            syn::Stmt::Expr(expr, _) => Some(expr),
            _ => None,
        },
        2 => {
            // `<call>?;` then `Ok(())` (optional `return`, optional `;`).
            let call_stmt = match &stmts[0] {
                syn::Stmt::Expr(expr, Some(_)) => expr,
                _ => return None,
            };
            // Must be a try-expr; otherwise the user hand-rolled validation
            // we shouldn't skip past.
            if !matches!(call_stmt, syn::Expr::Try(_)) {
                return None;
            }
            if !is_ok_unit_terminal(&stmts[1]) {
                return None;
            }
            Some(call_stmt)
        }
        _ => None,
    }
}

/// True for the accepted terminals after `<call>?;`: `Ok(())` or
/// `return Ok(())`, with or without `;`.
fn is_ok_unit_terminal(stmt: &syn::Stmt) -> bool {
    let expr = match stmt {
        syn::Stmt::Expr(expr, _) => expr,
        _ => return false,
    };
    let unwrapped = match expr {
        syn::Expr::Return(ret) => match &ret.expr {
            Some(inner) => inner.as_ref(),
            None => return false,
        },
        other => other,
    };
    let call = match unwrapped {
        syn::Expr::Call(c) => c,
        _ => return false,
    };
    let path = match &*call.func {
        syn::Expr::Path(p) => &p.path,
        _ => return false,
    };
    let last = match path.segments.last() {
        Some(s) => s,
        None => return false,
    };
    if last.ident != "Ok" {
        return false;
    }
    // Only `Ok(())` — `Ok(value)` propagates a produced value, which is
    // no longer pure forwarding.
    if call.args.len() != 1 {
        return false;
    }
    matches!(&call.args[0], syn::Expr::Tuple(t) if t.elems.is_empty())
}

/// `<call>?` → `<call>`; otherwise None.
fn unwrap_try(expr: &syn::Expr) -> Option<&syn::Expr> {
    match expr {
        syn::Expr::Try(t) => Some(&t.expr),
        _ => None,
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

    // PascalCase second-to-last segment → Type::method; else module path +
    // free fn.
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

/// First-char-uppercase heuristic — sufficient since modules are snake_case
/// and types PascalCase per Rust convention.
fn is_pascal_case(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

// ----------------------------------------------------------------------------
// Resolvers — find the actual ItemFn / ImplItemFn in the project sources
// ----------------------------------------------------------------------------

/// Walk `src/` for a `pub fn <fn_name>` whose module path matches the
/// forwarder's path; `Unrecognized` when absent. `pub` so `--handler`
/// overrides (hand-supplied free-fn forwarders) reuse the same lookup.
pub fn resolve_free_fn(
    module_path: &[String],
    fn_name: &str,
    program_root: &Path,
    lib_rs_path: &Path,
) -> Result<HandlerLocation> {
    let src_dir = program_root.join("src");
    let candidates = walk_rust_files(&src_dir);

    for path in &candidates {
        // Skip lib.rs for path-qualified lookups — its program mod shouldn't
        // shadow handler bodies. (Method resolution still searches lib.rs.)
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

        // Seed with the file's own module path so `instructions::buy`
        // resolves against `src/instructions/buy.rs`.
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

/// Recursive `.rs` walk, sorted for determinism.
fn walk_rust_files(dir: &Path) -> Vec<PathBuf> {
    crate::fs_walk::collect_rs_files(dir, crate::fs_walk::DEFAULT_SKIP_DIRS)
}

/// Find `pub fn <fn_name>` whose enclosing module path matches
/// `module_path`. `initial_path` seeds the file's own module path; nested
/// `pub mod` blocks extend it. Empty `module_path` matches anywhere.
fn find_pub_fn(
    file: &syn::File,
    module_path: &[String],
    fn_name: &str,
    initial_path: &[String],
) -> Option<syn::ItemFn> {
    find_pub_fn_in_items(&file.items, module_path, fn_name, initial_path)
}

/// `.rs` path under `src/` → Rust module path (Cargo conventions):
/// `src/lib.rs` → `[]`; `src/foo.rs` / `src/foo/mod.rs` → `["foo"]`;
/// `src/foo/bar.rs` → `["foo", "bar"]`. `[]` for files outside `src_dir`.
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
    // `<target_path>::<fn_name>` resolves either (a) directly at
    // `<target_path>` (`instructions/mod.rs` or `instructions.rs`), or
    // (b) at `<target_path>/<fn_name>.rs` re-exported via `pub use` from the
    // parent mod — the file-named-after-the-fn convention modern Anchor
    // scaffolds use.
    let path_matches = target_path.is_empty()
        || target_path == current_path
        || (current_path.len() == target_path.len() + 1
            && current_path.starts_with(target_path)
            && current_path.last().map(|s| s.as_str()) == Some(fn_name));
    for item in items {
        match item {
            syn::Item::Fn(item_fn)
                if matches!(item_fn.vis, syn::Visibility::Public(_))
                    && item_fn.sig.ident == fn_name
                    && path_matches =>
            {
                return Some(item_fn.clone());
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

/// `impl <type_name>` block containing `pub fn <method_name>`; generics
/// (`impl<'info> Buy<'info>`) are stripped before matching.
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
        // Match the bare type name; ignore generics.
        if let Some(last) = type_path.path.segments.last() {
            return last.ident == type_name;
        }
    }
    false
}

/// `Context<X>` in a program_fn signature → bare `X`; None when the first
/// arg isn't a `Context<...>`.
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
        // Multi-statement bodies with real logic are always Inline.
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
        // `Ok(handler(ctx)?)` — both wrappers strip cleanly.
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
        match kind {
            ForwarderKind::FreeFn {
                module_path,
                fn_name,
            } => {
                assert_eq!(module_path, vec!["instructions", "buy"]);
                assert_eq!(fn_name, "handler");
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn classifies_try_tail_as_forwarder() {
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>) -> Result<()> {
                    instructions::buy::handler(ctx)?
                }
            }
        "#,
        );
        match kind {
            ForwarderKind::FreeFn {
                module_path,
                fn_name,
            } => {
                assert_eq!(module_path, vec!["instructions", "buy"]);
                assert_eq!(fn_name, "handler");
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn classifies_two_stmt_propagate_then_ok_as_forwarder() {
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
                    instructions::buy::handler(ctx, amount)?;
                    Ok(())
                }
            }
        "#,
        );
        match kind {
            ForwarderKind::FreeFn {
                module_path,
                fn_name,
            } => {
                assert_eq!(module_path, vec!["instructions", "buy"]);
                assert_eq!(fn_name, "handler");
            }
            other => panic!("expected FreeFn, got {:?}", other),
        }
    }

    #[test]
    fn classifies_two_stmt_with_return_ok_as_forwarder() {
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>) -> Result<()> {
                    instructions::buy::handler(ctx)?;
                    return Ok(());
                }
            }
        "#,
        );
        assert!(matches!(kind, ForwarderKind::FreeFn { .. }));
    }

    #[test]
    fn classifies_three_stmt_forwarder_as_inline() {
        // User logic between the forwarder call and the return must flow
        // into the body hash → Inline.
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>) -> Result<()> {
                    instructions::buy::handler(ctx)?;
                    msg!("buy done");
                    Ok(())
                }
            }
        "#,
        );
        assert!(matches!(kind, ForwarderKind::Inline));
    }

    #[test]
    fn classifies_let_binding_then_ok_as_inline() {
        // A let-binding is user logic, not plumbing — stays in the body hash.
        let kind = classify(
            r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>) -> Result<()> {
                    let _r = instructions::buy::handler(ctx)?;
                    Ok(())
                }
            }
        "#,
        );
        assert!(matches!(kind, ForwarderKind::Inline));
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
        // `Context<'info, Buy<'info>>` → `Buy`.
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

    // End-to-end on disk: walk_rust_files + find_pub_fn integration.
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
