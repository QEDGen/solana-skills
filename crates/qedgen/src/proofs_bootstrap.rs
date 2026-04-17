//! Bootstrap a skeleton `Proofs.lean` (once) and check for orphan/missing
//! theorems on every `qedgen check`.
//!
//! `Spec.lean` is regenerated from the `.qedspec` every run. `Proofs.lean`
//! is user-owned — it holds preservation theorems with user-written tactic
//! scripts. The two talk to each other through theorem names: if the spec
//! drops a handler, the theorem referencing it goes stale; if the spec
//! adds a handler to `preserved_by`, a theorem is missing. Both surface
//! as check-time diagnostics.

use anyhow::Result;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;

use crate::check::ParsedSpec;

/// The set of preservation theorems the spec currently expects.
/// Format matches the historical `<property>_preserved_by_<handler>`.
pub fn expected_theorems(spec: &ParsedSpec) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for prop in &spec.properties {
        for handler in &prop.preserved_by {
            set.insert(format!("{}_preserved_by_{}", prop.name, handler));
        }
    }
    set
}

/// Extract every top-level `theorem <name>` identifier from a Lean source
/// file. Regex-only — we don't need syntactic parsing for this check.
pub fn extract_theorem_names(source: &str) -> BTreeSet<String> {
    let re = Regex::new(r"(?m)^\s*theorem\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    re.captures_iter(source)
        .map(|c| c[1].to_string())
        .collect()
}

/// Render the bootstrap `Proofs.lean` body (once, when the file is absent).
/// Emits `import Spec`, minimal `open` clauses, and a commented checklist
/// of the preservation obligations the spec expects. The user materializes
/// each theorem against the real signature in `Spec.lean`.
///
/// Intentionally does NOT emit `theorem X : True := by trivial` stubs —
/// those type-check but prove nothing meaningful, and a skimmed `Proofs.lean`
/// full of them reads as "everything is proven" when it isn't. Better to
/// force the user to write the real signature from the start.
pub fn render_bootstrap(spec: &ParsedSpec) -> String {
    let mut out = String::new();
    out.push_str("/-\n");
    out.push_str("Proofs.lean — user-owned preservation proofs.\n");
    out.push('\n');
    out.push_str("`qedgen codegen` bootstraps this file once and never touches it again.\n");
    out.push_str("Spec.lean is regenerated; this file is durable. `qedgen check`\n");
    out.push_str("(and `qedgen reconcile`) flag orphan theorems (handler removed from\n");
    out.push_str("spec) and missing obligations (new `preserved_by` declared).\n");
    out.push_str("-/\n");
    out.push_str("import Spec\n\n");
    out.push_str(&format!("namespace {}\n\n", spec.program_name));
    out.push_str("open QEDGen.Solana\n\n");

    let theorems = expected_theorems(spec);
    if theorems.is_empty() {
        out.push_str("-- No preservation obligations declared by the spec.\n");
        out.push_str("-- Add `property <name> preserved_by [...]` blocks to the `.qedspec`\n");
        out.push_str("-- and `qedgen check` will list the new obligations here.\n");
    } else {
        out.push_str("-- Preservation obligations the spec expects.\n");
        out.push_str("-- Write each theorem against the signature generated in Spec.lean\n");
        out.push_str("-- (the handler's transition + the property predicate). Close with\n");
        out.push_str("-- tactics like `unfold`, `omega`, or `simp_all` as appropriate, or\n");
        out.push_str("-- `QEDGen.Solana.IndexedState.forall_update_pres` for per-account\n");
        out.push_str("-- invariants in Map-backed specs.\n");
        out.push_str("--\n");
        for name in &theorems {
            out.push_str(&format!("--   theorem {}\n", name));
        }
    }

    out.push_str(&format!("\nend {}\n", spec.program_name));
    out
}

/// Bootstrap `Proofs.lean` if absent. Never overwrites an existing file.
/// Returns `true` if a new file was written.
pub fn bootstrap_if_missing(spec: &ParsedSpec, proofs_dir: &Path) -> Result<bool> {
    let path = proofs_dir.join("Proofs.lean");
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(proofs_dir)?;
    std::fs::write(&path, render_bootstrap(spec))?;
    eprintln!("Bootstrapped {}", path.display());
    Ok(true)
}

/// One orphan/missing theorem diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanFinding {
    Orphan(String),
    Missing(String),
}

impl std::fmt::Display for OrphanFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrphanFinding::Orphan(name) => write!(
                f,
                "orphan theorem `{}` in Proofs.lean — no matching handler in spec",
                name
            ),
            OrphanFinding::Missing(name) => write!(
                f,
                "missing theorem `{}` — spec declares this obligation; add a stub:\n  theorem {} ... := by sorry",
                name, name
            ),
        }
    }
}

/// Compare the spec's expected obligation set against the theorems declared
/// in `Proofs.lean`. Returns a list of orphans + missing obligations.
///
/// Theorems declared in Proofs.lean that follow the
/// `<property>_preserved_by_<handler>` convention are checked. Theorems
/// that don't match that pattern are ignored — users are free to add
/// helper lemmas in Proofs.lean without triggering false orphans.
pub fn check_orphans(spec: &ParsedSpec, proofs_dir: &Path) -> Result<Vec<OrphanFinding>> {
    let path = proofs_dir.join("Proofs.lean");
    if !path.exists() {
        // No Proofs.lean yet — all obligations are missing.
        return Ok(expected_theorems(spec)
            .into_iter()
            .map(OrphanFinding::Missing)
            .collect());
    }

    let source = std::fs::read_to_string(&path)?;
    let declared = extract_theorem_names(&source);
    let expected = expected_theorems(spec);

    let pat = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*_preserved_by_[A-Za-z_][A-Za-z0-9_]*$").unwrap();
    let mut findings = Vec::new();

    // Orphans: preservation-shaped theorems in Proofs.lean the spec doesn't
    // ask for. Non-preservation helper lemmas are ignored.
    for thm in &declared {
        if pat.is_match(thm) && !expected.contains(thm) {
            findings.push(OrphanFinding::Orphan(thm.clone()));
        }
    }

    // Missing: obligations the spec declares but Proofs.lean doesn't carry.
    for thm in &expected {
        if !declared.contains(thm) {
            findings.push(OrphanFinding::Missing(thm.clone()));
        }
    }

    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_names_finds_all() {
        let src = r#"
import Spec
namespace Foo

theorem a_preserved_by_x : True := by trivial

theorem b_preserved_by_y : True := by trivial

-- a comment
end Foo
"#;
        let names = extract_theorem_names(src);
        assert!(names.contains("a_preserved_by_x"));
        assert!(names.contains("b_preserved_by_y"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn extract_ignores_nontheorem_lines() {
        let src = r#"
-- theorem commented_out : True := by trivial
def not_a_theorem := 1
theorem real_one : True := by trivial
"#;
        let names = extract_theorem_names(src);
        assert!(names.contains("real_one"));
        assert!(!names.contains("commented_out"));
        assert_eq!(names.len(), 1);
    }
}
