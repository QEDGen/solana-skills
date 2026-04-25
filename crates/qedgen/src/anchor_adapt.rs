//! Brownfield adapter (v2.9 M4.3).
//!
//! Given the path to an existing Anchor program crate (the directory
//! holding `Cargo.toml`, with `src/lib.rs` inside), emit a starter
//! `.qedspec` covering every discovered instruction. The user fills in
//! the state machine, guards, and effects — the adapter handles the
//! mechanical work of listing handlers, extracting argument types,
//! recording the accounts struct, and leaving a breadcrumb to where
//! each body lives in source.
//!
//! Pipeline:
//!   1. `anchor_project::parse_anchor_project` finds the `#[program]`
//!      mod and lists its `pub fn` instructions.
//!   2. `anchor_resolver::resolve_handler` follows each forwarder to
//!      the actual handler ItemFn (or reports Unrecognized).
//!   3. This module renders the result as a parseable `.qedspec`
//!      skeleton with `// TODO:` markers for the parts that need
//!      semantic input.
//!
//! The output is round-tripped through `chumsky_adapter::parse_str` so
//! a regression in the renderer surfaces immediately as a parse error
//! at adapt-time rather than the next `qedgen check`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::anchor_project::{parse_anchor_project, AnchorProject, Instruction};
use crate::anchor_resolver::{resolve_handler, HandlerLocation};

/// Generate a starter `.qedspec` for an existing Anchor program.
///
/// `program_root` is the program crate's directory (sibling of `src/`).
/// Returns the rendered source so the caller can choose between
/// stdout (one-shot inspection) and writing to a file.
pub fn adapt(program_root: &Path) -> Result<String> {
    let project = parse_anchor_project(program_root).with_context(|| {
        format!(
            "failed to parse Anchor project at {}",
            program_root.display()
        )
    })?;

    let mut entries = Vec::with_capacity(project.instructions.len());
    for instruction in &project.instructions {
        let location = resolve_handler(instruction, &project.lib_rs_path, program_root)?;
        entries.push(HandlerEntry::from(instruction, &location, program_root));
    }

    let rendered = render_spec(&project, &entries, program_root);

    // Round-trip: a parse failure here is a renderer bug, not user
    // input — surface it loudly at adapt-time, not on the next check.
    crate::chumsky_adapter::parse_str(&rendered).context(
        "Generated .qedspec failed to parse — this is a bug in `qedgen adapt`. \
         Please report at https://github.com/qedgen/solana-skills/issues",
    )?;

    Ok(rendered)
}

/// Convenience wrapper: write the adapted `.qedspec` to disk.
pub fn adapt_to_file(program_root: &Path, output_path: &Path) -> Result<()> {
    let rendered = adapt(program_root)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    std::fs::write(output_path, &rendered)
        .with_context(|| format!("writing {}", output_path.display()))?;
    eprintln!("Wrote {} ({} bytes)", output_path.display(), rendered.len());
    Ok(())
}

// ----------------------------------------------------------------------------
// Attribute mode: `qedgen adapt --program <crate> --spec <path>`
//
// Given an existing .qedspec and the user's Anchor source, emit one
// `#[qed(verified, spec = ..., handler = ..., hash = ..., spec_hash = ...)]`
// attribute per spec handler so the user can paste them above each
// handler body. The body hash matches what `qedgen-macros` will
// recompute at compile time; the spec hash is computed via the shared
// `spec_hash::spec_hash_for_handler`.
// ----------------------------------------------------------------------------

/// One emitted attribute entry, ready for the user to paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeEntry {
    /// Handler name, as it appears in both the spec and the program's
    /// `#[program]` mod.
    pub handler: String,
    /// Path to the file holding the actual handler body, relative to
    /// the program root. Free-fn handlers point at e.g.
    /// `src/instructions/buy.rs`; inline handlers at `src/lib.rs`.
    pub source_path: PathBuf,
    /// The `#[qed(...)]` attribute line ready to paste verbatim above
    /// the handler `pub fn`.
    pub attribute: String,
    /// Why we couldn't emit an attribute, when `attribute` is empty.
    /// E.g. a method-shape forwarder (impl block — macro doesn't
    /// handle ImplItemFn yet) or an Unrecognized handler.
    pub note: Option<String>,
}

/// Compute the `#[qed]` attributes for every handler declared in
/// `spec_path` against the Anchor program at `program_root`. Returns
/// one entry per spec handler. Handlers that exist in the spec but
/// aren't in the program show up as a finding from
/// `anchor_check::check_anchor_coverage` instead.
pub fn compute_attributes(program_root: &Path, spec_path: &Path) -> Result<Vec<AttributeEntry>> {
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

    // Spec path written into the attribute is relative to program_root —
    // the macro resolves it against `CARGO_MANIFEST_DIR`, which is
    // exactly the program crate's root.
    let spec_rel = spec_path
        .strip_prefix(program_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| spec_path.to_path_buf());

    let mut out = Vec::new();
    for handler in &parsed_spec.handlers {
        let Some(instruction) = project.instructions.iter().find(|i| i.name == handler.name) else {
            // Spec handler with no matching `pub fn` in the program —
            // surface as a note; the user gets a richer diagnostic
            // from `qedgen check --anchor-project ...`.
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

        let location = resolve_handler(instruction, &project.lib_rs_path, program_root)?;
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
                let spec_hash = crate::spec_hash::spec_hash_for_handler(
                    &spec_source,
                    &handler.name,
                )
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "internal error: parsed handler `{}` but couldn't extract its block from {}",
                        handler.name,
                        spec_path.display()
                    )
                })?;
                AttributeEntry {
                    handler: handler.name.clone(),
                    source_path: rel_to(program_root, &source_path),
                    attribute: format!(
                        "#[qed(verified, spec = \"{}\", handler = \"{}\", hash = \"{}\", spec_hash = \"{}\")]",
                        spec_rel.display(),
                        handler.name,
                        body_hash,
                        spec_hash,
                    ),
                    note: None,
                }
            }
            HandlerLocation::Method {
                source_path,
                impl_type,
                ..
            } => AttributeEntry {
                handler: handler.name.clone(),
                source_path: rel_to(program_root, &source_path),
                attribute: String::new(),
                note: Some(format!(
                    "method-shape forwarder (`impl {}` block) — `#[qed]` annotation requires a free-fn handler in v2.9. Either refactor to a free fn or wait for v2.10's impl-method support",
                    impl_type
                )),
            },
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

/// Render the attribute entries as a paste-friendly text report:
/// per-handler section with the source file pointer + the attribute
/// line. Skipped handlers carry a `// note: …` block instead.
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

#[derive(Debug)]
struct HandlerEntry {
    name: String,
    /// `(arg_name, qedspec_type_or_raw_rust)` — the second slot is None
    /// when the renderer couldn't map the Rust type to a qedspec type
    /// (e.g. `Vec<MyStruct>`); we fall back to a TODO comment.
    args: Vec<(String, Option<String>)>,
    /// Type written in the handler's `Context<X>` (e.g. `Buy`). The
    /// adapter emits this as a comment so the user can copy
    /// constraint info from the `#[derive(Accounts)]` struct.
    accounts_type: Option<String>,
    /// Path to the file containing the actual handler body, relative
    /// to the program root. None when the resolver returned
    /// Unrecognized.
    source_breadcrumb: Option<PathBuf>,
    /// What the resolver classified this handler as. Inline / FreeFn /
    /// Method / Unrecognized — surfaced in a `// shape:` comment so
    /// the human reader can see at a glance how the body was reached.
    shape: HandlerShape,
}

#[derive(Debug)]
enum HandlerShape {
    Inline,
    FreeFn,
    Method { impl_type: String },
    Unrecognized { reason: String },
}

impl HandlerEntry {
    fn from(instruction: &Instruction, location: &HandlerLocation, program_root: &Path) -> Self {
        let args = extract_args(&instruction.program_fn);
        let accounts_type = extract_accounts_type(&instruction.program_fn);
        let (source_breadcrumb, shape) = match location {
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
        HandlerEntry {
            name: instruction.name.clone(),
            args,
            accounts_type,
            source_breadcrumb,
            shape,
        }
    }
}

fn rel_to(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| p.to_path_buf())
}

/// Walk `program_fn.sig.inputs` skipping the leading `Context<...>`
/// and produce `(name, qedspec_type_or_raw_rust)` pairs. Self/receiver
/// arguments don't appear in `#[program]` mod fns, so we don't handle
/// them.
fn extract_args(program_fn: &syn::ItemFn) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut skipped_ctx = false;
    for input in &program_fn.sig.inputs {
        let pat_type = match input {
            syn::FnArg::Typed(p) => p,
            // `&self` / `&mut self` shouldn't appear here, but skip
            // defensively rather than panic.
            syn::FnArg::Receiver(_) => continue,
        };
        // The first typed arg is always the Context<X>; skip exactly
        // one. Subsequent positional Context-typed args (rare) flow
        // through to the spec — the user can prune them.
        if !skipped_ctx && is_context_type(&pat_type.ty) {
            skipped_ctx = true;
            continue;
        }
        let name = match &*pat_type.pat {
            syn::Pat::Ident(pi) => pi.ident.to_string(),
            // Destructured / unusual patterns: emit a numbered
            // placeholder so the spec still parses; the user renames.
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

/// Pull the `X` out of `Context<X>` (or `Context<'info, X>`). Returns
/// the bare ident, no generics. None when the first arg isn't a
/// Context — the adapter still emits the handler, just without the
/// accounts breadcrumb.
fn extract_accounts_type(program_fn: &syn::ItemFn) -> Option<String> {
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
            if let Some(seg) = tp.path.segments.last() {
                return Some(seg.ident.to_string());
            }
        }
    }
    None
}

/// Best-effort Rust → qedspec type translation. Mirrors `idl2spec::map_type`
/// for primitive types; falls back to `None` (renderer emits a TODO
/// comment) for shapes we don't yet handle (Vec/Option/arrays/generics).
fn map_rust_type(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(tp) = ty else { return None };
    let last = tp.path.segments.last()?;
    // Reject types with generics (Vec<u8>, Option<T>, etc.) — leave
    // them for the user to model.
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
        // Treat unknown bare paths as user-defined types passed by
        // name. The user will declare them in the spec or the adapter
        // round-trip will catch a typo at parse-time.
        other if !other.is_empty() => return Some(other.to_string()),
        _ => return None,
    };
    Some(mapped.to_string())
}

fn render_spec(project: &AnchorProject, entries: &[HandlerEntry], program_root: &Path) -> String {
    let mut s = String::new();
    s.push_str("// Generated by `qedgen adapt`. Fill in the TODOs to make this verifiable.\n");
    // Use the program-root-relative path so snapshots are stable across
    // machines (the absolute path includes the user's home directory).
    let rel_lib_rs = rel_to(program_root, &project.lib_rs_path);
    s.push_str(&format!(
        "// Source: {} (program mod: `{}`)\n\n",
        rel_lib_rs.display(),
        project.program_mod_name,
    ));
    s.push_str(&format!(
        "spec {}\n\n",
        to_pascal_case(&project.program_mod_name)
    ));

    s.push_str("// TODO: replace with the actual lifecycle of your program.\n");
    s.push_str("type State\n");
    s.push_str("  | Init\n");
    s.push_str("  | Active\n\n");

    s.push_str("// TODO: list domain errors raised by the handlers below.\n");
    s.push_str("type Error\n");
    s.push_str("  | InvalidArgument\n\n");

    for entry in entries {
        render_handler(&mut s, entry);
        s.push('\n');
    }

    s
}

fn render_handler(s: &mut String, entry: &HandlerEntry) {
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
    }
    if let Some(path) = &entry.source_breadcrumb {
        s.push_str(&format!("/// discovered at: {}\n", path.display()));
    }
    if let Some(accounts) = &entry.accounts_type {
        s.push_str(&format!(
            "/// accounts struct: `{}` (see `#[derive(Accounts)]`)\n",
            accounts
        ));
    }

    // Header line: `handler <name> (a: T) (b: T) : State.Init -> State.Init {`
    // qedspec only accepts `//` line comments (no `/* */`), so any
    // arg-type fallback notes have to go inside the body, not in the
    // signature.
    s.push_str(&format!("handler {}", entry.name));
    let mut unknown_args: Vec<&str> = Vec::new();
    for (arg_name, arg_ty) in &entry.args {
        match arg_ty {
            Some(ty) => s.push_str(&format!(" ({} : {})", arg_name, ty)),
            None => {
                // Unknown type → use U64 as a placeholder so the spec
                // parses, and surface the fact in a body comment.
                s.push_str(&format!(" ({} : U64)", arg_name));
                unknown_args.push(arg_name.as_str());
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

/// snake_case → PascalCase. Used to coerce a program mod name like
/// `my_escrow` into a spec name `MyEscrow`. Same shape as
/// `idl2spec::map_type`'s passthrough branch — kept private here to
/// avoid a public dependency.
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

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

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

        let rendered = adapt(&root).unwrap();

        // Spec name is PascalCase'd from the program mod ident.
        assert!(
            rendered.contains("spec MyEscrow"),
            "rendered:\n{}",
            rendered
        );
        // Both handlers appear with their typed arguments.
        assert!(
            rendered.contains("handler initialize (deposit_amount : U64) (receive_amount : U64)")
        );
        assert!(rendered.contains("handler cancel : State.Init -> State.Init"));
        // Source breadcrumb points at the per-instruction file.
        assert!(rendered.contains("src/instructions/initialize.rs"));
        assert!(rendered.contains("src/instructions/cancel.rs"));
        // Accounts struct is surfaced as a comment for the user.
        assert!(rendered.contains("accounts struct: `Initialize`"));
        assert!(rendered.contains("accounts struct: `Cancel`"));
        // Round-trip parsability is enforced inside `adapt()`; if we
        // got here, the output parses.
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

        let rendered = adapt(&root).unwrap();
        assert!(rendered.contains("inline body in the `#[program]` mod"));
        assert!(rendered.contains("src/lib.rs"));
    }

    #[test]
    fn adapt_marks_unrecognized_handlers_with_todo() {
        // The forwarder names a free fn that doesn't exist anywhere
        // in the program crate. The classifier returns FreeFn, the
        // resolver fails to find it, the renderer marks the entry
        // UNRECOGNIZED. The output still has to parse.
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

        let rendered = adapt(&root).unwrap();
        assert!(rendered.contains("UNRECOGNIZED"), "rendered:\n{}", rendered);
        assert!(rendered.contains("classify this handler manually"));
    }

    #[test]
    fn adapt_emits_typed_arg_for_user_defined_struct() {
        // Bare-path type with no generics: passthrough as the name
        // (user declares the struct in the spec or fixes a typo).
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

        let rendered = adapt(&root).unwrap();
        assert!(
            rendered.contains("(args : CreateArgs)"),
            "expected user-defined type passthrough, got:\n{}",
            rendered
        );
    }

    #[test]
    fn adapt_falls_back_for_generic_arg_types() {
        // `Vec<u8>` has generics → renderer emits TODO placeholder.
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

        let rendered = adapt(&root).unwrap();
        // Placeholder type lives in the signature; the explanatory
        // TODO is in the body so the spec parses.
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
        adapt_to_file(&root, &out).unwrap();
        assert!(out.exists());
        let contents = std::fs::read_to_string(&out).unwrap();
        assert!(contents.contains("spec Tiny"));
        assert!(contents.contains("handler ping"));
    }

    /// Snapshot test against the worked-example fixture in
    /// `examples/anchor-brownfield-demo/`. Holds the renderer steady
    /// across refactors and gives the README's "the output matches
    /// before.qedspec byte-for-byte" claim something to rest on.
    ///
    /// To regenerate after an intentional renderer change:
    ///   cargo run -- adapt --program examples/anchor-brownfield-demo \
    ///     --out examples/anchor-brownfield-demo/before.qedspec
    #[test]
    fn adapt_matches_brownfield_demo_snapshot() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        // Tests run from `crates/qedgen/`; walk up to the repo root.
        let repo_root = Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root must be two parents up from CARGO_MANIFEST_DIR");
        let demo = repo_root.join("examples/anchor-brownfield-demo");
        let expected_path = demo.join("before.qedspec");

        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|e| {
            panic!(
                "could not read snapshot at {}: {}\n\
                 (run `cargo run -- adapt --program examples/anchor-brownfield-demo \\\n\
                 --out examples/anchor-brownfield-demo/before.qedspec` to create it)",
                expected_path.display(),
                e,
            )
        });

        let actual = adapt(&demo).expect("adapter must succeed on the demo fixture");

        assert_eq!(
            actual, expected,
            "snapshot drift in examples/anchor-brownfield-demo/before.qedspec.\n\
             If this is intentional, regenerate with:\n\
             cargo run -- adapt --program examples/anchor-brownfield-demo \\\n\
                --out examples/anchor-brownfield-demo/before.qedspec",
        );
    }

    #[test]
    fn to_pascal_case_handles_snake_and_already_pascal() {
        assert_eq!(to_pascal_case("my_escrow"), "MyEscrow");
        assert_eq!(to_pascal_case("token_mill"), "TokenMill");
        assert_eq!(to_pascal_case("escrow"), "Escrow");
        // Idempotent on PascalCase input.
        assert_eq!(to_pascal_case("AlreadyPascal"), "AlreadyPascal");
    }
}
