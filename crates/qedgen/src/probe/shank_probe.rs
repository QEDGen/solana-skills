//! `qedgen probe --bootstrap` — Shank-style native dispatcher discovery
//! (v2.20 §S2.1).
//!
//! Native Solana programs that pre-date Anchor (or deliberately opt
//! out) almost always concentrate dispatch in a single top-level
//! `process_instruction` fn whose body is a `match` over an
//! `Instruction` enum deserialised from the `instruction_data` slice.
//! Each arm calls one `process_*` handler. The probe walks `lib.rs`
//! (or any file under `src/` containing an `entrypoint!(...)`
//! invocation), recognises this shape, and emits the per-arm handler
//! list the auditor subagent needs.
//!
//! Heuristic (PRD §S2.1):
//!
//! 1. Top-level `fn process_instruction(_: &Pubkey, _: &[AccountInfo],
//!    _: &[u8]) -> ProgramResult` (or `Result<...>`).
//! 2. Body contains a top-level `match <Ident>` expression where
//!    `<Ident>` is bound — within the same fn body — from a `try_from`
//!    / `try_from_primitive!` style call on `instruction_data` (or
//!    any slice derived from it).
//! 3. Each arm patterns as `<Enum>::<Variant>` (with or without
//!    fields/struct shapes), and the arm body calls a single
//!    `process_*` function.
//!
//! This matches the Phoenix-style / Solana token-program-style
//! pre-Anchor central match. It does NOT match Anchor (caller routes
//! via the IDL extractor), Pinocchio (dedicated `pinocchio_probe`),
//! or Quasar (codegen markers preempt detection in `probe::run_bootstrap`).
//!
//! The probe is **deterministic AST pattern matching only** —
//! semantic interpretation (does the handler actually validate the
//! signer? does the enum live where we think?) is the agent's job.
//! Per `feedback_agent_lsp_substrate`, we emit structured handles and
//! let rust-analyzer do the impl reading.
//!
//! Output is `ShankCatalogue`, surfaced into the probe envelope as
//! `dispatcher_kind: "shank_central_match"` plus a populated
//! `handlers[]` list.

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use syn::{
    Expr, ExprCall, ExprMethodCall, FnArg, ImplItem, Item, ItemFn, Local, Pat, PathSegment, Stmt,
    Type, TypeReference,
};

/// One arm of a central-match dispatcher → one handler entry.
#[derive(Debug, Clone, Serialize)]
pub struct ShankHandler {
    /// Human-readable handler name. Derived from the variant identifier
    /// (e.g. `InitializeMarket`).
    pub name: String,
    /// Full enum-path string as it appeared in the arm pattern,
    /// e.g. `MarketInstruction::InitializeMarket`.
    pub enum_variant: String,
    /// Name of the `process_*` fn called in the arm body, e.g.
    /// `process_initialize_market`. Best-effort: just the terminal
    /// identifier in the call path (drops module prefixes so the
    /// auditor can grep on a stable name).
    pub entry_fn: String,
    /// Path to the source file that contains the dispatcher (the
    /// caller, not the callee). Relative to `project_root` when
    /// possible. Lines are 1-indexed.
    pub file: String,
    /// 1-indexed line of the match arm in `file`.
    pub line: u32,
}

/// Top-level probe result. `None` from [`detect_shank_dispatcher`] means
/// no Shank-shape match was found; the caller falls back to the
/// generic bootstrap envelope.
#[derive(Debug, Clone, Serialize)]
pub struct ShankCatalogue {
    /// Path to the file containing `process_instruction`. Relative to
    /// `project_root` when possible.
    pub dispatcher_file: String,
    /// 1-indexed line of the `match` expression within that file.
    pub dispatcher_line: u32,
    /// Discovered handler arms, in source order.
    pub handlers: Vec<ShankHandler>,
}

/// Walk `project_root` looking for a Shank-style central-match
/// dispatcher. Returns `Ok(None)` (not an error) when no candidate
/// `process_instruction` fn parses cleanly into the expected shape —
/// the caller falls back to the runtime-agnostic bootstrap path.
pub fn detect_shank_dispatcher(project_root: &Path) -> Result<Option<ShankCatalogue>> {
    let candidates = candidate_files(project_root);

    for file in &candidates {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let syntax = match syn::parse_file(&source) {
            Ok(f) => f,
            Err(_) => continue, // unparseable file shouldn't kill the probe
        };

        // Locate the dispatcher fn in this file.
        let Some(dispatcher) = find_process_instruction(&syntax.items) else {
            continue;
        };
        // Find the top-level match-on-instruction-data inside its body.
        let Some((match_expr, match_line, matched_ident)) =
            find_dispatch_match(dispatcher, &source)
        else {
            continue;
        };

        // The matched ident must be bound from instruction_data within
        // the same fn body (try_from / try_from_primitive! / similar).
        if !ident_derived_from_instruction_data(dispatcher, &matched_ident) {
            continue;
        }

        let mut handlers = Vec::new();
        for arm in &match_expr.arms {
            if let Some(h) = extract_handler_from_arm(arm, file, project_root, &source) {
                handlers.push(h);
            }
        }

        if handlers.is_empty() {
            // The shape was right but every arm failed extraction. Don't
            // claim a dispatcher we can't describe — fall back.
            continue;
        }

        let rel = relative_path(file, project_root);
        return Ok(Some(ShankCatalogue {
            dispatcher_file: rel,
            dispatcher_line: match_line,
            handlers,
        }));
    }

    Ok(None)
}

// ---------- file discovery ------------------------------------------------

fn candidate_files(project_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let src = project_root.join("src");
    let primary = src.join("lib.rs");
    if primary.is_file() {
        out.push(primary);
    }

    // Some workspaces nest the program under `program/src/lib.rs` or
    // `programs/<name>/src/lib.rs`. Probe every `lib.rs` under those
    // roots — the dispatcher detector itself decides whether the file
    // actually contains a Shank-shape match.
    for nested_root in ["program", "programs"] {
        let dir = project_root.join(nested_root);
        if !dir.is_dir() {
            continue;
        }
        collect_lib_rs(&dir, &mut out);
    }

    // Final pass: any `.rs` file that names `entrypoint!(...)`
    // explicitly. Some programs put dispatch in `processor.rs` instead
    // of `lib.rs`.
    if let Some(src_dir) = src.is_dir().then_some(&src) {
        collect_entrypoint_files(src_dir, &mut out);
    }

    // Deduplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

fn collect_lib_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let nested_lib = path.join("src").join("lib.rs");
            if nested_lib.is_file() {
                out.push(nested_lib);
            } else {
                collect_lib_rs(&path, out);
            }
        }
    }
}

fn collect_entrypoint_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, "target" | ".qed" | "formal_verification") {
            continue;
        }
        if path.is_dir() {
            collect_entrypoint_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if contents.contains("entrypoint!(") {
                    out.push(path);
                }
            }
        }
    }
}

fn relative_path(file: &Path, project_root: &Path) -> String {
    file.strip_prefix(project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file.display().to_string())
}

// ---------- dispatcher fn discovery ---------------------------------------

fn find_process_instruction(items: &[Item]) -> Option<&ItemFn> {
    for item in items {
        match item {
            Item::Fn(f) if is_process_instruction_signature(f) => {
                return Some(f);
            }
            // Some programs declare `impl Processor { pub fn process(...) }`.
            // We don't currently recognise that shape — `process_instruction`
            // at the file top level is the canonical Shank/Phoenix surface.
            Item::Impl(impl_block) => {
                for impl_item in &impl_block.items {
                    if let ImplItem::Fn(method) = impl_item {
                        // Reuse the same signature check by synthesising
                        // a minimal ItemFn-equivalent view: we only need
                        // sig + block, which `ImplItemFn` exposes.
                        if signature_matches_process_instruction(&method.sig) {
                            // We can't easily return the `ImplItemFn` as an
                            // `ItemFn`. For now, leave impl-block
                            // dispatchers to a future pass; document it
                            // here so the gap is explicit.
                            //
                            // NOTE(v2.20-followup): impl-block process()
                            // is rare in surveyed native programs. Surface
                            // a structural note if a real program needs it.
                            let _ = method;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn is_process_instruction_signature(f: &ItemFn) -> bool {
    if f.sig.ident != "process_instruction" {
        return false;
    }
    signature_matches_process_instruction(&f.sig)
}

/// Validate the parameter list matches `(&Pubkey, &[AccountInfo], &[u8])`.
/// Return type is permissive — `ProgramResult` is the convention, but
/// `Result<T, E>` from any error type counts too. Receiver-less only
/// (no `&self`).
fn signature_matches_process_instruction(sig: &syn::Signature) -> bool {
    if sig.inputs.len() != 3 {
        return false;
    }

    let mut iter = sig.inputs.iter();
    let arg0 = iter.next().unwrap();
    let arg1 = iter.next().unwrap();
    let arg2 = iter.next().unwrap();

    typed_arg_matches(arg0, |ty| is_ref_to_named_type(ty, "Pubkey"))
        && typed_arg_matches(arg1, |ty| is_ref_to_slice_of(ty, "AccountInfo"))
        && typed_arg_matches(arg2, |ty| is_ref_to_slice_of_primitive(ty, "u8"))
}

fn typed_arg_matches<F: Fn(&Type) -> bool>(arg: &FnArg, check: F) -> bool {
    if let FnArg::Typed(pt) = arg {
        check(&pt.ty)
    } else {
        false
    }
}

fn is_ref_to_named_type(ty: &Type, name: &str) -> bool {
    if let Type::Reference(TypeReference { elem, .. }) = ty {
        if let Type::Path(p) = &**elem {
            if let Some(last) = p.path.segments.last() {
                return last.ident == name;
            }
        }
    }
    false
}

fn is_ref_to_slice_of(ty: &Type, elem_name: &str) -> bool {
    if let Type::Reference(TypeReference { elem, .. }) = ty {
        if let Type::Slice(slice) = &**elem {
            if let Type::Path(p) = &*slice.elem {
                if let Some(last) = p.path.segments.last() {
                    return last.ident == elem_name;
                }
            }
        }
    }
    false
}

fn is_ref_to_slice_of_primitive(ty: &Type, prim: &str) -> bool {
    // Same shape as is_ref_to_slice_of for primitive idents like `u8`.
    is_ref_to_slice_of(ty, prim)
}

// ---------- match-on-instruction-data discovery ---------------------------

/// Locate the top-level `match <Ident>` inside the dispatcher fn. Returns
/// the match expression, the 1-indexed source line of the match
/// keyword, and the identifier the match scrutinises.
///
/// The line number is recovered from the source text (we look for the
/// nearest `match <ident>` literal at or after the fn definition);
/// `syn` 2 keeps `Span` information behind procmacro2 internals that
/// aren't easily traversed here. Best-effort — falls back to the fn's
/// declared line + 1.
fn find_dispatch_match<'a>(
    f: &'a ItemFn,
    source: &str,
) -> Option<(&'a syn::ExprMatch, u32, String)> {
    for stmt in &f.block.stmts {
        if let Some((m, ident)) = match_in_stmt(stmt) {
            let line = locate_match_line(source, &ident).unwrap_or(1);
            return Some((m, line, ident));
        }
    }
    None
}

fn match_in_stmt(stmt: &Stmt) -> Option<(&syn::ExprMatch, String)> {
    match stmt {
        Stmt::Expr(expr, _) => match_in_expr(expr),
        Stmt::Local(_) | Stmt::Item(_) | Stmt::Macro(_) => None,
    }
}

fn match_in_expr(expr: &Expr) -> Option<(&syn::ExprMatch, String)> {
    match expr {
        Expr::Match(m) => {
            let scrutinee_ident = ident_of_expr(&m.expr)?;
            Some((m, scrutinee_ident))
        }
        // `return match ... { ... }` is the second canonical shape; the
        // dispatcher fn body is exactly `return match instruction { ... }`.
        Expr::Return(ret) => ret.expr.as_deref().and_then(match_in_expr),
        // `Ok(match instruction { ... })` wraps the match — descend into
        // call args one level.
        Expr::Call(ExprCall { args, .. }) => args.iter().find_map(match_in_expr),
        Expr::MethodCall(ExprMethodCall { receiver, .. }) => match_in_expr(receiver),
        _ => None,
    }
}

fn ident_of_expr(expr: &Expr) -> Option<String> {
    if let Expr::Path(p) = expr {
        if p.path.segments.len() == 1 {
            return Some(p.path.segments[0].ident.to_string());
        }
    }
    None
}

fn locate_match_line(source: &str, scrutinee: &str) -> Option<u32> {
    let needle = format!("match {}", scrutinee);
    for (idx, line) in source.lines().enumerate() {
        if line.contains(&needle) {
            return Some((idx + 1) as u32);
        }
    }
    None
}

// ---------- "matched ident was bound from instruction_data" --------------

/// Walks `let <ident> = <expr>;` bindings inside the fn body and checks
/// whether the matched identifier originates from a `try_from*` call (or
/// `try_from_primitive` invocation) on something named `instruction_data`
/// or a derivative slice.
fn ident_derived_from_instruction_data(f: &ItemFn, ident: &str) -> bool {
    // Fast path: direct let-binding traces straight to instruction_data.
    for stmt in &f.block.stmts {
        if let Stmt::Local(local) = stmt {
            if local_binds_ident(local, ident) {
                if let Some(init) = &local.init {
                    if expr_traces_to_instruction_data(&init.expr) {
                        return true;
                    }
                }
            }
        }
    }
    // Transitive path (Phoenix-shape): `let (tag, _) =
    // instruction_data.split_first()...;` followed by `let instruction =
    // X::try_from(*tag)...;`. The matched ident traces to a `try_from*`-
    // style call (or a similar enum-conversion macro) AND the same fn
    // body has *some* let-binding traceable to instruction_data. We
    // don't try to chase the full data-flow graph — accepting on this
    // co-occurrence is conservative enough in practice (non-dispatcher
    // fns rarely combine the two shapes) and correctly catches the
    // canonical Shank/Phoenix layout.
    let ident_bound_by_try_from = f.block.stmts.iter().any(|stmt| {
        if let Stmt::Local(local) = stmt {
            if local_binds_ident(local, ident) {
                if let Some(init) = &local.init {
                    return expr_contains_try_from_like_call(&init.expr);
                }
            }
        }
        false
    });
    let body_touches_instruction_data = f.block.stmts.iter().any(|stmt| {
        if let Stmt::Local(local) = stmt {
            if let Some(init) = &local.init {
                return expr_traces_to_instruction_data(&init.expr);
            }
        }
        false
    });
    ident_bound_by_try_from && body_touches_instruction_data
}

/// True when `expr` (recursively through `?`, paren, return, and method-
/// chain receivers) contains a call whose callee path ends in a segment
/// named `try_from` / `try_from_primitive` / `from_bytes` / `from`.
/// Conservative on direction: we don't want to claim a dispatcher when
/// the let-binding is unrelated, so we only accept the well-known
/// instruction-discriminator conversion fn names.
fn expr_contains_try_from_like_call(expr: &Expr) -> bool {
    match expr {
        Expr::Try(t) => expr_contains_try_from_like_call(&t.expr),
        Expr::Paren(p) => expr_contains_try_from_like_call(&p.expr),
        Expr::Return(r) => r
            .expr
            .as_deref()
            .is_some_and(expr_contains_try_from_like_call),
        Expr::MethodCall(mc) => {
            // `X::try_from(tag).or(Err(...))` shape — the receiver is
            // the call we care about; method args may also contain it.
            expr_contains_try_from_like_call(&mc.receiver)
                || mc.args.iter().any(expr_contains_try_from_like_call)
        }
        Expr::Call(c) => {
            // Direct call: `X::try_from(...)`, `try_from_primitive!(...)`,
            // `FromPrimitive::from_u8(...)`, etc. Inspect the callee path.
            if let Expr::Path(p) = &*c.func {
                if let Some(last) = p.path.segments.last() {
                    let name = last.ident.to_string();
                    if matches!(
                        name.as_str(),
                        "try_from" | "try_from_primitive" | "from_bytes" | "from"
                    ) {
                        return true;
                    }
                }
            }
            c.args.iter().any(expr_contains_try_from_like_call)
        }
        Expr::Macro(m) => {
            let toks = m.mac.tokens.to_string();
            toks.contains("try_from") || toks.contains("from_primitive")
        }
        _ => false,
    }
}

fn local_binds_ident(local: &Local, ident: &str) -> bool {
    pat_binds_ident(&local.pat, ident)
}

fn pat_binds_ident(pat: &Pat, ident: &str) -> bool {
    match pat {
        Pat::Ident(p) => p.ident == ident,
        Pat::Type(t) => pat_binds_ident(&t.pat, ident),
        _ => false,
    }
}

/// True when `expr` traces — through `?`, `.method()` chains, and call
/// arguments — to an identifier or path segment named `instruction_data`.
/// Conservative on direction (false-negative biased): if we can't see
/// the binding, we fall through and don't claim a Shank dispatcher.
fn expr_traces_to_instruction_data(expr: &Expr) -> bool {
    match expr {
        Expr::Try(t) => expr_traces_to_instruction_data(&t.expr),
        Expr::MethodCall(mc) => {
            if expr_traces_to_instruction_data(&mc.receiver) {
                return true;
            }
            // Some chains do `instruction_data.split_first().ok_or(...)?`
            // — args may contain other expressions but the receiver is
            // the relevant path.
            mc.args.iter().any(expr_traces_to_instruction_data)
        }
        Expr::Call(c) => {
            // try_from(instruction_data) / try_from_primitive(value) /
            // FromBytes::from_bytes(...).
            if c.args.iter().any(expr_traces_to_instruction_data) {
                return true;
            }
            // The function path itself may carry `instruction_data`
            // through e.g. UFCS — exceedingly rare in practice; skip.
            false
        }
        Expr::Macro(m) => {
            // try_from_primitive!(...) etc. — substring-check the
            // tokenstream for instruction_data.
            let toks = m.mac.tokens.to_string();
            toks.contains("instruction_data")
        }
        Expr::Path(p) => p
            .path
            .segments
            .iter()
            .any(|s: &PathSegment| s.ident == "instruction_data"),
        Expr::Reference(r) => expr_traces_to_instruction_data(&r.expr),
        Expr::Paren(p) => expr_traces_to_instruction_data(&p.expr),
        Expr::Field(f) => expr_traces_to_instruction_data(&f.base),
        Expr::Index(i) => {
            expr_traces_to_instruction_data(&i.expr) || expr_traces_to_instruction_data(&i.index)
        }
        Expr::Block(b) => b.block.stmts.iter().any(|s| match s {
            Stmt::Expr(e, _) => expr_traces_to_instruction_data(e),
            _ => false,
        }),
        _ => false,
    }
}

// ---------- per-arm extraction --------------------------------------------

fn extract_handler_from_arm(
    arm: &syn::Arm,
    dispatcher_file: &Path,
    project_root: &Path,
    source: &str,
) -> Option<ShankHandler> {
    let (enum_variant, name) = pattern_to_variant(&arm.pat)?;
    let entry_fn = first_process_callee(&arm.body)?;
    // Line: find the variant string in source. Best-effort.
    let line = locate_first(source, &enum_variant).unwrap_or(1);
    Some(ShankHandler {
        name,
        enum_variant,
        entry_fn,
        file: relative_path(dispatcher_file, project_root),
        line,
    })
}

/// Convert a match-arm pattern into `(full_variant_path, terminal_name)`.
/// Supports:
/// - `Enum::Variant`
/// - `Enum::Variant { ... }`
/// - `Enum::Variant(_, _)`
/// - reference-prefixed variants (`&Enum::Variant`)
fn pattern_to_variant(pat: &Pat) -> Option<(String, String)> {
    match pat {
        Pat::TupleStruct(ts) => {
            let path = path_to_string(&ts.path)?;
            let name = ts.path.segments.last()?.ident.to_string();
            Some((path, name))
        }
        Pat::Struct(s) => {
            let path = path_to_string(&s.path)?;
            let name = s.path.segments.last()?.ident.to_string();
            Some((path, name))
        }
        Pat::Path(p) => {
            let path = path_to_string(&p.path)?;
            let name = p.path.segments.last()?.ident.to_string();
            Some((path, name))
        }
        Pat::Reference(r) => pattern_to_variant(&r.pat),
        Pat::Paren(p) => pattern_to_variant(&p.pat),
        // Other patterns (wildcards, literals, ranges) aren't Shank
        // variant arms — caller skips them.
        _ => None,
    }
}

fn path_to_string(path: &syn::Path) -> Option<String> {
    let mut parts = Vec::new();
    for seg in &path.segments {
        parts.push(seg.ident.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("::"))
    }
}

/// Find the first `process_*` fn called within an arm body. Returns
/// just the terminal identifier (drops module prefixes — auditor greps
/// on the bare name).
///
/// Match arm bodies in this style are typically:
/// - `process_initialize_market(program_id, accounts, data)`
/// - `Self::process_initialize_market(...)`
/// - `processor::initialize_market::process(...)` (less common)
/// - `msg!("..."); process_initialize_market(...)` (with logging)
fn first_process_callee(expr: &Expr) -> Option<String> {
    let mut visitor = CalleeVisitor { found: None };
    syn::visit::Visit::visit_expr(&mut visitor, expr);
    visitor.found
}

struct CalleeVisitor {
    found: Option<String>,
}

impl<'ast> syn::visit::Visit<'ast> for CalleeVisitor {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if self.found.is_some() {
            return;
        }
        if let Expr::Path(p) = &*node.func {
            if let Some(last) = p.path.segments.last() {
                let name = last.ident.to_string();
                if name.starts_with("process") {
                    self.found = Some(name);
                    return;
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if self.found.is_some() {
            return;
        }
        // Method call: `processor.process_initialize(...)` style.
        let name = node.method.to_string();
        if name.starts_with("process") {
            self.found = Some(name);
            return;
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn locate_first(source: &str, needle: &str) -> Option<u32> {
    for (idx, line) in source.lines().enumerate() {
        if line.contains(needle) {
            return Some((idx + 1) as u32);
        }
    }
    None
}

// ---------- tests ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace_tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("qedgen-shank-test-{}", name));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("src")).unwrap();
        p
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn three_arm_dispatcher_extracts_three_handlers() {
        let root = workspace_tmp("three-arm");
        write(
            &root.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        // Note: signatures are textual — the test doesn't compile this,
        // only parses it via syn.
        write(
            &root.join("src/lib.rs"),
            r#"
use solana_program::{
    account_info::AccountInfo, entrypoint, entrypoint::ProgramResult, pubkey::Pubkey,
};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = WidgetInstruction::try_from(instruction_data)?;
    match instruction {
        WidgetInstruction::InitializeWidget { capacity } => {
            process_initialize_widget(program_id, accounts, capacity)
        }
        WidgetInstruction::Tick => process_tick(program_id, accounts),
        WidgetInstruction::Close => process_close(program_id, accounts),
    }
}
"#,
        );

        let cat = detect_shank_dispatcher(&root).unwrap().unwrap();
        assert_eq!(cat.handlers.len(), 3, "should find 3 arms");
        assert_eq!(cat.handlers[0].name, "InitializeWidget");
        assert_eq!(
            cat.handlers[0].enum_variant,
            "WidgetInstruction::InitializeWidget"
        );
        assert_eq!(cat.handlers[0].entry_fn, "process_initialize_widget");
        assert_eq!(cat.handlers[1].name, "Tick");
        assert_eq!(cat.handlers[1].entry_fn, "process_tick");
        assert_eq!(cat.handlers[2].name, "Close");
        assert_eq!(cat.handlers[2].entry_fn, "process_close");
        assert!(cat.dispatcher_line >= 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn anchor_style_program_returns_none() {
        // Anchor programs have no top-level `process_instruction`. The
        // `#[program] mod` form is parsed by the dedicated IDL extractor.
        let root = workspace_tmp("anchor-shape");
        write(
            &root.join("src/lib.rs"),
            r#"
use anchor_lang::prelude::*;

#[program]
pub mod my_program {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        Ok(())
    }
}
"#,
        );
        assert!(detect_shank_dispatcher(&root).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn process_instruction_without_match_returns_none() {
        // Right-shaped signature, but the body is plain straight-line
        // code (no central match). Don't claim a Shank dispatcher.
        let root = workspace_tmp("no-match");
        write(
            &root.join("src/lib.rs"),
            r#"
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    msg!("not a dispatcher");
    Ok(())
}
"#,
        );
        assert!(detect_shank_dispatcher(&root).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fixture_shank_dispatcher_resolves_to_three_handlers() {
        // The committed fixture exercises the v2.20 §S2.1 / §S2.2
        // surface: three handlers with distinct intent shapes. We
        // assert the dispatcher discovery half here; per-handler
        // intent classification is tested via the end-to-end
        // `run_bootstrap` path in `probe`.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/native-fixtures/shank-dispatcher");
        let cat = detect_shank_dispatcher(&root)
            .expect("fixture must parse")
            .expect("fixture must match Shank shape");
        assert_eq!(cat.handlers.len(), 3);
        let names: Vec<&str> = cat.handlers.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["InitializeWidget", "Tick", "Close"]);
    }

    #[test]
    fn matched_ident_must_come_from_instruction_data() {
        // The fn has a top-level match, but the scrutinee `kind` is
        // bound from accounts[0].key, not instruction_data. Don't
        // misclassify.
        let root = workspace_tmp("wrong-source");
        write(
            &root.join("src/lib.rs"),
            r#"
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let kind = accounts[0].key;
    match kind {
        _ => Ok(()),
    }
}
"#,
        );
        assert!(detect_shank_dispatcher(&root).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
