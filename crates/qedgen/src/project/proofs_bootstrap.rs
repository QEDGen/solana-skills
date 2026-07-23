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

/// Extract the base names of the machine-owned obligation statements a
/// generated `Spec.lean` carries: `def <base>_stmt : Prop` → `<base>`.
/// Artifact-derived on purpose — reading the generated file cannot drift
/// from the emitter the way a second copy of its naming logic could.
pub fn extract_stmt_defs(source: &str) -> BTreeSet<String> {
    let re = Regex::new(r"(?m)^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)_stmt\s*:\s*Prop\b").unwrap();
    re.captures_iter(source).map(|c| c[1].to_string()).collect()
}

/// Does `source` declare `theorem <name>` typed directly against its
/// machine-owned statement — `theorem <name> : [<Ns>.]<name>_stmt`?
/// Any other form (inline binders, a hand-restated Prop) is a restatement:
/// it elaborates fine, but the statement is no longer machine-checked.
fn theorem_types_against_stmt(source: &str, name: &str) -> bool {
    let re = Regex::new(&format!(
        r"theorem\s+{name}\s*:\s*(?:[A-Za-z_][A-Za-z0-9_]*\.)*{name}_stmt\b",
        name = regex::escape(name)
    ))
    .unwrap();
    re.is_match(source)
}

/// Render the bootstrap `Proofs.lean` body: `import Spec`, `open` clauses,
/// and a commented checklist of expected obligations. `stmt_bases` is the
/// set of machine-owned `_stmt` obligations the sibling `Spec.lean` carries
/// (empty for flat shapes) — those checklist lines show the typed form and
/// the full stmt inventory is listed, not just preservation. Intentionally no
/// `theorem X : True := by trivial` stubs — they type-check but prove
/// nothing, and a Proofs.lean full of them reads as "everything is proven".
pub fn render_bootstrap(spec: &ParsedSpec, stmt_bases: &BTreeSet<String>) -> String {
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

    // Union: spec-declared preservation obligations + every machine-owned
    // statement Spec.lean carries. Names with a `_stmt` render the typed
    // form so the statement stays machine-checked.
    let mut checklist = expected_theorems(spec);
    checklist.extend(stmt_bases.iter().cloned());
    if checklist.is_empty() {
        out.push_str("-- No preservation obligations declared by the spec.\n");
        out.push_str("-- Add `property <name> preserved_by [...]` blocks to the `.qedspec`\n");
        out.push_str("-- and `qedgen check` will list the new obligations here.\n");
    } else {
        out.push_str("-- Obligations the spec expects.\n");
        if stmt_bases.is_empty() {
            out.push_str("-- Write each theorem against the signature generated in Spec.lean\n");
            out.push_str("-- (the handler's transition + the property predicate). Close with\n");
        } else {
            out.push_str("-- Spec.lean owns each statement as `def <name>_stmt : Prop`;\n");
            out.push_str("-- type the theorem against it (`theorem <name> : <name>_stmt`)\n");
            out.push_str("-- so the statement cannot drift from the spec. Close with\n");
        }
        out.push_str("-- tactics like `unfold`, `omega`, or `simp_all` as appropriate, or\n");
        out.push_str("-- `QEDGen.Solana.IndexedState.forall_update_pres` for per-account\n");
        out.push_str("-- invariants in Map-backed specs.\n");
        out.push_str("--\n");
        for name in &checklist {
            if stmt_bases.contains(name) {
                out.push_str(&format!(
                    "--   theorem {} : {}_stmt := by sorry\n",
                    name, name
                ));
            } else {
                out.push_str(&format!("--   theorem {}\n", name));
            }
        }
    }

    out.push_str(&format!("\nend {}\n", spec.program_name));
    out
}

/// Bootstrap `Proofs.lean` if absent. Never overwrites an existing file.
/// Reads the sibling generated `Spec.lean` (written just before this in the
/// codegen sequence) for machine-owned `_stmt` obligations so the checklist
/// suggests the typed form. Returns `true` if a new file was written.
pub fn bootstrap_if_missing(spec: &ParsedSpec, proofs_dir: &Path) -> Result<bool> {
    let path = proofs_dir.join("Proofs.lean");
    if path.exists() {
        return Ok(false);
    }
    let stmt_bases = match std::fs::read_to_string(proofs_dir.join("Spec.lean")) {
        Ok(source) => extract_stmt_defs(&source),
        Err(_) => BTreeSet::new(),
    };
    std::fs::create_dir_all(proofs_dir)?;
    std::fs::write(&path, render_bootstrap(spec, &stmt_bases))?;
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
    /// `Spec.lean` carries a machine-owned `def <name>_stmt : Prop` for this
    /// obligation, but the `Proofs.lean` theorem restates the obligation
    /// instead of typing against it (#336 follow-up). Informational, like
    /// `ForeignProofs`: the restated theorem is still a valid proof, but its
    /// statement can drift from the spec without any gate noticing.
    RestatedStatement {
        theorem: String,
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
            OrphanFinding::RestatedStatement { theorem } => write!(
                f,
                "theorem `{}` restates its obligation — Spec.lean owns the \
                 statement as `{}_stmt`. Retype it as\n  theorem {} : {}_stmt := by intro …\n\
                 so the statement stays machine-checked. (Informational — the \
                 proof is still valid, but its statement can drift from the spec.)",
                theorem, theorem, theorem, theorem
            ),
        }
    }
}

/// Compare the spec's expected obligations against the theorems in
/// `Proofs.lean`. Only `<property>_preserved_by_<handler>`-shaped names are
/// checked for orphan/missing — helper lemmas never trigger false orphans.
/// When the sibling `Spec.lean` carries machine-owned `_stmt` statements,
/// declared theorems that restate instead of typing against them get an
/// informational nudge (#349).
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

    // Restated-statement nudge (#349, the #336 follow-up): when the sibling
    // generated Spec.lean carries machine-owned `def <name>_stmt : Prop`
    // obligations, a declared theorem of the same name should type against
    // its `_stmt` — any other typing re-opens statement drift at the proof
    // site. Covers the full stmt inventory (preservation, aborts, ensures,
    // covers, liveness, environments), not just `expected_theorems`.
    let spec_lean = proofs_dir.join("Spec.lean");
    if spec_lean.exists() {
        let spec_source = std::fs::read_to_string(&spec_lean)?;
        for base in extract_stmt_defs(&spec_source) {
            if declared.contains(&base) && !theorem_types_against_stmt(&source, &base) {
                findings.push(OrphanFinding::RestatedStatement { theorem: base });
            }
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
    fn extract_stmt_defs_finds_bases() {
        let src = r#"
def threshold_bounded (s : State) : Prop := s.threshold > 0
def solvency_preserved_by_deposit_stmt : Prop :=
  ∀ (s s' : State), solvency s → depositTransition s = some s' → solvency s'
def cover_can_execute_stmt : Prop := ∃ s, True
-- def commented_out_stmt : Prop := True
"#;
        let bases = extract_stmt_defs(src);
        assert!(bases.contains("solvency_preserved_by_deposit"));
        assert!(bases.contains("cover_can_execute"));
        assert!(!bases.contains("commented_out"));
        assert!(
            !bases.contains("threshold_bounded"),
            "plain predicate defs are not statements"
        );
        assert_eq!(bases.len(), 2);
    }

    /// #349: a declared theorem that restates its obligation inline while
    /// Spec.lean owns the statement as `_stmt` gets an informational nudge.
    /// Covers the full stmt inventory — the cover obligation is not in
    /// `expected_theorems` and still nudges.
    #[test]
    fn restated_theorems_get_stmt_nudge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Spec.lean"),
            "def solvency_preserved_by_deposit_stmt : Prop := True\n\
             def cover_can_execute_stmt : Prop := True\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Proofs.lean"),
            "theorem solvency_preserved_by_deposit\n\
             \x20   (s s' : State) (h : depositTransition s = some s') :\n\
             \x20   solvency s' := by sorry\n\
             theorem cover_can_execute : ∃ s, True := by sorry\n",
        )
        .unwrap();
        let spec = spec_with_obligation("solvency", "deposit");
        let findings = check_orphans(&spec, dir.path()).unwrap();
        assert!(findings.contains(&OrphanFinding::RestatedStatement {
            theorem: "solvency_preserved_by_deposit".to_string()
        }));
        assert!(
            findings.contains(&OrphanFinding::RestatedStatement {
                theorem: "cover_can_execute".to_string()
            }),
            "non-preservation stmt obligations nudge too; got {findings:?}"
        );
    }

    /// The blessed form — `theorem X : X_stmt` (bare or namespace-qualified)
    /// — produces no nudge.
    #[test]
    fn stmt_typed_theorems_do_not_nudge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Spec.lean"),
            "def solvency_preserved_by_deposit_stmt : Prop := True\n\
             def cover_can_execute_stmt : Prop := True\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Proofs.lean"),
            "theorem solvency_preserved_by_deposit :\n\
             \x20   solvency_preserved_by_deposit_stmt := by sorry\n\
             theorem cover_can_execute : Vault.cover_can_execute_stmt := by sorry\n",
        )
        .unwrap();
        let spec = spec_with_obligation("solvency", "deposit");
        let findings = check_orphans(&spec, dir.path()).unwrap();
        assert!(
            !findings
                .iter()
                .any(|f| matches!(f, OrphanFinding::RestatedStatement { .. })),
            "stmt-typed theorems must not nudge; got {findings:?}"
        );
    }

    /// Flat shapes: no Spec.lean statements → no nudge, whatever the
    /// theorem's typing.
    #[test]
    fn no_stmt_defs_no_nudge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Spec.lean"),
            "def solvency (s : State) : Prop := True\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Proofs.lean"),
            "theorem solvency_preserved_by_deposit : True := by trivial\n",
        )
        .unwrap();
        let spec = spec_with_obligation("solvency", "deposit");
        let findings = check_orphans(&spec, dir.path()).unwrap();
        assert!(findings.is_empty(), "got {findings:?}");
    }

    /// Bootstrap checklist shows the typed form for stmt-backed obligations
    /// and lists the full stmt inventory, not just preservation.
    #[test]
    fn bootstrap_checklist_uses_stmt_form() {
        let spec = spec_with_obligation("solvency", "deposit");
        let stmts: BTreeSet<String> = ["solvency_preserved_by_deposit", "cover_can_execute"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_bootstrap(&spec, &stmts);
        assert!(out.contains(
            "--   theorem solvency_preserved_by_deposit : solvency_preserved_by_deposit_stmt := by sorry"
        ));
        assert!(out.contains("--   theorem cover_can_execute : cover_can_execute_stmt := by sorry"));

        // Flat shape unchanged: bare checklist line.
        let flat = render_bootstrap(&spec, &BTreeSet::new());
        assert!(flat.contains("--   theorem solvency_preserved_by_deposit\n"));
        assert!(!flat.contains("_stmt"));
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
