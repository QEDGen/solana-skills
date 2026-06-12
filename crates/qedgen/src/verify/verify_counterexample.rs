//! Shared structured counterexample types.
//!
//! `verify --proptest` and `verify --kani` both parse backend output into a
//! uniform `(harness, var, value, line)` shape (parsers in
//! `verify_proptest_parse.rs` / `verify_kani_parse.rs`). Downstream consumers
//! (auditor subagent, JSON, probe-repro gating) rely on this canonical model;
//! proptest omits per-var lines (single panic location), Kani fills them from
//! CBMC traces.

use serde::Serialize;

/// One concrete assignment. `line` = source line where the variable was
/// constrained (Kani only; `None` for proptest).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CounterexampleVar {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// One failed harness's counterexample (one per failing `#[test]` / `#[kani::proof]`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Counterexample {
    /// Harness / test function name.
    pub harness: String,
    /// Always `"failed"` for now; reserved for "verified"/"timeout".
    pub status: String,
    /// Concrete inputs that triggered the failure.
    pub assignments: Vec<CounterexampleVar>,
    /// Proptest seed for deterministic re-run, from `proptest-regressions/`:
    /// `<regression-file-path>::<seed-line>`. None for Kani.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    /// Failure message from the panic / Kani failed-check line, noise-stripped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    /// Best-effort `file:line:col` (or `file:line`) of the failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
}
