//! `qedgen probe --bootstrap` — Shank-style native dispatcher discovery.
//!
//! Pre-Anchor native programs concentrate dispatch in a top-level
//! `process_instruction` that matches on an instruction enum deserialised
//! from `instruction_data`, each arm calling one `process_*` handler. The
//! probe recognises that shape and emits the per-arm `ShankCatalogue`
//! (`dispatcher_kind: "shank_central_match"`). Does NOT match Anchor (IDL
//! extractor), Pinocchio (`pinocchio_probe`), or Quasar (codegen markers
//! preempt detection). Deterministic AST pattern matching only — semantic
//! interpretation is the agent's job (`feedback_agent_lsp_substrate`).

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
    /// Handler name, from the variant identifier (e.g. `InitializeMarket`).
    pub name: String,
    /// Enum path as written in the arm pattern, e.g. `MarketInstruction::InitializeMarket`.
    pub enum_variant: String,
    /// `process_*` fn called in the arm body; terminal identifier only
    /// (module prefixes dropped so the auditor greps a stable name).
    pub entry_fn: String,
    /// Dispatcher (caller) source file, relative to `project_root` when possible.
    pub file: String,
    /// 1-indexed line of the match arm in `file`.
    pub line: u32,
}

/// Top-level probe result.
#[derive(Debug, Clone, Serialize)]
pub struct ShankCatalogue {
    /// File containing `process_instruction`, relative to `project_root` when possible.
    pub dispatcher_file: String,
    /// 1-indexed line of the `match` expression within that file.
    pub dispatcher_line: u32,
    /// Discovered handler arms, in source order.
    pub handlers: Vec<ShankHandler>,
}

/// Walk `project_root` for a Shank-style central-match dispatcher.
/// `Ok(None)` (not an error) when nothing matches the shape — the caller
/// falls back to the runtime-agnostic bootstrap path.
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

        let Some(dispatcher) = find_process_instruction(&syntax.items) else {
            continue;
        };
        let Some((match_expr, match_line, matched_ident)) =
            find_dispatch_match(dispatcher, &source)
        else {
            continue;
        };

        // Scrutinee must be bound from instruction_data in the same fn body.
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
            // Right shape but every arm failed extraction — don't claim a
            // dispatcher we can't describe; fall back.
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

    // Workspaces may nest under `program/` or `programs/<name>/`. Probe
    // every `lib.rs` there — the detector decides whether a file matches.
    for nested_root in ["program", "programs"] {
        let dir = project_root.join(nested_root);
        if !dir.is_dir() {
            continue;
        }
        collect_lib_rs(&dir, &mut out);
    }

    // Final pass: any `.rs` naming `entrypoint!(...)` — some programs
    // dispatch from `processor.rs` instead of `lib.rs`.
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
    for path in crate::fs_walk::collect_rs_files(dir, crate::fs_walk::DEFAULT_SKIP_DIRS) {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if contents.contains("entrypoint!(") {
                out.push(path);
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
            // `impl Processor { pub fn process(...) }` dispatchers are not
            // recognised — top-level `process_instruction` is the canonical
            // Shank/Phoenix surface.
            Item::Impl(impl_block) => {
                for impl_item in &impl_block.items {
                    if let ImplItem::Fn(method) = impl_item {
                        if signature_matches_process_instruction(&method.sig) {
                            // Can't return an `ImplItemFn` as `ItemFn`; rare
                            // in surveyed native programs — explicit gap,
                            // left to a future pass.
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

/// Params must be `(&Pubkey, &[AccountInfo], &[u8])`; return type is
/// permissive (`ProgramResult` or any `Result`). No receiver.
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
    is_ref_to_slice_of(ty, prim)
}

// ---------- match-on-instruction-data discovery ---------------------------

/// Locate the top-level `match <Ident>` in the dispatcher fn; returns the
/// match expr, the 1-indexed line of the match keyword, and the scrutinised
/// ident. The line is recovered from source text (syn 2 hides `Span`
/// internals); best-effort.
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
        // `return match instruction { ... }` — second canonical shape.
        Expr::Return(ret) => ret.expr.as_deref().and_then(match_in_expr),
        // `Ok(match instruction { ... })` — descend one level into call args.
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

/// True when the matched ident is let-bound from a `try_from*`-style call
/// on `instruction_data` or a derivative slice.
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
    // Transitive path (Phoenix-shape): ident bound by a try_from*-style
    // call AND some let-binding in the same body traces to
    // instruction_data. No full data-flow chase — the co-occurrence is
    // conservative enough (non-dispatcher fns rarely combine both shapes).
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

/// True when `expr` (through `?`, parens, return, method-chain receivers)
/// contains a call ending in `try_from` / `try_from_primitive` /
/// `from_bytes` / `from`. Only well-known discriminator-conversion names —
/// an unrelated let-binding must not claim a dispatcher.
fn expr_contains_try_from_like_call(expr: &Expr) -> bool {
    match expr {
        Expr::Try(t) => expr_contains_try_from_like_call(&t.expr),
        Expr::Paren(p) => expr_contains_try_from_like_call(&p.expr),
        Expr::Return(r) => r
            .expr
            .as_deref()
            .is_some_and(expr_contains_try_from_like_call),
        Expr::MethodCall(mc) => {
            // `X::try_from(tag).or(Err(...))` — check receiver and args.
            expr_contains_try_from_like_call(&mc.receiver)
                || mc.args.iter().any(expr_contains_try_from_like_call)
        }
        Expr::Call(c) => {
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

/// True when `expr` traces (through `?`, method chains, call args) to an
/// ident named `instruction_data`. False-negative biased: if the binding
/// isn't visible, we don't claim a Shank dispatcher.
fn expr_traces_to_instruction_data(expr: &Expr) -> bool {
    match expr {
        Expr::Try(t) => expr_traces_to_instruction_data(&t.expr),
        Expr::MethodCall(mc) => {
            if expr_traces_to_instruction_data(&mc.receiver) {
                return true;
            }
            // Args may also carry it (`instruction_data.split_first().ok_or(...)?`).
            mc.args.iter().any(expr_traces_to_instruction_data)
        }
        Expr::Call(c) => {
            if c.args.iter().any(expr_traces_to_instruction_data) {
                return true;
            }
            // The fn path itself could carry it via UFCS — exceedingly
            // rare in practice; skip.
            false
        }
        Expr::Macro(m) => {
            // Substring-check the tokenstream (try_from_primitive!(...) etc.).
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
        // Wildcards/literals/ranges aren't variant arms — caller skips them.
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

/// First `process_*` fn called in an arm body; terminal identifier only
/// (auditor greps the bare name). Handles direct, `Self::`- and
/// module-qualified calls, method calls, and bodies with leading `msg!`.
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
        // Source is only parsed via syn, never compiled.
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
        // Anchor's `#[program] mod` form is handled by the IDL extractor,
        // not this probe.
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
        // Right signature, no central match — don't claim a dispatcher.
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
        // Fixture: three handlers with distinct intent shapes. Dispatcher
        // discovery asserted here; per-handler intent classification is
        // covered by the end-to-end `run_bootstrap` path in `probe`.
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
        // Scrutinee `kind` is bound from accounts[0].key, not
        // instruction_data — don't misclassify.
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
