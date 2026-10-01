//! Axiom policy gate for the `lean` backend's `#print axioms` report.
//!
//! The permitted set is derived from the spec, never declared by it:
//!   * the classical trio (`propext`, `Classical.choice`, `Quot.sound`);
//!   * every `<Iface>.<handler>.ensures_axiom_<idx>` the sibling axiom
//!     modules declare for a pinned interface
//!     (`lean_sidecars::declared_ensures_axioms`).
//!
//! `QEDGen.Solana` contributes no permitted axioms: the support library's
//! remaining axioms are unused or are being removed, so a proof that
//! depends on one is reported. `sorryAx`, `Lean.ofReduceBool`, and
//! `Lean.trustCompiler` are never permitted.
//!
//! Every other axiom is a CRIT finding. `verify --strict` fails on any;
//! without `--strict` they print and the run passes.

use serde::Serialize;
use std::collections::BTreeSet;

use super::{AxiomDependency, BackendReport};

/// Lean's classical-logic axioms. Every Mathlib proof pulls them in.
pub(crate) const CLASSICAL_AXIOMS: &[&str] = &["propext", "Classical.choice", "Quot.sound"];

/// Why an axiom is not permitted. Closed enum: counted through
/// `AxiomCounts::of` by exhaustive match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForbiddenReason {
    /// `sorryAx`: the proof is incomplete.
    Sorry,
    /// `Lean.ofReduceBool` / `Lean.trustCompiler`, or a per-use
    /// `<decl>._native.native_decide.ax_*` axiom (Lean v4.30 emits one per
    /// `native_decide`): the proof trusts the Lean compiler, not only the
    /// kernel.
    CompilerTrust,
    /// Any axiom the spec does not declare through a pinned interface.
    Undeclared,
}

impl ForbiddenReason {
    pub fn describe(self) -> &'static str {
        match self {
            ForbiddenReason::Sorry => "incomplete proof (sorry)",
            ForbiddenReason::CompilerTrust => "trusts the Lean compiler (native_decide)",
            ForbiddenReason::Undeclared => "not declared by a pinned interface",
        }
    }
}

/// Name component of the axiom Lean adds for each `native_decide` use.
const NATIVE_DECIDE_AXIOM_MARKER: &str = "._native.native_decide.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxiomVerdict {
    Permitted,
    Forbidden(ForbiddenReason),
}

/// One axiom outside the permitted set, attached to its theorem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForbiddenAxiom {
    pub axiom: String,
    pub reason: ForbiddenReason,
}

#[derive(Debug, Clone, Default)]
pub struct AxiomPolicy {
    declared: BTreeSet<String>,
}

impl AxiomPolicy {
    pub fn for_spec(spec: &crate::check::ParsedSpec) -> Self {
        Self::from_declared(crate::lean_sidecars::declared_ensures_axioms(spec))
    }

    fn from_declared<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        AxiomPolicy {
            declared: names.into_iter().map(|n| normalize(n.as_ref())).collect(),
        }
    }

    pub fn classify(&self, axiom: &str) -> AxiomVerdict {
        let name = normalize(axiom);
        match name.as_str() {
            "sorryAx" => AxiomVerdict::Forbidden(ForbiddenReason::Sorry),
            "Lean.ofReduceBool" | "Lean.trustCompiler" => {
                AxiomVerdict::Forbidden(ForbiddenReason::CompilerTrust)
            }
            n if n.contains(NATIVE_DECIDE_AXIOM_MARKER) => {
                AxiomVerdict::Forbidden(ForbiddenReason::CompilerTrust)
            }
            n if CLASSICAL_AXIOMS.contains(&n) || self.declared.contains(n) => {
                AxiomVerdict::Permitted
            }
            _ => AxiomVerdict::Forbidden(ForbiddenReason::Undeclared),
        }
    }
}

/// Lean prints reserved-word components as `«name»`; codegen may or may
/// not quote them. Compare without the guillemets.
fn normalize(name: &str) -> String {
    name.chars().filter(|c| *c != '«' && *c != '»').collect()
}

/// Classify every axiom in every backend's axiom report and record the
/// forbidden ones on their `AxiomDependency`.
pub fn apply(backends: &mut [BackendReport], policy: &AxiomPolicy) {
    for backend in backends {
        for dep in &mut backend.axioms {
            dep.forbidden = forbidden_in(dep, policy);
        }
    }
}

fn forbidden_in(dep: &AxiomDependency, policy: &AxiomPolicy) -> Vec<ForbiddenAxiom> {
    let mut out = Vec::new();
    for axiom in &dep.axioms {
        match policy.classify(axiom) {
            AxiomVerdict::Permitted => {}
            AxiomVerdict::Forbidden(reason) => out.push(ForbiddenAxiom {
                axiom: axiom.clone(),
                reason,
            }),
        }
    }
    out
}

/// Tally of classified axiom uses (one per theorem × axiom).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AxiomCounts {
    pub permitted: usize,
    pub sorry: usize,
    pub compiler_trust: usize,
    pub undeclared: usize,
}

impl AxiomCounts {
    pub fn of(backends: &[BackendReport], policy: &AxiomPolicy) -> Self {
        let mut counts = Self::default();
        for dep in backends.iter().flat_map(|b| &b.axioms) {
            for axiom in &dep.axioms {
                match policy.classify(axiom) {
                    AxiomVerdict::Permitted => counts.permitted += 1,
                    AxiomVerdict::Forbidden(ForbiddenReason::Sorry) => counts.sorry += 1,
                    AxiomVerdict::Forbidden(ForbiddenReason::CompilerTrust) => {
                        counts.compiler_trust += 1
                    }
                    AxiomVerdict::Forbidden(ForbiddenReason::Undeclared) => counts.undeclared += 1,
                }
            }
        }
        counts
    }

    pub fn forbidden(&self) -> usize {
        self.sorry + self.compiler_trust + self.undeclared
    }

    /// `verify --strict` fails on any forbidden axiom use.
    pub fn gates_strict(&self) -> bool {
        self.forbidden() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{BackendReport, BackendStatus};

    fn policy_with(declared: &[&str]) -> AxiomPolicy {
        AxiomPolicy::from_declared(declared)
    }

    #[test]
    fn classifier_permits_classical_trio_and_declared_ensures_axioms() {
        let p = policy_with(&["Token.transfer.ensures_axiom_1"]);
        for a in CLASSICAL_AXIOMS {
            assert_eq!(p.classify(a), AxiomVerdict::Permitted, "{a}");
        }
        assert_eq!(
            p.classify("Token.transfer.ensures_axiom_1"),
            AxiomVerdict::Permitted
        );
    }

    #[test]
    fn classifier_rejects_sorry_compiler_trust_and_unknown() {
        let p = policy_with(&["Token.transfer.ensures_axiom_1"]);
        assert_eq!(
            p.classify("sorryAx"),
            AxiomVerdict::Forbidden(ForbiddenReason::Sorry)
        );
        assert_eq!(
            p.classify("Lean.ofReduceBool"),
            AxiomVerdict::Forbidden(ForbiddenReason::CompilerTrust)
        );
        assert_eq!(
            p.classify("Lean.trustCompiler"),
            AxiomVerdict::Forbidden(ForbiddenReason::CompilerTrust)
        );
        // Lean v4.30 names one axiom per `native_decide` use.
        assert_eq!(
            p.classify("CounterProg.insn_0._native.native_decide.ax_1_1"),
            AxiomVerdict::Forbidden(ForbiddenReason::CompilerTrust)
        );
        // An unpinned callee, or a pinned callee's undeclared index.
        assert_eq!(
            p.classify("Token.transfer.ensures_axiom_7"),
            AxiomVerdict::Forbidden(ForbiddenReason::Undeclared)
        );
        assert_eq!(
            p.classify("QEDGen.Solana.Account.find_map_update_same"),
            AxiomVerdict::Forbidden(ForbiddenReason::Undeclared)
        );
    }

    #[test]
    fn classifier_ignores_guillemet_quoting() {
        let p = policy_with(&["Pool.«initialize».ensures_axiom_0"]);
        assert_eq!(
            p.classify("Pool.initialize.ensures_axiom_0"),
            AxiomVerdict::Permitted
        );
    }

    #[test]
    fn apply_records_forbidden_axioms_and_counts_gate_strict() {
        let p = policy_with(&["Token.transfer.ensures_axiom_1"]);
        let mut backends = vec![BackendReport {
            name: "lean",
            status: BackendStatus::Passed,
            duration_ms: 0,
            detail: None,
            log_path: None,
            counterexamples: Vec::new(),
            axioms: vec![
                AxiomDependency {
                    theorem: "P.ok".into(),
                    axioms: vec!["Token.transfer.ensures_axiom_1".into()],
                    forbidden: Vec::new(),
                },
                AxiomDependency {
                    theorem: "P.bad".into(),
                    axioms: vec!["sorryAx".into(), "Lean.ofReduceBool".into()],
                    forbidden: Vec::new(),
                },
            ],
        }];
        apply(&mut backends, &p);
        assert!(backends[0].axioms[0].forbidden.is_empty());
        assert_eq!(
            backends[0].axioms[1].forbidden,
            vec![
                ForbiddenAxiom {
                    axiom: "sorryAx".into(),
                    reason: ForbiddenReason::Sorry
                },
                ForbiddenAxiom {
                    axiom: "Lean.ofReduceBool".into(),
                    reason: ForbiddenReason::CompilerTrust
                },
            ]
        );
        let counts = AxiomCounts::of(&backends, &p);
        assert_eq!(
            counts,
            AxiomCounts {
                permitted: 1,
                sorry: 1,
                compiler_trust: 1,
                undeclared: 0
            }
        );
        assert!(counts.gates_strict());

        let clean = AxiomCounts::of(&backends[..0], &p);
        assert!(!clean.gates_strict());
    }
}
