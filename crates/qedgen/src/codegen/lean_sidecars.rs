//! Lean codegen sidecar writer — the renderer-agnostic half of
//! `lean_gen_mir::generate`.
//!
//! `write_spec_with_sidecars` takes a rendered `Spec.lean` body and emits
//! it plus every pinned-interface sidecar artifact: the sibling
//! `<Iface>.lean` axiom modules, the `import <Iface>` lines injected into
//! the body, and the consumer lakefile's `roots` / verified-callee
//! `require` directives.
//!
//! Extracted from the former `lean_gen.rs` in v2.32 so the sidecar
//! closure outlived that module's deletion. Self-contained: it carries
//! its own private copies of the small leaf helpers (`safe_name`,
//! `param_sig_str`, `map_type`, `handler_is_pinned`, `scan_abstract_fields`,
//! `rewrite_axiom_body_to_accessors`) — these and `render_interface_axiom_module`
//! are gated against drift by the `axiom_module_matches_golden` test below.

use crate::check::ParsedSpec;
use anyhow::Result;
use std::path::Path;

/// Write the rendered `Spec.lean` plus every pinned-interface sidecar
/// (sibling axiom module, lakefile roots update, verified-callee
/// `require` directives). Shared between `lean_gen::generate` and
/// `lean_gen_mir::generate` so the codegens emit identical sidecar
/// layouts regardless of which renderer produced the `Spec.lean` body.
pub(crate) fn write_spec_with_sidecars(
    content: String,
    spec: &ParsedSpec,
    output_path: &Path,
) -> Result<()> {
    let pinned = collect_pinned_interfaces(spec);

    // v2.26 Track F: prepend `import <Iface>` lines for every pinned
    // interface module. The renderer already places `import
    // QEDGen.Solana.*` at the top of every output flavor (single,
    // multi, ADT); we inject the interface-module imports immediately
    // after the existing import block so the namespace order matches
    // Lean's expectation (imports before `namespace`).
    let final_content = if !pinned.is_empty() {
        inject_interface_imports(&content, &pinned)
    } else {
        content
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, &final_content)?;
    eprintln!("  wrote {}", output_path.display());

    // Write sibling `<Iface>.lean` axiom modules for every pinned
    // interface. The set is recomputed here (independent of the
    // render pass) so `render` keeps its single-String signature;
    // the call-site discharge path inside `render_cpi_theorems` uses
    // the same `handler_is_pinned` predicate so the two sides agree
    // on which interfaces need axioms.
    //
    // v2.27 Track B — verified callees (those in `spec.verified_callees`)
    // get their proof modules from the provider package via a `require`
    // directive in the consumer's lakefile. Skip writing the local
    // sibling axiom module for them and don't add them to the lakefile
    // `roots := #[...]` array (the imported package owns those modules).
    // Unverified pinned callees stay on the v2.26 stance-1 path:
    // sibling axiom module + roots entry.
    if let Some(parent) = output_path.parent() {
        let local_pinned: std::collections::BTreeSet<String> = pinned
            .iter()
            .filter(|i| !spec.verified_callees.contains_key(i.as_str()))
            .cloned()
            .collect();
        for iface_name in &local_pinned {
            let iface = spec
                .interfaces
                .iter()
                .find(|i| &i.name == iface_name)
                .expect("pinned interface must exist in spec.interfaces");
            let iface_path = parent.join(format!("{}.lean", safe_module_name(iface_name)));
            let module = render_interface_axiom_module(iface);
            std::fs::write(&iface_path, &module)?;
            eprintln!("  wrote {}", iface_path.display());
        }

        // Update the lakefile's roots to include any newly-written
        // sibling axiom modules. Best-effort: lakefile may not exist
        // yet (the `qedgen init` step ships it). When it does, append
        // the modules deterministically so the rewrite is idempotent.
        if !pinned.is_empty() {
            let lakefile_path = parent.join("lakefile.lean");
            if lakefile_path.exists() {
                // v2.27 Track B — strip stale sibling-module roots for
                // callees that transitioned from unverified to
                // verified. The local `<Iface>.lean` is no longer
                // written, so its `roots` entry would point at a
                // non-existent module and break `lake build`. Narrow:
                // only removes roots whose name matches a verified
                // callee.
                let verified_roots: Vec<String> = spec
                    .verified_callees
                    .keys()
                    .map(|n| safe_module_name(n))
                    .collect();
                if !verified_roots.is_empty() {
                    remove_lakefile_roots(&lakefile_path, &verified_roots)?;
                }
                update_lakefile_roots(&lakefile_path, &local_pinned)?;
                // v2.27 Track B — inject a `require <pkg> from
                // "<rel-path>"` directive for every verified callee.
                // The relative path is computed from the consumer's
                // lakefile location to the provider's proof package
                // root recorded in `spec.verified_callees`.
                let verified_for_emit: Vec<(String, std::path::PathBuf)> = pinned
                    .iter()
                    .filter_map(|name| {
                        spec.verified_callees
                            .get(name)
                            .map(|pkg_root| (name.clone(), pkg_root.clone()))
                    })
                    .collect();
                if !verified_for_emit.is_empty() {
                    inject_verified_callee_requires(&lakefile_path, &verified_for_emit)?;
                }
            }
        }
    }

    Ok(())
}

/// v2.27 Track B — idempotent injection of `require <pkg> from "<path>"`
/// directives for every verified callee (one per imported interface
/// whose provider shipped a Lake-buildable proof package).
fn inject_verified_callee_requires(
    lakefile_path: &Path,
    verified: &[(String, std::path::PathBuf)],
) -> Result<()> {
    let original = std::fs::read_to_string(lakefile_path)?;
    let lakefile_parent = lakefile_path.parent().unwrap_or(Path::new("."));
    let mut to_add: Vec<String> = Vec::new();
    for (iface_name, pkg_root) in verified {
        let pkg = proof_pkg_name(iface_name);
        let needle = format!("require {} from", pkg);
        if original.contains(&needle) {
            continue;
        }
        let rel =
            pathdiff_relative_from(pkg_root, lakefile_parent).unwrap_or_else(|| pkg_root.clone());
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        to_add.push(format!(
            "-- v2.27 Track B: verified-callee proof package (Stance 2).\n\
             require {} from \"{}\"\n",
            pkg, rel_str,
        ));
    }
    if to_add.is_empty() {
        return Ok(());
    }
    // Anchor: prefer the line right after `package <name>` (always
    // present in qedgen-emitted lakefiles). Falls back to file-end.
    let injected = match original.find("package ") {
        Some(start) => {
            let line_end = original[start..]
                .find('\n')
                .map(|n| start + n + 1)
                .unwrap_or(original.len());
            let mut rewritten = String::with_capacity(original.len() + 128);
            rewritten.push_str(&original[..line_end]);
            rewritten.push('\n');
            for block in &to_add {
                rewritten.push_str(block);
            }
            rewritten.push_str(&original[line_end..]);
            rewritten
        }
        None => {
            // Unusual shape; append to end so we never silently drop.
            let mut rewritten = original.clone();
            if !rewritten.ends_with('\n') {
                rewritten.push('\n');
            }
            for block in &to_add {
                rewritten.push_str(block);
            }
            rewritten
        }
    };
    std::fs::write(lakefile_path, injected)?;
    eprintln!(
        "  updated {} (added {} verified-callee require(s))",
        lakefile_path.display(),
        to_add.len()
    );
    Ok(())
}

/// Compute `target` relative to `base`. Pure-string version of
/// `std::path` semantics — only descends when components match. Falls
/// back to an absolute path when no common prefix exists (so the
/// lakefile still compiles even when the provider lives outside the
/// consumer's tree).
fn pathdiff_relative_from(target: &Path, base: &Path) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    let mut t_iter = target.components();
    let mut b_iter = base.components();
    loop {
        match (t_iter.clone().next(), b_iter.clone().next()) {
            (Some(a), Some(b)) if a == b => {
                t_iter.next();
                b_iter.next();
            }
            _ => break,
        }
    }
    let mut out = std::path::PathBuf::new();
    for _ in b_iter.filter(|c| !matches!(c, Component::RootDir | Component::Prefix(_))) {
        out.push("..");
    }
    for c in t_iter {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        Some(std::path::PathBuf::from("."))
    } else {
        Some(out)
    }
}

/// Inject `import <Iface>` lines immediately after the existing
/// `import QEDGen.Solana.*` block. Idempotent: pre-existing imports
/// for the same module are left in place.
fn inject_interface_imports(content: &str, pinned: &std::collections::BTreeSet<String>) -> String {
    // Find the position just after the last `import QEDGen.Solana.*`
    // line at the top of the file. If no such line exists (sBPF mode,
    // indexed-state mode), inject at the very top.
    let mut insert_at: usize = 0;
    for (i, line) in content.lines().enumerate() {
        if line.starts_with("import ") {
            insert_at = content
                .lines()
                .take(i + 1)
                .map(|l| l.len() + 1)
                .sum::<usize>();
        } else if !line.is_empty() {
            break;
        }
    }
    let mut imports = String::new();
    for iface in pinned {
        let module = safe_module_name(iface);
        let needle = format!("import {}", module);
        if content.contains(&needle) {
            continue;
        }
        imports.push_str(&format!("import {}\n", module));
    }
    if imports.is_empty() {
        return content.to_string();
    }
    let mut out = String::with_capacity(content.len() + imports.len());
    out.push_str(&content[..insert_at]);
    out.push_str(&imports);
    out.push_str(&content[insert_at..]);
    out
}

/// v2.26 Track F: walk every handler's `call Interface.handler(...)`
/// sites and collect the set of interfaces that meet both pinning
/// requirements (binary_hash + non-empty `ensures`).
fn collect_pinned_interfaces(spec: &ParsedSpec) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for handler in &spec.handlers {
        for call in &handler.calls {
            let Some(iface) = spec
                .interfaces
                .iter()
                .find(|i| i.name == call.target_interface)
            else {
                continue;
            };
            let Some(ih) = iface
                .handlers
                .iter()
                .find(|h| h.name == call.target_handler)
            else {
                continue;
            };
            if handler_is_pinned(iface, ih) {
                out.insert(iface.name.clone());
            }
        }
    }
    out
}

/// Sanitize an interface name for use as a Lean module file name.
/// Lean module names must be valid identifiers; the same name is used
/// in the `import` line and the `roots` list of the lakefile.
fn safe_module_name(name: &str) -> String {
    // Replace anything that isn't a Lean ident-char with underscore.
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// v2.27 Track B — Lake package name convention for a verified-callee's
/// proof package. Convention: lowercase the interface's first character
/// + append `Proofs`. `Token` → `tokenProofs`.
///
/// `pub(crate)` — `check.rs` reuses it when surfacing verified-callee
/// lint diagnostics so the package name it reports matches the one this
/// module emits into the lakefile.
pub(crate) fn proof_pkg_name(iface_name: &str) -> String {
    let safe = safe_module_name(iface_name);
    let mut chars = safe.chars();
    match chars.next() {
        Some(c) => {
            let lower: String = c.to_lowercase().collect();
            format!("{}{}Proofs", lower, chars.as_str())
        }
        None => "stdlibProofs".to_string(),
    }
}

/// Render the `<Iface>.lean` sibling axiom module body. Emits one
/// `axiom <handler>.ensures_axiom_<idx>` per `(handler, ensures)` pair
/// on the interface, plus the `binary_hash` constant.
fn render_interface_axiom_module(iface: &crate::check::ParsedInterface) -> String {
    let mut out = String::new();
    out.push_str("-- v2.26 Track F: bundled-interface axiom module.\n");
    out.push_str("-- Stance 1 — the upstream binary_hash pin is the contract\n");
    out.push_str("-- boundary. Each `axiom ensures_axiom_<idx>` corresponds to one\n");
    out.push_str("-- `ensures` clause on the interface handler; the caller's\n");
    out.push_str("-- Lean proof discharges its CPI post-condition by applying\n");
    out.push_str("-- the relevant axiom, instead of carrying a `sorry`.\n--\n");
    out.push_str("-- Axioms have two shapes:\n");
    out.push_str("--   * v2.26 callee-frame — parameters and predicates only\n");
    out.push_str("--     reference the callee's own ABI, never the caller's\n");
    out.push_str("--     State type. Reusable across every caller.\n");
    out.push_str("--   * v2.27 Track A caller-State-aware — the ensures\n");
    out.push_str("--     references abstract State fields (applied-accessor\n");
    out.push_str("--     form, e.g. `from_balance pre`). The axiom is\n");
    out.push_str("--     polymorphic in `State` and takes pre+post snapshots\n");
    out.push_str("--     plus one `State \u{2192} T` accessor per abstract\n");
    out.push_str("--     field, where `T` comes from the interface's\n");
    out.push_str("--     `state { name : Type, ... }` declaration (v2.27\n");
    out.push_str("--     Phase 0): `Nat` for the `U*` family, `Int` for\n");
    out.push_str("--     `I*`, `Bool` for `Bool`, `Pubkey` for `Pubkey`.\n");
    out.push_str("--     Fields not declared in the state block default to\n");
    out.push_str("--     `Nat` (back-compat). Callers apply the axiom with\n");
    out.push_str("--     `(\u{00B7}.<caller_field>)` per slot via their per-call\n");
    out.push_str("--     `state_binders { ... }` block.\n\n");
    out.push_str("import QEDGen.Solana.Account\n");
    out.push_str("import QEDGen.Solana.Cpi\n");
    out.push_str("import QEDGen.Solana.Valid\n\n");
    out.push_str(&format!("namespace {}\n\n", safe_name(&iface.name)));
    out.push_str("open QEDGen.Solana\n\n");

    let binary_hash = iface
        .upstream
        .as_ref()
        .and_then(|u| u.binary_hash.as_deref())
        .unwrap_or("");
    out.push_str(&format!(
        "/-- Content pin against the deployed program at\n    `{}`. Callers commit to this hash; if the deployed\n    binary changes, the lock must be regenerated. -/\n",
        iface.program_id.as_deref().unwrap_or("<unknown>"),
    ));
    out.push_str(&format!(
        "def binary_hash : String := \"{}\"\n\n",
        binary_hash,
    ));

    for handler in &iface.handlers {
        if handler.ensures.is_empty() {
            continue;
        }
        out.push_str(&format!("namespace {}\n\n", safe_name(&handler.name)));
        for (ens_idx, ensures) in handler.ensures.iter().enumerate() {
            let params_sig = param_sig_str(&handler.params);
            // v2.27 Track A — scan the callee's lean_expr for any
            // abstract State-field references (`s.X` / `s'.X`,
            // produced by the `Ctx::Ensures` lowering of `state.X`).
            let abstract_fields = scan_abstract_fields(&ensures.lean_expr);
            out.push_str(&format!(
                "/-- `{}.{}` post-condition #{} (axiomatized; discharged by binary_hash pin). -/\n",
                iface.name, handler.name, ens_idx,
            ));
            if abstract_fields.is_empty() {
                // v2.26 path — callee-frame, param-only.
                if handler.params.is_empty() {
                    out.push_str(&format!(
                        "axiom ensures_axiom_{} : {}\n\n",
                        ens_idx, ensures.lean_expr,
                    ));
                } else {
                    out.push_str(&format!(
                        "axiom ensures_axiom_{}{} : {}\n\n",
                        ens_idx, params_sig, ensures.lean_expr,
                    ));
                }
            } else {
                // v2.27 Track A path — caller-State-aware.
                let mut sig = String::new();
                sig.push_str(" {State : Type} [Inhabited State]");
                sig.push_str(" (pre post : State)");
                sig.push_str(&params_sig);
                for field in &abstract_fields {
                    let codomain = iface
                        .state_fields
                        .iter()
                        .find(|(n, _)| n == field)
                        .map(|(_, t)| map_type(t.as_str()))
                        .unwrap_or("Nat");
                    sig.push_str(&format!(" ({} : State \u{2192} {})", field, codomain));
                }
                // Body rewrite: `s'.X` → `(X post)`, `s.X` → `(X pre)`.
                let body = rewrite_axiom_body_to_accessors(&ensures.lean_expr);
                out.push_str(&format!(
                    "axiom ensures_axiom_{}{} : {}\n\n",
                    ens_idx, sig, body,
                ));
            }
        }
        out.push_str(&format!("end {}\n\n", safe_name(&handler.name)));
    }

    out.push_str(&format!("end {}\n", safe_name(&iface.name)));
    out
}

/// v2.27 Track A — scan a callee's Lean-rendered `ensures` text for
/// abstract State-field references (`s.X` / `s'.X`). Returns the
/// abstract field names in first-occurrence order.
fn scan_abstract_fields(ensures_lean: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\bs'?\.([A-Za-z_][A-Za-z0-9_]*)")
        .expect("regex compiles for abstract-field scan");
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for cap in re.captures_iter(ensures_lean) {
        let field = cap.get(1).unwrap().as_str();
        if seen.insert(field.to_string()) {
            out.push(field.to_string());
        }
    }
    out
}

/// v2.27 Track A — rewrite a callee's Lean `ensures` text into the
/// abstract-accessor form used inside the bundled axiom body. Each
/// `s'.X` becomes `(X post)` and each `s.X` becomes `(X pre)`.
fn rewrite_axiom_body_to_accessors(ensures_lean: &str) -> String {
    // Order matters: do `s'.X` first so we don't accidentally match
    // the `s` half of `s'.X` after the apostrophe.
    let re_post = regex::Regex::new(r"\bs'\.([A-Za-z_][A-Za-z0-9_]*)")
        .expect("regex compiles for post-state accessor rewrite");
    let after_post = re_post.replace_all(ensures_lean, "($1 post)").into_owned();
    let re_pre = regex::Regex::new(r"\bs\.([A-Za-z_][A-Za-z0-9_]*)")
        .expect("regex compiles for pre-state accessor rewrite");
    re_pre.replace_all(&after_post, "($1 pre)").into_owned()
}

/// v2.27 Track B — strip the named modules from a lakefile's
/// `roots := #[...]` array. Counterpart to `update_lakefile_roots`.
/// Idempotent: when none of the named modules are present, the file
/// is left untouched.
fn remove_lakefile_roots(lakefile_path: &Path, to_remove: &[String]) -> Result<()> {
    if to_remove.is_empty() {
        return Ok(());
    }
    let original = std::fs::read_to_string(lakefile_path)?;
    let needle = "roots := #[";
    let Some(start) = original.find(needle) else {
        return Ok(());
    };
    let after_open = start + needle.len();
    let Some(end_rel) = original[after_open..].find(']') else {
        return Ok(());
    };
    let end = after_open + end_rel;
    let inner = original[after_open..end].trim();
    if inner.is_empty() {
        return Ok(());
    }
    let target_strs: Vec<String> = to_remove.iter().map(|m| format!("`{}", m)).collect();
    let current: Vec<String> = inner.split(',').map(|s| s.trim().to_string()).collect();
    let retained: Vec<String> = current
        .iter()
        .filter(|r| !target_strs.iter().any(|t| t == *r))
        .cloned()
        .collect();
    if retained.len() == current.len() {
        return Ok(());
    }
    let new_inner = retained.join(", ");
    let mut rewritten = String::new();
    rewritten.push_str(&original[..after_open]);
    rewritten.push_str(&new_inner);
    rewritten.push_str(&original[end..]);
    std::fs::write(lakefile_path, rewritten)?;
    eprintln!(
        "  reconciled {} (removed {} stale verified-callee root(s))",
        lakefile_path.display(),
        current.len() - retained.len(),
    );
    Ok(())
}

/// Idempotent lakefile update: ensures every pinned-interface module is
/// listed in the `roots := #[...]` array. Other roots and any non-roots
/// content are preserved verbatim.
fn update_lakefile_roots(
    lakefile_path: &Path,
    pinned: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let original = std::fs::read_to_string(lakefile_path)?;
    let modules: Vec<String> = pinned
        .iter()
        .map(|i| format!("`{}", safe_module_name(i)))
        .collect();
    // Find a `roots := #[ ... ]` segment and add any missing modules.
    let needle = "roots := #[";
    let Some(start) = original.find(needle) else {
        return Ok(()); // unknown shape; leave the file alone.
    };
    let after_open = start + needle.len();
    let Some(end_rel) = original[after_open..].find(']') else {
        return Ok(());
    };
    let end = after_open + end_rel;
    let inner = original[after_open..end].trim();
    let mut current: Vec<String> = if inner.is_empty() {
        Vec::new()
    } else {
        inner.split(',').map(|s| s.trim().to_string()).collect()
    };
    let mut changed = false;
    for m in &modules {
        if !current.iter().any(|c| c.trim() == m.as_str()) {
            current.push(m.clone());
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let new_inner = current.join(", ");
    let mut rewritten = String::new();
    rewritten.push_str(&original[..after_open]);
    rewritten.push_str(&new_inner);
    rewritten.push_str(&original[end..]);
    std::fs::write(lakefile_path, rewritten)?;
    eprintln!(
        "  updated {} (added {} sibling module(s))",
        lakefile_path.display(),
        modules.len()
    );
    Ok(())
}

// ----------------------------------------------------------------------
// Leaf helpers — small pure functions shared with the legacy
// `lean_gen` renderer. Carried as private copies so this module is
// self-contained; the `lean_gen.rs` copies die with that module in
// workstream 3.
// ----------------------------------------------------------------------

/// True iff an interface handler is Tier-1/2 pinned: it declares
/// `ensures` AND its interface carries a non-empty `binary_hash`.
fn handler_is_pinned(
    iface: &crate::check::ParsedInterface,
    handler: &crate::check::ParsedInterfaceHandler,
) -> bool {
    if handler.ensures.is_empty() {
        return false;
    }
    match &iface.upstream {
        Some(u) => u
            .binary_hash
            .as_deref()
            .is_some_and(|h| !h.trim().is_empty()),
        None => false,
    }
}

/// Map DSL numeric types to their Lean codomain.
fn map_type(t: &str) -> &str {
    match t {
        "U8" | "U16" | "U32" | "U64" | "U128" => "Nat",
        "I8" | "I16" | "I32" | "I64" | "I128" => "Int",
        _ => t,
    }
}

/// Quote Lean keywords as «name» so they survive as identifiers.
fn safe_name(name: &str) -> String {
    let keywords = [
        "open",
        "close",
        "initialize",
        "import",
        "namespace",
        "end",
        "where",
        "with",
        "do",
        "let",
        "if",
        "then",
        "else",
        "match",
        "return",
        "in",
        "for",
    ];
    if keywords.contains(&name) {
        format!("\u{00AB}{}\u{00BB}", name)
    } else {
        name.to_string()
    }
}

/// Build a parameter signature string (` (n : T)` per param) for axiom
/// statements.
fn param_sig_str(params: &[(String, String)]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = params
            .iter()
            .map(|(n, t)| format!(" ({} : {})", n, map_type(t)))
            .collect();
        parts.join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pinned-but-unverified interface (`upstream { binary_hash }` +
    /// `ensures` referencing `state.X`) — the only shape that drives the
    /// sibling `<Iface>.lean` axiom-module writer (`render_interface_axiom_module`).
    /// The bundled examples' pinned interfaces are all *verified callees*
    /// (lakefile `require`, no sibling module), so the snapshot suites
    /// don't cover this path; this fixture does. It exercises the v2.27
    /// Track A branch (polymorphic State + accessor params).
    const LP_POOL_SPEC: &str = r#"spec LpPool
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"

  upstream {
    package      "spl-token"
    version      "4.0.3"
    binary_hash  "sha256:0000000000000000000000000000000000000000000000000000000000000000"
  }

  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures  state.from_balance + amount == old(state.from_balance)
  }
}

type Error | InvalidAmount
type State = { pool_balance : U64, lp_supply : U64 }
handler deposit (amount : U64) {
  modifies [pool_balance, lp_supply]
  requires amount > 0 else InvalidAmount
  effect { pool_balance += amount }
  call Token.transfer(
    amount = amount,
    state_binders { from_balance = state.pool_balance },
  )
}
"#;

    /// Regression gate for the sibling `<Iface>.lean` axiom-module
    /// renderer. The golden was captured from the v2.32 port, proven
    /// byte-identical to the (now-deleted) legacy
    /// `lean_gen::render_interface_axiom_module` before deletion.
    /// Regenerate intentionally if the renderer changes:
    /// `UPDATE_AXIOM_GOLDEN=1 cargo test axiom_module_matches_golden`.
    const TOKEN_AXIOM_GOLDEN: &str =
        include_str!("../../tests/fixtures/token_axiom_module.lean.golden");

    #[test]
    fn axiom_module_matches_golden() {
        let spec = crate::chumsky_adapter::parse_str(LP_POOL_SPEC).expect("parse LpPool spec");
        let iface = spec
            .interfaces
            .iter()
            .find(|i| i.name == "Token")
            .expect("Token interface present");

        let ported = render_interface_axiom_module(iface);

        if std::env::var("UPDATE_AXIOM_GOLDEN").is_ok() {
            std::fs::write(
                format!(
                    "{}/tests/fixtures/token_axiom_module.lean.golden",
                    std::env::var("CARGO_MANIFEST_DIR").unwrap()
                ),
                &ported,
            )
            .unwrap();
            return;
        }

        // Guard against a vacuous golden: the Track A path must fire.
        for marker in [
            "axiom ensures_axiom_0",
            "{State : Type} [Inhabited State]",
            "(pre post : State)",
            "(from_balance : State \u{2192} Nat)",
        ] {
            assert!(
                ported.contains(marker),
                "ported axiom module missing `{marker}`:\n{ported}"
            );
        }
        assert_eq!(
            TOKEN_AXIOM_GOLDEN, ported,
            "axiom-module renderer drifted from the golden — \
             regenerate with UPDATE_AXIOM_GOLDEN=1 if intentional"
        );
    }
}
