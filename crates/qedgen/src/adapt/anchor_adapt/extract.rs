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
    let accounts = resolve_account_roles(&instruction.program_fn, program_root);
    HandlerModel {
        name: instruction.name.clone(),
        args,
        accounts_type,
        accounts,
        source_path,
        shape,
    }
}

/// Resolve the handler's `#[derive(Accounts)]` struct fields into qedspec
/// `accounts { }` roles. Mechanically derivable from Anchor types + `#[account]`
/// constraints; empty when the struct can't be located/parsed (renderer then
/// falls back to a `TODO`). Qualified `Context<crate::a::Shared>` paths
/// prioritize the matching source module so duplicate struct names do not
/// silently select the wrong account layout.
fn resolve_account_roles(program_fn: &syn::ItemFn, program_root: &Path) -> Vec<AccountRoleModel> {
    let Some(segments) = extract_accounts_path(program_fn) else {
        return Vec::new();
    };
    let Some(struct_name) = segments.last() else {
        return Vec::new();
    };
    let module_prefix = normalize_module_prefix(&segments[..segments.len() - 1]);
    let src_dir = program_root.join("src");
    let candidates = walk_rust_files(&src_dir);
    for path in prioritize_candidates(&candidates, &src_dir, &module_prefix) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        if let Some(item_struct) = find_struct_in_items(&file.items, struct_name) {
            return item_struct
                .fields
                .iter()
                .filter_map(field_account_role)
                .collect();
        }
    }
    Vec::new()
}

/// Depth-first search for `pub struct <name>`, recursing into inline `mod`s so
/// Accounts structs nested in a submodule still resolve.
fn find_struct_in_items<'a>(items: &'a [syn::Item], name: &str) -> Option<&'a syn::ItemStruct> {
    for item in items {
        match item {
            syn::Item::Struct(s) if s.ident == name => return Some(s),
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    if let Some(found) = find_struct_in_items(inner, name) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// One Accounts-struct field → its qedspec descriptor. `None` for a field that
/// can't be classified into at least one valid attribute (e.g. a read-only
/// `UncheckedAccount` with no data type) — skipped rather than emitting an
/// unparseable zero-attr descriptor.
fn field_account_role(field: &syn::Field) -> Option<AccountRoleModel> {
    let name = field.ident.as_ref()?.to_string();
    let is_mut = account_attr_has_word(&field.attrs, "mut");
    let (is_signer_type, is_program, inner_ty) = classify_account_type(&field.ty);
    let is_signer = is_signer_type || account_attr_has_word(&field.attrs, "signer");

    let mut attrs = Vec::new();
    if is_signer {
        attrs.push("signer".to_string());
    }
    if is_mut {
        attrs.push("writable".to_string());
    }
    if is_program {
        attrs.push("program".to_string());
    }
    if attrs.is_empty() {
        // Read-only, non-signer, non-program: describe by its data type when
        // known (`type <T>`); otherwise there's no valid single-attr form.
        let ty = inner_ty?;
        attrs.push(format!("type {}", ty));
    }
    Some(AccountRoleModel {
        name,
        attrs,
        is_signer,
    })
}

/// True when any `#[account(...)]` attribute's token list contains `word` as a
/// standalone token (`#[account(mut)]` → `mut`). Uses `TokenStream` Display,
/// which space-separates tokens, so `mut` never matches inside another ident.
fn account_attr_has_word(attrs: &[syn::Attribute], word: &str) -> bool {
    for attr in attrs {
        if let syn::Meta::List(list) = &attr.meta {
            if list.path.is_ident("account")
                && list
                    .tokens
                    .to_string()
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|tok| tok == word)
            {
                return true;
            }
        }
    }
    false
}

/// Classify an Anchor account field type → (is_signer, is_program, data_type).
/// Unwraps `Box<...>`; keys off the outermost path segment.
fn classify_account_type(ty: &syn::Type) -> (bool, bool, Option<String>) {
    let syn::Type::Path(tp) = ty else {
        return (false, false, None);
    };
    let Some(seg) = tp.path.segments.last() else {
        return (false, false, None);
    };
    let ident = seg.ident.to_string();
    match ident.as_str() {
        "Signer" => (true, false, None),
        "Program" | "Sysvar" => (false, true, None),
        // `Box<Account<'info, T>>` — unwrap and recurse.
        "Box" => {
            if let Some(inner) = first_generic_type(&seg.arguments) {
                classify_account_type(inner)
            } else {
                (false, false, None)
            }
        }
        // Typed data accounts: the qedspec `type` is the last generic type arg.
        "Account" | "InterfaceAccount" | "AccountLoader" => {
            (false, false, last_generic_type_ident(&seg.arguments))
        }
        // System-owned / opaque accounts carry no data type to name.
        _ => (false, false, None),
    }
}

/// First angle-bracketed generic type argument (for `Box<T>`).
fn first_generic_type(args: &syn::PathArguments) -> Option<&syn::Type> {
    let syn::PathArguments::AngleBracketed(ab) = args else {
        return None;
    };
    ab.args.iter().find_map(|a| match a {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// Ident of the last generic *type* argument (`Account<'info, Settings>` →
/// `Settings`), skipping lifetimes.
fn last_generic_type_ident(args: &syn::PathArguments) -> Option<String> {
    let syn::PathArguments::AngleBracketed(ab) = args else {
        return None;
    };
    ab.args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(syn::Type::Path(tp)) => {
                tp.path.segments.last().map(|s| s.ident.to_string())
            }
            _ => None,
        })
        .next_back()
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

/// Derive a lifecycle `State` from an `#[account]` struct's status-enum field
/// (e.g. `Proposal.status: ProposalStatus`). Two passes over `src/`: collect
/// every program-defined enum, then find the `#[account]` field typed by one —
/// preferring a field named `status`/`state`, then the richest enum. The
/// transition *edges* aren't derivable (they need the impl), so only the
/// variant set is returned. `None` when nothing qualifies.
pub(super) fn discover_state_enum(program_root: &Path) -> Option<StateModel> {
    let src_dir = program_root.join("src");
    let files = walk_rust_files(&src_dir);

    // Pass 1: every `enum X { .. }` in the program, name -> (variants, file).
    // First definition wins (deterministic walk order).
    let mut enums: HashMap<String, (Vec<String>, PathBuf)> = HashMap::new();
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        collect_enums(&file.items, path, &mut enums);
    }
    if enums.is_empty() {
        return None;
    }

    // Pass 2: `#[account]` struct fields typed by one of those enums; keep the
    // highest-scoring (status/state field name, then variant count).
    let mut best: Option<(i64, StateModel)> = None;
    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&source) else {
            continue;
        };
        for (struct_name, field_name, enum_name) in account_enum_fields(&file.items, &enums) {
            let (variants, enum_path) = &enums[&enum_name];
            let mut score = variants.len() as i64;
            if field_name == "status" || field_name == "state" {
                score += 100;
            }
            if best.as_ref().is_none_or(|(bs, _)| score > *bs) {
                best = Some((
                    score,
                    StateModel {
                        source_path: Some(rel_to(program_root, enum_path)),
                        enum_name: enum_name.clone(),
                        variants: variants.clone(),
                        account_struct: struct_name,
                        field_name,
                    },
                ));
            }
        }
    }
    best.map(|(_, m)| m)
}

/// Collect every `enum` declaration (recursing into inline mods). First
/// definition of a given name wins.
fn collect_enums(
    items: &[syn::Item],
    path: &Path,
    out: &mut HashMap<String, (Vec<String>, PathBuf)>,
) {
    for item in items {
        match item {
            syn::Item::Enum(e) => {
                let variants = e.variants.iter().map(|v| v.ident.to_string()).collect();
                out.entry(e.ident.to_string())
                    .or_insert((variants, path.to_path_buf()));
            }
            syn::Item::Mod(m) => {
                if let Some((_, sub)) = &m.content {
                    collect_enums(sub, path, out);
                }
            }
            _ => {}
        }
    }
}

/// `(struct, field, enum_name)` for each `#[account]` struct field whose type
/// is one of the program's enums. Recurses into inline mods.
fn account_enum_fields(
    items: &[syn::Item],
    enums: &HashMap<String, (Vec<String>, PathBuf)>,
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for item in items {
        match item {
            syn::Item::Struct(s) if has_account_attr(&s.attrs) => {
                for field in &s.fields {
                    let Some(fname) = field.ident.as_ref().map(|i| i.to_string()) else {
                        continue;
                    };
                    if let Some(enum_name) = enum_type_name(&field.ty, enums) {
                        out.push((s.ident.to_string(), fname, enum_name));
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, sub)) = &m.content {
                    out.extend(account_enum_fields(sub, enums));
                }
            }
            _ => {}
        }
    }
    out
}

/// True for a struct-level `#[account]` attribute (Anchor state accounts),
/// matched by last path segment.
fn has_account_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "account")
    })
}

/// The enum name when `ty`'s outermost path segment is a known program enum.
fn enum_type_name(
    ty: &syn::Type,
    enums: &HashMap<String, (Vec<String>, PathBuf)>,
) -> Option<String> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    let ident = tp.path.segments.last()?.ident.to_string();
    enums.contains_key(&ident).then_some(ident)
}

pub(super) fn walk_rust_files(dir: &Path) -> Vec<PathBuf> {
    crate::fs_walk::collect_rs_files(dir, crate::fs_walk::DEFAULT_SKIP_DIRS)
}
