use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use syn::ItemFn;

use crate::spec_hash;

/// Status of a verified function's hash.
#[derive(Debug, PartialEq)]
pub enum DriftStatus {
    /// Hash matches — code is unchanged since verification
    Ok,
    /// Hash mismatch — code has drifted
    Drifted { expected: String, actual: String },
    /// No hash provided (setup mode)
    NoHash { computed: String },
}

/// A verified function found in a source file.
#[derive(Debug)]
pub struct VerifiedEntry {
    pub file: PathBuf,
    pub fn_name: String,
    pub status: DriftStatus,
}

/// Content hash for a function. MUST match the proc-macro's recomputation
/// byte-for-byte: `to_token_stream().to_string()` subtly diverges from the
/// macro's `canonical_token_string` walker (rustc-vs-`from_str` spacing),
/// which made `qedgen check --update-hashes` write hashes the proc-macro
/// immediately rejected as drifted. Delegating to the shared
/// `spec_hash::body_hash_for_fn` keeps both sides agreeing by construction.
fn content_hash(func: &ItemFn) -> String {
    spec_hash::body_hash_for_fn(func)
}

/// All key=value fields that may appear inside `#[qed(verified, ...)]`.
/// Used by `--update-hashes` to know which hash legs to refresh, and by
/// `reconcile` (which shares this parser so the two commands agree on the
/// attribute grammar).
#[derive(Debug, Default, Clone)]
pub(crate) struct VerifiedAttr {
    pub spec: Option<String>,
    pub handler: Option<String>,
    pub hash: Option<String>,
    pub spec_hash: Option<String>,
    pub accounts: Option<String>,
    pub accounts_file: Option<String>,
    pub accounts_hash: Option<String>,
}

/// Every `key = "value"` pair inside a `#[qed(verified, ...)]` attribute.
/// `None` = not `qed(verified, ...)`-shaped; `Some(default())` = bare
/// `#[qed(verified)]`.
pub(crate) fn parse_verified_attr(attr: &syn::Attribute) -> Option<VerifiedAttr> {
    let path = attr.path();
    if !path.is_ident("qed") {
        return None;
    }
    let tokens = match &attr.meta {
        syn::Meta::List(list) => &list.tokens,
        _ => return None,
    };
    let tv: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();
    match tv.first() {
        Some(proc_macro2::TokenTree::Ident(i)) if i == "verified" => {}
        _ => return None,
    }

    let mut out = VerifiedAttr::default();
    let mut i = 0;
    while i < tv.len() {
        if let proc_macro2::TokenTree::Ident(id) = &tv[i] {
            let name = id.to_string();
            if matches!(
                name.as_str(),
                "spec"
                    | "handler"
                    | "hash"
                    | "spec_hash"
                    | "accounts"
                    | "accounts_file"
                    | "accounts_hash"
            ) && i + 2 < tv.len()
            {
                let eq =
                    matches!(&tv[i + 1], proc_macro2::TokenTree::Punct(p) if p.as_char() == '=');
                if eq {
                    if let proc_macro2::TokenTree::Literal(lit) = &tv[i + 2] {
                        let v = lit.to_string().trim_matches('"').to_string();
                        match name.as_str() {
                            "spec" => out.spec = Some(v),
                            "handler" => out.handler = Some(v),
                            "hash" => out.hash = Some(v),
                            "spec_hash" => out.spec_hash = Some(v),
                            "accounts" => out.accounts = Some(v),
                            "accounts_file" => out.accounts_file = Some(v),
                            "accounts_hash" => out.accounts_hash = Some(v),
                            _ => unreachable!(),
                        }
                        i += 3;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    Some(out)
}

/// Walk parents of `start` for the named file (first hit wins). Matches the
/// proc-macro's `CARGO_MANIFEST_DIR`-relative resolution of `spec = "..."`.
fn find_relative_file(start: &Path, rel: &str) -> Option<PathBuf> {
    let mut dir = start.parent();
    while let Some(d) = dir {
        let candidate = d.join(rel);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// One `#[qed(verified, ...)]`-stamped function: name, full parsed
/// attribute, 1-based line the attribute starts on, and the function body.
/// Single walker — replaces the former twin traversals
/// (`collect_from_items` for hashes, `walk_verified_attrs` for full
/// attributes) that callers zipped by index.
pub(crate) struct VerifiedFn {
    pub(crate) name: String,
    pub(crate) attr: VerifiedAttr,
    pub(crate) attr_line: usize,
    pub(crate) func: ItemFn,
}

/// First `#[qed(verified, ...)]` attribute on a fn-shaped item, with its
/// 1-based start line (needs proc-macro2 `span-locations`).
fn first_verified_attr(attrs: &[syn::Attribute]) -> Option<(VerifiedAttr, usize)> {
    use syn::spanned::Spanned;
    attrs
        .iter()
        .find_map(|attr| parse_verified_attr(attr).map(|a| (a, attr.span().start().line)))
}

/// Recursively collect verified functions from a list of items.
fn collect_verified_fns(items: &[syn::Item], out: &mut Vec<VerifiedFn>) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                if let Some((attr, attr_line)) = first_verified_attr(&f.attrs) {
                    out.push(VerifiedFn {
                        name: f.sig.ident.to_string(),
                        attr,
                        attr_line,
                        func: f.clone(),
                    });
                }
            }
            syn::Item::Impl(i) => {
                for impl_item in &i.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        if let Some((attr, attr_line)) = first_verified_attr(&method.attrs) {
                            let func = ItemFn {
                                attrs: method.attrs.clone(),
                                vis: method.vis.clone(),
                                sig: method.sig.clone(),
                                block: Box::new(method.block.clone()),
                            };
                            out.push(VerifiedFn {
                                name: method.sig.ident.to_string(),
                                attr,
                                attr_line,
                                func,
                            });
                        }
                    }
                }
            }
            syn::Item::Trait(t) => {
                for trait_item in &t.items {
                    if let syn::TraitItem::Fn(method) = trait_item {
                        if let Some((attr, attr_line)) = first_verified_attr(&method.attrs) {
                            // Default-body-less trait fns can't be hashed —
                            // skipped.
                            if let Some(ref block) = method.default {
                                let func = ItemFn {
                                    attrs: method.attrs.clone(),
                                    vis: syn::Visibility::Inherited,
                                    sig: method.sig.clone(),
                                    block: Box::new(block.clone()),
                                };
                                out.push(VerifiedFn {
                                    name: method.sig.ident.to_string(),
                                    attr,
                                    attr_line,
                                    func,
                                });
                            }
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner_items)) = &m.content {
                    collect_verified_fns(inner_items, out);
                }
            }
            _ => {}
        }
    }
}

/// Parse `source` and return every stamped fn. Shared entry for `drift`
/// and `reconcile` — one attribute grammar, one traversal.
pub(crate) fn scan_verified_fns(source: &str) -> syn::Result<Vec<VerifiedFn>> {
    let syntax = syn::parse_file(source)?;
    let mut out = Vec::new();
    collect_verified_fns(&syntax.items, &mut out);
    Ok(out)
}

/// Scan a single Rust source file for `#[qed(verified)]` functions.
fn scan_file(path: &Path) -> Result<Vec<VerifiedEntry>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let scanned =
        scan_verified_fns(&source).with_context(|| format!("parsing {}", path.display()))?;

    let results = scanned
        .into_iter()
        .map(|entry| {
            let actual = content_hash(&entry.func);
            let status = match entry.attr.hash {
                Some(expected) if expected == actual => DriftStatus::Ok,
                Some(expected) => DriftStatus::Drifted { expected, actual },
                None => DriftStatus::NoHash { computed: actual },
            };
            VerifiedEntry {
                file: path.to_path_buf(),
                fn_name: entry.name,
                status,
            }
        })
        .collect();

    Ok(results)
}

/// Collect all `.rs` files under a path (file or directory) via the
/// shared walker (skips `target`, `tests`, `.git`, … — see
/// `fs_walk::DEFAULT_SKIP_DIRS`).
fn collect_rs_files(path: &Path) -> Vec<PathBuf> {
    crate::fs_walk::collect_rs_files(path, crate::fs_walk::DEFAULT_SKIP_DIRS)
}

// ============================================================================
// Transitive drift detection (--deep)
// ============================================================================

/// A callee-changed warning for transitive drift.
#[derive(Debug)]
pub struct TransitiveDriftEntry {
    pub file: PathBuf,
    pub fn_name: String,
    pub changed_callees: Vec<String>,
}

/// AST visitor that extracts function call identifiers from a function body.
struct CalleeVisitor {
    callees: Vec<String>,
}

impl<'ast> syn::visit::Visit<'ast> for CalleeVisitor {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(ref path) = *node.func {
            if let Some(ident) = path.path.get_ident() {
                self.callees.push(ident.to_string());
            } else if let Some(seg) = path.path.segments.last() {
                self.callees.push(seg.ident.to_string());
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        self.callees.push(node.method.to_string());
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Extract identifiers of functions called within a function body.
fn extract_callees(func: &ItemFn) -> Vec<String> {
    use syn::visit::Visit;
    let mut visitor = CalleeVisitor {
        callees: Vec::new(),
    };
    visitor.visit_block(&func.block);
    visitor.callees.sort();
    visitor.callees.dedup();
    visitor.callees
}

/// Collect ALL function definitions in a file (not just verified ones).
fn collect_all_fns(syntax: &syn::File) -> HashMap<String, ItemFn> {
    let mut map = HashMap::new();
    collect_all_fns_from_items(&syntax.items, &mut map);
    map
}

fn collect_all_fns_from_items(items: &[syn::Item], map: &mut HashMap<String, ItemFn>) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                map.insert(f.sig.ident.to_string(), f.clone());
            }
            syn::Item::Impl(i) => {
                for impl_item in &i.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        let item_fn = ItemFn {
                            attrs: method.attrs.clone(),
                            vis: method.vis.clone(),
                            sig: method.sig.clone(),
                            block: Box::new(method.block.clone()),
                        };
                        map.insert(method.sig.ident.to_string(), item_fn);
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner_items)) = &m.content {
                    collect_all_fns_from_items(inner_items, map);
                }
            }
            _ => {}
        }
    }
}

/// Scan a file for transitive drift: verified functions whose verified
/// callees have themselves drifted directly (GH issue #28).
///
/// The stored `hash = "..."` seals only the function body, so without
/// per-callee stored hashes the only sound transitive signal is "one of my
/// verified callees is itself drifted". For each function whose direct hash
/// is OK, surface the verified callees that drifted directly; non-verified
/// callees have no anchor and can't drift. No false positives; the trade-off
/// is non-verified callee changes do not surface.
fn scan_file_deep(path: &Path) -> Result<Vec<TransitiveDriftEntry>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let syntax = syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;

    let all_fns = collect_all_fns(&syntax);

    let mut scanned = Vec::new();
    collect_verified_fns(&syntax.items, &mut scanned);

    // Which `#[qed(verified)]` functions have drifted directly?
    let mut directly_drifted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in &scanned {
        let Some(expected) = &entry.attr.hash else {
            continue;
        };
        let actual = content_hash(&entry.func);
        if expected != &actual {
            directly_drifted.insert(entry.name.clone());
        }
    }

    // For each function whose direct hash IS OK, surface verified callees
    // that drifted directly.
    let mut results = Vec::new();
    for entry in &scanned {
        let Some(expected) = &entry.attr.hash else {
            continue;
        };
        let actual = content_hash(&entry.func);
        if expected != &actual {
            continue; // direct drift handled by check(); don't double-report
        }

        let callees = extract_callees(&entry.func);
        let mut changed: Vec<String> = callees
            .into_iter()
            .filter(|name| all_fns.contains_key(name) && directly_drifted.contains(name))
            .collect();
        changed.sort();
        changed.dedup();

        if !changed.is_empty() {
            results.push(TransitiveDriftEntry {
                file: path.to_path_buf(),
                fn_name: entry.name.clone(),
                changed_callees: changed,
            });
        }
    }

    Ok(results)
}

/// Run deep (transitive) drift analysis across all files.
pub fn check_deep(input: &Path) -> Result<Vec<TransitiveDriftEntry>> {
    let files = collect_rs_files(input);
    let mut all_entries = Vec::new();
    for file in &files {
        match scan_file_deep(file) {
            Ok(entries) => all_entries.extend(entries),
            Err(e) => {
                eprintln!("warning: skipping {}: {}", file.display(), e);
            }
        }
    }
    Ok(all_entries)
}

/// Print a human-readable transitive drift report.
pub fn print_deep_report(entries: &[TransitiveDriftEntry]) {
    if entries.is_empty() {
        eprintln!("No transitive drift detected.");
        return;
    }

    for entry in entries {
        let file = entry.file.file_name().unwrap_or_default().to_string_lossy();
        eprintln!(
            "  {}  {}  TRANSITIVE DRIFT  callees changed: {}",
            file,
            entry.fn_name,
            entry.changed_callees.join(", ")
        );
    }
    eprintln!(
        "\n{} function(s) have callees that changed — re-verify",
        entries.len()
    );
}

/// Scan all Rust files under `input` for verified functions and report their status.
pub fn check(input: &Path) -> Result<Vec<VerifiedEntry>> {
    let files = collect_rs_files(input);
    let mut all_entries = Vec::new();
    for file in &files {
        match scan_file(file) {
            Ok(entries) => all_entries.extend(entries),
            Err(e) => {
                // Skip files that fail to parse (may not be valid Rust)
                eprintln!("warning: skipping {}: {}", file.display(), e);
            }
        }
    }
    Ok(all_entries)
}

/// A `#[qed(verified, ...)]` stamp whose `hash`, `spec_hash`, or
/// `accounts_hash` is stale. Surfaced by `check_stamped_drift` so
/// `qedgen codegen` can warn right after regen instead of waiting for the
/// proc-macro's `compile_error!` on the next build.
#[derive(Debug)]
pub struct StampedDriftEntry {
    pub file: PathBuf,
    pub fn_name: String,
}

/// Outcome of recomputing one stamp leg (`spec_hash` / `accounts_hash`).
enum LegHash {
    /// Recomputed on-disk hash.
    Hash(String),
    /// The `spec` / `accounts_file` path did not resolve from the stamped
    /// file's parents — `update` warns on this; `check_stamped_drift`
    /// stays silent.
    Unresolved,
    /// Resolved but unreadable / handler-or-struct not found.
    Unavailable,
}

/// Shared recompute leg for `check_stamped_drift` and `update`: resolve
/// `rel` against the stamped file's parent dirs, read it, hash via
/// `hash_fn(source, name)`.
fn actual_leg_hash(
    stamped_file: &Path,
    rel: &str,
    name: &str,
    hash_fn: impl Fn(&str, &str) -> Option<String>,
) -> LegHash {
    let Some(resolved) = find_relative_file(stamped_file, rel) else {
        return LegHash::Unresolved;
    };
    let Ok(src) = std::fs::read_to_string(&resolved) else {
        return LegHash::Unavailable;
    };
    match hash_fn(&src, name) {
        Some(h) => LegHash::Hash(h),
        None => LegHash::Unavailable,
    }
}

/// Read-only complement to `update`: same staleness logic across all three
/// hash legs, no rewrites. One entry per stale stamp; empty = all current.
pub fn check_stamped_drift(input: &Path) -> Result<Vec<StampedDriftEntry>> {
    let files = collect_rs_files(input);
    let mut entries = Vec::new();

    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let scanned = match scan_verified_fns(&source) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for entry in &scanned {
            let attr = &entry.attr;
            let mut stale = false;

            // Body hash leg
            let actual_body = content_hash(&entry.func);
            if let Some(expected) = &attr.hash {
                if expected != &actual_body {
                    stale = true;
                }
            }

            // spec_hash leg
            if let (Some(spec_path), Some(handler_name), Some(expected_spec)) =
                (&attr.spec, &attr.handler, &attr.spec_hash)
            {
                if let LegHash::Hash(actual_spec) =
                    actual_leg_hash(file, spec_path, handler_name, |src, h| {
                        spec_hash::spec_hash_for_handler(src, h)
                    })
                {
                    if &actual_spec != expected_spec {
                        stale = true;
                    }
                }
            }

            // accounts_hash leg
            if let (Some(struct_name), Some(accounts_file), Some(expected_acct)) =
                (&attr.accounts, &attr.accounts_file, &attr.accounts_hash)
            {
                if let LegHash::Hash(actual_acct) =
                    actual_leg_hash(file, accounts_file, struct_name, |src, s| {
                        spec_hash::accounts_struct_hash(src, s)
                    })
                {
                    if &actual_acct != expected_acct {
                        stale = true;
                    }
                }
            }

            if stale {
                entries.push(StampedDriftEntry {
                    file: file.clone(),
                    fn_name: entry.name.clone(),
                });
            }
        }
    }

    Ok(entries)
}

/// Print a human-readable drift report.
pub fn print_report(entries: &[VerifiedEntry]) {
    if entries.is_empty() {
        eprintln!("No #[qed(verified)] functions found.");
        return;
    }

    for entry in entries {
        let file = entry.file.file_name().unwrap_or_default().to_string_lossy();
        match &entry.status {
            DriftStatus::Ok => {
                eprintln!("  {}  {}  OK", file, entry.fn_name);
            }
            DriftStatus::Drifted { expected, actual } => {
                eprintln!(
                    "  {}  {}  DRIFT  expected {} got {}",
                    file, entry.fn_name, expected, actual
                );
            }
            DriftStatus::NoHash { computed } => {
                eprintln!(
                    "  {}  {}  NO HASH  computed {}",
                    file, entry.fn_name, computed
                );
            }
        }
    }

    let ok = entries
        .iter()
        .filter(|e| e.status == DriftStatus::Ok)
        .count();
    let drifted = entries
        .iter()
        .filter(|e| matches!(e.status, DriftStatus::Drifted { .. }))
        .count();
    let no_hash = entries
        .iter()
        .filter(|e| matches!(e.status, DriftStatus::NoHash { .. }))
        .count();
    eprintln!(
        "\n{} verified, {} drifted, {} unhashed",
        ok, drifted, no_hash
    );
}

/// Update `#[qed(verified, ...)]` stamps in-place, refreshing all three hash
/// legs (`hash`, `spec_hash`, `accounts_hash`) — refreshing only `hash` leaves
/// the proc-macro rejecting the build on the other legs.
///
/// `spec` / `accounts_file` paths resolve by walking parent dirs (matching
/// the proc-macro's `CARGO_MANIFEST_DIR`-relative behavior); an unresolvable
/// file skips that leg with a warning so partial trees still work.
pub fn update(input: &Path) -> Result<usize> {
    let files = collect_rs_files(input);
    let mut updated = 0;

    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let scanned = match scan_verified_fns(&source) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut new_source = source.clone();
        let mut changed = false;

        for entry in &scanned {
            let attr = &entry.attr;
            // Body hash leg; bare `#[qed(verified)]` gets stamped.
            let actual_body = content_hash(&entry.func);
            match &attr.hash {
                Some(expected) if expected != &actual_body => {
                    let old = format!("hash = \"{}\"", expected);
                    let new = format!("hash = \"{}\"", actual_body);
                    if new_source.contains(&old) {
                        new_source = new_source.replacen(&old, &new, 1);
                        changed = true;
                        updated += 1;
                    }
                }
                Some(_) => {} // body hash already correct
                None => {
                    // No `hash` field — stamp with the computed hash.
                    let patterns = [
                        "qed(verified)",
                        "qed( verified )",
                        "qed(verified )",
                        "qed( verified)",
                    ];
                    for pat in &patterns {
                        let replacement = format!("qed(verified, hash = \"{}\")", actual_body);
                        if new_source.contains(pat) {
                            new_source = new_source.replacen(pat, &replacement, 1);
                            changed = true;
                            updated += 1;
                            break;
                        }
                    }
                }
            }

            // spec_hash leg: needs `spec` + `handler` set.
            if let (Some(spec_path), Some(handler_name), Some(expected_spec)) =
                (&attr.spec, &attr.handler, &attr.spec_hash)
            {
                match actual_leg_hash(file, spec_path, handler_name, |src, h| {
                    spec_hash::spec_hash_for_handler(src, h)
                }) {
                    LegHash::Hash(actual_spec) => {
                        if &actual_spec != expected_spec {
                            let old = format!("spec_hash = \"{}\"", expected_spec);
                            let new = format!("spec_hash = \"{}\"", actual_spec);
                            if new_source.contains(&old) {
                                new_source = new_source.replacen(&old, &new, 1);
                                changed = true;
                                updated += 1;
                            }
                        }
                    }
                    LegHash::Unresolved => {
                        eprintln!(
                            "warning: --update-hashes: could not resolve `spec = \"{}\"` from {} \
                             (skipping spec_hash refresh for this entry)",
                            spec_path,
                            file.display()
                        );
                    }
                    LegHash::Unavailable => {}
                }
            }

            // accounts_hash leg: needs `accounts` + `accounts_file` set.
            // The macro enforces all-or-nothing (#29), so partial configs are
            // compile errors, not silent skips here.
            if let (Some(struct_name), Some(accounts_file), Some(expected_acct)) =
                (&attr.accounts, &attr.accounts_file, &attr.accounts_hash)
            {
                match actual_leg_hash(file, accounts_file, struct_name, |src, s| {
                    spec_hash::accounts_struct_hash(src, s)
                }) {
                    LegHash::Hash(actual_acct) => {
                        if &actual_acct != expected_acct {
                            let old = format!("accounts_hash = \"{}\"", expected_acct);
                            let new = format!("accounts_hash = \"{}\"", actual_acct);
                            if new_source.contains(&old) {
                                new_source = new_source.replacen(&old, &new, 1);
                                changed = true;
                                updated += 1;
                            }
                        }
                    }
                    LegHash::Unresolved => {
                        eprintln!(
                            "warning: --update-hashes: could not resolve `accounts_file = \"{}\"` \
                             from {} (skipping accounts_hash refresh for this entry)",
                            accounts_file,
                            file.display()
                        );
                    }
                    LegHash::Unavailable => {}
                }
            }
        }

        if changed {
            std::fs::write(file, &new_source)?;
        }
    }

    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp_rs(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".rs").unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn scan_finds_verified_function() {
        let f = write_temp_rs(
            r#"
            fn not_verified() {}

            #[qed(verified, hash = "0000000000000000")]
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
            "#,
        );
        let entries = scan_file(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fn_name, "deposit");
        // Hash won't match "0000000000000000" so it should be Drifted
        assert!(matches!(entries[0].status, DriftStatus::Drifted { .. }));
    }

    #[test]
    fn scan_no_hash_mode() {
        let f = write_temp_rs(
            r#"
            #[qed(verified)]
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
            "#,
        );
        let entries = scan_file(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].status, DriftStatus::NoHash { .. }));
    }

    #[test]
    fn scan_correct_hash() {
        // First compute the hash, then verify it
        let source = r#"
            #[qed(verified)]
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
        "#;
        let f = write_temp_rs(source);
        let entries = scan_file(f.path()).unwrap();
        let computed = match &entries[0].status {
            DriftStatus::NoHash { computed } => computed.clone(),
            _ => panic!("expected NoHash"),
        };

        // Now write with the correct hash
        let source_with_hash = source.replace(
            "qed(verified)",
            &format!("qed(verified, hash = \"{}\")", computed),
        );
        let f2 = write_temp_rs(&source_with_hash);
        let entries2 = scan_file(f2.path()).unwrap();
        assert_eq!(entries2[0].status, DriftStatus::Ok);
    }

    #[test]
    fn scan_impl_method() {
        let f = write_temp_rs(
            r#"
            struct Foo;
            impl Foo {
                #[qed(verified)]
                pub fn handler(&mut self, amount: u64) {
                    self.x = amount;
                }
            }
            "#,
        );
        let entries = scan_file(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fn_name, "handler");
    }

    #[test]
    fn scan_trait_method_with_default() {
        let f = write_temp_rs(
            r#"
            trait Handler {
                #[qed(verified)]
                fn handle(&self) -> u64 {
                    42
                }
            }
            "#,
        );
        let entries = scan_file(f.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fn_name, "handle");
        assert!(matches!(entries[0].status, DriftStatus::NoHash { .. }));
    }

    #[test]
    fn scan_trait_method_without_body_ignored() {
        let f = write_temp_rs(
            r#"
            trait Handler {
                #[qed(verified)]
                fn handle(&self) -> u64;
            }
            "#,
        );
        let entries = scan_file(f.path()).unwrap();
        // No default body, so it can't be hashed — should be skipped
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn deep_detects_verified_callee_drift() {
        // --deep flags transitive drift only when a *verified* callee has
        // itself drifted directly (the only sound signal; see scan_file_deep).
        let source = r#"
            #[qed(verified)]
            fn helper() -> u64 { 42 }

            #[qed(verified)]
            pub fn main_fn() -> u64 {
                helper()
            }
        "#;

        // Stamp both with their direct hashes.
        let f1 = write_temp_rs(source);
        let entries = scan_file(f1.path()).unwrap();
        let helper_hash = match &entries
            .iter()
            .find(|e| e.fn_name == "helper")
            .unwrap()
            .status
        {
            DriftStatus::NoHash { computed } => computed.clone(),
            _ => panic!("expected NoHash for helper"),
        };
        let main_hash = match &entries
            .iter()
            .find(|e| e.fn_name == "main_fn")
            .unwrap()
            .status
        {
            DriftStatus::NoHash { computed } => computed.clone(),
            _ => panic!("expected NoHash for main_fn"),
        };

        let stamped = source
            .replacen(
                "#[qed(verified)]\n            fn helper",
                &format!(
                    "#[qed(verified, hash = \"{}\")]\n            fn helper",
                    helper_hash
                ),
                1,
            )
            .replacen(
                "#[qed(verified)]\n            pub fn main_fn",
                &format!(
                    "#[qed(verified, hash = \"{}\")]\n            pub fn main_fn",
                    main_hash
                ),
                1,
            );

        // Drift the helper body (still stamped with the OLD hash → direct
        // drift on helper itself).
        let modified = stamped.replace("{ 42 }", "{ 99 }");
        let f2 = write_temp_rs(&modified);

        // Direct check: helper Drifted, main_fn Ok.
        let entries = scan_file(f2.path()).unwrap();
        let helper_status = &entries
            .iter()
            .find(|e| e.fn_name == "helper")
            .unwrap()
            .status;
        assert!(matches!(helper_status, DriftStatus::Drifted { .. }));
        assert_eq!(
            entries
                .iter()
                .find(|e| e.fn_name == "main_fn")
                .unwrap()
                .status,
            DriftStatus::Ok
        );

        // Deep: main_fn surfaces because its verified callee (helper) drifted.
        let deep_entries = scan_file_deep(f2.path()).unwrap();
        assert_eq!(deep_entries.len(), 1);
        assert_eq!(deep_entries[0].fn_name, "main_fn");
        assert!(deep_entries[0]
            .changed_callees
            .contains(&"helper".to_string()));
    }

    #[test]
    fn deep_silent_on_non_verified_callee_change() {
        // Non-verified callee changes intentionally do NOT surface — they
        // have no anchor to compare against.
        let source = r#"
            fn helper() -> u64 { 42 }

            #[qed(verified)]
            pub fn main_fn() -> u64 {
                helper()
            }
        "#;
        let f1 = write_temp_rs(source);
        let entries = scan_file(f1.path()).unwrap();
        let computed = match &entries[0].status {
            DriftStatus::NoHash { computed } => computed.clone(),
            _ => panic!("expected NoHash"),
        };
        let stamped = source.replace(
            "qed(verified)",
            &format!("qed(verified, hash = \"{}\")", computed),
        );
        let modified = stamped.replace("{ 42 }", "{ 99 }");
        let f2 = write_temp_rs(&modified);

        // main_fn body is unchanged → OK.
        let entries = scan_file(f2.path()).unwrap();
        assert_eq!(entries[0].status, DriftStatus::Ok);

        // helper is non-verified, so its drift is invisible to the
        // transitive check. No false positive.
        let deep_entries = scan_file_deep(f2.path()).unwrap();
        assert!(
            deep_entries.is_empty(),
            "non-verified callee change must not surface: {deep_entries:#?}"
        );
    }

    #[test]
    fn deep_no_false_positive_when_callee_unchanged() {
        // Nothing drifted → --deep emits nothing.
        let source = r#"
            fn helper() -> u64 { 42 }

            #[qed(verified)]
            pub fn main_fn() -> u64 {
                helper()
            }
        "#;

        let f1 = write_temp_rs(source);
        let entries = scan_file(f1.path()).unwrap();
        let computed = match &entries[0].status {
            DriftStatus::NoHash { computed } => computed.clone(),
            _ => panic!("expected NoHash"),
        };

        // Stamp it — don't change anything
        let stamped = source.replace(
            "qed(verified)",
            &format!("qed(verified, hash = \"{}\")", computed),
        );
        let f2 = write_temp_rs(&stamped);

        let deep_entries = scan_file_deep(f2.path()).unwrap();
        assert!(
            deep_entries.is_empty(),
            "no callee change must produce no transitive drift: {deep_entries:#?}"
        );
    }

    #[test]
    fn content_hash_matches_macro() {
        // CLI hash must be the 16-char hex shape the proc macro produces.
        use quote::quote;
        let func: ItemFn = syn::parse2(quote! {
            pub fn deposit(amount: u64) -> u64 {
                amount + 1
            }
        })
        .unwrap();
        let hash = content_hash(&func);
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn content_hash_equals_spec_hash_body_hash() {
        // Regression lock: drift::content_hash MUST agree with
        // spec_hash::body_hash_for_fn (and thus the proc-macro). They once
        // diverged (to_token_stream().to_string vs canonical_token_string),
        // making --update-hashes write hashes the proc-macro rejected.
        use quote::quote;
        for tokens in [
            quote! { pub fn deposit(amount: u64) -> u64 { amount + 1 } },
            quote! { pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> { Ok(()) } },
            quote! { pub fn no_args() -> u8 { 42 } },
        ] {
            let func: ItemFn = syn::parse2(tokens).unwrap();
            assert_eq!(
                content_hash(&func),
                spec_hash::body_hash_for_fn(&func),
                "drift::content_hash diverged from spec_hash::body_hash_for_fn — \
                 the proc-macro will reject the written hash. See drift.rs's \
                 content_hash docstring for the alignment requirement."
            );
        }
    }

    #[test]
    fn check_stamped_drift_flags_stale_spec_hash() {
        let dir = tempfile::tempdir().unwrap();

        // Body hash matches; only the `spec_hash` leg is deliberately stale.
        let spec_src = r#"program foo

handler foo (n : U64) : Active -> Active {
  effect { n := n + 1 }
}
"#;
        let spec_path = dir.path().join("foo.qedspec");
        std::fs::write(&spec_path, spec_src).unwrap();
        let real_spec_hash = spec_hash::spec_hash_for_handler(spec_src, "foo").unwrap();

        // Compute the body hash via scan_file (needs the `#[qed(verified)]` marker).
        let body_only = r#"
            #[qed(verified)]
            pub fn foo(n: u64) -> u64 { n + 1 }
        "#;
        let f0 = write_temp_rs(body_only);
        let entries = scan_file(f0.path()).unwrap();
        assert!(!entries.is_empty(), "scan_file should find the verified fn");
        let body_hash = match &entries[0].status {
            DriftStatus::NoHash { computed } => computed.clone(),
            _ => panic!("expected NoHash"),
        };

        let rs_path = dir.path().join("foo.rs");
        let stamped = format!(
            r#"
            #[qed(verified, spec = "foo.qedspec", handler = "foo", hash = "{}", spec_hash = "{}")]
            pub fn foo(n: u64) -> u64 {{ n + 1 }}
            "#,
            body_hash, "deadbeef_stale_hash"
        );
        std::fs::write(&rs_path, stamped).unwrap();
        // Sanity: live spec_hash is non-empty and not equal to the stale value.
        assert!(!real_spec_hash.is_empty());
        assert_ne!(real_spec_hash, "deadbeef_stale_hash");

        let stale = check_stamped_drift(dir.path()).unwrap();
        assert_eq!(stale.len(), 1, "expected 1 stale stamp, got {:?}", stale);
        assert_eq!(stale[0].fn_name, "foo");
    }

    #[test]
    fn check_stamped_drift_silent_when_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        // No stamped .rs files at all — should return an empty Vec, not
        // an error.
        std::fs::write(dir.path().join("plain.rs"), "fn plain() {}").unwrap();
        let stale = check_stamped_drift(dir.path()).unwrap();
        assert!(stale.is_empty());
    }
}
