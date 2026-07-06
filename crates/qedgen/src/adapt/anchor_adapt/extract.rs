use super::*;

// ----------------------------------------------------------------------------
// Rendering
// ----------------------------------------------------------------------------

pub(super) fn handler_model_from_anchor(
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

pub(super) fn rel_to(root: &Path, p: &Path) -> PathBuf {
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
pub(super) fn extract_accounts_path(program_fn: &syn::ItemFn) -> Option<Vec<String>> {
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
pub(super) fn discover_error_enum(program_root: &Path) -> Option<ErrorModel> {
    let src_dir = program_root.join("src");
    let files = walk_rust_files(&src_dir);
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

pub(super) fn walk_rust_files(dir: &Path) -> Vec<PathBuf> {
    crate::fs_walk::collect_rs_files(dir, crate::fs_walk::DEFAULT_SKIP_DIRS)
}
