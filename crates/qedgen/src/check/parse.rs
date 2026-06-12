use super::*;
use anyhow::{Context, Result};
use std::path::Path;

/// Parse a spec from disk (.qedspec only). `path` is a single `.qedspec`
/// file or a directory: every `.qedspec` under it (recursively) must declare
/// the same `spec Name`; top items merge in sorted source-path order. The
/// multi-file form is pure convention — no grammar, no `import`/`module`.
pub fn parse_spec_file(path: &Path) -> Result<ParsedSpec> {
    parse_spec_file_with_opts(
        path,
        crate::qed_lock::LockMode::Auto,
        crate::import_resolver::CacheOpts::default(),
    )
}

/// Parse with explicit qed.lock mode (e.g. `qedgen check --frozen` passes
/// `LockMode::Frozen`). Thin wrapper kept for existing external callers.
#[allow(dead_code)]
pub fn parse_spec_file_with_lock(
    path: &Path,
    lock_mode: crate::qed_lock::LockMode,
) -> Result<ParsedSpec> {
    parse_spec_file_with_opts(
        path,
        lock_mode,
        crate::import_resolver::CacheOpts::default(),
    )
}

/// Full-control entry: explicit lock mode + cache policy.
/// `qedgen check --frozen --no-cache` calls this with both overrides.
pub fn parse_spec_file_with_opts(
    path: &Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> Result<ParsedSpec> {
    if path.is_dir() {
        return parse_spec_dir_with_opts(path, lock_mode, cache_opts);
    }

    // A non-existent path would otherwise fall through to the extension
    // check and report a confusing "Unsupported spec format: .".
    if !path.exists() {
        anyhow::bail!(
            "spec path does not exist: {}\n\
             Pass either a `.qedspec` file or a directory containing `.qedspec` files.",
            path.display()
        );
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "qedspec" {
        anyhow::bail!(
            "Unsupported spec format: .{}. Only .qedspec files are supported.\n\
             Convert Lean specs to .qedspec format (see examples/).",
            ext
        );
    }

    let src =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let typed = crate::chumsky_parser::parse(&src).map_err(|errs| {
        let msg = errs
            .iter()
            .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, &src)))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!("parse error in {}:\n{}", path.display(), msg)
    })?;
    let mut parsed = crate::chumsky_adapter::adapt(&typed);
    crate::chumsky_adapter::typecheck_spec(&typed, &parsed)?;
    let manifest_dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve_and_merge_imports(&mut parsed, manifest_dir, lock_mode, cache_opts)?;
    validate_imported_account_refs(&parsed)?;
    Ok(parsed)
}

/// Parse every `.qedspec` under `dir` (recursively), require a shared
/// `spec Name`, and merge top items. Files are visited in sorted path order
/// so the `ParsedSpec` and all downstream artifacts are deterministic.
fn parse_spec_dir_with_opts(
    dir: &Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> Result<ParsedSpec> {
    let mut files = Vec::new();
    collect_qedspec_files(dir, &mut files)?;
    files.sort();

    anyhow::ensure!(
        !files.is_empty(),
        "no .qedspec files found under {}",
        dir.display()
    );

    let mut merged_name: Option<String> = None;
    let mut merged_items: Vec<crate::ast::Node<crate::ast::TopItem>> = Vec::new();

    for file in &files {
        let src =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let typed = crate::chumsky_parser::parse(&src).map_err(|errs| {
            let msg = errs
                .iter()
                .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, &src)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::anyhow!("parse error in {}:\n{}", file.display(), msg)
        })?;

        match &merged_name {
            None => merged_name = Some(typed.name.clone()),
            Some(existing) if existing != &typed.name => {
                anyhow::bail!(
                    "spec name mismatch in {}: declared `spec {}`, but a sibling \
                     file declares `spec {}`. Every .qedspec fragment in a \
                     multi-file spec directory must declare the same name.",
                    file.display(),
                    typed.name,
                    existing,
                );
            }
            _ => {}
        }

        merged_items.extend(typed.items);
    }

    let merged = crate::ast::Spec {
        name: merged_name.expect("non-empty files implies non-empty name"),
        items: merged_items,
    };
    let mut parsed = crate::chumsky_adapter::adapt(&merged);
    crate::chumsky_adapter::typecheck_spec(&merged, &parsed)?;
    resolve_and_merge_imports(&mut parsed, dir, lock_mode, cache_opts)?;
    validate_imported_account_refs(&parsed)?;
    Ok(parsed)
}

/// Every `acct : Ident.Ident` binding (parsed into
/// `ParsedHandlerAccount::imported_namespace`) must reference a known
/// namespace AND a known type within it. Bare bindings (`acct : signer`,
/// `acct : LocalState`) bypass this validator.
fn validate_imported_account_refs(parsed: &ParsedSpec) -> Result<()> {
    for handler in &parsed.handlers {
        for acct in &handler.accounts {
            let Some(ref ns) = acct.imported_namespace else {
                continue;
            };
            let Some(ref ty) = acct.account_type else {
                anyhow::bail!(
                    "handler `{}` account `{}` declares an imported namespace `{}` \
                     but no type name after the `.` — write `type {}.<TypeName>`",
                    handler.name,
                    acct.name,
                    ns,
                    ns,
                );
            };
            let imported_ns = parsed.imported_namespaces.get(ns).ok_or_else(|| {
                let known = if parsed.imported_namespaces.is_empty() {
                    "no imports declared".to_string()
                } else {
                    format!(
                        "known namespaces: {}",
                        parsed
                            .imported_namespaces
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                };
                anyhow::anyhow!(
                    "handler `{}` account `{}` references unknown namespace `{}` \
                     (in `type {}.{}`); {}. Add `import {} from \"<dep_key>\"` \
                     at the top of the spec.",
                    handler.name,
                    acct.name,
                    ns,
                    ns,
                    ty,
                    known,
                    ns,
                )
            })?;
            let known_in_ns = imported_ns.account_types.iter().any(|a| &a.name == ty);
            if !known_in_ns {
                anyhow::bail!(
                    "handler `{}` account `{}` references type `{}.{}` but namespace \
                     `{}` declares no such type (known types in namespace: {}). \
                     Check the imported spec at dep `{}`.",
                    handler.name,
                    acct.name,
                    ns,
                    ty,
                    ns,
                    imported_ns
                        .account_types
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    imported_ns.dep_key,
                );
            }
        }
    }
    Ok(())
}

/// Resolve every `import Name from "key"` against `qed.toml` in
/// `manifest_dir`, fetch the imported source(s) (path or github), parse, and
/// merge the matching `interface Name { ... }` into `parsed.interfaces`.
///
/// Resolution is shallow: imported specs' own `import` statements are not
/// transitively walked — each consumer declares its direct deps in its own
/// qed.toml.
fn resolve_and_merge_imports(
    parsed: &mut ParsedSpec,
    manifest_dir: &Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> anyhow::Result<()> {
    if parsed.imports.is_empty() {
        return Ok(());
    }

    // Locate qed.toml. Required when imports are present, EXCEPT when
    // every import resolves to a bundled-stdlib builtin (`from "spl"`,
    // `from "system"`). The resolver short-circuits those before
    // consulting the manifest, so an empty manifest is fine.
    let manifest = match crate::qed_manifest::load_from_dir(manifest_dir)? {
        Some(m) => m,
        None => {
            if crate::import_resolver::all_imports_are_builtins(&parsed.imports) {
                crate::qed_manifest::Manifest::default()
            } else {
                anyhow::bail!(
                    "spec has {} `import` statement(s) but no `qed.toml` next to it (expected at {})",
                    parsed.imports.len(),
                    manifest_dir
                        .join(crate::qed_manifest::MANIFEST_FILENAME)
                        .display(),
                )
            }
        }
    };

    let resolved = crate::import_resolver::resolve_imports_with_opts(
        &parsed.imports,
        &manifest,
        manifest_dir,
        cache_opts,
    )?;

    let mut lock = crate::qed_lock::LockFile::new();

    for r in resolved {
        let imported = parse_imported_sources(&r).with_context(|| {
            format!(
                "parsing imported spec `{}` (dep key `{}`)",
                r.bound_name, r.dep_key,
            )
        })?;

        // Imported source may declare an explicit `interface <name>` block
        // OR rely on implicit synthesis from top-level handlers (DSL ref:
        // every handler in the imported spec is public).
        let explicit = imported.interfaces.iter().find(|i| i.name == r.bound_name);
        let synthesized: Option<ParsedInterface> = if explicit.is_none() {
            synthesize_interface_from_imported(&r.bound_name, &imported)
        } else {
            None
        };
        // Data-only import: no `interface <bound>` block and no top-level
        // handlers, but at least one `type` declaration. Synthesize a
        // minimal empty interface (program_id only) so the merge loop runs
        // and `imported_namespaces` gets populated — supports
        // `acct : Foreign.State` field reads without any CPI surface.
        let data_only_iface: Option<ParsedInterface> =
            if explicit.is_none() && synthesized.is_none() && !imported.account_types.is_empty() {
                Some(ParsedInterface {
                    name: r.bound_name.clone(),
                    doc: None,
                    program_id: imported.program_id.clone(),
                    upstream: None,
                    state_fields: Vec::new(),
                    handlers: Vec::new(),
                })
            } else {
                None
            };
        let iface = match (explicit, &synthesized, &data_only_iface) {
            (Some(i), _, _) => i,
            (None, Some(i), _) => i,
            (None, None, Some(i)) => i,
            (None, None, None) => {
                let where_clause = if r.sources.len() == 1 {
                    format!("at {}", r.sources[0].0.display())
                } else {
                    format!("(merged from {} fragments)", r.sources.len())
                };
                anyhow::bail!(
                    "import `{}` from `{}` — imported source {} declares no `interface {}` block, no top-level handlers, and no `type` declarations. Add an `interface {{ ... }}`, at least one `handler`, or at least one `type` block to the imported spec.",
                    r.bound_name,
                    r.dep_key,
                    where_clause,
                    r.bound_name,
                );
            }
        };

        // Build the lock entry while everything is in scope. Bundled-stdlib
        // builtins don't appear in `manifest.dependencies`; their entry uses
        // a synthetic `builtin:<key>` source identifier. Imported
        // account-type names go on the entry so `--frozen` notices a
        // renamed/removed type before codegen breaks on a missing mirror;
        // comma-joined to keep the on-disk shape one TOML string.
        let imported_type_names = imported
            .account_types
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let lock_entry = if let Some(dep) = manifest.dependencies.get(&r.dep_key) {
            crate::qed_lock::entry_for_resolved(&r, dep, iface, &imported_type_names)
        } else {
            crate::qed_lock::entry_for_builtin(&r, iface, &imported_type_names)
        };
        lock.dependencies.push(lock_entry);

        // Apply the optional `as <alias>` rename when merging.
        let mut merged = iface.clone();
        if let Some(alias) = &r.local_alias {
            merged.name = alias.clone();
        }
        // Register the verified-callee mapping under the local (post-alias)
        // name — lean_gen looks up by this name. Each pkg_root also goes
        // onto `verified_proof_pkgs` (path-deduped after the loop) so
        // `verify --recursive` can walk the dep graph without re-resolving;
        // the resolver returns DFS-pre-order, naturally bottom-up-by-leaf.
        if r.has_proofs {
            if let Some(ref pkg_root) = r.proof_pkg_root {
                parsed
                    .verified_callees
                    .insert(merged.name.clone(), pkg_root.clone());
                parsed.verified_proof_pkgs.push(pkg_root.clone());
            }
        }
        let local_ns_name = merged.name.clone();
        parsed.interfaces.push(merged);

        // Every imported source registers here — including bundled stubs
        // with empty `account_types`. `imported_namespaces` is the canonical
        // parse-layer truth for "every imported source"; the empty case is
        // meaningful (Tier-0 stubs), not a suppression signal — "anything to
        // mirror?" is codegen's call (`generate_imported_mirror`). Local
        // name follows the same alias-or-bound-name rule as the interface
        // merge so type refs match call names.
        let ns = ImportedNamespace {
            dep_key: r.dep_key.clone(),
            account_types: imported.account_types.clone(),
            records: imported.records.clone(),
        };
        parsed.imported_namespaces.insert(local_ns_name, ns);
    }
    // Dedup preserving first-seen DFS order — handles diamond dep shapes.
    let mut seen = std::collections::HashSet::new();
    parsed
        .verified_proof_pkgs
        .retain(|p| seen.insert(p.clone()));

    let proof_hash_findings = crate::qed_lock::handle_lock(manifest_dir, &lock, lock_mode)?;
    parsed.proof_hash_findings = proof_hash_findings;

    Ok(())
}

/// Synthesize a `ParsedInterface` from the imported spec's top-level
/// handlers when no explicit `interface { … }` block is declared (DSL ref:
/// every handler in the imported spec is public). Tier-2 contract:
/// requires/ensures from the handlers' clauses, accounts from their accounts
/// blocks. `None` when there are no top-level handlers (caller emits a
/// clearer error).
fn synthesize_interface_from_imported(
    bound_name: &str,
    imported: &ParsedSpec,
) -> Option<ParsedInterface> {
    if imported.handlers.is_empty() {
        return None;
    }
    let handlers = imported
        .handlers
        .iter()
        .map(|h| ParsedInterfaceHandler {
            name: h.name.clone(),
            doc: h.doc.clone(),
            params: h.takes_params.clone(),
            discriminant: None,
            accounts: h.accounts.clone(),
            requires: h.requires.clone(),
            ensures: h.ensures.clone(),
            // Top-level handlers can't declare a return type or named
            // binder until the handler grammar grows them: `let x = call …`
            // bindings drop with a lint warning, and substitution falls
            // back to the literal "result".
            return_type: None,
            result_binder: None,
        })
        .collect();
    Some(ParsedInterface {
        name: bound_name.to_string(),
        doc: None,
        program_id: imported.program_id.clone(),
        upstream: None,
        // Synthesized interfaces carry no abstract-state vocabulary:
        // top-level handlers express ensures with concrete `state.X`
        // references, so the bundled-axiom path needing typed accessors
        // never fires for Tier-2 callees.
        state_fields: Vec::new(),
        handlers,
    })
}

/// Parse the source bytes for one resolved import. Single-file deps go
/// through `chumsky_adapter::parse_str`; multi-file deps follow the same
/// path-sorted merge logic as `parse_spec_dir` (same `spec Name`, top items
/// merged before the adapter runs).
fn parse_imported_sources(r: &crate::import_resolver::ResolvedImport) -> Result<ParsedSpec> {
    if r.sources.len() == 1 {
        let (src_path, src_bytes) = &r.sources[0];
        return crate::chumsky_adapter::parse_str(src_bytes)
            .with_context(|| format!("parsing imported spec source at {}", src_path.display()));
    }

    // Multi-file: parse each, merge AST top items, validate name consistency.
    let mut merged_name: Option<String> = None;
    let mut merged_items: Vec<crate::ast::Node<crate::ast::TopItem>> = Vec::new();
    for (path, src) in &r.sources {
        let typed = crate::chumsky_parser::parse(src).map_err(|errs| {
            let msg = errs
                .iter()
                .map(|e| format!("  {}", crate::chumsky_parser::format_parse_error(e, src)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::anyhow!("parse error in imported {}:\n{}", path.display(), msg)
        })?;
        match &merged_name {
            None => merged_name = Some(typed.name.clone()),
            Some(existing) if existing != &typed.name => anyhow::bail!(
                "imported spec fragment {} declares `spec {}`, but a sibling \
                 fragment declares `spec {}`. Every fragment of a multi-file \
                 imported dep must declare the same name.",
                path.display(),
                typed.name,
                existing,
            ),
            _ => {}
        }
        merged_items.extend(typed.items);
    }
    let merged = crate::ast::Spec {
        name: merged_name.expect("non-empty source list implies a name"),
        items: merged_items,
    };
    let parsed = crate::chumsky_adapter::adapt(&merged);
    crate::chumsky_adapter::typecheck_spec(&merged, &parsed)?;
    Ok(parsed)
}

/// Read the spec source — file or directory of fragments — as one string,
/// joined in the loader's sorted-path order. Raw-text consumers (e.g.
/// `spec_hash_for_handler`) MUST use this so their hash matches what the
/// proc-macro computes at compile time.
pub fn read_spec_source(path: &Path) -> Result<String> {
    if path.is_dir() {
        let mut files = Vec::new();
        collect_qedspec_files(path, &mut files)?;
        files.sort();
        let mut out = String::new();
        for f in &files {
            let src =
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
            out.push_str(&src);
            if !src.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(out)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }
}

/// Recursive collector for `.qedspec` files under a directory, depth-first.
/// Silently skips non-UTF8 paths (pathologically rare in a source tree).
fn collect_qedspec_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
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
