use super::*;

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
pub(super) fn normalize_module_prefix(prefix: &[String]) -> Vec<String> {
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
pub(super) fn prioritize_candidates(
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
