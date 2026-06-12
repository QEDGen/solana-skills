//! Shared structured counterexample types.
//!
//! PLAN-v2.16 D1/D2: both `qedgen verify --proptest` and `qedgen verify
//! --kani` parse their backend's counterexample output into a uniform
//! `(harness, var, value, line)` tuple shape. This module owns the
//! shared types; each backend has its own parser
//! (`verify_proptest_parse.rs`, future `verify_kani_parse.rs`) that
//! produces them.
//!
//! Why uniform: downstream consumers (the auditor subagent, JSON
//! consumers, the future `qedgen verify --probe-repros` gating) treat
//! "a counterexample is a list of (var, value) assignments tied to a
//! harness, with optional line numbers from CBMC traces" as the
//! canonical model. Both proptest and Kani fit this — proptest just
//! omits per-var line numbers (it has a single panic location).

use serde::Serialize;

/// A single concrete assignment Kani / proptest produced as a
/// counterexample. `line` is the source line where the variable was
/// constrained (Kani CBMC traces carry this; proptest doesn't, so
/// `None` is the proptest-side norm).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CounterexampleVar {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// One failed harness's counterexample. A backend run can produce
/// multiple `Counterexample`s (one per failing `#[test]` / `#[kani::proof]`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Counterexample {
    /// Harness / test function name (e.g. `deposit_preserves_pool_solvency`).
    pub harness: String,
    /// Always `"failed"` for now; reserved for future "verified"
    /// or "timeout" promotion if D1's structured output extends.
    pub status: String,
    /// Concrete inputs that triggered the failure.
    pub assignments: Vec<CounterexampleVar>,
    /// Proptest seed for deterministic re-run, if discoverable from
    /// the `proptest-regressions/` directory. Form:
    /// `<regression-file-path>::<seed-line>`. None for Kani.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// Human-readable failure message extracted from the panic / Kani
    /// failed-check line. Stripped of file paths and noise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// Source location of the failure as `file:line:col` (or `file:line`
    /// when col is unavailable). Best-effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
}
