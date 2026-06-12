//! Brownfield adapter: emit a starter `.qedspec` for an existing Anchor crate.
//!
//! Pipeline: `anchor_project` lists `#[program]` instructions; `anchor_resolver`
//! follows each forwarder to its handler ItemFn (or Unrecognized); this module
//! renders a parseable skeleton with `// TODO:` markers. Output is round-tripped
//! through `chumsky_adapter::parse_str` so renderer bugs surface at adapt-time.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::anchor_project::{parse_anchor_project, Instruction};
use crate::anchor_resolver::{resolve_handler, HandlerLocation};
use crate::program_model::{
    ErrorModel, HandlerArgModel, HandlerModel, HandlerShape, ProgramAdapter, ProgramFramework,
    ProgramModel,
};

/// Per-handler override naming the real implementation when the classifier
/// can't follow a forwarder (custom dispatchers). Path parses like a free-fn
/// forwarder: `module::sub::function`, fn name last.
#[derive(Debug, Clone)]
pub struct HandlerOverride {
    pub module_path: Vec<String>,
    pub fn_name: String,
}

impl HandlerOverride {
    /// `module::sub::function` → override; bare `function` → empty module
    /// path. None on empty input or empty segment.
    pub fn parse(rust_path: &str) -> Option<Self> {
        let trimmed = rust_path.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut segments: Vec<String> = trimmed.split("::").map(|s| s.trim().to_string()).collect();
        if segments.iter().any(|s| s.is_empty()) {
            return None;
        }
        let fn_name = segments.pop()?;
        Some(HandlerOverride {
            module_path: segments,
            fn_name,
        })
    }
}

/// Parse one `--handler <name>=<rust_path>` CLI value into
/// `(handler_name, override)`; errors on malformed input.
pub fn parse_handler_override(value: &str) -> Result<(String, HandlerOverride)> {
    let (name, path) = value.split_once('=').ok_or_else(|| {
        anyhow::anyhow!(
            "expected `<handler>=<rust_path>` for `--handler`, got `{}`",
            value
        )
    })?;
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("`--handler` value `{}` has empty handler name", value);
    }
    let rust_override = HandlerOverride::parse(path).ok_or_else(|| {
        anyhow::anyhow!(
            "`--handler {}=<path>` rust path is empty or has empty segments",
            name
        )
    })?;
    Ok((name.to_string(), rust_override))
}

pub struct AnchorAdapter<'a> {
    overrides: &'a HashMap<String, HandlerOverride>,
}

impl<'a> AnchorAdapter<'a> {
    pub fn new(overrides: &'a HashMap<String, HandlerOverride>) -> Self {
        Self { overrides }
    }
}

impl ProgramAdapter for AnchorAdapter<'_> {
    fn framework(&self) -> ProgramFramework {
        ProgramFramework::Anchor
    }

    fn detect(&self, root: &Path) -> bool {
        parse_anchor_project(root).is_ok()
    }

    fn extract(&self, root: &Path) -> Result<ProgramModel> {
        extract_program_model(root, self.overrides)
    }

    fn render_spec(&self, model: &ProgramModel) -> Result<String> {
        Ok(render_spec(model))
    }

    fn adapt(&self, root: &Path) -> Result<String> {
        let model = self.extract(root)?;
        let rendered = self.render_spec(&model)?;

        // Round-trip: a parse failure here is a renderer bug, not user input.
        crate::chumsky_adapter::parse_str(&rendered).context(
            "Generated .qedspec failed to parse — this is a bug in `qedgen adapt`. \
             Please report at https://github.com/qedgen/solana-skills/issues",
        )?;

        Ok(rendered)
    }
}

/// Parse-independent "is this an Anchor crate?" check: an `anchor-lang`
/// dependency in the crate's `Cargo.toml`. Adapter detection consults this so
/// a malformed Anchor program surfaces the real Anchor parse error instead of
/// being swallowed by the permissive native source-walk (which regex-scans
/// for `pub fn` and would emit a wrong-shaped skeleton).
pub(crate) fn looks_like_anchor(program_root: &Path) -> bool {
    std::fs::read_to_string(program_root.join("Cargo.toml"))
        .map(|s| s.contains("anchor-lang"))
        .unwrap_or(false)
}

/// Generate a starter `.qedspec` for the Anchor program at `program_root`
/// (the crate dir holding `src/`). `overrides` points unrecognized handlers
/// at their actual implementation.
#[allow(dead_code)]
pub fn adapt(program_root: &Path, overrides: &HashMap<String, HandlerOverride>) -> Result<String> {
    let adapter = AnchorAdapter::new(overrides);
    adapter.adapt(program_root)
}

/// Extract an Anchor program into the neutral brownfield adapter model.
pub fn extract_program_model(
    program_root: &Path,
    overrides: &HashMap<String, HandlerOverride>,
) -> Result<ProgramModel> {
    let project = parse_anchor_project(program_root).with_context(|| {
        format!(
            "failed to parse Anchor project at {}",
            program_root.display()
        )
    })?;

    let mut model = ProgramModel::new(ProgramFramework::Anchor, project.program_mod_name.clone());
    model.primary_source = Some(rel_to(program_root, &project.lib_rs_path));
    model.entry_module = Some(project.program_mod_name.clone());
    model.handlers = Vec::with_capacity(project.instructions.len());

    for instruction in &project.instructions {
        let location = resolve_with_override(
            instruction,
            &project.lib_rs_path,
            program_root,
            overrides.get(&instruction.name),
        )?;
        model.handlers.push(handler_model_from_anchor(
            instruction,
            &location,
            program_root,
        ));
    }

    model.errors = discover_error_enum(program_root);
    Ok(model)
}

/// Convenience wrapper: write the adapted `.qedspec` to disk.
#[allow(dead_code)]
pub fn adapt_to_file(
    program_root: &Path,
    output_path: &Path,
    overrides: &HashMap<String, HandlerOverride>,
) -> Result<()> {
    let rendered = adapt(program_root, overrides)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    std::fs::write(output_path, &rendered)
        .with_context(|| format!("writing {}", output_path.display()))?;
    eprintln!("Wrote {} ({} bytes)", output_path.display(), rendered.len());
    Ok(())
}

/// Resolve a handler; a supplied CLI override always wins. Overrides cover
/// what the classifier can't reach: `Unrecognized` forwarders (custom
/// dispatchers), multi-stmt forwarders conservatively classified `Inline`,
/// and walks that landed on the wrong file. The override is treated as a
/// free-fn forwarder: walk `src/` for `pub fn <name>` at its module path.
fn resolve_with_override(
    instruction: &Instruction,
    lib_rs_path: &Path,
    program_root: &Path,
    override_: Option<&HandlerOverride>,
) -> Result<HandlerLocation> {
    if let Some(o) = override_ {
        return crate::anchor_resolver::resolve_free_fn(
            &o.module_path,
            &o.fn_name,
            program_root,
            lib_rs_path,
        );
    }
    resolve_handler(instruction, lib_rs_path, program_root)
}

// ----------------------------------------------------------------------------
// Attribute mode (`qedgen adapt --program <crate> --spec <path>`): emit one
// paste-ready `#[qed(verified, ...)]` attribute per spec handler. Body hash
// matches what `qedgen-macros` recomputes at compile time.
// ----------------------------------------------------------------------------

/// One emitted attribute entry, ready for the user to paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeEntry {
    /// Handler name (same in spec and `#[program]` mod).
    pub handler: String,
    /// File holding the actual handler body, relative to the program root.
    pub source_path: PathBuf,
    /// `#[qed(...)]` line to paste verbatim above the handler `pub fn`.
    pub attribute: String,
    /// Why no attribute was emitted, when `attribute` is empty.
    pub note: Option<String>,
}

/// Compute `#[qed]` attributes for every handler in `spec_path` against the
/// program at `program_root`; one entry per spec handler. Spec-only handlers
/// are also reported by `anchor_check::check_anchor_coverage`.
pub fn compute_attributes(
    program_root: &Path,
    spec_path: &Path,
    overrides: &HashMap<String, HandlerOverride>,
) -> Result<Vec<AttributeEntry>> {
    let project = parse_anchor_project(program_root).with_context(|| {
        format!(
            "failed to parse Anchor project at {}",
            program_root.display()
        )
    })?;

    let spec_source = std::fs::read_to_string(spec_path)
        .with_context(|| format!("reading spec {}", spec_path.display()))?;
    let parsed_spec = crate::chumsky_adapter::parse_str(&spec_source)
        .with_context(|| format!("parsing spec {}", spec_path.display()))?;

    // Spec path in the attribute is relative to program_root — the macro
    // resolves it against `CARGO_MANIFEST_DIR` (the program crate root).
    let spec_rel = spec_path
        .strip_prefix(program_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| spec_path.to_path_buf());

    let mut out = Vec::new();
    for handler in &parsed_spec.handlers {
        let Some(instruction) = project.instructions.iter().find(|i| i.name == handler.name) else {
            // No matching `pub fn` in the program — surface as a note.
            out.push(AttributeEntry {
                handler: handler.name.clone(),
                source_path: program_root.to_path_buf(),
                attribute: String::new(),
                note: Some(format!(
                    "handler `{}` is in the spec but not in the program's `#[program]` mod — re-run `qedgen check --anchor-project {}` for a deeper diff",
                    handler.name,
                    program_root.display()
                )),
            });
            continue;
        };

        let location = resolve_with_override(
            instruction,
            &project.lib_rs_path,
            program_root,
            overrides.get(&instruction.name),
        )?;
        let spec_hash = crate::spec_hash::spec_hash_for_handler(&spec_source, &handler.name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "internal error: parsed handler `{}` but couldn't extract its block from {}",
                    handler.name,
                    spec_path.display()
                )
            })?;

        // Find and hash the `pub struct X` named by `Context<X>`. Optional:
        // when absent, the attribute still works in body-only mode.
        let accounts_meta = accounts_struct_for_handler(&instruction.program_fn, program_root);

        let entry = match location {
            HandlerLocation::Inline {
                item_fn,
                source_path,
            }
            | HandlerLocation::FreeFn {
                item_fn,
                source_path,
            } => {
                let body_hash = crate::spec_hash::body_hash_for_fn(&item_fn);
                AttributeEntry {
                    handler: handler.name.clone(),
                    source_path: rel_to(program_root, &source_path),
                    attribute: render_attribute(
                        &spec_rel,
                        &handler.name,
                        &body_hash,
                        &spec_hash,
                        accounts_meta.as_ref(),
                    ),
                    note: None,
                }
            }
            HandlerLocation::Method {
                item_fn,
                source_path,
                ..
            } => {
                let body_hash = crate::spec_hash::body_hash_for_impl_fn(&item_fn);
                AttributeEntry {
                    handler: handler.name.clone(),
                    source_path: rel_to(program_root, &source_path),
                    attribute: render_attribute(
                        &spec_rel,
                        &handler.name,
                        &body_hash,
                        &spec_hash,
                        accounts_meta.as_ref(),
                    ),
                    note: None,
                }
            }
            HandlerLocation::Unrecognized { reason } => AttributeEntry {
                handler: handler.name.clone(),
                source_path: program_root.to_path_buf(),
                attribute: String::new(),
                note: Some(format!(
                    "unrecognized forwarder shape ({}) — annotate manually or refactor",
                    reason
                )),
            },
        };
        out.push(entry);
    }

    Ok(out)
}

/// The `#[derive(Accounts)]` struct backing a handler's `Context<X>`: what
/// the macro recomputes against, plus the `CARGO_MANIFEST_DIR`-relative path.
struct AccountsMeta {
    /// Type name written in `Context<X>`.
    struct_name: String,
    /// File holding `pub struct <struct_name>`, relative to `program_root`.
    file_rel: PathBuf,
    /// Sealed hash of the canonicalized struct.
    hash: String,
}

/// Walk `src/` for the `pub struct X` named by the signature's `Context<X>`;
/// None when there's no `Context<X>` or no match. A qualifying path
/// (`Context<crate::accounts::Shared>`) narrows the walk to files whose module
/// path matches, so same-named structs in different modules don't collide.
fn accounts_struct_for_handler(
    program_fn: &syn::ItemFn,
    program_root: &Path,
) -> Option<AccountsMeta> {
    let segments = extract_accounts_path(program_fn)?;
    let struct_name = segments.last()?.clone();
    let module_prefix = normalize_module_prefix(&segments[..segments.len() - 1]);

    let src_dir = program_root.join("src");
    let candidates = walk_rust_files(&src_dir);

    // Files matching the qualifying prefix first; bare `Context<Shared>`
    // (empty prefix) keeps first-match-wins ordering.
    let prioritized = prioritize_candidates(&candidates, &src_dir, &module_prefix);

    for path in prioritized {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(hash) = crate::spec_hash::accounts_struct_hash(&source, &struct_name) {
            let file_rel = path
                .strip_prefix(program_root)
                .map(Path::to_path_buf)
                .unwrap_or(path);
            return Some(AccountsMeta {
                struct_name,
                file_rel,
                hash,
            });
        }
    }
    None
}

/// Drop a leading `crate`/`self` segment. `super` is left in place — the walk
/// won't match and falls through to the whole-tree pass; resolving it would
/// need the program-mod fn's source position.
fn normalize_module_prefix(prefix: &[String]) -> Vec<String> {
    let mut out: Vec<String> = prefix.to_vec();
    if matches!(
        out.first().map(String::as_str),
        Some("crate") | Some("self")
    ) {
        out.remove(0);
    }
    out
}

/// Files matching `module_prefix` first, rest in original order. Empty
/// prefix is a no-op (preserves first-match-wins).
fn prioritize_candidates(
    candidates: &[PathBuf],
    src_dir: &Path,
    module_prefix: &[String],
) -> Vec<PathBuf> {
    if module_prefix.is_empty() {
        return candidates.to_vec();
    }
    let (matching, rest): (Vec<_>, Vec<_>) = candidates
        .iter()
        .cloned()
        .partition(|p| file_module_path(p, src_dir) == module_prefix);
    let mut out = matching;
    out.extend(rest);
    out
}

/// `src/foo/bar.rs` / `src/foo/bar/mod.rs` → `["foo", "bar"]`; `src/lib.rs`
/// → `[]`. Duplicates `anchor_resolver::file_module_path` (private there).
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

/// Render one `#[qed(verified, ...)]` line; includes the `accounts*` triplet
/// when the adapter found the struct.
fn render_attribute(
    spec_rel: &Path,
    handler_name: &str,
    body_hash: &str,
    spec_hash: &str,
    accounts: Option<&AccountsMeta>,
) -> String {
    match accounts {
        Some(meta) => format!(
            "#[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\", accounts = \"{}\", accounts_file = \"{}\", accounts_hash = \"{}\")]",
            spec_rel.display(),
            handler_name,
            body_hash,
            spec_hash,
            meta.struct_name,
            meta.file_rel.display(),
            meta.hash,
        ),
        None => format!(
            "#[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\")]",
            spec_rel.display(),
            handler_name,
            body_hash,
            spec_hash,
        ),
    }
}

/// Paste-friendly text report: per-handler source pointer + attribute line;
/// skipped handlers carry a `// note: …` instead.
pub fn render_attributes(entries: &[AttributeEntry]) -> String {
    let mut s = String::new();
    s.push_str("// `qedgen adapt --spec ...` — paste each attribute above the named handler.\n");
    s.push_str("// The body hash matches what `qedgen-macros` recomputes at compile time;\n");
    s.push_str("// editing the body fires `compile_error!` until you re-run this command.\n\n");
    for entry in entries {
        s.push_str(&format!("// === handler: {} ===\n", entry.handler));
        s.push_str(&format!("// source: {}\n", entry.source_path.display()));
        if let Some(note) = &entry.note {
            s.push_str(&format!("// note: {}\n", note));
        }
        if !entry.attribute.is_empty() {
            s.push_str(&entry.attribute);
            s.push('\n');
        }
        s.push('\n');
    }
    s
}

// ----------------------------------------------------------------------------
// Rendering
// ----------------------------------------------------------------------------

fn handler_model_from_anchor(
    instruction: &Instruction,
    location: &HandlerLocation,
    program_root: &Path,
) -> HandlerModel {
    let args = extract_args(&instruction.program_fn)
        .into_iter()
        .map(|(name, qedspec_type)| HandlerArgModel { name, qedspec_type })
        .collect();
    let accounts_type = extract_accounts_type(&instruction.program_fn);
    let (source_path, shape) = match location {
        HandlerLocation::Inline { source_path, .. } => (
            Some(rel_to(program_root, source_path)),
            HandlerShape::Inline,
        ),
        HandlerLocation::FreeFn { source_path, .. } => (
            Some(rel_to(program_root, source_path)),
            HandlerShape::FreeFn,
        ),
        HandlerLocation::Method {
            source_path,
            impl_type,
            ..
        } => (
            Some(rel_to(program_root, source_path)),
            HandlerShape::Method {
                impl_type: impl_type.clone(),
            },
        ),
        HandlerLocation::Unrecognized { reason } => (
            None,
            HandlerShape::Unrecognized {
                reason: reason.clone(),
            },
        ),
    };
    HandlerModel {
        name: instruction.name.clone(),
        args,
        accounts_type,
        source_path,
        shape,
    }
}

fn rel_to(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| p.to_path_buf())
}

/// `program_fn.sig.inputs` minus the leading `Context<...>`, as
/// `(name, mapped_type)` pairs.
fn extract_args(program_fn: &syn::ItemFn) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut skipped_ctx = false;
    for input in &program_fn.sig.inputs {
        let pat_type = match input {
            syn::FnArg::Typed(p) => p,
            // Receivers shouldn't appear in `#[program]` fns; skip defensively.
            syn::FnArg::Receiver(_) => continue,
        };
        // Skip exactly one leading Context<X>; later Context-typed args
        // (rare) flow into the spec for the user to prune.
        if !skipped_ctx && is_context_type(&pat_type.ty) {
            skipped_ctx = true;
            continue;
        }
        let name = match &*pat_type.pat {
            syn::Pat::Ident(pi) => pi.ident.to_string(),
            // Destructured patterns: numbered placeholder so the spec parses.
            _ => format!("arg_{}", out.len()),
        };
        let mapped = map_rust_type(&pat_type.ty);
        out.push((name, mapped));
    }
    out
}

fn is_context_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    tp.path
        .segments
        .last()
        .is_some_and(|s| s.ident == "Context")
}

/// `Context<X>` / `Context<'info, X>` → bare `X`; None when the first arg
/// isn't a Context (handler is still emitted, sans accounts breadcrumb).
fn extract_accounts_type(program_fn: &syn::ItemFn) -> Option<String> {
    extract_accounts_path(program_fn)?.pop()
}

/// Full qualifying path of the accounts type, ident last:
/// `Context<crate::a::Shared>` → `["crate", "a", "Shared"]`. Narrows the
/// struct lookup when same-named structs live in different modules.
fn extract_accounts_path(program_fn: &syn::ItemFn) -> Option<Vec<String>> {
    let first = program_fn.sig.inputs.first()?;
    let syn::FnArg::Typed(pt) = first else {
        return None;
    };
    let syn::Type::Path(tp) = &*pt.ty else {
        return None;
    };
    let last = tp.path.segments.last()?;
    if last.ident != "Context" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(ab) = &last.arguments else {
        return None;
    };
    for arg in &ab.args {
        if let syn::GenericArgument::Type(syn::Type::Path(tp)) = arg {
            let segments: Vec<String> = tp
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            if segments.is_empty() {
                continue;
            }
            return Some(segments);
        }
    }
    None
}

/// Best-effort Rust → qedspec type mapping (mirrors `idl2spec::map_type`);
/// None for unhandled shapes (Vec/Option/arrays/generics) → renderer TODO.
fn map_rust_type(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    // Generic types (Vec<u8>, Option<T>) are left for the user to model.
    if !matches!(last.arguments, syn::PathArguments::None) {
        return None;
    }
    let mapped = match last.ident.to_string().as_str() {
        "u8" => "U8",
        "u16" => "U16",
        "u32" => "U32",
        "u64" => "U64",
        "u128" => "U128",
        "i8" => "I8",
        "i16" => "I16",
        "i32" => "I32",
        "i64" => "I64",
        "i128" => "I128",
        "bool" => "Bool",
        "Pubkey" => "Pubkey",
        "String" => "String",
        // Unknown bare paths pass through as user-defined type names; the
        // round-trip catches typos at parse-time.
        other if !other.is_empty() => return Some(other.to_string()),
        _ => return None,
    };
    Some(mapped.to_string())
}

/// First `#[error_code] pub enum` found in `src/` (deterministic walk
/// order); None when absent.
fn discover_error_enum(program_root: &Path) -> Option<ErrorModel> {
    let src_dir = program_root.join("src");
    let mut files = walk_rust_files(&src_dir);
    files.sort();
    for path in files {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let file: syn::File = match syn::parse_str(&source) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if let Some((enum_name, variants)) = find_error_code_enum(&file.items) {
            return Some(ErrorModel {
                source_path: Some(rel_to(program_root, &path)),
                enum_name,
                variants,
            });
        }
    }
    None
}

/// Recursively scan `items` (incl. nested mods) for `#[error_code] pub enum`;
/// attribute matched by last path segment (handles `anchor_lang::error_code`).
fn find_error_code_enum(items: &[syn::Item]) -> Option<(String, Vec<String>)> {
    for item in items {
        match item {
            syn::Item::Enum(item_enum) => {
                let has_attr = item_enum.attrs.iter().any(|a| {
                    a.path()
                        .segments
                        .last()
                        .is_some_and(|s| s.ident == "error_code")
                });
                if has_attr {
                    let variants = item_enum
                        .variants
                        .iter()
                        .map(|v| v.ident.to_string())
                        .collect();
                    return Some((item_enum.ident.to_string(), variants));
                }
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, sub_items)) = &item_mod.content {
                    if let Some(found) = find_error_code_enum(sub_items) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn walk_rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_rust_files_inner(dir, &mut out);
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

fn render_spec(model: &ProgramModel) -> String {
    let mut s = String::new();
    s.push_str("// Generated by `qedgen adapt`. Fill in the TODOs to make this verifiable.\n");
    if let (Some(primary_source), Some(entry_module)) = (&model.primary_source, &model.entry_module)
    {
        s.push_str(&format!(
            "// Source: {} (program mod: `{}`)\n\n",
            primary_source.display(),
            entry_module,
        ));
    }
    s.push_str(&format!("spec {}\n\n", to_pascal_case(&model.name)));

    s.push_str("// TODO: replace with the actual lifecycle of your program.\n");
    s.push_str("type State\n");
    s.push_str("  | Init\n");
    s.push_str("  | Active\n\n");

    match model.errors.as_ref() {
        Some(info) if !info.variants.is_empty() => {
            let source_path = info
                .source_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            s.push_str(&format!(
                "// Error variants discovered in {} (`#[error_code] pub enum {}`).\n",
                source_path, info.enum_name,
            ));
            s.push_str("type Error\n");
            for variant in &info.variants {
                s.push_str(&format!("  | {}\n", variant));
            }
            s.push('\n');
        }
        Some(info) => {
            let source_path = info
                .source_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            s.push_str(&format!(
                "// Found `#[error_code] pub enum {}` in {} but it has no variants.\n",
                info.enum_name, source_path,
            ));
            s.push_str("// TODO: list domain errors raised by the handlers below.\n");
            s.push_str("type Error\n");
            s.push_str("  | InvalidArgument\n\n");
        }
        None => {
            s.push_str("// TODO: list domain errors raised by the handlers below.\n");
            s.push_str("// (No `#[error_code]` enum found in the program's source.)\n");
            s.push_str("type Error\n");
            s.push_str("  | InvalidArgument\n\n");
        }
    }

    for handler in &model.handlers {
        render_handler(&mut s, handler);
        s.push('\n');
    }

    s
}

fn render_handler(s: &mut String, entry: &HandlerModel) {
    match &entry.shape {
        HandlerShape::Inline => {
            s.push_str(&format!(
                "/// `{}` — inline body in the `#[program]` mod\n",
                entry.name
            ));
        }
        HandlerShape::FreeFn => {
            s.push_str(&format!("/// `{}` — free-fn forwarder\n", entry.name));
        }
        HandlerShape::Method { impl_type } => {
            s.push_str(&format!(
                "/// `{}` — method on `{}`\n",
                entry.name, impl_type
            ));
        }
        HandlerShape::Unrecognized { reason } => {
            s.push_str(&format!(
                "/// `{}` — UNRECOGNIZED forwarder ({})\n",
                entry.name, reason
            ));
            s.push_str(
                "/// TODO: classify this handler manually. The body may use a\n\
                 ///       custom dispatcher or a shape the adapter doesn't\n\
                 ///       cover yet.\n",
            );
        }
        HandlerShape::SourceWalk => {
            s.push_str(&format!(
                "/// `{}` — discovered via source-walk\n",
                entry.name
            ));
        }
    }
    if let Some(path) = &entry.source_path {
        s.push_str(&format!("/// discovered at: {}\n", path.display()));
    }
    if let Some(accounts) = &entry.accounts_type {
        s.push_str(&format!(
            "/// accounts struct: `{}` (see `#[derive(Accounts)]`)\n",
            accounts
        ));
    }

    // qedspec only accepts `//` line comments (no `/* */`), so arg-type
    // fallback notes go inside the body, not the signature.
    s.push_str(&format!("handler {}", entry.name));
    let mut unknown_args: Vec<&str> = Vec::new();
    for arg in &entry.args {
        match &arg.qedspec_type {
            Some(ty) => s.push_str(&format!(" ({} : {})", arg.name, ty)),
            None => {
                // Unknown type → U64 placeholder so the spec parses;
                // surfaced in a body comment.
                s.push_str(&format!(" ({} : U64)", arg.name));
                unknown_args.push(arg.name.as_str());
            }
        }
    }
    s.push_str(" : State.Init -> State.Init {\n");
    if !unknown_args.is_empty() {
        s.push_str(&format!(
            "  // TODO: refine arg types — could not map {} from Rust source (likely generic / Vec / Option).\n",
            unknown_args
                .iter()
                .map(|a| format!("`{}`", a))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    s.push_str("  // TODO: auth <signer>\n");
    s.push_str("  // TODO: accounts { ... }\n");
    s.push_str("  // TODO: requires\n");
    s.push_str("  // TODO: effect { ... }\n");
    s.push_str("}\n");
}

/// snake_case → PascalCase (program mod name `my_escrow` → spec name
/// `MyEscrow`).
fn to_pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = true;
    for ch in s.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(ch.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_project(tmp: &tempfile::TempDir, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = tmp.path().to_path_buf();
        for (rel, contents) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
        }
        root
    }

    #[test]
    fn adapt_renders_anchor_scaffold_program() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[
                (
                    "src/lib.rs",
                    r#"
                use anchor_lang::prelude::*;

                pub mod instructions;

                #[program]
                pub mod my_escrow {
                    use super::*;
                    pub fn initialize(ctx: Context<Initialize>, deposit_amount: u64, receive_amount: u64) -> Result<()> {
                        instructions::initialize::handler(ctx, deposit_amount, receive_amount)
                    }
                    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
                        instructions::cancel::handler(ctx)
                    }
                }
                "#,
                ),
                (
                    "src/instructions/mod.rs",
                    "pub mod initialize;\npub mod cancel;\n",
                ),
                (
                    "src/instructions/initialize.rs",
                    r#"
                use anchor_lang::prelude::*;
                pub fn handler(ctx: Context<Initialize>, deposit_amount: u64, receive_amount: u64) -> Result<()> {
                    Ok(())
                }
                "#,
                ),
                (
                    "src/instructions/cancel.rs",
                    r#"
                use anchor_lang::prelude::*;
                pub fn handler(ctx: Context<Cancel>) -> Result<()> {
                    Ok(())
                }
                "#,
                ),
            ],
        );

        let rendered = adapt(&root, &HashMap::new()).unwrap();

        assert!(
            rendered.contains("spec MyEscrow"),
            "rendered:\n{}",
            rendered
        );
        assert!(
            rendered.contains("handler initialize (deposit_amount : U64) (receive_amount : U64)")
        );
        assert!(rendered.contains("handler cancel : State.Init -> State.Init"));
        assert!(rendered.contains("src/instructions/initialize.rs"));
        assert!(rendered.contains("src/instructions/cancel.rs"));
        assert!(rendered.contains("accounts struct: `Initialize`"));
        assert!(rendered.contains("accounts struct: `Cancel`"));
        // Round-trip parsability is enforced inside `adapt()` itself.
    }

    #[test]
    fn extract_program_model_captures_anchor_handlers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[
                (
                    "src/lib.rs",
                    r#"
                use anchor_lang::prelude::*;
                pub mod instructions;

                #[program]
                pub mod my_escrow {
                    use super::*;
                    pub fn initialize(ctx: Context<Initialize>, amount: u64) -> Result<()> {
                        instructions::initialize::handler(ctx, amount)
                    }
                }
                "#,
                ),
                ("src/instructions/mod.rs", "pub mod initialize;\n"),
                (
                    "src/instructions/initialize.rs",
                    r#"
                use anchor_lang::prelude::*;
                pub fn handler(ctx: Context<Initialize>, amount: u64) -> Result<()> {
                    Ok(())
                }
                "#,
                ),
            ],
        );

        let model = extract_program_model(&root, &HashMap::new()).unwrap();

        assert_eq!(model.framework, ProgramFramework::Anchor);
        assert_eq!(model.name, "my_escrow");
        assert_eq!(
            model.primary_source.as_deref(),
            Some(Path::new("src/lib.rs"))
        );
        assert_eq!(model.entry_module.as_deref(), Some("my_escrow"));
        assert_eq!(model.handlers.len(), 1);

        let handler = &model.handlers[0];
        assert_eq!(handler.name, "initialize");
        assert_eq!(handler.accounts_type.as_deref(), Some("Initialize"));
        assert_eq!(
            handler.source_path.as_deref(),
            Some(Path::new("src/instructions/initialize.rs"))
        );
        assert_eq!(handler.shape, HandlerShape::FreeFn);
        assert_eq!(handler.args.len(), 1);
        assert_eq!(handler.args[0].name, "amount");
        assert_eq!(handler.args[0].qedspec_type.as_deref(), Some("U64"));
    }

    #[test]
    fn anchor_adapter_trait_detects_extracts_and_renders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod inline_prog {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>, x: u64) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
            )],
        );
        let overrides = HashMap::new();
        let adapter = AnchorAdapter::new(&overrides);

        assert_eq!(adapter.framework(), ProgramFramework::Anchor);
        assert!(adapter.detect(&root));

        let model = adapter.extract(&root).unwrap();
        assert_eq!(model.name, "inline_prog");
        let rendered = adapter.render_spec(&model).unwrap();
        assert!(rendered.contains("spec InlineProg"));
        assert!(rendered.contains("handler initialize (x : U64)"));
    }

    #[test]
    fn adapt_handles_inline_handler_body() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod inline_prog {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>, x: u64) -> Result<()> {
                        require!(x > 0, ErrorCode::Bad);
                        ctx.accounts.state.x = x;
                        Ok(())
                    }
                }
                "#,
            )],
        );

        let rendered = adapt(&root, &HashMap::new()).unwrap();
        assert!(rendered.contains("inline body in the `#[program]` mod"));
        assert!(rendered.contains("src/lib.rs"));
    }

    #[test]
    fn adapt_marks_unrecognized_handlers_with_todo() {
        // Forwarder names a nonexistent free fn: classifier says FreeFn,
        // resolver fails, renderer marks UNRECOGNIZED; output must still parse.
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn dispatch(ctx: Context<Dispatch>, data: u64) -> Result<()> {
                        nowhere::missing(ctx, data)
                    }
                }
                "#,
            )],
        );

        let rendered = adapt(&root, &HashMap::new()).unwrap();
        assert!(rendered.contains("UNRECOGNIZED"), "rendered:\n{}", rendered);
        assert!(rendered.contains("classify this handler manually"));
    }

    #[test]
    fn adapt_emits_typed_arg_for_user_defined_struct() {
        // Bare-path type with no generics passes through by name.
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn create(ctx: Context<Create>, args: CreateArgs) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
            )],
        );

        let rendered = adapt(&root, &HashMap::new()).unwrap();
        assert!(
            rendered.contains("(args : CreateArgs)"),
            "expected user-defined type passthrough, got:\n{}",
            rendered
        );
    }

    #[test]
    fn adapt_falls_back_for_generic_arg_types() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn ingest(ctx: Context<Ingest>, payload: Vec<u8>) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
            )],
        );

        let rendered = adapt(&root, &HashMap::new()).unwrap();
        // U64 placeholder in the signature; explanatory TODO in the body.
        assert!(rendered.contains("(payload : U64)"));
        assert!(
            rendered.contains("could not map `payload` from Rust source"),
            "rendered:\n{}",
            rendered
        );
    }

    #[test]
    fn adapt_to_file_writes_and_creates_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                #[program]
                pub mod tiny {
                    use super::*;
                    pub fn ping(ctx: Context<Ping>) -> Result<()> { Ok(()) }
                }
                "#,
            )],
        );

        let out = tmp.path().join("nested/out/tiny.qedspec");
        adapt_to_file(&root, &out, &HashMap::new()).unwrap();
        assert!(out.exists());
        let contents = std::fs::read_to_string(&out).unwrap();
        assert!(contents.contains("spec Tiny"));
        assert!(contents.contains("handler ping"));
    }

    /// Asserts `adapt(<repo>/<demo_rel>)` matches `<demo_rel>/before.qedspec`
    /// byte-for-byte. Regenerate after intentional renderer changes:
    ///   cargo run -- adapt --program <demo_rel> --out <demo_rel>/before.qedspec
    fn assert_snapshot(demo_rel: &str) {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let repo_root = Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root must be two parents up from CARGO_MANIFEST_DIR");
        let demo = repo_root.join(demo_rel);
        let expected_path = demo.join("before.qedspec");

        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!(
                "could not read snapshot at {}: {}\n\
                 (run `cargo run -- adapt --program {} --out {}` to create it)",
                expected_path.display(),
                e,
                demo_rel,
                expected_path.display(),
            )
        });

        let actual = adapt(&demo, &HashMap::new()).expect("adapter must succeed on the fixture");

        assert_eq!(
            actual,
            expected,
            "snapshot drift in {}/before.qedspec.\n\
             If intentional, regenerate with:\n\
             cargo run -- adapt --program {} --out {}",
            demo_rel,
            demo_rel,
            expected_path.display(),
        );
    }

    /// Anchor-scaffold style: free-fn forwarders into `instructions/<name>.rs`
    /// (`FreeFn` classifier).
    #[test]
    fn adapt_matches_brownfield_demo_snapshot() {
        assert_snapshot("crates/qedgen/tests/fixtures/anchor-brownfield-demo");
    }

    /// Marinade style: `ctx.accounts.<method>(...)` forwarder
    /// (`AccountsMethod` classifier + impl-method resolution).
    #[test]
    fn adapt_matches_marinade_style_snapshot() {
        assert_snapshot(
            "crates/qedgen/tests/fixtures/regressions/anchor-adapter-shapes/marinade-style",
        );
    }

    /// Squads V4 style: `<Type>::<method>(ctx, args)` forwarder (`TypeAssoc`
    /// classifier; impls inline with the program mod, not a sibling file).
    #[test]
    fn adapt_matches_squads_style_snapshot() {
        assert_snapshot(
            "crates/qedgen/tests/fixtures/regressions/anchor-adapter-shapes/squads-style",
        );
    }

    #[test]
    fn discovers_error_code_enum_with_variants() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[
                (
                    "src/lib.rs",
                    r#"
                    #[program]
                    pub mod p {
                        use super::*;
                        pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                    }
                    "#,
                ),
                (
                    "src/errors.rs",
                    r#"
                    use anchor_lang::prelude::*;

                    #[error_code]
                    pub enum ErrorCode {
                        #[msg("invalid")]
                        InvalidArgument,
                        #[msg("overflow")]
                        Overflow,
                        #[msg("not authorized")]
                        NotAuthorized,
                    }
                    "#,
                ),
            ],
        );

        let rendered = adapt(&root, &HashMap::new()).unwrap();
        assert!(
            rendered.contains("`#[error_code] pub enum ErrorCode`"),
            "rendered:\n{}",
            rendered
        );
        assert!(
            rendered.contains("| InvalidArgument"),
            "rendered:\n{}",
            rendered
        );
        assert!(rendered.contains("| Overflow"));
        assert!(rendered.contains("| NotAuthorized"));
        assert!(!rendered.contains("(No `#[error_code]` enum found"));
    }

    #[test]
    fn falls_back_to_placeholder_when_no_error_code_enum() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                }
                "#,
            )],
        );
        let rendered = adapt(&root, &HashMap::new()).unwrap();
        assert!(rendered.contains("(No `#[error_code]` enum found"));
        assert!(rendered.contains("| InvalidArgument"));
    }

    #[test]
    fn handles_qualified_error_code_attribute() {
        // `#[anchor_lang::error_code]` matches via the last path segment.
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
                }

                #[anchor_lang::error_code]
                pub enum MyError {
                    Bad,
                }
                "#,
            )],
        );
        let rendered = adapt(&root, &HashMap::new()).unwrap();
        assert!(rendered.contains("`#[error_code] pub enum MyError`"));
        assert!(rendered.contains("| Bad"));
    }

    /// Method-shape handlers (`ctx.accounts.process(...)`) emit a sealed
    /// `#[qed]` attribute via `body_hash_for_impl_fn`.
    #[test]
    fn compute_attributes_seals_method_shape_handlers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[
                (
                    "src/lib.rs",
                    r#"
                    use anchor_lang::prelude::*;

                    pub mod instructions;

                    #[program]
                    pub mod stake {
                        use super::*;
                        pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
                            ctx.accounts.process(amount)
                        }
                    }

                    pub struct Deposit;
                    "#,
                ),
                ("src/instructions/mod.rs", "pub mod deposit;\n"),
                (
                    "src/instructions/deposit.rs",
                    r#"
                    use anchor_lang::prelude::*;
                    use crate::Deposit;

                    impl Deposit {
                        pub fn process(&mut self, amount: u64) -> Result<()> {
                            Ok(())
                        }
                    }
                    "#,
                ),
            ],
        );

        let spec_path = tmp.path().join("stake.qedspec");
        std::fs::write(
            &spec_path,
            r#"
            spec Stake
            type State | Active
            handler deposit (amount : U64) : State.Active -> State.Active {
              effect { lamports += amount }
            }
            type Error | Bad
            "#,
        )
        .unwrap();

        let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.handler, "deposit");
        assert!(
            e.note.is_none(),
            "method-shape should seal cleanly: {:?}",
            e.note
        );
        assert!(e.attribute.contains("hash = \""), "attr: {}", e.attribute);
        assert!(
            e.attribute.contains("spec_hash = \""),
            "attr: {}",
            e.attribute
        );
    }

    /// A found `Context<X>` struct adds the `accounts*` triplet so the macro
    /// can seal the struct too.
    #[test]
    fn compute_attributes_includes_accounts_struct_seal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                use anchor_lang::prelude::*;

                #[program]
                pub mod p {
                    use super::*;
                    pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
                        Ok(())
                    }
                }

                #[derive(Accounts)]
                pub struct Buy<'info> {
                    pub buyer: Signer<'info>,
                    #[account(mut)]
                    pub vault: Account<'info, Vault>,
                }

                pub struct Vault;
                "#,
            )],
        );

        let spec_path = tmp.path().join("p.qedspec");
        std::fs::write(
            &spec_path,
            r#"
            spec P
            type State | Active
            handler buy (amount : U64) : State.Active -> State.Active {
              effect { count += amount }
            }
            type Error | Bad
            "#,
        )
        .unwrap();

        let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
        let buy = entries.iter().find(|e| e.handler == "buy").unwrap();
        assert!(
            buy.attribute.contains("accounts = \"Buy\""),
            "attr: {}",
            buy.attribute
        );
        assert!(buy.attribute.contains("accounts_file = \"src/lib.rs\""));
        assert!(buy.attribute.contains("accounts_hash = \""));
    }

    /// Without a resolvable `Context<X>` struct, the adapter falls back to
    /// the body+spec-only attribute.
    #[test]
    fn compute_attributes_omits_accounts_when_struct_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[(
                "src/lib.rs",
                r#"
                #[program]
                pub mod p {
                    use super::*;
                    pub fn ping(ctx: Context<MissingType>) -> Result<()> {
                        Ok(())
                    }
                }
                "#,
            )],
        );

        let spec_path = tmp.path().join("p.qedspec");
        std::fs::write(
            &spec_path,
            r#"
            spec P
            type State | Active
            handler ping : State.Active -> State.Active { effect { } }
            type Error | Bad
            "#,
        )
        .unwrap();

        let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
        let ping = entries.iter().find(|e| e.handler == "ping").unwrap();
        assert!(
            !ping.attribute.contains("accounts = "),
            "attr: {}",
            ping.attribute
        );
        assert!(ping.attribute.contains("hash = \""));
    }

    /// Two `pub struct Shared` in different modules + `Context<crate::b::Shared>`
    /// MUST seal against `crate::b::Shared`, not the first ident match.
    #[test]
    fn compute_attributes_respects_qualified_accounts_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[
                (
                    "src/lib.rs",
                    r#"
                    use anchor_lang::prelude::*;

                    pub mod a;
                    pub mod b;

                    #[program]
                    pub mod p {
                        use super::*;
                        pub fn act(ctx: Context<crate::b::Shared>, amount: u64) -> Result<()> {
                            Ok(())
                        }
                    }
                    "#,
                ),
                (
                    "src/a.rs",
                    r#"
                    use anchor_lang::prelude::*;

                    #[derive(Accounts)]
                    pub struct Shared<'info> {
                        pub user: Signer<'info>,
                        // a's version: just a signer.
                    }
                    "#,
                ),
                (
                    "src/b.rs",
                    r#"
                    use anchor_lang::prelude::*;

                    #[derive(Accounts)]
                    pub struct Shared<'info> {
                        #[account(mut)]
                        pub vault: Account<'info, Vault>,
                        pub authority: Signer<'info>,
                    }

                    pub struct Vault;
                    "#,
                ),
            ],
        );

        let spec_path = tmp.path().join("p.qedspec");
        std::fs::write(
            &spec_path,
            r#"
            spec P
            type State | Active
            handler act (amount : U64) : State.Active -> State.Active {
              effect { count += amount }
            }
            type Error | Bad
            "#,
        )
        .unwrap();

        let entries = compute_attributes(&root, &spec_path, &HashMap::new()).unwrap();
        let act = entries.iter().find(|e| e.handler == "act").unwrap();
        assert!(
            act.attribute.contains("accounts_file = \"src/b.rs\""),
            "qualified path `crate::b::Shared` should resolve to src/b.rs, got: {}",
            act.attribute
        );
        // And the hash MUST be the b.rs version, not the a.rs first-match.
        let b_hash = crate::spec_hash::accounts_struct_hash(
            &std::fs::read_to_string(root.join("src/b.rs")).unwrap(),
            "Shared",
        )
        .unwrap();
        assert!(
            act.attribute
                .contains(&format!("accounts_hash = \"{}\"", b_hash)),
            "expected hash from b.rs, got: {}",
            act.attribute
        );
    }

    #[test]
    fn handler_override_parses_module_paths() {
        let p = HandlerOverride::parse("instructions::buy::handler").unwrap();
        assert_eq!(p.module_path, vec!["instructions", "buy"]);
        assert_eq!(p.fn_name, "handler");

        let bare = HandlerOverride::parse("handler").unwrap();
        assert!(bare.module_path.is_empty());
        assert_eq!(bare.fn_name, "handler");

        // Empty input or empty segments → None
        assert!(HandlerOverride::parse("").is_none());
        assert!(HandlerOverride::parse("instructions::buy::").is_none());
        assert!(HandlerOverride::parse("::handler").is_none());
    }

    #[test]
    fn parse_handler_override_splits_on_first_equals() {
        let (name, parsed) =
            parse_handler_override("dispatch=instructions::dispatch::run").unwrap();
        assert_eq!(name, "dispatch");
        assert_eq!(parsed.module_path, vec!["instructions", "dispatch"]);
        assert_eq!(parsed.fn_name, "run");

        // Missing `=`, empty name, empty path: all errors
        assert!(parse_handler_override("dispatch").is_err());
        assert!(parse_handler_override("=path::fn").is_err());
        assert!(parse_handler_override("dispatch=").is_err());
    }

    #[test]
    fn override_resolves_unrecognized_handler_to_free_fn() {
        // Custom-dispatcher shape the classifier can't follow; a `--handler`
        // override resolves it cleanly.
        let tmp = tempfile::tempdir().unwrap();
        let root = write_project(
            &tmp,
            &[
                (
                    "src/lib.rs",
                    r#"
                    use anchor_lang::prelude::*;

                    pub mod instructions;

                    #[program]
                    pub mod dispatcher {
                        use super::*;
                        pub fn dispatch(ctx: Context<Dispatch>, data: u64) -> Result<()> {
                            // Custom dispatcher — classifier can't follow this.
                            DISPATCH_TABLE.lookup(data)(ctx, data)
                        }
                    }

                    pub struct Dispatch;
                    "#,
                ),
                ("src/instructions/mod.rs", "pub mod dispatch;\n"),
                (
                    "src/instructions/dispatch.rs",
                    r#"
                    use anchor_lang::prelude::*;
                    use crate::Dispatch;

                    pub fn handler(ctx: Context<Dispatch>, data: u64) -> Result<()> {
                        Ok(())
                    }
                    "#,
                ),
            ],
        );

        let mut overrides = HashMap::new();
        overrides.insert(
            "dispatch".to_string(),
            HandlerOverride::parse("instructions::dispatch::handler").unwrap(),
        );

        let rendered = adapt(&root, &overrides).unwrap();
        assert!(
            !rendered.contains("UNRECOGNIZED"),
            "rendered:\n{}",
            rendered
        );
        assert!(rendered.contains("free-fn forwarder"));
        assert!(rendered.contains("src/instructions/dispatch.rs"));
    }

    #[test]
    fn to_pascal_case_handles_snake_and_already_pascal() {
        assert_eq!(to_pascal_case("my_escrow"), "MyEscrow");
        assert_eq!(to_pascal_case("token_mill"), "TokenMill");
        assert_eq!(to_pascal_case("escrow"), "Escrow");
        assert_eq!(to_pascal_case("AlreadyPascal"), "AlreadyPascal");
    }
}
