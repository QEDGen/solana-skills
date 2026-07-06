//! Source-derived Pinocchio proof profile — the extraction side of the
//! Pinocchio `--kani-impl` layer. Reads committed Pinocchio source and
//! recovers what the generic Kani emitter needs: dispatcher tags, account
//! slice order, numeric instruction-data fields, and PDA derivation seeds.
//! ABI schemas can extend the profile without teaching the Kani backend
//! about any specific program.
//!
//! Split into per-concern submodules; this facade owns the shared imports
//! (so submodules use `use super::*`), the inference entry points, and the
//! re-exports that keep the external `crate::pinocchio_profile::<name>`
//! surface (the 12 `Pinocchio*` structs + `infer_from_context`) intact.

use anyhow::Result;
use quote::ToTokens;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use syn::{Expr, Item, ItemFn, Pat, Stmt};

mod abi;
mod ast_util;
mod infer;
mod types;

// The directory rename keeps the module path `crate::adapt::pinocchio_profile`
// (and the root re-export `crate::pinocchio_profile`) intact; these globs
// re-export each submodule's items so the existing
// `crate::pinocchio_profile::<name>` call sites in `codegen::kani_impl` — and
// the cross-submodule references — keep resolving unchanged.
pub(crate) use types::*;

pub(in crate::adapt::pinocchio_profile) use abi::*;
pub(in crate::adapt::pinocchio_profile) use ast_util::*;
pub(in crate::adapt::pinocchio_profile) use infer::*;

#[cfg(test)]
mod tests;

/// Infer a proof profile from a Pinocchio crate's `src/` directory. Missing
/// or unrecognized patterns produce an empty/partial profile instead of an
/// error; the Kani emitter can then fall back to spec-order generation.
/// Unparseable Rust, by contrast, is a hard error — silent under-inference
/// is worse than a loud failure.
#[cfg(test)]
pub(crate) fn infer_from_src_dir(src_dir: &Path) -> Result<PinocchioProofProfile> {
    infer_from_src_dirs([(src_dir.to_path_buf(), false)])
}

/// Infer a proof profile from the generated output location plus the source
/// tree implied by the `.qedspec` path. Later candidates override earlier
/// candidates, so committed source/ABI facts win over generated scaffolds.
pub(crate) fn infer_from_context(
    output_src_dir: &Path,
    spec_path: Option<&Path>,
) -> Result<PinocchioProofProfile> {
    let cwd = std::env::current_dir()?;
    let output_src_dir = absolutize_context_path(output_src_dir, &cwd);
    let spec_path = spec_path.map(|path| absolutize_context_path(path, &cwd));
    let mut candidates = vec![(output_src_dir, false)];
    if let Some(spec_path) = spec_path.as_deref() {
        if let Some(parent) = spec_path.parent() {
            candidates.push((parent.join("src"), true));
            if let Some(program_root) = parent.parent() {
                candidates.push((program_root.join("src"), true));
            }
        }
    }
    infer_from_src_dirs(candidates)
}

fn absolutize_context_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn infer_from_src_dirs<I>(src_dirs: I) -> Result<PinocchioProofProfile>
where
    I: IntoIterator<Item = (PathBuf, bool)>,
{
    let mut merged = PinocchioProofProfile {
        handlers: BTreeMap::new(),
        pda_derivations: BTreeMap::new(),
        record_layouts: BTreeMap::new(),
        account_layouts: BTreeMap::new(),
    };
    let mut candidates = Vec::<(PathBuf, bool)>::new();
    for (src_dir, include_siblings) in src_dirs {
        if !src_dir.is_dir() {
            continue;
        }
        if let Some((_, existing)) = candidates.iter_mut().find(|(path, _)| path == &src_dir) {
            *existing |= include_siblings;
        } else {
            candidates.push((src_dir, include_siblings));
        }
    }
    for (src_dir, include_siblings) in candidates {
        let profile = infer_single_src_dir(&src_dir, include_siblings)?;
        merged.merge_profile(profile);
    }
    Ok(merged)
}

fn infer_single_src_dir(src_dir: &Path, include_siblings: bool) -> Result<PinocchioProofProfile> {
    let mut files = Vec::new();
    collect_rust_files(src_dir, &mut files);
    files.sort();

    let mut handlers: BTreeMap<String, PinocchioHandlerProfile> = BTreeMap::new();
    let mut pda_derivations: BTreeMap<String, PinocchioPdaDerivation> = BTreeMap::new();
    let mut parsed_files = Vec::new();
    let mut parse_errors = Vec::new();
    let mut contracted_fns = BTreeMap::new();
    let mut call_graph = BTreeMap::new();
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let syntax = match syn::parse_file(&source) {
            Ok(syntax) => syntax,
            Err(err) => {
                let start = err.span().start();
                parse_errors.push(format!(
                    "{}:{}:{}: {err}",
                    path.display(),
                    start.line,
                    start.column + 1
                ));
                continue;
            }
        };
        let fns = collect_item_fns(&syntax.items);
        for item_fn in &fns {
            call_graph.insert(
                item_fn.sig.ident.to_string(),
                infer_called_fn_names_from_block(&item_fn.block),
            );
            if item_fn_has_kani_contract(item_fn) {
                contracted_fns.insert(
                    item_fn.sig.ident.to_string(),
                    crate_fn_path(src_dir, &path, &item_fn.sig.ident.to_string()),
                );
            }
        }
        parsed_files.push(syntax);
    }
    if !parse_errors.is_empty() {
        anyhow::bail!(
            "unparseable Rust under {}: {} file(s) failed to parse:\n  {}",
            src_dir.display(),
            parse_errors.len(),
            parse_errors.join("\n  ")
        );
    }
    for syntax in parsed_files {
        let fns = collect_item_fns(&syntax.items);
        for item_fn in &fns {
            let Some(name) = process_handler_name(item_fn) else {
                continue;
            };
            let entry = handlers
                .entry(name.clone())
                .or_insert_with(|| empty_handler_profile(name));
            if entry.accounts.is_empty() {
                entry.accounts = infer_accounts_from_block(&item_fn.block);
            }
            let role_accounts = if entry.accounts.is_empty() {
                infer_accounts_from_block(&item_fn.block)
            } else {
                entry.accounts.clone()
            };
            for (account, role) in infer_account_roles_from_block(&item_fn.block, &role_accounts) {
                entry.account_roles.entry(account).or_default().merge(role);
            }
            for (account, param) in infer_mint_decimal_bindings_from_block(&item_fn.block) {
                entry.mint_decimal_bindings.insert(account, param);
            }
            let key_account_aliases = infer_key_account_aliases_from_block(&item_fn.block);
            let local_key_derivations = infer_local_key_derivations_from_block(&item_fn.block);
            for (account, binding) in infer_token_account_bindings_from_block(
                &item_fn.block,
                &key_account_aliases,
                &local_key_derivations,
            ) {
                entry.token_account_bindings.insert(account, binding);
            }
            for (account, derivation) in
                infer_account_key_derivations_from_block(&item_fn.block, &local_key_derivations)
            {
                entry.account_key_derivations.insert(account, derivation);
            }
            for (expr, alias) in infer_source_expr_aliases_from_block(&item_fn.block) {
                entry.source_expr_aliases.insert(expr, alias);
            }
            for stub in
                infer_verified_stubs_from_block(&item_fn.block, &contracted_fns, &call_graph)
            {
                if !entry.verified_stubs.contains(&stub) {
                    entry.verified_stubs.push(stub);
                }
            }
            if entry.params.is_empty() {
                entry.params = infer_params_from_block(&item_fn.block);
            }
        }
        infer_dispatch_tags_from_items(&syntax.items, &mut handlers);
        infer_pda_derivations_from_fns(&fns, &mut pda_derivations);
    }

    let mut profile = PinocchioProofProfile {
        handlers,
        pda_derivations,
        record_layouts: BTreeMap::new(),
        account_layouts: BTreeMap::new(),
    };
    for schema in load_nearby_abi_schemas(src_dir, include_siblings)? {
        profile.merge_abi_schema(schema);
    }
    Ok(profile)
}

fn empty_handler_profile(name: String) -> PinocchioHandlerProfile {
    PinocchioHandlerProfile {
        name,
        ..Default::default()
    }
}
