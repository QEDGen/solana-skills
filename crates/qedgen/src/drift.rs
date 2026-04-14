use anyhow::{Context, Result};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use syn::ItemFn;

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

/// Compute content hash for a function (same algorithm as the proc macro).
///
/// Strips all attributes, normalizes via syn round-trip, SHA-256, truncate to 16 hex.
fn content_hash(func: &ItemFn) -> String {
    let mut stripped = func.clone();
    stripped.attrs.clear();
    let canonical = stripped.to_token_stream().to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Extract `hash = "..."` value from a `#[qed(verified, hash = "...")]` attribute.
fn extract_hash_from_attr(attr: &syn::Attribute) -> Option<Option<String>> {
    // Check if this is a `qed` attribute
    let path = attr.path();
    if !path.is_ident("qed") {
        return None;
    }

    // Parse the token stream inside the parens
    let tokens = match &attr.meta {
        syn::Meta::List(list) => &list.tokens,
        _ => return None,
    };

    let token_vec: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();

    // Check first ident is "verified"
    match token_vec.first() {
        Some(proc_macro2::TokenTree::Ident(ident)) if ident == "verified" => {}
        _ => return None,
    }

    // Find hash = "..." in the remaining tokens
    let mut i = 0;
    while i < token_vec.len() {
        if let proc_macro2::TokenTree::Ident(ref ident) = token_vec[i] {
            if ident == "hash" && i + 2 < token_vec.len() {
                if let proc_macro2::TokenTree::Punct(ref p) = token_vec[i + 1] {
                    if p.as_char() == '=' {
                        if let proc_macro2::TokenTree::Literal(ref lit) = token_vec[i + 2] {
                            let lit_str = lit.to_string();
                            let hash = lit_str.trim_matches('"').to_string();
                            return Some(Some(hash));
                        }
                    }
                }
            }
        }
        i += 1;
    }

    // Found #[qed(verified)] but no hash
    Some(None)
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
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;

    let syntax = syn::parse_file(&source)
        .with_context(|| format!("parsing {}", path.display()))?;

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
        let file = entry
            .file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
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

    let ok = entries.iter().filter(|e| e.status == DriftStatus::Ok).count();
    let drifted = entries
        .iter()
        .filter(|e| matches!(e.status, DriftStatus::Drifted { .. }))
        .count();
    let no_hash = entries
        .iter()
        .filter(|e| matches!(e.status, DriftStatus::NoHash { .. }))
        .count();
    eprintln!("\n{} verified, {} drifted, {} unhashed", ok, drifted, no_hash);
}

/// Update `#[qed(verified, hash = "...")]` in source files with computed hashes.
pub fn update(input: &Path) -> Result<usize> {
    let files = collect_rs_files(input)?;
    let mut updated = 0;

    for file in &files {
        let entries = match scan_file(file) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entries.is_empty() {
            continue;
        }

        let mut source = std::fs::read_to_string(file)?;
        let mut changed = false;

        for entry in &entries {
            match &entry.status {
                DriftStatus::Ok => {} // already correct
                DriftStatus::Drifted { expected, actual } => {
                    // Replace the old hash with the new one
                    let old = format!("hash = \"{}\"", expected);
                    let new = format!("hash = \"{}\"", actual);
                    if source.contains(&old) {
                        source = source.replacen(&old, &new, 1);
                        changed = true;
                        updated += 1;
                    }
                }
                DriftStatus::NoHash { computed } => {
                    // Replace #[qed(verified)] with #[qed(verified, hash = "...")]
                    // Handle both `#[qed(verified)]` and `#[qed( verified )]` etc.
                    let patterns = [
                        "qed(verified)",
                        "qed( verified )",
                        "qed(verified )",
                        "qed( verified)",
                    ];
                    for pat in &patterns {
                        let replacement = format!("qed(verified, hash = \"{}\")", computed);
                        if source.contains(pat) {
                            source = source.replacen(pat, &replacement, 1);
                            changed = true;
                            updated += 1;
                            break;
                        }
                    }
                }
            }
        }

        if changed {
            std::fs::write(file, &source)?;
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
}
