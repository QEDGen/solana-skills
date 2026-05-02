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

/// Compute content hash for a function. MUST match the proc-macro's
/// recomputation byte-for-byte — pre-v2.11.3 this used
/// `to_token_stream().to_string()` directly, which subtly diverges from
/// the macro's `canonical_token_string` walker (rustc-vs-`from_str`
/// spacing). The result was that `qedgen check --update-hashes` wrote
/// hashes that the proc-macro then immediately rejected as drifted.
/// Delegate to the shared `spec_hash::body_hash_for_fn` so both sides
/// agree by construction.
fn content_hash(func: &ItemFn) -> String {
    spec_hash::body_hash_for_fn(func)
}

/// All key=value fields that may appear inside `#[qed(verified, ...)]`.
/// Used by `--update-hashes` to know which hash legs to refresh.
#[derive(Debug, Default, Clone)]
struct VerifiedAttr {
    pub spec: Option<String>,
    pub handler: Option<String>,
    pub hash: Option<String>,
    pub spec_hash: Option<String>,
    pub accounts: Option<String>,
    pub accounts_file: Option<String>,
    pub accounts_hash: Option<String>,
}

/// Extract every `key = "value"` pair inside a `#[qed(verified, ...)]`
/// attribute. Returns `None` when the attribute is not `qed(verified,
/// ...)` shaped. Returns `Some(VerifiedAttr::default())` when the
/// attribute is `#[qed(verified)]` with no key/value pairs.
fn parse_verified_attr(attr: &syn::Attribute) -> Option<VerifiedAttr> {
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

/// Backward-compat wrapper used by `scan_file`. Returns `Some(Option)`
/// matching the pre-v2.15 signature: outer Some = "this is a
/// `#[qed(verified)]` attribute", inner Option = the body `hash` value.
fn extract_hash_from_attr(attr: &syn::Attribute) -> Option<Option<String>> {
    parse_verified_attr(attr).map(|a| a.hash)
}

/// Walk parents of `start` looking for the named file. Returns the
/// absolute path of the first hit. Used by `--update-hashes` to resolve
/// `spec = "X.qedspec"` relative paths the same way the proc-macro
/// resolves them via `CARGO_MANIFEST_DIR` — the macro's resolution dir
/// is whichever ancestor the spec lives in, matching this walk.
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

/// Collected entry from scanning: function name, expected hash, parsed function.
type ScannedEntry = (String, Option<String>, ItemFn);

/// Collect verified functions from a top-level function item.
fn collect_from_fn(func: &ItemFn, out: &mut Vec<ScannedEntry>) {
    for attr in &func.attrs {
        if let Some(hash) = extract_hash_from_attr(attr) {
            out.push((func.sig.ident.to_string(), hash, func.clone()));
            break;
        }
    }
}

/// Collect verified functions from an impl block.
fn collect_from_impl(item: &syn::ItemImpl, out: &mut Vec<ScannedEntry>) {
    for impl_item in &item.items {
        if let syn::ImplItem::Fn(method) = impl_item {
            for attr in &method.attrs {
                if let Some(hash) = extract_hash_from_attr(attr) {
                    let item_fn = ItemFn {
                        attrs: method.attrs.clone(),
                        vis: method.vis.clone(),
                        sig: method.sig.clone(),
                        block: Box::new(method.block.clone()),
                    };
                    out.push((method.sig.ident.to_string(), hash, item_fn));
                    break;
                }
            }
        }
    }
}

/// Recursively collect verified functions from a list of items.
fn collect_from_items(items: &[syn::Item], out: &mut Vec<ScannedEntry>) {
    for item in items {
        match item {
            syn::Item::Fn(f) => collect_from_fn(f, out),
            syn::Item::Impl(i) => collect_from_impl(i, out),
            syn::Item::Trait(t) => {
                for trait_item in &t.items {
                    if let syn::TraitItem::Fn(method) = trait_item {
                        for attr in &method.attrs {
                            if let Some(hash) = extract_hash_from_attr(attr) {
                                if let Some(ref block) = method.default {
                                    let item_fn = ItemFn {
                                        attrs: method.attrs.clone(),
                                        vis: syn::Visibility::Inherited,
                                        sig: method.sig.clone(),
                                        block: Box::new(block.clone()),
                                    };
                                    out.push((method.sig.ident.to_string(), hash, item_fn));
                                }
                                break;
                            }
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner_items)) = &m.content {
                    collect_from_items(inner_items, out);
                }
            }
            _ => {}
        }
    }
}

/// Scan a single Rust source file for `#[qed(verified)]` functions.
fn scan_file(path: &Path) -> Result<Vec<VerifiedEntry>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    let syntax = syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;

    let mut scanned = Vec::new();
    collect_from_items(&syntax.items, &mut scanned);

    let results = scanned
        .into_iter()
        .map(|(fn_name, expected_hash, func)| {
            let actual = content_hash(&func);
            let status = match expected_hash {
                Some(expected) if expected == actual => DriftStatus::Ok,
                Some(expected) => DriftStatus::Drifted { expected, actual },
                None => DriftStatus::NoHash { computed: actual },
            };
            VerifiedEntry {
                file: path.to_path_buf(),
                fn_name,
                status,
            }
        })
        .collect();

    Ok(results)
}

/// Collect all `.rs` files under a path (file or directory).
fn collect_rs_files(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files = Vec::new();
    for entry in walkdir(path)? {
        if entry.extension().is_some_and(|e| e == "rs") {
            files.push(entry);
        }
    }
    files.sort();
    Ok(files)
}

/// Simple recursive directory walk (avoids adding walkdir dependency).
fn walkdir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();
    if !dir.is_dir() {
        return Ok(results);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            results.extend(walkdir(&path)?);
        } else {
            results.push(path);
        }
    }
    Ok(results)
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
/// callees have themselves drifted directly. (GH issue #28.)
///
/// The pre-v2.15 implementation built a transitive hash from `(body +
/// sorted(callee_name:callee_hash))` and compared it against the stored
/// `hash = "..."` — which seals only the function body, not the
/// transitive closure. The two hash semantics never match when any
/// callee exists, producing a false drift on every function with
/// non-trivial body. Without per-callee stored hashes (a new attribute
/// shape that does not exist today), the only sound transitive signal
/// available is "one of my verified callees is itself drifted."
///
/// Algorithm:
/// 1. Gather every `#[qed(verified)]` function in the file with its
///    expected hash.
/// 2. Compute each function's current direct hash; a function is
///    "directly drifted" when current ≠ expected.
/// 3. For every function whose direct hash IS OK, walk its callees;
///    surface a transitive entry naming the verified callees that are
///    themselves directly drifted. Non-verified callees can't drift —
///    they have no anchor.
///
/// Net effect: `--deep` becomes a directly-drifted aggregator showing
/// the upward fan-out of a primitive drift event. No false positives;
/// trade-off is non-verified callee changes do not surface (matches
/// the existing v2.14 test `deep_no_false_positive_when_callee_unchanged`'s
/// stated semantics).
fn scan_file_deep(path: &Path) -> Result<Vec<TransitiveDriftEntry>> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let syntax = syn::parse_file(&source).with_context(|| format!("parsing {}", path.display()))?;

    let all_fns = collect_all_fns(&syntax);

    let mut scanned = Vec::new();
    collect_from_items(&syntax.items, &mut scanned);

    // Step 1: which `#[qed(verified)]` functions have drifted directly?
    let mut directly_drifted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (fn_name, expected_hash, func) in &scanned {
        let Some(expected) = expected_hash else {
            continue;
        };
        let actual = content_hash(func);
        if expected != &actual {
            directly_drifted.insert(fn_name.clone());
        }
    }

    // Step 2: for each function whose direct hash IS OK, find verified
    // callees that drifted directly. Those callees are the transitive
    // signal.
    let mut results = Vec::new();
    for (fn_name, expected_hash, func) in &scanned {
        let Some(expected) = expected_hash else {
            continue;
        };
        let actual = content_hash(func);
        if expected != &actual {
            continue; // direct drift handled by check(); don't double-report
        }

        let callees = extract_callees(func);
        let mut changed: Vec<String> = callees
            .into_iter()
            .filter(|name| all_fns.contains_key(name) && directly_drifted.contains(name))
            .collect();
        changed.sort();
        changed.dedup();

        if !changed.is_empty() {
            results.push(TransitiveDriftEntry {
                file: path.to_path_buf(),
                fn_name: fn_name.clone(),
                changed_callees: changed,
            });
        }
    }

    Ok(results)
}

/// Run deep (transitive) drift analysis across all files.
pub fn check_deep(input: &Path) -> Result<Vec<TransitiveDriftEntry>> {
    let files = collect_rs_files(input)?;
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
    let files = collect_rs_files(input)?;
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

/// Update `#[qed(verified, ...)]` in source files with computed hashes.
///
/// v2.15 (GH issue #27): refreshes all three hash legs — `hash`,
/// `spec_hash`, and `accounts_hash` — not just `hash`. Pre-v2.15 only
/// `hash` was updated; users who ran `--update-hashes` expecting drift
/// to be fully fixed found the proc-macro still rejecting their build
/// with a stale `spec_hash` or `accounts_hash`. This walks every
/// `#[qed(verified, ...)]` attribute, recomputes whichever hash legs
/// are present, and replaces stale values in-place.
///
/// Resolution: `spec` and `accounts_file` paths are resolved by walking
/// parent directories from the source file (matching the proc-macro's
/// `CARGO_MANIFEST_DIR`-relative behavior). When the referenced file
/// can't be located, that hash leg is skipped with a warning rather
/// than failing — users may run `--update-hashes` against a partial
/// tree.
pub fn update(input: &Path) -> Result<usize> {
    let files = collect_rs_files(input)?;
    let mut updated = 0;

    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let syntax = match syn::parse_file(&source) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let mut scanned = Vec::new();
        collect_from_items(&syntax.items, &mut scanned);

        // Re-extract the full attribute alongside each scanned function.
        // The pre-v2.15 path stored only the body hash; we now need the
        // full key/value set to know which legs to refresh.
        let attrs = collect_verified_attrs(&syntax.items);

        if scanned.is_empty() {
            continue;
        }

        let mut new_source = source.clone();
        let mut changed = false;

        for ((_fn_name, _expected_body, func), attr) in scanned.iter().zip(attrs.iter()) {
            // Body hash (hash = "..."): same logic as before, plus the
            // `#[qed(verified)]` → stamped form.
            let actual_body = content_hash(func);
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
                    // `#[qed(verified)]` with no `hash` field at all —
                    // stamp it with the computed hash. (Stays compatible
                    // with the v2.14 NoHash → stamped flow.)
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

            // spec_hash leg: present only when `spec` + `handler` are
            // also set. Resolve the spec path, compute, replace if stale.
            if let (Some(spec_path), Some(handler_name), Some(expected_spec)) =
                (&attr.spec, &attr.handler, &attr.spec_hash)
            {
                if let Some(resolved) = find_relative_file(file, spec_path) {
                    if let Ok(spec_src) = std::fs::read_to_string(&resolved) {
                        if let Some(actual_spec) =
                            spec_hash::spec_hash_for_handler(&spec_src, handler_name)
                        {
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
                    }
                } else {
                    eprintln!(
                        "warning: --update-hashes: could not resolve `spec = \"{}\"` from {} \
                         (skipping spec_hash refresh for this entry)",
                        spec_path,
                        file.display()
                    );
                }
            }

            // accounts_hash leg: present only when `accounts` +
            // `accounts_file` are also set. Issue #29 already enforces
            // all-or-nothing at the macro side, so partial configs
            // surface as compile errors rather than silently skipping
            // here.
            if let (Some(struct_name), Some(accounts_file), Some(expected_acct)) =
                (&attr.accounts, &attr.accounts_file, &attr.accounts_hash)
            {
                if let Some(resolved) = find_relative_file(file, accounts_file) {
                    if let Ok(acct_src) = std::fs::read_to_string(&resolved) {
                        if let Some(actual_acct) =
                            spec_hash::accounts_struct_hash(&acct_src, struct_name)
                        {
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
                    }
                } else {
                    eprintln!(
                        "warning: --update-hashes: could not resolve `accounts_file = \"{}\"` \
                         from {} (skipping accounts_hash refresh for this entry)",
                        accounts_file,
                        file.display()
                    );
                }
            }
        }

        if changed {
            std::fs::write(file, &new_source)?;
        }
    }

    Ok(updated)
}

/// Parallel collector to `collect_from_items` that captures the full
/// attribute (not just the body hash) for each verified function.
/// Indices match `collect_from_items` so callers can zip the two.
fn collect_verified_attrs(items: &[syn::Item]) -> Vec<VerifiedAttr> {
    let mut out = Vec::new();
    walk_verified_attrs(items, &mut out);
    out
}

fn walk_verified_attrs(items: &[syn::Item], out: &mut Vec<VerifiedAttr>) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                for attr in &f.attrs {
                    if let Some(parsed) = parse_verified_attr(attr) {
                        out.push(parsed);
                        break;
                    }
                }
            }
            syn::Item::Impl(i) => {
                for impl_item in &i.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        for attr in &method.attrs {
                            if let Some(parsed) = parse_verified_attr(attr) {
                                out.push(parsed);
                                break;
                            }
                        }
                    }
                }
            }
            syn::Item::Trait(t) => {
                for trait_item in &t.items {
                    if let syn::TraitItem::Fn(method) = trait_item {
                        for attr in &method.attrs {
                            if let Some(parsed) = parse_verified_attr(attr) {
                                if method.default.is_some() {
                                    out.push(parsed);
                                }
                                break;
                            }
                        }
                    }
                }
            }
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk_verified_attrs(inner, out);
                }
            }
            _ => {}
        }
    }
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
        // v2.15 fix for issue #28: --deep now flags transitive drift only
        // when a *verified* callee has itself drifted directly. Without
        // per-callee stored hashes (a new attribute shape that does not
        // exist), this is the only sound transitive signal — comparing a
        // body+callees transitive hash against the body-only stored hash
        // produced false positives on every function with non-trivial
        // body. Non-verified callees can't drift; they have no anchor.
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
        // The complement to issue #28: changes to *non-verified* callees
        // do NOT surface as transitive drift. This is intentional —
        // non-verified callees have no anchor to compare against. The
        // pre-v2.15 code falsely reported every non-trivial function as
        // drifted; v2.15 reports nothing here, which is the correct
        // floor.
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
        // v2.15 fix for #28: when nothing has drifted, --deep emits
        // nothing. Pre-v2.15 the assertion was discarded (`let _ =
        // deep_entries`) because the implementation always reported
        // false drift on functions with non-trivial bodies — comparing
        // a body+callee transitive hash against a body-only stored
        // hash. The new implementation only flags verified-callee
        // direct drift, so this case correctly returns empty.
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
        // Ensure the CLI hash algorithm matches what the proc macro computes.
        // This test uses the same function and checks for 16-char hex output.
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
        // drift.rs's content_hash MUST agree with spec_hash::body_hash_for_fn,
        // which in turn agrees with qedgen-macros::verified::content_hash.
        // Pre-v2.11.3 these diverged (drift used to_token_stream().to_string,
        // spec_hash uses canonical_token_string), causing
        // `qedgen check --update-hashes` to write hashes the proc-macro
        // immediately rejected. Lock the alignment as a regression test.
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
}
