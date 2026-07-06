//! Lean codegen sidecar writer — the renderer-agnostic half of
//! `lean_gen_mir::generate`.
//!
//! `write_spec_with_sidecars` takes a rendered `Spec.lean` body and emits
//! it plus every pinned-interface sidecar artifact: sibling `<Iface>.lean`
//! axiom modules, injected `import <Iface>` lines, and the consumer
//! lakefile's `roots` / verified-callee `require` directives. Self-contained
//! (private leaf-helper copies), drift-gated by `axiom_module_matches_golden`.

use crate::check::ParsedSpec;
use crate::codegen_shared::write_generated_file;
use anyhow::Result;
use std::path::Path;

/// Write the rendered `Spec.lean` plus every pinned-interface sidecar
/// (sibling axiom module, lakefile roots update, verified-callee
/// `require` directives).
pub(crate) fn write_spec_with_sidecars(
    content: String,
    spec: &ParsedSpec,
    output_path: &Path,
) -> Result<()> {
    let pinned = collect_pinned_interfaces(spec);

    // Inject `import <Iface>` lines for every pinned interface right
    // after the existing import block (Lean requires imports before
    // `namespace`).
    let final_content = if !pinned.is_empty() {
        inject_interface_imports(&content, &pinned)
    } else {
        content
    };

    write_generated_file(output_path, &final_content)?;
    eprintln!("  wrote {}", output_path.display());

    // Sibling `<Iface>.lean` axiom modules. The pinned set is recomputed
    // here (render keeps a single-String signature); `render_cpi_theorems`
    // uses the same `handler_is_pinned` predicate so both sides agree on
    // which interfaces need axioms. Verified callees instead pull proof
    // modules via a lakefile `require` — no local sibling module, no
    // `roots` entry (the imported package owns those modules).
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
            write_generated_file(&iface_path, &module)?;
            eprintln!("  wrote {}", iface_path.display());
        }

        // Best-effort: lakefile may not exist yet (`qedgen init` ships
        // it); when present, the roots rewrite is idempotent.
        if !pinned.is_empty() {
            let lakefile_path = parent.join("lakefile.lean");
            if lakefile_path.exists() {
                // Strip stale sibling-module roots for callees that became
                // verified — their local `<Iface>.lean` is no longer
                // written, so a stale root would break `lake build`.
                // Narrow: only removes roots matching a verified callee.
                let verified_roots: Vec<String> = spec
                    .verified_callees
                    .keys()
                    .map(|n| safe_module_name(n))
                    .collect();
                if !verified_roots.is_empty() {
                    remove_lakefile_roots(&lakefile_path, &verified_roots)?;
                }
                update_lakefile_roots(&lakefile_path, &local_pinned)?;
                // Inject `require <pkg> from "<rel-path>"` per verified
                // callee, relative to the consumer's lakefile.
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

/// Idempotent injection of `require <pkg> from "<path>"` directives,
/// one per verified callee with a Lake-buildable proof package.
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
    // Insert right after `package <name>` (always present in
    // qedgen-emitted lakefiles); fall back to file-end.
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

/// Compute `target` relative to `base` (pure-string `std::path`
/// semantics). Falls back to an absolute path when no common prefix
/// exists, so the lakefile still compiles when the provider lives
/// outside the consumer's tree.
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
    // Insert after the last leading `import` line; if none (sBPF mode,
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

/// Collect interfaces referenced by `call Interface.handler(...)` sites
/// that meet both pinning requirements (binary_hash + non-empty `ensures`).
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

/// Sanitize an interface name into a valid Lean module identifier (used
/// in both the `import` line and the lakefile `roots` list).
fn safe_module_name(name: &str) -> String {
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

/// Lake package name for a verified-callee's proof package: lowercase
/// first char + `Proofs` (`Token` → `tokenProofs`).
///
/// `pub(crate)`: `check.rs` reuses it so lint diagnostics report the
/// same package name this module emits into the lakefile.
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
            // Abstract State-field refs (`s.X` / `s'.X`, from the
            // `Ctx::Ensures` lowering of `state.X`).
            let abstract_fields = scan_abstract_fields(&ensures.lean_expr);
            out.push_str(&format!(
                "/-- `{}.{}` post-condition #{} (axiomatized; discharged by binary_hash pin). -/\n",
                iface.name, handler.name, ens_idx,
            ));
            if abstract_fields.is_empty() {
                // Callee-frame, param-only.
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
                // Caller-State-aware path.
                let mut sig = String::new();
                sig.push_str(" {State : Type} [Inhabited State]");
                sig.push_str(" (pre post : State)");
                sig.push_str(&params_sig);
                for field in &abstract_fields {
                    let codomain = iface
                        .state_fields
                        .iter()
                        .find(|(n, _)| n == field)
                        .map(|(_, t)| map_dsl_ty(t.as_str()))
                        .unwrap_or_else(|| "Nat".to_string());
                    sig.push_str(&format!(" ({} : State \u{2192} {})", field, codomain));
                }
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

/// Scan a callee's Lean-rendered `ensures` text for abstract State-field
/// references (`s.X` / `s'.X`); names returned in first-occurrence order.
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

/// Rewrite a callee's Lean `ensures` into the abstract-accessor form used
/// inside the axiom body: `s'.X` → `(X post)`, `s.X` → `(X pre)`.
fn rewrite_axiom_body_to_accessors(ensures_lean: &str) -> String {
    // `s'.X` must rewrite first so the pre-pass can't match its `s` half.
    let re_post = regex::Regex::new(r"\bs'\.([A-Za-z_][A-Za-z0-9_]*)")
        .expect("regex compiles for post-state accessor rewrite");
    let after_post = re_post.replace_all(ensures_lean, "($1 post)").into_owned();
    let re_pre = regex::Regex::new(r"\bs\.([A-Za-z_][A-Za-z0-9_]*)")
        .expect("regex compiles for pre-state accessor rewrite");
    re_pre.replace_all(&after_post, "($1 pre)").into_owned()
}

/// Strip the named modules from a lakefile's `roots := #[...]` array.
/// Idempotent counterpart to `update_lakefile_roots`.
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
// Leaf helpers — shared with `lean_gen_mir` via `crate::lean_names`.
// ----------------------------------------------------------------------

use crate::lean_names::{map_dsl_ty, safe_name};

/// True iff an interface handler is Tier-1/2 pinned: it declares
/// `ensures` AND its interface carries a non-empty `binary_hash`
/// (parse-layer twin of `lean_gen_mir::handler_is_pinned_mir` — the
/// pin predicate itself is `lean_names::binary_hash_is_pinned`).
fn handler_is_pinned(
    iface: &crate::check::ParsedInterface,
    handler: &crate::check::ParsedInterfaceHandler,
) -> bool {
    !handler.ensures.is_empty()
        && crate::lean_names::binary_hash_is_pinned(
            iface
                .upstream
                .as_ref()
                .and_then(|u| u.binary_hash.as_deref()),
        )
}

/// Build a parameter signature string (` (n : T)` per param) for axiom
/// statements.
fn param_sig_str(params: &[(String, String)]) -> String {
    crate::lean_names::param_sig_str_with(params, |t| map_dsl_ty(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned-but-unverified interface (`upstream { binary_hash }` +
    /// `ensures` over `state.X`) — the only shape that drives the sibling
    /// `<Iface>.lean` axiom-module writer. Bundled examples' pinned
    /// interfaces are all *verified callees* (lakefile `require`, no
    /// sibling module), so the snapshot suites don't cover this path;
    /// this fixture exercises the polymorphic-State + accessor branch.
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

    /// Regression gate for the sibling axiom-module renderer. Regenerate
    /// intentionally: `UPDATE_AXIOM_GOLDEN=1 cargo test axiom_module_matches_golden`.
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
