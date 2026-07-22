//! Backend-obligation manifest (#332).
//!
//! Every verification obligation a `.qedspec` requests must finish, per
//! backend, as exactly one of: emitted (with the generated harness /
//! theorem / test identifier), unsupported (with a machine-readable
//! capability reason), or failed (generation should have succeeded and
//! did not). The manifest is recorded at the emission sites inside the
//! backend renderers — never by scanning generated files — and then
//! reconciled against the expected inventory enumerated from the lowered
//! spec (`inventory.rs`): an expected obligation the backend never
//! reported becomes `failed`, so a future silent skip surfaces as red
//! instead of absent.
//!
//! Three coverage levels (see #324):
//!   1. spec coverage — a handler is named by a property or file-level
//!      obligation (the existing `check --coverage` matrix);
//!   2. backend coverage — the selected backend emitted a faithful
//!      obligation (this manifest);
//!   3. execution coverage — the artifact compiled and the verifier ran
//!      it (`verify` backends + `.qed/verify-evidence.json`).
//!
//! Accounting discipline: `StatusCounts::of` matches exhaustively over
//! `ObligationStatus` (no `_` arm) so a new status variant is a compile
//! error at every accounting site, not a silently dropped case
//! (the #260/#270 lesson; see `SeverityCounts` in `check/model.rs`).

pub(crate) mod inventory;
#[cfg(test)]
mod spec_tests;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bump when the JSON shape changes. `load` rejects unknown versions
/// (the `qed_lock` pattern) rather than misreading them.
pub const OBLIGATIONS_SCHEMA_VERSION: u32 = 1;
pub const OBLIGATIONS_FILENAME: &str = "obligations.json";

/// The model-level backends the manifest tracks. `kani_impl`, unit tests,
/// and fuzzing are execution-layer artifacts, not obligation emitters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationBackend {
    Kani,
    Lean,
    Proptest,
}

impl ObligationBackend {
    pub fn name(self) -> &'static str {
        match self {
            ObligationBackend::Kani => "kani",
            ObligationBackend::Lean => "lean",
            ObligationBackend::Proptest => "proptest",
        }
    }
}

/// What kind of obligation an entry tracks. Closed enum — extend it when
/// a backend learns a new obligation shape; never reuse a variant for a
/// semantically different thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObligationKind {
    /// Invalid-input rejection for a guarded handler.
    GuardRejection,
    /// Property preserved by a handler (`preserved_by`).
    PropertyPreservation,
    /// Handler `ensures` clause holds post-transition.
    EnsuresPreservation,
    /// Handler preserves / establishes a named invariant.
    InvariantPreservation,
    /// State transition matches the declared effects.
    EffectConformance,
    /// Checked arithmetic aborts instead of wrapping.
    Overflow,
    /// `requires … else Err` abort obligation (Lean abort theorems).
    Abort,
    /// Bare `requires` conjunct enforced in the transition guard.
    TransitionGuard,
    /// `cover` block reachability.
    Cover,
    /// `liveness` block leads-to claim.
    Liveness,
    /// `environment` block preservation (per property × environment).
    Environment,
    /// CPI call-site callee-`ensures` composition.
    CpiEnsures,
    /// Token-transfer envelope correctness (`{handler}.cpi_correct`).
    TransferEnvelope,
    /// The state carrier the backend actually verified against
    /// (`pragma state_repr = adt` parity, #326).
    StateRepresentation,
    /// A whole per-account model in multi-account mode (#324/#331:
    /// empty / no-handler account skips must be visible).
    AccountModel,
    /// Backend-internal extras (frame theorems, sequence harnesses).
    /// Never part of the expected inventory; recorded as evidence only.
    BackendExtra,
}

/// Machine-readable capability reasons. Closed enum: consumers must be
/// able to match on these (and a new reason must be a compile error at
/// every consumer), so free-text is confined to `Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    /// #324 — multi-account Kani emits no file-level features.
    KaniMultiAccountFileLevel,
    /// #326 — Kani verifies a flat state model for `state_repr = adt`.
    KaniAdtStateRepr,
    /// Kani guard-rejection: no expressible negation for this guard.
    KaniGuardNegationInexpressible,
    /// Kani liveness: spec has no lifecycle, no target predicate.
    KaniLivenessNoLifecycle,
    /// #328 — Lean drops predicates naming a handler account's pubkey.
    LeanHandlerAccountPubkey,
    /// CPI callee ensures not composed: call site lacks `state_binders`.
    CpiMissingStateBinders,
    /// Lean transfer envelope: no authority declared on the transfer.
    LeanTransferNoAuthority,
    /// #331 — multi-account proptest cannot model spec-global ghosts.
    ProptestMultiAccountGhost,
    /// Proptest overflow: effect target is not a bounded numeric field.
    ProptestNonNumericOverflowTarget,
    /// Proptest guard rejection: no guard clause survives the simplified
    /// state model (e.g. every clause names a handler-account pubkey).
    ProptestGuardNotExpressible,
    /// Multi-account: account has no concrete fields to model.
    AccountHasNoFields,
    /// Multi-account: no handler routes to this account.
    AccountHasNoHandlers,
    /// Multi-account: the obligation spans accounts (property scoped to
    /// one account module, handler routed to another; unrouted handler).
    /// Product-state lowering (#324/#331) is the real fix.
    MultiAccountCrossAccountObligation,
    /// Predicate body is missing or uses an unsupported construct
    /// (e.g. an untranslatable quantifier).
    UnsupportedPredicateBody,
    /// Indexed-state Lean (`Map[N]` fields) emits predicate definitions
    /// only; preservation / abort / cover / liveness theorems are
    /// delegated to the user-owned `Proofs.lean` skeleton
    /// (`proofs_bootstrap`), so the generated `Spec.lean` carries no
    /// machine-emitted obligation for them.
    LeanIndexedShapeProofsExternal,
}

impl UnsupportedReason {
    /// One-line human explanation, shown next to the snake_case tag.
    pub fn describe(self) -> &'static str {
        match self {
            UnsupportedReason::KaniMultiAccountFileLevel => {
                "multi-account Kani does not lower file-level cover/liveness/environment obligations yet (#324)"
            }
            UnsupportedReason::KaniAdtStateRepr => {
                "Kani verifies a flat state model; `pragma state_repr = adt` parity is not implemented (#326)"
            }
            UnsupportedReason::KaniGuardNegationInexpressible => {
                "no expressible negation for this guard; rejection harness skipped"
            }
            UnsupportedReason::KaniLivenessNoLifecycle => {
                "spec has no lifecycle, so the liveness target predicate cannot be stated"
            }
            UnsupportedReason::LeanHandlerAccountPubkey => {
                "predicate names a handler account pubkey, which the Lean transition signature does not bind (#328)"
            }
            UnsupportedReason::CpiMissingStateBinders => {
                "callee ensures not composed: call site lacks `state_binders` for the referenced fields"
            }
            UnsupportedReason::LeanTransferNoAuthority => {
                "transfer declares no authority; envelope theorem skipped"
            }
            UnsupportedReason::ProptestMultiAccountGhost => {
                "multi-account proptest cannot model spec-global ghosts yet (#331)"
            }
            UnsupportedReason::ProptestNonNumericOverflowTarget => {
                "overflow target field has no numeric bound; strategy cannot be constructed"
            }
            UnsupportedReason::ProptestGuardNotExpressible => {
                "no guard clause survives the simplified state model; rejection test skipped"
            }
            UnsupportedReason::AccountHasNoFields => {
                "account type declares no concrete fields; per-account model skipped"
            }
            UnsupportedReason::AccountHasNoHandlers => {
                "no handler routes to this account; per-account model skipped"
            }
            UnsupportedReason::MultiAccountCrossAccountObligation => {
                "obligation spans account modules; product-state lowering is not implemented (#324/#331)"
            }
            UnsupportedReason::UnsupportedPredicateBody => {
                "predicate body is missing or uses an unsupported construct"
            }
            UnsupportedReason::LeanIndexedShapeProofsExternal => {
                "indexed-state Lean delegates theorems to the user-owned Proofs.lean skeleton; Spec.lean has no machine-emitted obligation"
            }
        }
    }
}

/// Terminal status of one obligation on one backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ObligationStatus {
    /// The backend emitted it; `artifact` is the harness / theorem /
    /// test identifier in the generated file.
    Emitted { artifact: String },
    /// The backend cannot express it; `reason` is machine-readable.
    Unsupported { reason: UnsupportedReason },
    /// Generation should have succeeded and did not (including: the
    /// expected inventory requested it and the backend never reported).
    Failed { reason: String },
}

/// One obligation on one backend.
///
/// Identity is `(backend, kind, scope, key)`:
///   * `scope` — handler name, account name, or `"file"`;
///   * `key` — stable spec-side discriminator within the scope
///     (property / invariant / cover name, clause index, field name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObligationEntry {
    pub backend: ObligationBackend,
    pub kind: ObligationKind,
    pub scope: String,
    pub key: String,
    #[serde(flatten)]
    pub status: ObligationStatus,
}

/// The persisted manifest (`.qed/obligations.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObligationManifest {
    pub schema_version: u32,
    pub spec: String,
    pub spec_hash: String,
    /// Which backends this run covered. A backend absent here has no
    /// entries by construction, not "zero obligations".
    pub backends: Vec<ObligationBackend>,
    pub entries: Vec<ObligationEntry>,
}

/// Exhaustive status accounting — the `SeverityCounts` pattern. Never
/// tally statuses with open-coded `.filter(...)` chains; a new
/// `ObligationStatus` variant must be a compile error here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatusCounts {
    pub emitted: usize,
    pub unsupported: usize,
    pub failed: usize,
}

impl StatusCounts {
    pub fn of(entries: &[ObligationEntry]) -> Self {
        let mut counts = StatusCounts::default();
        for entry in entries {
            match &entry.status {
                ObligationStatus::Emitted { .. } => counts.emitted += 1,
                ObligationStatus::Unsupported { .. } => counts.unsupported += 1,
                ObligationStatus::Failed { .. } => counts.failed += 1,
            }
        }
        counts
    }

    /// Strict gate: anything not emitted blocks a strict verify.
    pub fn gates_strict(&self) -> bool {
        self.unsupported > 0 || self.failed > 0
    }

    /// `12 emitted, 2 unsupported, 0 failed` — the per-backend summary line.
    pub fn summary(&self) -> String {
        format!(
            "{} emitted, {} unsupported, {} failed",
            self.emitted, self.unsupported, self.failed
        )
    }
}

/// Side-channel collector threaded through a backend renderer. Recording
/// MUST NOT change rendered output — snapshots stay byte-identical.
///
/// Duplicate identities are collapsed: a split guard harness reports the
/// same `(kind, scope, key)` once per sub-harness and the obligation
/// stays one entry (first status wins; later duplicates are dropped).
#[derive(Debug)]
pub struct ObligationRecorder {
    backend: ObligationBackend,
    entries: Vec<ObligationEntry>,
}

impl ObligationRecorder {
    pub fn new(backend: ObligationBackend) -> Self {
        ObligationRecorder {
            backend,
            entries: Vec::new(),
        }
    }

    fn push(&mut self, kind: ObligationKind, scope: &str, key: &str, status: ObligationStatus) {
        let duplicate = self
            .entries
            .iter()
            .any(|e| e.kind == kind && e.scope == scope && e.key == key);
        if duplicate {
            return;
        }
        self.entries.push(ObligationEntry {
            backend: self.backend,
            kind,
            scope: scope.to_string(),
            key: key.to_string(),
            status,
        });
    }

    pub fn emitted(&mut self, kind: ObligationKind, scope: &str, key: &str, artifact: &str) {
        self.push(
            kind,
            scope,
            key,
            ObligationStatus::Emitted {
                artifact: artifact.to_string(),
            },
        );
    }

    pub fn unsupported(
        &mut self,
        kind: ObligationKind,
        scope: &str,
        key: &str,
        reason: UnsupportedReason,
    ) {
        self.push(kind, scope, key, ObligationStatus::Unsupported { reason });
    }

    pub fn failed(&mut self, kind: ObligationKind, scope: &str, key: &str, reason: &str) {
        self.push(
            kind,
            scope,
            key,
            ObligationStatus::Failed {
                reason: reason.to_string(),
            },
        );
    }

    pub fn into_entries(self) -> Vec<ObligationEntry> {
        self.entries
    }
}

/// Reconcile one backend's recorded entries against its expected
/// inventory. An expected obligation the backend never reported gets a
/// status matched to the known capability boundary:
///   * Lean on an indexed-shape spec (`Map[N]` fields) → theorems are
///     delegated to the user-owned `Proofs.lean` skeleton;
///   * multi-account specs → cross-module scoping drops (#324/#331);
///   * otherwise → `failed`: a genuine silent skip.
pub fn reconciled(
    backend: ObligationBackend,
    mir: &crate::mir::Mir,
    parsed: &crate::check::ParsedSpec,
    recorded: Vec<ObligationEntry>,
) -> Vec<ObligationEntry> {
    let expected = inventory::expected_obligations(mir, parsed, backend);
    let missing_status =
        if backend == ObligationBackend::Lean && crate::lean_gen_mir::uses_indexed_shape(mir) {
            ObligationStatus::Unsupported {
                reason: UnsupportedReason::LeanIndexedShapeProofsExternal,
            }
        } else if parsed.account_types.len() > 1 {
            ObligationStatus::Unsupported {
                reason: UnsupportedReason::MultiAccountCrossAccountObligation,
            }
        } else {
            ObligationStatus::Failed {
                reason: format!(
                    "requested by the spec but not reported by the {} backend",
                    backend.name()
                ),
            }
        };
    inventory::reconcile(backend, expected, recorded, missing_status)
}

/// Collect the full reconciled manifest entries for every model backend,
/// in memory, without writing artifacts. `check --coverage` and
/// `verify --strict` call this — the renderers are pure string builders,
/// so status always reflects the CURRENT spec, never a stale file.
/// sBPF assembly specs return no entries (out of manifest scope v1).
pub fn collect_all(
    mir: &crate::mir::Mir,
    parsed: &crate::check::ParsedSpec,
) -> Vec<ObligationEntry> {
    if mir.is_assembly {
        return Vec::new();
    }
    let mut entries = Vec::new();
    entries.extend(reconciled(
        ObligationBackend::Kani,
        mir,
        parsed,
        crate::kani_mir::collect_obligations(mir, parsed),
    ));
    entries.extend(reconciled(
        ObligationBackend::Lean,
        mir,
        parsed,
        crate::lean_gen_mir::collect_obligations(mir),
    ));
    entries.extend(reconciled(
        ObligationBackend::Proptest,
        mir,
        parsed,
        crate::proptest_gen_mir::collect_obligations(mir, parsed),
    ));
    entries
}

/// Assemble the persistable manifest for one generation run.
pub fn build_manifest(
    spec_path: &Path,
    backends: Vec<ObligationBackend>,
    entries: Vec<ObligationEntry>,
) -> ObligationManifest {
    let spec_hash = std::fs::read_to_string(spec_path)
        .map(|s| qedgen_hash_core::sha256_hex16(&s))
        .unwrap_or_default();
    ObligationManifest {
        schema_version: OBLIGATIONS_SCHEMA_VERSION,
        spec: spec_path.display().to_string(),
        spec_hash,
        backends,
        entries,
    }
}

/// One `  <backend> obligations: N emitted, M unsupported, K failed` line
/// per backend present in `entries`, to stderr (matching codegen's
/// progress-line convention). Backends are grouped through the closed
/// enum so a new backend variant is a compile error here.
pub fn print_backend_summaries(entries: &[ObligationEntry]) {
    for backend in [
        ObligationBackend::Kani,
        ObligationBackend::Lean,
        ObligationBackend::Proptest,
    ] {
        let backend_entries: Vec<ObligationEntry> = entries
            .iter()
            .filter(|e| e.backend == backend)
            .cloned()
            .collect();
        if backend_entries.is_empty() {
            continue;
        }
        let counts = StatusCounts::of(&backend_entries);
        eprintln!("{} obligations: {}", backend.name(), counts.summary());
    }
}

/// Per-backend coverage rollup for `check --coverage` (level 2 of the
/// coverage taxonomy; the handler × property matrix is level 1).
#[derive(Debug, Serialize)]
pub struct BackendCoverageReport {
    pub backend: ObligationBackend,
    pub emitted: usize,
    pub unsupported: usize,
    pub failed: usize,
    pub entries: Vec<ObligationEntry>,
}

pub fn backend_coverage_reports(entries: &[ObligationEntry]) -> Vec<BackendCoverageReport> {
    [
        ObligationBackend::Kani,
        ObligationBackend::Lean,
        ObligationBackend::Proptest,
    ]
    .into_iter()
    .filter_map(|backend| {
        let backend_entries: Vec<ObligationEntry> = entries
            .iter()
            .filter(|e| e.backend == backend)
            .cloned()
            .collect();
        if backend_entries.is_empty() {
            return None;
        }
        let counts = StatusCounts::of(&backend_entries);
        Some(BackendCoverageReport {
            backend,
            emitted: counts.emitted,
            unsupported: counts.unsupported,
            failed: counts.failed,
            entries: backend_entries,
        })
    })
    .collect()
}

/// Text rendering of backend coverage, to stderr (matching
/// `check::print_coverage_table`).
pub fn print_backend_coverage(entries: &[ObligationEntry]) {
    if entries.is_empty() {
        eprintln!("\nBackend coverage: n/a (sBPF specs are not modeled by the manifest yet)");
        return;
    }
    eprintln!("\nBackend coverage (obligations emitted per backend):");
    print_backend_summaries(entries);
    let problems = problem_entries(entries);
    for problem in &problems {
        eprintln!("  {}", describe_problem(problem));
    }
}

/// The non-emitted entries, for detail listings.
pub fn problem_entries(entries: &[ObligationEntry]) -> Vec<&ObligationEntry> {
    entries
        .iter()
        .filter(|e| match &e.status {
            ObligationStatus::Emitted { .. } => false,
            ObligationStatus::Unsupported { .. } | ObligationStatus::Failed { .. } => true,
        })
        .collect()
}

/// One-line rendering of a non-emitted entry for human output.
pub fn describe_problem(entry: &ObligationEntry) -> String {
    let status = match &entry.status {
        ObligationStatus::Emitted { .. } => "emitted".to_string(),
        ObligationStatus::Unsupported { reason } => {
            format!("unsupported — {}", reason.describe())
        }
        ObligationStatus::Failed { reason } => format!("FAILED — {}", reason),
    };
    format!(
        "{}: {:?} {}/{}: {}",
        entry.backend.name(),
        entry.kind,
        entry.scope,
        entry.key,
        status
    )
}

/// `<spec_dir>/.qed/obligations.json` — sibling of `verify-evidence.json`.
pub fn obligations_path_for_spec(spec: &Path) -> PathBuf {
    let dir = spec.parent().unwrap_or_else(|| Path::new("."));
    dir.join(".qed").join(OBLIGATIONS_FILENAME)
}

/// Persist the manifest. Pretty JSON + trailing newline, matching
/// `verify/evidence.rs`.
pub fn record(manifest: &ObligationManifest, spec: &Path) -> Result<PathBuf> {
    let path = obligations_path_for_spec(spec);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut json = serde_json::to_string_pretty(manifest)?;
    json.push('\n');
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Load a persisted manifest, rejecting unknown schema versions instead
/// of misreading them (the `qed_lock` pattern). No in-tree consumer reads
/// the file yet — `check`/`verify` recompute in memory — but the loader
/// is part of the manifest contract for external tooling.
#[cfg_attr(not(test), allow(dead_code))]
pub fn load(path: &Path) -> Result<ObligationManifest> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let manifest: ObligationManifest =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    if manifest.schema_version > OBLIGATIONS_SCHEMA_VERSION {
        anyhow::bail!(
            "{} declares unsupported obligations schema version {} (this qedgen supports up to {})",
            path.display(),
            manifest.schema_version,
            OBLIGATIONS_SCHEMA_VERSION
        );
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: ObligationStatus) -> ObligationEntry {
        ObligationEntry {
            backend: ObligationBackend::Kani,
            kind: ObligationKind::PropertyPreservation,
            scope: "deposit".to_string(),
            key: "solvent".to_string(),
            status,
        }
    }

    #[test]
    fn counts_are_exhaustive_and_gate_strict() {
        let entries = vec![
            entry(ObligationStatus::Emitted {
                artifact: "verify_deposit_preserves_solvent".to_string(),
            }),
            ObligationEntry {
                key: "other".to_string(),
                ..entry(ObligationStatus::Unsupported {
                    reason: UnsupportedReason::KaniMultiAccountFileLevel,
                })
            },
        ];
        let counts = StatusCounts::of(&entries);
        assert_eq!(counts.emitted, 1);
        assert_eq!(counts.unsupported, 1);
        assert_eq!(counts.failed, 0);
        assert!(counts.gates_strict());
        assert!(!StatusCounts::of(&entries[..1]).gates_strict());
    }

    #[test]
    fn recorder_collapses_duplicate_identities() {
        let mut rec = ObligationRecorder::new(ObligationBackend::Kani);
        rec.emitted(
            ObligationKind::GuardRejection,
            "withdraw",
            "withdraw",
            "verify_withdraw_rejects_invalid_1_amount",
        );
        rec.emitted(
            ObligationKind::GuardRejection,
            "withdraw",
            "withdraw",
            "verify_withdraw_rejects_invalid_2_signer",
        );
        let entries = rec.into_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].status,
            ObligationStatus::Emitted {
                artifact: "verify_withdraw_rejects_invalid_1_amount".to_string()
            }
        );
    }

    #[test]
    fn manifest_json_roundtrip_and_version_reject() {
        let manifest = ObligationManifest {
            schema_version: OBLIGATIONS_SCHEMA_VERSION,
            spec: "vault.qedspec".to_string(),
            spec_hash: "abc123".to_string(),
            backends: vec![ObligationBackend::Kani, ObligationBackend::Lean],
            entries: vec![entry(ObligationStatus::Unsupported {
                reason: UnsupportedReason::LeanHandlerAccountPubkey,
            })],
        };
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("vault.qedspec");
        let path = record(&manifest, &spec).unwrap();
        assert_eq!(path, dir.path().join(".qed").join(OBLIGATIONS_FILENAME));

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.entries, manifest.entries);
        assert_eq!(loaded.backends, manifest.backends);

        // The snake_case tag + flattened status must be stable: consumers
        // key on these strings.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"status\": \"unsupported\""));
        assert!(raw.contains("\"reason\": \"lean_handler_account_pubkey\""));

        let mut future = manifest.clone();
        future.schema_version = OBLIGATIONS_SCHEMA_VERSION + 1;
        record(&future, &spec).unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("unsupported obligations schema version"));
    }
}
