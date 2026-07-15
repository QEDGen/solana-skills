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
    // Local `import` dependencies (declared in qed.toml, e.g. an `imports/`
    // subtree) are *separate* specs, not fragments of this multi-file spec.
    // Exclude their paths from the sibling-fragment sweep — otherwise their
    // own `spec <Name>` trips the shared-name check below (issue #100).
    // `import` resolution reads them later via `resolve_and_merge_imports`.
    let import_roots = import_path_roots(dir);
    let mut files = Vec::new();
    collect_qedspec_files(dir, &import_roots, &mut files)?;
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
        // mirror?" is codegen's call (`codegen_mir::emit_imported_mirror`). Local
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
        // Hash source: pass NO exclusions. This must stay byte-for-byte
        // identical to the proc-macro's own dir walk
        // (qedgen-macros::spec_bind::collect_qedspec_files) so the spec_hash
        // agrees at compile time. The import-subtree exclusion is a
        // *semantic-merge* concern (parse_spec_dir_with_opts), not a hash one.
        let mut files = Vec::new();
        collect_qedspec_files(path, &[], &mut files)?;
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
/// Local dependency-path roots from `dir`'s `qed.toml`, canonicalized. These
/// are the subtrees `import` resolution reads from; `collect_qedspec_files`
/// skips them so imported specs aren't merged as sibling fragments (#100).
/// A missing/unreadable manifest or a path that doesn't resolve yields no
/// exclusion — the authoritative manifest check runs later in
/// `resolve_and_merge_imports`. GitHub deps resolve from the on-disk cache,
/// never a spec-dir subtree, so they need no exclusion.
fn import_path_roots(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(Some(manifest)) = crate::qed_manifest::load_from_dir(dir) else {
        return Vec::new();
    };
    manifest
        .dependencies
        .values()
        .filter_map(|dep| match dep {
            crate::qed_manifest::Dependency::Path { path } => {
                std::fs::canonicalize(dir.join(path)).ok()
            }
            crate::qed_manifest::Dependency::Github { .. } => None,
        })
        .collect()
}

fn collect_qedspec_files(
    dir: &Path,
    excluded: &[std::path::PathBuf],
    out: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();

        // Skip anything at or under a local `import` dependency root (#100).
        // Compare canonical paths so the check is robust to `..`/symlinks.
        if !excluded.is_empty() {
            if let Ok(canon) = std::fs::canonicalize(&path) {
                if excluded.iter().any(|root| canon.starts_with(root)) {
                    continue;
                }
            }
        }

        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            collect_qedspec_files(&path, excluded, out)?;
        } else if file_type.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("qedspec")
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- import resolution + interface merge -----

    #[test]
    fn parse_spec_file_resolves_path_imports_and_merges_interface() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Imported interface lives at <dir>/token.qedspec.
        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();

        // Manifest declares a path source.
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();

        // Consumer spec imports the interface.
        let consumer_path = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer_path,
            r#"spec Escrow
import Token from "spl_token"

type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer_path).expect("parse + resolve should succeed");
        assert_eq!(parsed.imports.len(), 1);
        assert_eq!(parsed.imports[0].name, "Token");
        // Token interface from token.qedspec should now be in parsed.interfaces.
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Token"),
            "Token interface should be merged into parsed.interfaces; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn parse_spec_file_errors_when_imports_present_but_no_qed_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer_path = tmp.path().join("escrow.qedspec");
        std::fs::write(
            &consumer_path,
            r#"spec Escrow
import Token from "spl_token"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let err = format!("{:#}", parse_spec_file(&consumer_path).unwrap_err());
        assert!(
            err.contains("no `qed.toml`"),
            "expected `no qed.toml` error, got: {err}"
        );
    }

    #[test]
    fn parse_spec_file_errors_when_bound_name_not_in_imported_source() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        std::fs::write(
            spec_dir.join("other.qedspec"),
            r#"spec OtherIface
interface NotToken {
  program_id "11111111111111111111111111111111"
}
"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "other.qedspec" }
"#,
        )
        .unwrap();
        let consumer_path = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer_path,
            r#"spec Escrow
import Token from "spl_token"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let err = format!("{:#}", parse_spec_file(&consumer_path).unwrap_err());
        assert!(
            err.contains("declares no `interface Token`"),
            "expected `no interface Token` error, got: {err}"
        );
    }

    #[test]
    fn parse_spec_file_no_imports_does_not_require_qed_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("plain.qedspec");
        std::fs::write(
            &path,
            r#"spec Plain
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();
        // No qed.toml, no imports — should parse cleanly.
        let parsed = parse_spec_file(&path).unwrap();
        assert!(parsed.imports.is_empty());
    }

    // ----- qed.lock integration -----

    fn write_simple_path_dep_setup(spec_dir: &std::path::Path) -> std::path::PathBuf {
        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:9c1edeadbeef"
    verified_with ["proptest"]
    verified_at  "2026-04-25"
  }
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();
        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token"

type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();
        consumer
    }

    #[test]
    fn parse_spec_file_auto_writes_lock_with_resolved_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Lock should not exist before parse.
        assert!(!tmp.path().join("qed.lock").exists());

        parse_spec_file(&consumer).expect("parse should succeed and write lock");

        let lock = crate::qed_lock::read(tmp.path())
            .unwrap()
            .expect("lock should be written");
        assert_eq!(lock.dependencies.len(), 1);
        let entry = &lock.dependencies[0];
        assert_eq!(entry.name, "spl_token");
        assert_eq!(entry.source, "path:token.qedspec");
        assert!(entry.spec_hash.starts_with("sha256:"));
        // Path source — no commit, no ref, no sub-path.
        assert!(entry.git_ref.is_none());
        assert!(entry.resolved_commit.is_none());
        // Upstream block from the imported interface flowed through.
        assert_eq!(
            entry.upstream_binary_hash.as_deref(),
            Some("sha256:9c1edeadbeef")
        );
        assert_eq!(entry.upstream_version.as_deref(), Some("4.0.3"));
    }

    #[test]
    fn parse_spec_file_frozen_errors_when_lock_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Frozen mode + no lock on disk → error.
        let err = format!(
            "{:#}",
            parse_spec_file_with_opts(
                &consumer,
                crate::qed_lock::LockMode::Frozen,
                crate::import_resolver::CacheOpts::default(),
            )
            .unwrap_err()
        );
        assert!(err.contains("stale (--frozen)"), "got: {err}");
    }

    #[test]
    fn parse_spec_file_frozen_succeeds_when_lock_current() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Auto first to write the lock, then Frozen to verify it stays current.
        parse_spec_file(&consumer).unwrap();
        parse_spec_file_with_opts(
            &consumer,
            crate::qed_lock::LockMode::Frozen,
            crate::import_resolver::CacheOpts::default(),
        )
        .expect("frozen should pass when lock is current");
    }

    #[test]
    fn parse_spec_file_renames_imported_interface_via_as_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();
        // Consumer uses `as Tk` to rename Token → Tk in its namespace.
        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token" as Tk
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer).expect("alias-renamed import should parse + merge");
        // Imported interface should appear under its alias name `Tk`,
        // not the source-side `Token`.
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Tk"),
            "expected interface renamed to `Tk`; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
        assert!(
            !parsed.interfaces.iter().any(|i| i.name == "Token"),
            "the source-side name `Token` should not leak into consumer when an alias is set"
        );
    }

    #[test]
    fn parse_spec_file_resolves_multi_file_imported_dep() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Imported dep is a *directory* of fragments. Each declares the
        // same `spec MultiToken`; one carries the interface, another
        // carries a sidecar event used in the interface's docs.
        let dep_dir = spec_dir.join("multitoken");
        std::fs::create_dir(&dep_dir).unwrap();
        std::fs::write(
            dep_dir.join("a-iface.qedspec"),
            r#"spec MultiToken
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            dep_dir.join("b-event.qedspec"),
            r#"spec MultiToken
event TokenMoved {
  amount : U64,
}
"#,
        )
        .unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "multitoken" }
"#,
        )
        .unwrap();

        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer)
            .expect("multi-file imported dep should parse + merge end-to-end");
        // Token interface from a-iface.qedspec lives in the merged consumer.
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Token"),
            "interface from multi-file dep should be merged in; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn parse_spec_file_errors_when_multi_file_dep_fragments_disagree_on_spec_name() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        let dep_dir = spec_dir.join("bad-multi");
        std::fs::create_dir(&dep_dir).unwrap();
        std::fs::write(
            dep_dir.join("a.qedspec"),
            "spec NameOne\ninterface Token { program_id \"x\" }\n",
        )
        .unwrap();
        std::fs::write(
            dep_dir.join("b.qedspec"),
            "spec NameTwo\nevent E { amount : U64 }\n",
        )
        .unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
bad = { path = "bad-multi" }
"#,
        )
        .unwrap();

        let consumer = spec_dir.join("c.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Caller
import Token from "bad"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let err = format!("{:#}", parse_spec_file(&consumer).unwrap_err());
        assert!(
            err.contains("must declare the same name"),
            "expected name-mismatch error; got: {err}"
        );
    }

    #[test]
    fn parse_spec_dir_excludes_import_subtree_from_sibling_sweep() {
        // Regression for #100: `check --spec <dir>/` must not sweep a local
        // `import` dependency's `.qedspec` into the multi-file sibling-fragment
        // merge. The dep declares its own `spec <Name>`, which previously
        // tripped the shared-name check before import resolution ever ran.
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Imported dep lives under an `imports/` subtree, declaring a DIFFERENT
        // `spec` name than the main fragment — the exact #100 shape.
        let dep_dir = spec_dir.join("imports").join("admin");
        std::fs::create_dir_all(&dep_dir).unwrap();
        std::fs::write(
            dep_dir.join("admin.qedspec"),
            "spec AdminConfig\n\
             interface Admin { program_id \"11111111111111111111111111111111\" }\n",
        )
        .unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            "[dependencies.admin_config]\npath = \"imports/admin\"\n",
        )
        .unwrap();

        // Main fragment in the dir root imports the dep by its interface name.
        std::fs::write(
            spec_dir.join("vault.qedspec"),
            r#"spec Vault
import Admin from "admin_config"
type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        // Dir-form parse must succeed: the import subtree is excluded from the
        // sibling sweep, then resolved normally.
        let parsed = parse_spec_file(spec_dir).expect("dir-form parse should succeed (#100)");
        assert_eq!(parsed.program_name, "Vault");
        assert!(
            parsed.interfaces.iter().any(|i| i.name == "Admin"),
            "Admin interface should resolve from the import; got {:?}",
            parsed
                .interfaces
                .iter()
                .map(|i| &i.name)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn parse_spec_file_frozen_errors_when_imported_source_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());

        // Auto-write a baseline lock, then mutate the imported source — the
        // spec hash should drift, so Frozen catches it.
        parse_spec_file(&consumer).unwrap();
        std::fs::write(
            tmp.path().join("token.qedspec"),
            r#"spec SplTokenInterface
interface Token {
  program_id "DIFFERENT11111111111111111111111111111111"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable, type token
      to        : writable, type token
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}
"#,
        )
        .unwrap();
        let err = format!(
            "{:#}",
            parse_spec_file_with_opts(
                &consumer,
                crate::qed_lock::LockMode::Frozen,
                crate::import_resolver::CacheOpts::default(),
            )
            .unwrap_err()
        );
        assert!(err.contains("spec_hash"), "got: {err}");
    }

    // ------------------------------------------------------------------
    // ParsedSpec.verified_proof_pkgs population — pins the
    // resolver→ParsedSpec wiring (the `lake build` runner is exercised
    // end-to-end elsewhere).
    // ------------------------------------------------------------------

    #[test]
    fn verified_proof_pkgs_populated_when_provider_ships_proof_package() {
        let tmp = tempfile::tempdir().unwrap();
        let spec_dir = tmp.path();

        // Provider qedspec at spec_dir/token.qedspec.
        std::fs::write(
            spec_dir.join("token.qedspec"),
            r#"spec TokenLib
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}
"#,
        )
        .unwrap();
        // Proof package alongside the qedspec — both module + lakefile
        // must be present for `has_proofs` to be true.
        let proofs_dir = spec_dir.join(".qed").join("proofs");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        std::fs::write(proofs_dir.join("Token.lean"), "-- stub proof").unwrap();
        std::fs::write(proofs_dir.join("lakefile.lean"), "package tokenProofs").unwrap();

        std::fs::write(
            spec_dir.join("qed.toml"),
            r#"
[dependencies]
spl_token = { path = "token.qedspec" }
"#,
        )
        .unwrap();
        let consumer = spec_dir.join("escrow.qedspec");
        std::fs::write(
            &consumer,
            r#"spec Escrow
import Token from "spl_token"

type State | A of { x : U64 }
handler h : State.A -> State.A { effect { x := 1 } }
"#,
        )
        .unwrap();

        let parsed = parse_spec_file(&consumer).expect("parse should succeed");
        assert_eq!(
            parsed.verified_proof_pkgs.len(),
            1,
            "expected 1 proof package; got {:?}",
            parsed.verified_proof_pkgs
        );
        assert!(
            parsed.verified_proof_pkgs[0].ends_with(".qed/proofs")
                || parsed.verified_proof_pkgs[0].ends_with(".qed\\proofs"),
            "should point at the provider's proof package root; got: {}",
            parsed.verified_proof_pkgs[0].display()
        );
    }

    #[test]
    fn verified_proof_pkgs_empty_when_no_provider_proofs() {
        // No `.qed/proofs/` alongside the provider qedspec → resolver
        // sets has_proofs=false → no entry in verified_proof_pkgs.
        let tmp = tempfile::tempdir().unwrap();
        let consumer = write_simple_path_dep_setup(tmp.path());
        let parsed = parse_spec_file(&consumer).expect("parse should succeed");
        assert!(
            parsed.verified_proof_pkgs.is_empty(),
            "no provider proofs → empty list; got: {:?}",
            parsed.verified_proof_pkgs
        );
    }

    // ----- colocated import/merge + parse-surface tests -----

    // ──────────────────────────────────────────────────────────────────────
    // Multi-file spec loader
    // ──────────────────────────────────────────────────────────────────────

    const SPEC_ROOT: &str = r#"
    spec Demo

    type State
      | Active of { count : U64 }
    "#;

    const SPEC_INC: &str = r#"
    spec Demo

    /// Increments count
    handler inc (x : U64) : State.Active -> State.Active {
      effect { count += x }
    }
    "#;

    const SPEC_DEC: &str = r#"
    spec Demo

    handler dec (x : U64) : State.Active -> State.Active {
      effect { count -= x }
    }
    "#;

    #[test]
    fn multi_file_spec_merges_handlers_across_fragments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo.qedspec"), SPEC_ROOT).unwrap();
        std::fs::create_dir_all(dir.path().join("handlers")).unwrap();
        std::fs::write(dir.path().join("handlers/inc.qedspec"), SPEC_INC).unwrap();
        std::fs::write(dir.path().join("handlers/dec.qedspec"), SPEC_DEC).unwrap();

        let parsed = parse_spec_file(dir.path()).unwrap();
        assert_eq!(parsed.program_name, "Demo");
        let names: Vec<_> = parsed.handlers.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"inc"), "got handlers: {:?}", names);
        assert!(names.contains(&"dec"), "got handlers: {:?}", names);
    }

    #[test]
    fn parse_spec_file_surfaces_clear_error_for_missing_path() {
        // A non-existent --spec path must say so explicitly instead of
        // falling through to the extension check ("Unsupported spec format: .").
        let missing = std::path::PathBuf::from("/tmp/does_not_exist_g5.qedspec");
        let err = parse_spec_file(&missing).unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "expected 'does not exist' in error, got: {err}"
        );
        assert!(
            !err.contains("Unsupported spec format"),
            "should not surface the extension-check error for missing path: {err}"
        );
    }

    #[test]
    fn multi_file_spec_rejects_name_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.qedspec"), SPEC_ROOT).unwrap();
        std::fs::write(
            dir.path().join("b.qedspec"),
            "spec Other\n\nhandler noop : State.Active -> State.Active { effect {} }\n",
        )
        .unwrap();

        let err = parse_spec_file(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("spec name mismatch"),
            "expected name-mismatch error, got: {err}"
        );
    }

    // ----- end cpi_unverified_callee -----

    #[test]
    fn call_clause_populates_handler_calls() {
        let src = r#"spec Demo

    handler exchange : State.A -> State.B {
      call Token.transfer(from = taker_ta, to = initializer_ta, amount = taker_amount)
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = &parsed.handlers[0];
        assert_eq!(handler.calls.len(), 1);
        let c = &handler.calls[0];
        assert_eq!(c.target_interface, "Token");
        assert_eq!(c.target_handler, "transfer");
        assert_eq!(c.args.len(), 3);
        assert_eq!(c.args[0].name, "from");
        assert_eq!(c.args[2].name, "amount");
        // Args carry the Rust rendering + typed tree (Lean renders from
        // the tree).
        assert!(!c.args[0].rust_expr.is_empty());
        assert!(c.args[0].tree.is_some());
    }

    // ──────────────────────────────────────────────────────────────────────
    // pragma sbpf { ... } adaptation
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn pragma_sbpf_unpacks_inner_items() {
        let src = r#"spec Transfer

    pragma sbpf {
      pubkey TOKEN_PROGRAM [6, 221, 246, 225]

      instruction transfer {
        discriminant 3
        entry 0
      }
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
        assert_eq!(parsed.pubkeys.len(), 1);
        assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].name, "transfer");
    }

    #[test]
    fn pragma_body_adapts_into_standard_parsed_spec_fields() {
        // Items wrapped in `pragma sbpf { ... }` must land in the same
        // ParsedSpec fields downstream consumers already read — pubkeys,
        // instructions, etc. The pragma is a grammatical namespace, not
        // a new parallel tree.
        let src = r#"spec T

    pragma sbpf {
      pubkey TOKEN_PROGRAM [1, 2, 3, 4]

      instruction foo {
        discriminant 1
        entry 0
      }
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.pragmas, vec!["sbpf".to_string()]);
        assert!(parsed.has_pragma("sbpf"));
        assert_eq!(parsed.pubkeys.len(), 1);
        assert_eq!(parsed.pubkeys[0].name, "TOKEN_PROGRAM");
        assert_eq!(parsed.instructions.len(), 1);
        assert_eq!(parsed.instructions[0].name, "foo");
    }

    #[test]
    fn top_level_sbpf_items_now_rejected() {
        // Platform-specifics (pubkey, instruction, assembly) only parse
        // behind `pragma sbpf { ... }` — the grammar keeps them out of the
        // core surface.
        let src = r#"spec T

    pubkey TOKEN_PROGRAM [1, 2, 3, 4]
    "#;
        assert!(
            crate::chumsky_adapter::parse_str(src).is_err(),
            "top-level `pubkey` should no longer parse"
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // ML syntax — let...in in expressions
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn let_in_renders_to_lean_and_rust() {
        let src = r#"spec T
    type State | A of { balance : U64 }

    handler h (amount : U64) : State.A -> State.A {
      ensures let delta = old(state.balance) - state.balance in delta == amount
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        let handler = &parsed.handlers[0];
        assert_eq!(handler.ensures.len(), 1);
        let e = &handler.ensures[0];
        // Lean form uses Lean's let-binding syntax.
        assert!(
            e.lean_expr.contains("let delta :="),
            "expected Lean let-binding, got: {}",
            e.lean_expr
        );
        // Rust form lowers to a block expression.
        assert!(
            e.rust_expr.contains("let delta ="),
            "expected Rust let-in-block, got: {}",
            e.rust_expr
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // Smoke test — match and ctors in the grammar.
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn ml_match_and_ctor_already_parse() {
        let src = r#"spec T
    type State | Active of { count : U64 } | Closed

    handler inspect : State.Active -> State.Active {
      ensures
        match state with
        | Active a => a.count >= 0
        | Closed => true
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.handlers.len(), 1);
        assert_eq!(parsed.handlers[0].ensures.len(), 1);
        // The rendered form should reference both variants.
        let lean = &parsed.handlers[0].ensures[0].lean_expr;
        assert!(lean.contains("Active"), "got: {}", lean);
        assert!(lean.contains("Closed"), "got: {}", lean);
    }

    #[test]
    fn interface_block_populates_parsed_spec() {
        let src = r#"spec Escrow

    interface Token {
      program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

      upstream {
        package      "spl-token"
        version      "4.0.3"
        binary_hash  "sha256:abc"
        verified_with ["proptest", "kani"]
        verified_at  "2026-04-18"
      }

      handler transfer (amount : U64) {
        accounts {
          from      : writable, type token
          to        : writable, type token
          authority : signer
        }
        requires amount > 0
        ensures  amount > 0
      }
    }
    "#;
        let parsed = crate::chumsky_adapter::parse_str(src).unwrap();
        assert_eq!(parsed.interfaces.len(), 1);
        let i = &parsed.interfaces[0];
        assert_eq!(i.name, "Token");
        assert_eq!(
            i.program_id.as_deref(),
            Some("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
        );

        let u = i.upstream.as_ref().expect("upstream present");
        assert_eq!(u.binary_hash.as_deref(), Some("sha256:abc"));
        // Lean absent by design — no overclaiming.
        assert!(!u.verified_with.contains(&"lean".to_string()));

        assert_eq!(i.handlers.len(), 1);
        let h = &i.handlers[0];
        assert_eq!(h.name, "transfer");
        assert_eq!(h.params, vec![("amount".to_string(), "U64".to_string())]);
        assert_eq!(h.accounts.len(), 3);
        assert_eq!(h.requires.len(), 1);
        assert_eq!(h.ensures.len(), 1);
    }

    #[test]
    fn multi_file_spec_source_matches_single_file_concat() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1.qedspec"), SPEC_ROOT).unwrap();
        std::fs::write(dir.path().join("2.qedspec"), SPEC_INC).unwrap();

        // read_spec_source must emit fragments in sorted-path order so
        // spec_hash_for_handler finds handler bodies regardless of which
        // fragment they live in.
        let src = read_spec_source(dir.path()).unwrap();
        assert!(
            src.contains("type State"),
            "root fragment missing in merged source"
        );
        assert!(
            src.contains("handler inc"),
            "handler fragment missing in merged source"
        );
    }
}
