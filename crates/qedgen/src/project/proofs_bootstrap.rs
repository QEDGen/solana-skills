//! Bootstrap a skeleton `Proofs.lean` (once) and check for orphan/missing
//! theorems on every `qedgen check`.
//!
//! `Spec.lean` is regenerated each run; `Proofs.lean` is user-owned. They
//! link via theorem names: a dropped handler orphans its theorem, a new
//! `preserved_by` entry makes one missing — both are check-time diagnostics.

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
    re.captures_iter(source).map(|c| c[1].to_string()).collect()
}

/// Render the bootstrap `Proofs.lean` body: `import Spec`, `open` clauses,
/// and a commented checklist of expected obligations. Intentionally no
/// `theorem X : True := by trivial` stubs — they type-check but prove
/// nothing, and a Proofs.lean full of them reads as "everything is proven".
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
    /// `Proofs.lean` carries preservation theorems, but NONE match this
    /// spec's obligations — it was generated from a *different* spec (a
    /// leftover in a reused workspace, the #166 repro). One informational
    /// note replaces the full orphan+missing noise, and it does not fail
    /// the check: a Kani-first workflow may legitimately never regenerate
    /// Lean. `declared`/`expected` are the disjoint counts.
    ForeignProofs {
        declared: usize,
        expected: usize,
    },
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
            OrphanFinding::ForeignProofs { declared, expected } => write!(
                f,
                "Proofs.lean holds {} preservation theorem(s), none matching this \
                 spec's {} obligation(s) — it was generated from a different spec. \
                 Regenerate with `qedgen codegen --lean`, or point --proofs at the \
                 right directory. (Informational — not a failure; Kani-only \
                 workflows can ignore it.)",
                declared, expected
            ),
        }
    }
}

/// Compare the spec's expected obligations against the theorems in
/// `Proofs.lean`. Only `<property>_preserved_by_<handler>`-shaped names are
/// checked — helper lemmas never trigger false orphans.
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

    // Foreign-proofs gate (#166): preservation theorems exist on BOTH sides
    // with ZERO overlap → this Proofs.lean belongs to a different spec (a
    // stale leftover in a reused workspace). Emitting the full orphan+missing
    // list would be pure noise — every theorem lands on both lists — so
    // collapse it to one informational note. Same-spec evolution (any
    // overlap at all, or an empty side) keeps the precise drift findings.
    let declared_preservation: Vec<&String> = declared.iter().filter(|t| pat.is_match(t)).collect();
    if !declared_preservation.is_empty()
        && !expected.is_empty()
        && !declared_preservation.iter().any(|t| expected.contains(*t))
    {
        return Ok(vec![OrphanFinding::ForeignProofs {
            declared: declared_preservation.len(),
            expected: expected.len(),
        }]);
    }

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

    fn spec_with_obligation(prop: &str, handler: &str) -> ParsedSpec {
        let mut spec = ParsedSpec::default();
        spec.properties.push(crate::check::ParsedProperty {
            name: prop.to_string(),
            expression: None,
            rust_expression: None,
            rust_expression_pod: None,
            rust_expression_math: None,
            preserved_by: vec![handler.to_string()],
            per_slot: None,
            quantifier_lint: None,
            class: crate::check::PropertyClass::Unary,
            ast_body: None,
            tree: None,
        });
        spec
    }

    fn push_obligation(spec: &mut ParsedSpec, prop: &str, handler: &str) {
        let extra = spec_with_obligation(prop, handler)
            .properties
            .pop()
            .unwrap();
        spec.properties.push(extra);
    }

    /// #166: a Proofs.lean whose preservation theorems share ZERO overlap
    /// with the spec's obligations is a leftover from a DIFFERENT spec —
    /// one informational `ForeignProofs` note, not the full orphan+missing
    /// noise (which would name every theorem twice).
    #[test]
    fn foreign_proofs_collapse_to_one_note() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Proofs.lean"),
            "theorem old_prop_preserved_by_old_handler : True := by trivial\n",
        )
        .unwrap();
        let spec = spec_with_obligation("solvency", "deposit");
        let findings = check_orphans(&spec, dir.path()).unwrap();
        assert_eq!(
            findings,
            vec![OrphanFinding::ForeignProofs {
                declared: 1,
                expected: 1
            }],
            "disjoint theorem sets collapse to one foreign-proofs note"
        );
    }

    /// Same-spec evolution — ANY overlap — keeps the precise per-theorem
    /// drift findings (the foreign gate must not swallow real drift).
    #[test]
    fn partial_overlap_keeps_precise_drift() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Proofs.lean"),
            "theorem solvency_preserved_by_deposit : True := by trivial\n\
             theorem stale_preserved_by_removed : True := by trivial\n",
        )
        .unwrap();
        let mut spec = spec_with_obligation("solvency", "deposit");
        push_obligation(&mut spec, "conservation", "withdraw");
        let findings = check_orphans(&spec, dir.path()).unwrap();
        assert!(
            findings.contains(&OrphanFinding::Orphan(
                "stale_preserved_by_removed".to_string()
            )) && findings.contains(&OrphanFinding::Missing(
                "conservation_preserved_by_withdraw".to_string()
            )),
            "overlap keeps per-theorem orphan+missing findings; got {findings:?}"
        );
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
