//! `qedgen probe` — spec-coverage gap analyzer.
//!
//! Walks a parsed `.qedspec` and emits JSON findings for categories the
//! spec is silent on, consumed by the harness-native auditor subagent.
//! The CLI does **not** read implementation source — that's the auditor's
//! job. The predicates (see `spec_predicates`) are runtime-agnostic
//! compose-able primitives the auditor chains into kill-chains (SKILL.md
//! "Compose-with-what cookbook"); spec-less / impl-side categories live in
//! the auditor SKILL.md.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::Path;

use crate::anchor_project::parse_anchor_project;
use crate::check::parse_spec_file;

// Submodules — the audit data layer (probes, repro emitters, interview
// scaffolding) plus the spec-aware predicates driven by the enumerator.
pub(crate) mod arithmetic_symbol_probe;
pub(crate) mod cluster;
pub(crate) mod crucible_brownfield;
pub(crate) mod crucible_probe;
pub(crate) mod crucible_replay;
pub(crate) mod dead_guard_probe;
pub(crate) mod domain_account_overlay;
pub(crate) mod domain_extract;
pub(crate) mod domain_interview;
pub(crate) mod domain_sequence;
pub(crate) mod domain_sequence_binding;
pub(crate) mod domain_sequence_seed;
pub(crate) mod elicit;
pub(crate) mod handler_intent;
pub(crate) mod hypothesize;
pub(crate) mod idl_overlay;
pub(crate) mod lifecycle_probe;
pub(crate) mod paired_validator_probe;
pub(crate) mod pinocchio_probe;
pub(crate) mod probe_repro;
pub(crate) mod prompts;
pub(crate) mod ratify;
pub(crate) mod scan_util;
pub(crate) mod shank_probe;
pub(crate) mod spec_predicates;

use spec_predicates::*;

/// Probe output schema version. Bump on incompatible finding-shape changes;
/// the auditor pins against this.
///
/// - v2: spec-aware findings carry a `reproducer` (drop-on-fail pipeline).
/// - v3 (#227): the evidence model. Every predicate hit that can't (yet)
///   acquire a reproducer is preserved in `candidates[]` instead of being
///   silently dropped; `engine_runs[]` records per-engine status (including
///   candidate-drop counts and skipped files); `coverage` reports what was
///   discovered/exercised; and `outcome` distinguishes a real pass from a
///   low-coverage empty result or a budget-zero dry run. `findings[]` keeps
///   its v2 reproducer-only contract unchanged.
///
/// Additive for a v2 reader that ignores unknown fields: `findings[]` still
/// means the same thing. A v2 reader that treated "empty findings" as
/// "clean", however, MUST now also consult `candidates[]` and `outcome` —
/// see `docs/design/probe-schema-v3-migration.md`.
const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Variants populated incrementally across v2.x retrofits
pub enum Category {
    MissingSigner,
    ArbitraryCpi,
    ArithmeticOverflowWrapping,
    LifecycleOneShotViolation,
    /// Handler accepts an integer-shaped param used in `transfers.amount` or
    /// in an `effects` RHS, with no `requires` clause that bounds it. Pair
    /// with `permissionless` or `missing_signer` → drain.
    UnboundedAmountParam,
    /// Handler is marked `permissionless` AND mutates shared state. Anyone
    /// can grief, fill, or contend the resource. Composes with
    /// `unbounded_amount_param` and `arithmetic_overflow_wrapping` to amplify.
    PermissionlessStateWriter,
    /// Init-shape handler (transitions from initial lifecycle state) but no
    /// writable account with `pda` seeds. Default-address state collision —
    /// two callers can both target the same canonical address. Pair with
    /// `missing_signer` → spoof another user's init.
    InitWithoutPda,
    /// State field declared on an `account` type and read somewhere in the
    /// spec (`auth <field>`, `requires`, effect RHS, property
    /// expression) but never written by any handler `effect`. On
    /// Quasar/Anchor, `auth X` lowers to `has_one = X`, so an unset Pubkey
    /// makes the constraint unsatisfiable; a never-written counter makes a
    /// `preserved_by all` invariant prove vacuously.
    StoredFieldNeverWritten,
    /// Coverage-guided fuzz crash — Crucible found an action sequence that
    /// violates a spec invariant or triggers a runtime abort. Unlike the
    /// pattern-match categories above, carries concrete path evidence.
    CrucibleFuzzCrash,
    // ----- Pinocchio -------------------------------------------------
    /// `_unchecked` account-data load (e.g. `load_mut::<Account>(
    /// account.borrow_mut_data_unchecked())`) where the SAFETY comment
    /// claims owner / init / length / discriminator preconditions the
    /// agent cannot verify are upheld on every CF path.
    PinocchioUncheckedAccountLoad,
    /// Manual arithmetic on token amounts / lamports that doesn't use
    /// `checked_add` / `checked_sub` and isn't guarded by a bound
    /// proof. Covers `set_amount(amount() + delta)` and
    /// `*lamports -= n` patterns.
    PinocchioUncheckedArith,
    /// Same `AccountInfo` loaded as type T1 in handler A and T2 in
    /// handler B without a discriminator distinguishing them — a
    /// Pinocchio program has no `#[derive(Accounts)]` validating layout.
    PinocchioAccountTypeConfusion,
    /// Two `borrow_mut_*_unchecked()` calls on the same account whose
    /// lifetimes overlap. RefCell normally catches this; the unchecked
    /// variants bypass the check.
    PinocchioMutableBorrowAliasing,
    /// `accounts[N]` used after length check but without owner or
    /// type verification — fast-path style without discriminator
    /// guarding.
    PinocchioPositionWithoutTypeTag,
    /// `IndexedDataSlice` with `OFFSET + N > min_account_size` — short
    /// account triggers panic or partial read.
    PinocchioOffsetOverrun,
    /// Account treated as program-owned PDA but no `find_program_address`
    /// derivation reachable in the handler.
    PinocchioMissingPdaVerification,
    /// SAFETY comment claims invariant X, agent's CF read can't find X
    /// enforced. Highest-signal Pinocchio probe — explicitly weaponizes
    /// the authors' own preconditions.
    PinocchioStaleSafetyComment,
    /// Miri-detected UB on host disagrees with Mollusk's runtime
    /// outcome — typically Miri-fail with Mollusk-pass. Surfaced as
    /// Critical because the deployed `.so`'s release-mode wrap +
    /// sBPF alignment hides UB the host interpreter exposes.
    ExecutionDivergence,
    // ----- Arithmetic-symbol catalog ---------------------------------
    /// `saturating_sub` / `saturating_add` on a timestamp-shape receiver
    /// (`current_ts`, `unix_timestamp`, `slot`, `epoch`, `block_height`)
    /// whose result feeds a `>=`/`>` comparison gating a non-trivial
    /// effect (transfer / mint / state mutation). Saturation collapses
    /// two semantically distinct states into the boundary value (0 / MAX),
    /// opening a fund-flow gate that should have stayed closed.
    SilentSuccessArithmetic,
    /// `checked_sub` / `checked_add` / `checked_mul` whose `Err`
    /// propagation permanently bricks a deterministic / PDA-derived
    /// address. Correct in isolation; the bug is the failure mode ×
    /// address *permanence* — every subsequent init attempt hits the
    /// same underflow, locking the address forever.
    GracefulErrorAsDos,
    /// Unchecked `*` / `+` / `-` on integers inside a handler that also
    /// contains a token / system CPI. Locally safe under current upstream
    /// bounds, but no local invariant claim — if the bound loosens, the
    /// arithmetic wraps and the fund-flow effect proceeds on a corrupted
    /// value. Low severity: most sites are safe today, recommendation is
    /// preventive (`checked_*`).
    UncheckedArithWithFundFlow,
    /// Two or more validator-shape sites apply distinct accept-domains to
    /// the same logical field — sentinel-semantics drift across handlers.
    /// Canonical: `create_*::validate` rejects `field == 0` ("past
    /// expiry") while `transfer_validation` treats `0` as "never expires";
    /// users following one path's docs hit a hard rejection on the other.
    PairedValidatorInputDomainMismatch,
    /// Handler closes a PDA that holds external authority (SPL Approve
    /// delegate, mint authority, ATA delegate, …) without the reverse CPI
    /// (`Revoke`, `SetAuthority::None`, `Assign`). The closed PDA remains
    /// registered as live permission on the external account.
    ExternalAuthorityNotRevokedOnClose,
    /// Spec-less overlay (#235): the on-disk IDL and source handler
    /// discovery disagree — a source handler the IDL doesn't declare
    /// (undeclared surface, invisible to IDL-driven clients/audits) or an
    /// IDL instruction with no matching source handler (stale shipped
    /// interface). Deterministic cross-check; emitted as a candidate, not
    /// a finding — there is no runnable reproducer for drift by nature.
    IdlSourceDrift,
    /// Dead-guard sweep (#240): an error-enum variant that is *defined* but
    /// has no enforcement call-site anywhere in the crate's `src/` — the
    /// maintainer named a check (the variant often spells out the invariant)
    /// that no `require!` / `err!` / `return Err` ever fires, so the path it
    /// was meant to protect proceeds unchecked. Deterministic (enumerate the
    /// enum, grep each variant); emitted as a candidate, not a finding — an
    /// absence has no runnable reproducer. The model triages and grades it at
    /// the impact ceiling of the unguarded path, not a dead-variant floor.
    UnwiredErrorVariant,
}

impl Category {
    /// The snake_case tag — must match the serde `rename_all` serialization
    /// of the variant. Single source for `Finding.category_tag` and the
    /// stable-id salt at probe constructor sites. (Pinocchio site findings
    /// carry a finer-grained per-probe tag instead — see
    /// `pinocchio_probe.rs` — so not every constructor routes through this.)
    pub fn tag(&self) -> &'static str {
        match self {
            Category::MissingSigner => "missing_signer",
            Category::ArbitraryCpi => "arbitrary_cpi",
            Category::ArithmeticOverflowWrapping => "arithmetic_overflow_wrapping",
            Category::LifecycleOneShotViolation => "lifecycle_one_shot_violation",
            Category::UnboundedAmountParam => "unbounded_amount_param",
            Category::PermissionlessStateWriter => "permissionless_state_writer",
            Category::InitWithoutPda => "init_without_pda",
            Category::StoredFieldNeverWritten => "stored_field_never_written",
            Category::CrucibleFuzzCrash => "crucible_fuzz_crash",
            Category::PinocchioUncheckedAccountLoad => "pinocchio_unchecked_account_load",
            Category::PinocchioUncheckedArith => "pinocchio_unchecked_arith",
            Category::PinocchioAccountTypeConfusion => "pinocchio_account_type_confusion",
            Category::PinocchioMutableBorrowAliasing => "pinocchio_mutable_borrow_aliasing",
            Category::PinocchioPositionWithoutTypeTag => "pinocchio_position_without_type_tag",
            Category::PinocchioOffsetOverrun => "pinocchio_offset_overrun",
            Category::PinocchioMissingPdaVerification => "pinocchio_missing_pda_verification",
            Category::PinocchioStaleSafetyComment => "pinocchio_stale_safety_comment",
            Category::ExecutionDivergence => "execution_divergence",
            Category::SilentSuccessArithmetic => "silent_success_arithmetic",
            Category::GracefulErrorAsDos => "graceful_error_as_dos",
            Category::UncheckedArithWithFundFlow => "unchecked_arith_with_fund_flow",
            Category::PairedValidatorInputDomainMismatch => {
                "paired_validator_input_domain_mismatch"
            }
            Category::ExternalAuthorityNotRevokedOnClose => {
                "external_authority_not_revoked_on_close"
            }
            Category::IdlSourceDrift => "idl_source_drift",
            Category::UnwiredErrorVariant => "unwired_error_variant",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Low used by upcoming categories
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

/// A concrete artifact the user can re-run deterministically to observe the
/// finding. Pipeline contract: a `Finding` without a `Reproducer` is
/// dropped, never emitted — no "advisory" / "possibly" tier; either the bug
/// is reproducible or the probe is silent.
///
/// Reproducers live under `target/qedgen-repros/<finding_id>/` — ephemeral
/// (regenerated every probe run; never committed). The `.invocation` field
/// is the claim that travels with the finding; the generated artifact under
/// `target/` makes it re-runnable.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(dead_code)] // Variants populated incrementally during v2.16 retrofit
pub enum Reproducer {
    /// Symbolic counterexample produced by Kani BMC.
    Kani {
        /// Path to the committed harness file (relative to project root).
        harness_path: String,
        /// Harness function name, e.g. `probe_overflow_transfer`.
        harness_fn: String,
        /// Exact `cargo kani` invocation that re-fails.
        invocation: String,
        /// Captured assignment of symbolic inputs that triggers the violation.
        counterexample: KaniTrace,
        /// Pinned Kani version the counterexample was captured with.
        kani_version: String,
    },
    /// Concrete failing seed produced by proptest.
    Proptest {
        /// Path to the committed test file (relative to project root).
        test_path: String,
        /// Test function name.
        test_fn: String,
        /// Exact `cargo test` invocation that re-fails on `seed`.
        invocation: String,
        /// Canonical `PROPTEST_SEED` value.
        seed: String,
        /// JSON projection of the failing input for human inspection.
        failing_input: serde_json::Value,
    },
    /// Mollusk-driven Rust integration test under
    /// `<project_root>/target/qedgen-repros/tests/probe_<finding_id>.rs`.
    /// Invokes the user's deployed handler via `qedgen-sandbox` and asserts
    /// the bug fires. Run via `qedgen verify --probe-repros` or `cargo test
    /// --manifest-path target/qedgen-repros/Cargo.toml --test probe_<id>`.
    Sandbox {
        /// Path to the test file, relative to project root.
        test_path: String,
        /// Test function name (canonical form: `probe_<finding_id>`).
        test_fn: String,
        /// Exact invocation that runs just this test.
        invocation: String,
        /// True when the skeleton has agent-fill TODO markers (the test
        /// panics at runtime, so the finding is dropped per the
        /// reproducer-only contract). Flips to false once
        /// `qedgen probe --fill-repros` fills the TODOs.
        needs_fill: bool,
    },
    /// Pinocchio probe: structured prompt the audit subagent expands into
    /// a Mollusk-driven Rust test. The CLI emits the prompt + substitution
    /// map; the agent writes the `repro.rs` body. Template-driven (one
    /// markdown per probe) rather than codegen-emitted.
    MolluskPrompt {
        /// Path to the markdown template under
        /// `references/probes/pinocchio/<probe>.md#reproducer`.
        template_path: String,
        /// Per-finding values the agent substitutes into the template
        /// (e.g. `${HANDLER}` → `process_transfer`).
        substitutions: std::collections::BTreeMap<String, String>,
        /// Where the agent writes the filled repro. Relative to the
        /// project root: `.qed/probes/pinocchio/<finding-id>/repro_mollusk.rs`.
        repro_path: String,
    },
    /// Pinocchio Miri repro: structured prompt for a direct handler-call
    /// test (no SVM) run under `cargo +nightly miri test`. Catches the UB
    /// class (aliasing, OOB, overflow, uninit, invalid transmute) that
    /// Mollusk's SVM-level execution can't see.
    MiriPrompt {
        template_path: String,
        substitutions: std::collections::BTreeMap<String, String>,
        repro_path: String,
        /// Adversarial inputs derived from the site's SAFETY-comment
        /// clauses — each a claim the agent negates in the generated test.
        adversarial_inputs: Vec<AdversarialInput>,
        /// Invariant assertions the agent brackets the handler call with
        /// (conservation, distinctness, owner-write). Selected from
        /// `_harness/invariants.rs`.
        invariant_asserts: Vec<String>,
    },
    /// Coverage-guided fuzz crash discovered by Crucible: the on-disk
    /// crash blob from `crucible run` plus the minimized action sequence
    /// after auto-`tmin`. Re-fire deterministically with
    /// `crucible show <harness_dir> <crash_path> --replay`.
    Crucible {
        /// Path to the harness root directory (e.g. `fuzz/escrow`),
        /// relative to project root.
        harness_path: String,
        /// Path to the `.meta.json` crash file written by Crucible.
        crash_path: String,
        /// Exact CLI invocation that re-runs the minimized crash.
        invocation: String,
        /// Action sequence after `crucible tmin` minimization — what the
        /// human render shows; the full pre-min chain stays on disk in
        /// `crash_path` for audit.
        action_sequence: Vec<CrucibleActionRecord>,
        /// Extra per-seed reproducer paths deduplicated into this
        /// (handler, invariant) finding; one canonical reproducer renders
        /// in the human output.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        extra_seeds: Vec<String>,
        /// Crucible binary version at run time — re-running against a
        /// different build surfaces as version mismatch, not silent drift.
        crucible_version: String,
        /// The specific invariant/property name replay evidence named
        /// (#229), e.g. `"conservation"` from `"invariant conservation
        /// violated"`. Absent when the crash was a protocol-guard break,
        /// a bare assertion, or classified by the fallback heuristic. Keys
        /// dedupe so two distinct invariants tripped by one handler stay
        /// distinct findings.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        invariant_id: Option<String>,
    },
    /// Deterministic boundary-value harness generated by `qedgen probe`
    /// (#228) and confirmed to reproduce by building + running it (exit 0).
    /// Unlike `Proptest` there is no random seed — the witness is closed-form
    /// (e.g. a wrapping op driven to its overflow boundary).
    BoundaryValue {
        /// Path to the generated harness source, relative to project root.
        harness_path: String,
        /// Exact build-and-run command that re-confirms the reproduction.
        invocation: String,
        /// The concrete input that reproduces the violation.
        failing_input: String,
    },
}

/// One adversarial input for a Miri reproducer — a SAFETY-comment clause
/// the generated test negates.
#[derive(Debug, Clone, Serialize)]
pub struct AdversarialInput {
    /// Verbatim SAFETY-comment clause this input attacks.
    pub claim_text: String,
    /// Symbolic strategy identifier — keyed to a builder in
    /// `crates/qedgen/tests/fixtures/pinocchio-fixtures/_harness/adversarial.rs`.
    /// Known strategies: `alias_buffer`, `short_buffer`, `swap_position`,
    /// `uninit_init_flag`, `foreign_owner`, `short_balance`,
    /// `oversized_amount`.
    pub negation_strategy: String,
    /// What the test should observe under the negated input — either
    /// the handler returning `Err`, Miri flagging UB, or "either" when
    /// both outcomes satisfy the claim.
    pub expected_outcome: String,
}

/// Replica of Crucible's on-disk `<hash>.meta.json` shape — we don't pull
/// `crucible-fuzz-cli` as a library (heavy LibAFL transitive deps). If
/// Crucible changes the format, the parse error surfaces a re-pin hint.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CrucibleCrashMetadata {
    pub test_name: String,
    pub timestamp: String,
    pub iteration: u64,
    #[serde(default)]
    pub seed: Option<u64>,
    pub actions: Vec<CrucibleActionRecord>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CrucibleActionRecord {
    /// snake_case action name — matches the spec handler's name 1:1.
    pub name: String,
    /// JSON of the action's args (preserves `#[range(..)]`-mutated values).
    pub params: serde_json::Value,
    /// Whether the handler returned Ok (true) or surfaced a runtime error (false).
    pub success: bool,
    /// `Custom(N)` error code when the handler aborted, otherwise None.
    #[serde(default)]
    pub error_code: Option<u32>,
}

/// Captured Kani counterexample — just enough to understand the finding
/// without re-running Kani; the parent `Reproducer::Kani.invocation` is
/// the source of truth for re-validation.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // Populated incrementally during v2.16 retrofit
pub struct KaniTrace {
    /// One-line summary of which assertion fired.
    pub assertion: String,
    /// Symbolic input → concrete value assignments Kani produced.
    pub assignments: Vec<KaniAssignment>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // Populated incrementally during v2.16 retrofit
pub struct KaniAssignment {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    SpecAware,
    SpecLess,
}

/// Runtime detected by `--bootstrap`. Determines which categories apply
/// in spec-less mode and which auditor SKILL.md predicate set to invoke.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    /// Anchor (anchor-lang dep + Anchor.toml or `#[program]` mod present).
    Anchor,
    /// Native Rust solana-program (no anchor-lang dep).
    Native,
    /// sBPF assembly (`.s` files in src/).
    Sbpf,
    /// Hand-written Quasar (quasar-lang dep, NO qedgen markers / spec /
    /// `formal_verification/`). Idiomatic Quasar code that hasn't adopted
    /// qedgen — categories are Anchor-shaped + Quasar-specific.
    Quasar,
    /// QEDGen's own codegen target (quasar-lang dep AND qedgen markers
    /// — `#[qed(verified)]`, `formal_verification/`, or `qed.toml`).
    /// Categories collapse to user-owned-handler-body + Quasar-specific
    /// drift / unanchored-field / bounty-intent shapes.
    QedgenCodegen,
    /// Pinocchio (no_std, hand-rolled `unsafe` serde), identified by the
    /// `pinocchio` Cargo dep. Every safety check Anchor discharges
    /// automatically (owner, init, length, discriminator, alias) is the
    /// author's responsibility; routes to pinocchio_probe.rs, which
    /// enumerates `unsafe` serde sites + parsed SAFETY comments.
    Pinocchio,
    /// Detection inconclusive — auditor falls back to source-walking.
    Unknown,
}

/// One discovered handler in bootstrap (spec-less) mode. Auditor reads
/// `source_file` to investigate per-handler categories. The
/// Shank-dispatcher fields (`enum_variant`, `entry_fn`, `line`) are
/// optional + `omitempty` — present only when the dispatcher is
/// Shank-shape, so Anchor / IDL consumers see no change.
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapHandler {
    pub name: String,
    /// Path to the source file containing the handler, relative to
    /// `project_root` if possible. Auditor uses this for Read tool dispatch.
    pub source_file: String,
    /// Full enum-path string from the dispatch arm pattern, e.g.
    /// `MarketInstruction::InitializeMarket`. Shank dispatcher only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_variant: Option<String>,
    /// Terminal `process_*` callee name extracted from the arm body,
    /// e.g. `process_initialize_market`. Shank dispatcher only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_fn: Option<String>,
    /// 1-indexed line of the arm in the dispatcher file. Shank
    /// dispatcher only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Per-handler narrowing of the global `applicable_categories` list,
    /// from intent-tag classification of the handler body
    /// (`handler_intent.rs`). Absent = global list applies; present =
    /// "walk only these". Set only when Shank discovery resolves the body
    /// AND the classifier emits a non-trivial narrowing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applicable_categories: Option<Vec<String>>,
    /// Intent tag the classifier derived (`authority_gated` /
    /// `trader_gated` / `permissionless`); absent when no rule matched.
    /// Surfaced for auditor explainability when phrasing findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_tag: Option<String>,
    /// IDL overlay (#235): the instruction's account metas (signer /
    /// writable flags) from the on-disk IDL. Anchor/Quasar flags are
    /// runtime-enforced; Codama/Shank flags are declarative only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idl_accounts: Option<Vec<idl_overlay::IdlAccountMeta>>,
    /// IDL overlay (#235): the instruction's args (name + type) from the
    /// on-disk IDL, discriminator args elided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idl_args: Option<Vec<idl_overlay::IdlArgMeta>>,
    /// `"idl"` when this handler entry was filled from the IDL because
    /// source discovery yielded nothing (Pinocchio bootstrap);
    /// `source_file` then points at the IDL, not a `.rs` file. Absent for
    /// source-discovered handlers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovered_via: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Stable hash of (handler, category). Suppression rules key off this.
    pub id: String,
    pub category: Category,
    pub severity: Severity,
    pub handler: String,
    /// What the spec is silent on (human-readable).
    pub spec_silent_on: String,
    /// Minimal spec edit that would close the finding.
    pub suppression_hint: String,
    /// Where/how the auditor should investigate the impl.
    pub investigation_hint: String,
    /// Category identifier for documentation / grouping.
    pub category_tag: String,
    /// Concrete artifact reproducing the bug. `None` is transitional —
    /// findings without one are dropped at the pipeline level (no
    /// "advisory" tier). Serialized `omitempty` for v1 consumers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproducer: Option<Reproducer>,
    /// Gate names detected upstream of this finding's site (canonical
    /// Pinocchio zero-copy triad: `["length_check", "discriminator_check",
    /// "owner_check"]`; `offset_overrun`: `["length_check"]`). Fired gates
    /// mean the unsafe pattern is defensively fenced and likely not a real
    /// bug; the auditor bulk-suppresses belt-and-braces findings keyed on
    /// the triad. `None` = gate detector doesn't analyze this finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gated_by: Option<Vec<String>>,
}

/// A predicate hit or static pattern that warrants investigation but is
/// NOT a demonstrated vulnerability. The evidence tier below `findings[]`:
/// deliberately carries **no severity and no reproducer** so it can never
/// be mistaken for a confirmed finding (the "no advisory tier" rule applies
/// to `findings[]`, and candidates stay clearly on the other side of that
/// line). The auditor surfaces these as a work list, not as results.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub category: Category,
    pub category_tag: String,
    pub handler: String,
    /// What the spec is silent on (human-readable) — same text a finding
    /// would carry, minus any claim of exploitability.
    pub spec_silent_on: String,
    /// Minimal spec edit that would close it, if it is a real gap.
    pub suppression_hint: String,
    /// Where/how to investigate the impl to confirm or dismiss.
    pub investigation_hint: String,
    /// Why this predicate hit is a candidate rather than a finding — almost
    /// always "no constructible reproducer yet" (the constructor for this
    /// category is not implemented). Distinguishes "we can't prove it" from
    /// "we proved it safe".
    pub reason: String,
    /// A generated, ready-to-run reproducer harness (#228) the agent/CI can
    /// execute to promote this candidate to a finding. Present when the CLI
    /// mechanically generated a harness but did not run it (the default —
    /// running is opt-in via `--execute-repros`). A pointer, NOT a
    /// confirmation: the candidate still makes no exploitability claim until
    /// the harness reproduces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repro_harness: Option<ReproHarness>,
}

/// A generated reproducer harness attached to a candidate. Running it (exit 0
/// ⇒ the violation reproduces) is what promotes the candidate to a finding.
#[derive(Debug, Clone, Serialize)]
pub struct ReproHarness {
    /// Path to the generated harness source, relative to the project root.
    pub path: String,
    /// Exact command that builds and runs it; exits 0 iff the violation
    /// reproduces.
    pub invocation: String,
    /// Harness kind — `boundary_value` for the deterministic overflow witness.
    pub kind: String,
    /// The concrete input the harness pins (e.g. field/operand at the
    /// overflow boundary), for human inspection.
    pub failing_input: String,
}

/// Per-engine execution record. `findings[]`/`candidates[]` say *what* was
/// found; this says *whether the engine that would find it actually ran to
/// completion* — the difference between "no bugs" and "the scanner skipped
/// half the files".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // `failed`/`skipped` are constructed by #228/#229 engines
pub enum EngineStatus {
    /// Ran to completion over all inputs.
    Passed,
    /// Ran, but skipped some inputs (unreadable/unparseable files, dropped
    /// candidates) — results are incomplete.
    Partial,
    /// Could not run because a precondition was unmet (e.g. budget-zero
    /// harness dry run, missing IDL) — not a failure, but no coverage.
    Blocked,
    /// Attempted and errored (build failure, crash) — coverage unknown.
    Failed,
    /// Not requested this invocation.
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineRun {
    /// Stable engine id: `spec_predicates`, `crucible_fuzz`,
    /// `arithmetic_symbol`, `paired_validator`, `lifecycle`,
    /// `pinocchio_sites`, …
    pub engine: String,
    pub status: EngineStatus,
    /// One-line human explanation of a non-`passed` status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Predicate hits this engine produced that were demoted to candidates
    /// (or dropped) for want of a reproducer. `0` for engines that don't
    /// run the reproducer pipeline.
    #[serde(skip_serializing_if = "is_zero")]
    #[serde(default)]
    pub candidates_dropped: u32,
    /// Source files the engine could not read or parse, relative to the
    /// project root. Empty unless `status == partial`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub skipped_files: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What the probe actually discovered, generated, executed, and asserted —
/// so a zero-finding result is interpretable. All counts are best-effort
/// and engine-dependent; `None` fields mean "this engine doesn't measure
/// that", not "zero".
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProbeCoverage {
    /// Handlers the probe saw (spec handlers, or discovered brownfield
    /// handlers).
    pub handlers_discovered: u32,
    /// Fuzz actions generated from those handlers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions_generated: Option<u32>,
    /// Generated actions still carrying an agent-fill `todo!()` (cannot be
    /// exercised until filled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions_stubbed: Option<u32>,
    /// Spec invariants actually evaluated by the fuzz harness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invariants_evaluated: Option<u32>,
    /// Fuzz corpus size after the run (seeds on disk).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corpus_size: Option<u32>,
    /// Deepest stateful action sequence reached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sequence_depth: Option<u32>,
    /// Whether minimized crashes replayed successfully (Crucible triage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_success: Option<bool>,
}

/// Top-level interpretation of the run. Lets a consumer branch on outcome
/// instead of guessing from an empty `findings[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // `blocked_incomplete_harness`/`engine_failed` land with #228/#229
pub enum ProbeOutcome {
    /// Engines ran to completion; findings/candidates reflect real coverage.
    PassedWithCoverage,
    /// Engines ran but exercised little — an empty result here is weak
    /// evidence, not a clean bill of health.
    NoFindingsLowCoverage,
    /// A harness was required but is incomplete (stubbed actions,
    /// budget-zero emit) — the probe did not really run.
    BlockedIncompleteHarness,
    /// An engine errored; results are unreliable.
    EngineFailed,
    /// Harness was emitted for preview only (budget 0) — nothing executed.
    DryRun,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeOutput {
    pub version: u32,
    pub mode: Mode,
    /// Path to `.qedspec` (spec-aware mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_path: Option<String>,
    /// Project root walked in spec-less mode (`--bootstrap`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Detected runtime (spec-less mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,
    /// Handlers discovered via runtime-aware walking (spec-less mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handlers: Option<Vec<BootstrapHandler>>,
    /// Categories the auditor should investigate per handler (spec-less mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applicable_categories: Option<Vec<String>>,
    /// Findings (spec-aware mode only — spec-less is investigation-by-auditor).
    /// v2 reproducer-only contract unchanged: every entry has a reproducer.
    pub findings: Vec<Finding>,
    /// v3 (#227): predicate hits preserved as investigation candidates when
    /// no reproducer could be constructed — ends the silent drop. Always
    /// present (may be empty); a v2 consumer ignores it.
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    /// v3 (#227): per-engine execution status, so an empty `findings[]` can
    /// be told apart from an engine that skipped files or didn't run.
    #[serde(default)]
    pub engine_runs: Vec<EngineRun>,
    /// v3 (#227): what the run discovered/exercised/asserted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<ProbeCoverage>,
    /// v3 (#227): top-level interpretation — a consumer branches on this
    /// instead of guessing from `findings.is_empty()`.
    pub outcome: ProbeOutcome,
    /// Candidate spec clauses derived from findings + runtime signals.
    /// Populated only under `--emit-spec-candidates` (additive — older
    /// consumers ignore it). The auditor reads these to drive the
    /// scaffold-to-spec interview. (Distinct from `candidates[]`: these are
    /// clustered proto-spec-clauses, not investigation candidates.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clusters: Option<Vec<crate::cluster::Cluster>>,
    /// IDL overlay (#235, spec-less mode): on-disk IDL consumed to enrich
    /// `handlers[]`, relative to `project_root`. Absent = no IDL found
    /// (source-only envelope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idl_path: Option<String>,
    /// IDL overlay (#235/#238, spec-less mode): `"anchor"` / `"quasar"` /
    /// `"shank"` / `"codama"` when no IDL is on disk but the runtime or a
    /// project marker says one is mechanically derivable (`anchor build` /
    /// `shank idl` / `codama run`) — a hint for the agent, not something
    /// the CLI shells out to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivable_idl: Option<String>,
    /// Structural shape of the native dispatcher when detected. Only
    /// `"shank_central_match"` is emitted; other runtimes leave it absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatcher_kind: Option<String>,
    /// Spec elicitation (PRD Phase 0): stable per-run identifier, carried
    /// through the audit working set into `qedgen ratify` outputs so
    /// funnel conversion and time-to-first-check are joinable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Spec elicitation (PRD Phase 1): evidence-anchored, confirmable
    /// invariant hypotheses about *this* program. Spec-less mode only;
    /// always computed (not gated behind a flag) — the ranked summary on
    /// stderr is the default tail of every spec-less probe run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheses: Option<Vec<hypothesize::InvariantHypothesis>>,
    /// Spec elicitation (PRD Phase 0): hypothesis supply counts by class
    /// and confidence — the funnel's supply-side instrumentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_readiness: Option<hypothesize::SpecReadiness>,
}

impl ProbeOutput {
    /// Canonical empty envelope at the current schema version. Every
    /// construction site must start here (struct-update syntax) rather
    /// than spelling `version:` out — fuzz-mode outputs shipped
    /// `version: 1` against the v2 schema before this seam existed.
    pub fn envelope(mode: Mode) -> Self {
        ProbeOutput {
            version: SCHEMA_VERSION,
            mode,
            spec_path: None,
            project_root: None,
            runtime: None,
            handlers: None,
            applicable_categories: None,
            findings: Vec::new(),
            candidates: Vec::new(),
            engine_runs: Vec::new(),
            coverage: None,
            // Overwritten by every real construction site; the neutral
            // default suits a bare/empty envelope (bootstrap work list).
            outcome: ProbeOutcome::PassedWithCoverage,
            clusters: None,
            idl_path: None,
            derivable_idl: None,
            dispatcher_kind: None,
            run_id: None,
            hypotheses: None,
            spec_readiness: None,
        }
    }
}

/// Spec-elicitation finalizer for spec-less envelopes — the one seam every
/// spec-less print site calls right before serializing (PRD §6.1/§6.4):
/// stamps a per-run `run_id`, runs the hypothesizer over the discovered
/// handlers, records `spec_readiness`, and prints the ranked hypothesis
/// summary to stderr (stdout JSON stays the agent surface). When an audit
/// working set exists, also persists `hypotheses.json` and threads the
/// `run_id` into `run-manifest.json` so `ratify` can carry it through.
pub fn finalize_specless(output: &mut ProbeOutput, project_root: &Path, audit_dir: Option<&Path>) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Canonicalize so `--program .` still yields the directory's real
    // name in the run tag.
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let root_tag: String = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("program")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let run_id = format!("run-{}-{}", root_tag.trim_matches('_'), secs);
    output.run_id = Some(run_id.clone());

    let hypotheses = hypothesize::hypothesize(
        project_root,
        output.handlers.as_deref().unwrap_or(&[]),
        &output.candidates,
    );
    output.spec_readiness = Some(hypothesize::spec_readiness(&hypotheses));
    if !hypotheses.is_empty() {
        eprint!("{}", hypothesize::render_summary(&hypotheses));
    }
    output.hypotheses = Some(hypotheses);

    if let Some(dir) = audit_dir {
        if let Err(e) = write_elicitation_artifacts(dir, output, secs) {
            eprintln!(
                "warning: failed to write elicitation artifacts to {}: {}",
                dir.display(),
                e
            );
        }
    }
}

/// Persist the hypothesis set + run identity into the audit working set:
/// `hypotheses.json` (ratify's lowering input) and a `run_id` +
/// `spec_readiness` patch on `run-manifest.json` (Phase-0 funnel joins).
fn write_elicitation_artifacts(
    audit_dir: &Path,
    output: &ProbeOutput,
    generated_at_unix: u64,
) -> Result<()> {
    if !audit_dir.exists() {
        return Ok(());
    }
    let doc = serde_json::json!({
        "schema_version": 1,
        "run_id": output.run_id,
        "generated_at_unix": generated_at_unix,
        "spec_readiness": output.spec_readiness,
        "hypotheses": output.hypotheses.as_deref().unwrap_or_default(),
    });
    std::fs::write(
        audit_dir.join("hypotheses.json"),
        format!("{}\n", serde_json::to_string_pretty(&doc)?),
    )?;

    let manifest_path = audit_dir.join("run-manifest.json");
    if manifest_path.exists() {
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
        manifest["run_id"] = serde_json::to_value(&output.run_id)?;
        manifest["spec_readiness"] = serde_json::to_value(&output.spec_readiness)?;
        if let Some(artifacts) = manifest.get_mut("artifacts") {
            artifacts["hypotheses"] = serde_json::Value::String("hypotheses.json".to_string());
        }
        std::fs::write(
            &manifest_path,
            format!("{}\n", serde_json::to_string_pretty(&manifest)?),
        )?;
    }
    Ok(())
}

impl Candidate {
    /// Demote a predicate `Finding` whose reproducer couldn't be built into
    /// an investigation candidate. Drops severity + reproducer + gate info —
    /// a candidate makes no exploitability claim.
    pub fn from_dropped_finding(f: Finding, reason: impl Into<String>) -> Self {
        Candidate {
            category: f.category,
            category_tag: f.category_tag,
            handler: f.handler,
            spec_silent_on: f.spec_silent_on,
            suppression_hint: f.suppression_hint,
            investigation_hint: f.investigation_hint,
            reason: reason.into(),
            repro_harness: None,
        }
    }

    /// A candidate carrying a generated (but un-run) reproducer harness (#228).
    pub fn with_repro_harness(mut self, harness: ReproHarness) -> Self {
        self.repro_harness = Some(harness);
        self
    }
}

/// Accounting for the `reproducers` engine (#228).
#[derive(Default)]
struct ReproEngineStats {
    generated: u32,
    executed: u32,
    reproduced: u32,
    build_errors: u32,
}

/// Spec-aware probe. `execute_repros` (from `--execute-repros`) opts into
/// building + running generated reproducer harnesses; the default only
/// generates them (agent/CI runs them) so the default path performs no builds
/// and no execution.
pub fn run_probe(spec_path: &Path, execute_repros: bool) -> Result<ProbeOutput> {
    let spec = parse_spec_file(spec_path)?;
    let spec_models_lifecycle = !spec.lifecycle_states.is_empty()
        || spec.account_types.iter().any(|a| !a.lifecycle.is_empty());
    let initial_state = spec.lifecycle_states.first().cloned();
    let mut findings = Vec::new();

    for handler in &spec.handlers {
        if let Some(f) = predicate_missing_signer(handler) {
            findings.push(f);
        }
        if let Some(f) = predicate_arbitrary_cpi(handler) {
            findings.push(f);
        }
        findings.extend(predicate_arithmetic_overflow_wrapping(handler));
        if let Some(f) = predicate_lifecycle_one_shot_violation(handler, spec_models_lifecycle) {
            findings.push(f);
        }
        findings.extend(predicate_unbounded_amount_param(handler));
        if let Some(f) = predicate_permissionless_state_writer(handler) {
            findings.push(f);
        }
        if let Some(f) = predicate_init_without_pda(handler, initial_state.as_deref()) {
            findings.push(f);
        }
    }
    findings.extend(predicate_stored_field_never_written(&spec));

    // v3 (#227): a predicate hit either acquires a concrete reproducer and
    // becomes a `finding`, or is preserved as an investigation `candidate`.
    // The old pipeline dropped the latter silently, making a spec with live
    // predicate hits indistinguishable from a clean one.
    //
    // #228: `ArithmeticOverflowWrapping` now generates a real boundary
    // reproducer. By default the harness is generated but NOT run — it rides
    // on the candidate as a `repro_harness` pointer (agent/CI runs it). Under
    // `--execute-repros` the CLI builds + runs it and promotes to a finding
    // only when it reproduces (agent-authored-repros default preserved).
    let ctx = crate::probe_repro::ReproducerContext::from_spec_path(&spec, spec_path);
    let mut kept: Vec<Finding> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut repro_stats = ReproEngineStats::default();
    for mut finding in findings {
        // Try the executable-reproducer path first (currently only wrapping
        // arithmetic). `None` = not harnessable → fall through to the generic
        // constructor (which still stubs → candidate).
        if let Some(harness) =
            crate::probe_repro::build_arith_overflow_harness(&spec, &finding, &ctx)
        {
            repro_stats.generated += 1;
            if let Err(reason) = crate::probe_repro::write_harness(&harness) {
                candidates.push(Candidate::from_dropped_finding(
                    finding,
                    format!(
                        "boundary reproducer generated but not writable: {}",
                        crate::probe_repro::describe_failure(&reason)
                    ),
                ));
                continue;
            }
            if !execute_repros {
                // Default: harness generated, not run. Candidate carries the
                // pointer + exact invocation for the agent/CI to execute.
                candidates.push(
                    Candidate::from_dropped_finding(
                        finding,
                        "boundary reproducer generated; run it (or pass --execute-repros) to confirm",
                    )
                    .with_repro_harness(harness.as_repro_harness()),
                );
                continue;
            }
            repro_stats.executed += 1;
            match crate::probe_repro::execute_harness(&harness, ctx.kani_budget) {
                crate::probe_repro::ExecOutcome::Reproduced => {
                    repro_stats.reproduced += 1;
                    finding.reproducer = Some(harness.as_reproducer());
                    kept.push(finding);
                }
                crate::probe_repro::ExecOutcome::NotReproduced => {
                    candidates.push(
                        Candidate::from_dropped_finding(
                            finding,
                            "boundary reproducer ran but did not reproduce",
                        )
                        .with_repro_harness(harness.as_repro_harness()),
                    );
                }
                crate::probe_repro::ExecOutcome::BuildError(e) => {
                    repro_stats.build_errors += 1;
                    candidates.push(
                        Candidate::from_dropped_finding(
                            finding,
                            format!("boundary reproducer could not be built/run: {e}"),
                        )
                        .with_repro_harness(harness.as_repro_harness()),
                    );
                }
            }
            continue;
        }

        match crate::probe_repro::construct_reproducer(&finding, &ctx) {
            Ok(repro) => {
                finding.reproducer = Some(repro);
                kept.push(finding);
            }
            Err(reason) => {
                candidates.push(Candidate::from_dropped_finding(
                    finding,
                    crate::probe_repro::describe_failure(&reason),
                ));
            }
        }
    }

    let coverage = ProbeCoverage {
        handlers_discovered: spec.handlers.len() as u32,
        ..ProbeCoverage::default()
    };
    // The predicate engine ran to completion over every handler — `passed`
    // even when all hits demoted to candidates (that's an honest scan, not
    // an incomplete one). The demotions are recorded, not hidden.
    let mut engine_runs = vec![EngineRun {
        engine: "spec_predicates".to_string(),
        status: EngineStatus::Passed,
        detail: (!candidates.is_empty()).then(|| {
            format!(
                "{} predicate hit(s) preserved as candidates",
                candidates.len()
            )
        }),
        candidates_dropped: candidates.len() as u32,
        skipped_files: Vec::new(),
    }];
    // #228: a `reproducers` engine run when we generated any harness, so the
    // envelope records how many were generated / executed / confirmed.
    if repro_stats.generated > 0 {
        let detail = if execute_repros {
            format!(
                "{} harness(es) generated, {} executed, {} reproduced, {} build error(s)",
                repro_stats.generated,
                repro_stats.executed,
                repro_stats.reproduced,
                repro_stats.build_errors
            )
        } else {
            format!(
                "{} harness(es) generated (not run; pass --execute-repros to confirm)",
                repro_stats.generated
            )
        };
        engine_runs.push(EngineRun {
            engine: "reproducers".to_string(),
            status: if execute_repros {
                EngineStatus::Passed
            } else {
                EngineStatus::Blocked
            },
            detail: Some(detail),
            candidates_dropped: 0,
            skipped_files: Vec::new(),
        });
    }

    Ok(ProbeOutput {
        spec_path: Some(spec_path.display().to_string()),
        findings: kept,
        candidates,
        engine_runs,
        coverage: Some(coverage),
        // The predicate engine visits every handler, so its coverage is
        // always complete — a clean result here is a real (predicate-scoped)
        // pass, and live candidates are surfaced explicitly rather than
        // hidden behind an empty `findings[]`.
        outcome: ProbeOutcome::PassedWithCoverage,
        ..ProbeOutput::envelope(Mode::SpecAware)
    })
}

/// Spec-less probe (`--bootstrap`): walk a project root, detect runtime,
/// discover handlers, emit the work-list envelope the auditor consumes.
/// **The CLI does not investigate handlers in this mode** — its role is
/// structured dispatch: tell the auditor what runtime, which handlers,
/// and which categories to investigate.
///
/// Handler discovery: Anchor via `parse_anchor_project` (`#[program]`
/// mod's `pub fn`s); Native via the Shank detector; sBPF / qedgen-codegen
/// leave the list empty (auditor walks source directly).
pub fn run_bootstrap(project_root: &Path) -> Result<ProbeOutput> {
    if !project_root.exists() {
        return Err(anyhow!(
            "project root does not exist: {}",
            project_root.display()
        ));
    }

    let runtime = detect_runtime(project_root);
    let (mut handlers, dispatcher_kind) = match runtime {
        // Quasar's `#[program] mod` form is structurally compatible with
        // the Anchor parser — `#[instruction(discriminator = N)]` is an
        // extra attribute that doesn't disturb `pub fn` extraction.
        Runtime::Anchor | Runtime::Quasar | Runtime::QedgenCodegen => (
            discover_anchor_handlers(project_root).unwrap_or_default(),
            None,
        ),
        // Native: try the Shank central-match detector first; on no-match,
        // fall back to an empty handler list (auditor walks source
        // directly). Each handler body is also classified to emit a
        // narrowed `applicable_categories`.
        Runtime::Native => match crate::shank_probe::detect_shank_dispatcher(project_root) {
            Ok(Some(cat)) => {
                let global = applicable_categories(&runtime);
                let h: Vec<BootstrapHandler> = cat
                    .handlers
                    .into_iter()
                    .map(|sh| {
                        let (intent_tag, narrowed) =
                            classify_shank_handler(&sh.name, &sh.entry_fn, project_root, &global);
                        BootstrapHandler {
                            name: sh.name,
                            source_file: sh.file,
                            enum_variant: Some(sh.enum_variant),
                            entry_fn: Some(sh.entry_fn),
                            line: Some(sh.line),
                            applicable_categories: narrowed,
                            intent_tag,
                            idl_accounts: None,
                            idl_args: None,
                            discovered_via: None,
                        }
                    })
                    .collect();
                (h, Some("shank_central_match".to_string()))
            }
            _ => (Vec::new(), None),
        },
        _ => (Vec::new(), None),
    };
    let applicable = applicable_categories(&runtime);

    // #235: opportunistic IDL overlay — enrich handlers with account/arg
    // metas, narrow untagged Anchor/Quasar handlers via enforced signer
    // flags, fill an empty (Pinocchio) handler list from the IDL, and
    // surface source/IDL handler-set drift as candidates.
    let overlay = idl_overlay::apply(project_root, &runtime, &mut handlers, &applicable);

    // #240: deterministic dead-guard sweep — every `#[error_code]` variant
    // defined but wired into no enforcement call-site in `src/` becomes an
    // `unwired_error_variant` candidate for the model to triage. Runs on the
    // spec-less envelope (source-scanner lens the bootstrap previously
    // lacked); a no-op where there is no error enum.
    let mut candidates = overlay.drift_candidates;
    candidates.extend(dead_guard_probe::scan_program(project_root).unwrap_or_default());

    Ok(ProbeOutput {
        project_root: Some(project_root.display().to_string()),
        runtime: Some(runtime),
        handlers: Some(handlers),
        applicable_categories: Some(applicable),
        candidates,
        idl_path: overlay.idl_path,
        derivable_idl: overlay.derivable_idl,
        dispatcher_kind,
        ..ProbeOutput::envelope(Mode::SpecLess)
    })
}

/// Public wrapper for the main.rs `qedgen probe --program <path>` dispatcher.
pub fn detect_runtime_public(root: &Path) -> Runtime {
    detect_runtime(root)
}

/// Public wrapper for the main.rs dispatcher.
pub fn applicable_categories_public(runtime: &Runtime) -> Vec<String> {
    applicable_categories(runtime)
}

/// Runtime detection by filesystem heuristics. Order matters: a project
/// with both `Anchor.toml` and `solana-program` dep is Anchor.
fn detect_runtime(root: &Path) -> Runtime {
    // QedgenCodegen wins over Anchor.toml: codegen scaffolds an
    // `Anchor.toml` for the test harness, and without this precedence the
    // scaffold would be misclassified as Anchor and skip the
    // Quasar-specific category overlay.
    if has_qedgen_markers(root) {
        return Runtime::QedgenCodegen;
    }

    if root.join("Anchor.toml").exists() {
        return Runtime::Anchor;
    }

    // sBPF: any `.s` file under src/ or programs/.
    let asm_roots = [root.join("src"), root.join("programs")];
    for asm_root in &asm_roots {
        if let Ok(entries) = std::fs::read_dir(asm_root) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("s") {
                    return Runtime::Sbpf;
                }
            }
        }
    }

    // Cargo.toml dep heuristics.
    let cargo = root.join("Cargo.toml");
    if cargo.exists() {
        let content = std::fs::read_to_string(&cargo).unwrap_or_default();
        // Pinocchio pre-empts Anchor/Native: a Pinocchio crate may also
        // list `solana-program`; the `pinocchio` dep is canonical.
        if has_pinocchio_dep(&content) {
            return Runtime::Pinocchio;
        }
        if content.contains("quasar-lang") {
            // qedgen markers split codegen output from hand-written Quasar.
            if has_qedgen_markers(root) {
                return Runtime::QedgenCodegen;
            }
            return Runtime::Quasar;
        }
        if content.contains("anchor-lang") {
            return Runtime::Anchor;
        }
        if content.contains("solana-program") || content.contains("solana_program") {
            return Runtime::Native;
        }
    }

    Runtime::Unknown
}

/// Pinocchio dep check. Matches `pinocchio = ...`, `pinocchio.workspace =
/// true`, or `pinocchio-token`/`-system` siblings (siblings require the
/// root pinocchio surface).
fn has_pinocchio_dep(cargo_toml: &str) -> bool {
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        if let Some(after) = t.strip_prefix("pinocchio") {
            if after.starts_with(['=', '.', '-', ' ']) {
                return true;
            }
        }
    }
    false
}

/// Did codegen run against this crate? Any one of three signals suffices.
/// Splits `Runtime::Quasar` from `Runtime::QedgenCodegen` when the Cargo
/// dep alone is ambiguous.
fn has_qedgen_markers(root: &Path) -> bool {
    if root.join("formal_verification").is_dir() {
        return true;
    }
    if root.join("qed.toml").is_file() {
        return true;
    }
    let lib_rs = root.join("src").join("lib.rs");
    if let Ok(src) = std::fs::read_to_string(&lib_rs) {
        if src.contains("#[qed(verified") {
            return true;
        }
    }
    false
}

/// Map `parse_anchor_project` instructions into `BootstrapHandler`
/// entries; empty vec on failure (auditor falls back to source-walking).
/// Handles both a program crate root (`<root>/src/lib.rs`) and an Anchor
/// workspace root (`<root>/programs/*/src/lib.rs`, aggregated) — the
/// latter is the common brownfield case.
fn discover_anchor_handlers(root: &Path) -> Result<Vec<BootstrapHandler>> {
    let direct_lib = root.join("src").join("lib.rs");
    if direct_lib.is_file() {
        return single_crate_handlers(root, root);
    }

    let programs_dir = root.join("programs");
    if !programs_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut all = Vec::new();
    for entry in std::fs::read_dir(&programs_dir)?.flatten() {
        let crate_root = entry.path();
        if !crate_root.join("src").join("lib.rs").is_file() {
            continue;
        }
        if let Ok(handlers) = single_crate_handlers(&crate_root, root) {
            all.extend(handlers);
        }
    }
    Ok(all)
}

fn single_crate_handlers(crate_root: &Path, project_root: &Path) -> Result<Vec<BootstrapHandler>> {
    let project = parse_anchor_project(crate_root)?;
    let lib_path = project
        .lib_rs_path
        .strip_prefix(project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| project.lib_rs_path.display().to_string());
    Ok(project
        .instructions
        .into_iter()
        .map(|ix| BootstrapHandler {
            name: ix.name,
            source_file: lib_path.clone(),
            enum_variant: None,
            entry_fn: None,
            line: None,
            applicable_categories: None,
            intent_tag: None,
            idl_accounts: None,
            idl_args: None,
            discovered_via: None,
        })
        .collect())
}

/// Categories the auditor should investigate per runtime in spec-less mode.
fn applicable_categories(runtime: &Runtime) -> Vec<String> {
    let universal = [
        "missing_signer",
        "arbitrary_cpi",
        "arithmetic_overflow_wrapping",
        "lifecycle_one_shot_violation",
    ];
    let anchor_native = ["cpi_param_swap", "pda_canonical_bump"];
    // QedgenCodegen: codegen mechanizes the "universal" categories from
    // the spec, so only handler-body-level numeric / lifecycle bugs and
    // the Quasar-specific drift / unanchored-field / bounty-intent shapes
    // apply.
    let quasar_handler_body = [
        "arithmetic_overflow_wrapping",
        "lifecycle_one_shot_violation",
    ];
    let quasar_specific = [
        "spec_impl_drift_user_owned",
        "generated_guard_bypass",
        "stored_field_never_written",
        "qed_hash_drift_or_forgery",
        "field_chain_missing_root_anchor",
        "init_config_field_unanchored",
        "bounty_intent_drift",
    ];
    // Multi-actor / quorum primitive family — walked as part of the
    // standard catalog on any program with a multi-party state shape.
    let multi_actor = [
        "quorum_dup_inflation",
        "quorum_set_dup_at_init",
        "nonce_absent_action_replay",
        "creator_admin_outside_quorum",
        "signer_set_pinned_to_creator_pda_only",
    ];
    // Permissionless-shape categories. Per-handler narrowing
    // (handler_intent classifier) filters these back out when the handler
    // is `authority_gated`.
    let permissionless_shapes = [
        "permissionless_state_writer",
        "permissionless_create_account_dos",
    ];
    // Pinocchio surface — every Anchor-framework-discharged obligation
    // is now author-side. See references/probes/pinocchio/*.md for the
    // full catalog.
    let pinocchio_specific = [
        "pinocchio_unchecked_account_load",
        "pinocchio_unchecked_amount_arith",
        "pinocchio_unchecked_lamport_arith",
        "pinocchio_account_type_confusion",
        "pinocchio_mutable_borrow_aliasing",
        "pinocchio_position_without_type_tag",
        "pinocchio_offset_overrun",
        "pinocchio_missing_pda_verification",
        "pinocchio_stale_safety_comment",
    ];

    match runtime {
        Runtime::Anchor | Runtime::Native => universal
            .iter()
            .chain(anchor_native.iter())
            .chain(permissionless_shapes.iter())
            .chain(multi_actor.iter())
            .map(|s| s.to_string())
            .collect(),
        Runtime::Sbpf => universal.iter().map(|s| s.to_string()).collect(),
        // Hand-written Quasar shares Anchor's full universal-categories
        // surface (the codegen-mechanization claim does NOT apply), plus
        // the Quasar-specific shapes that exist independent of codegen.
        Runtime::Quasar => universal
            .iter()
            .chain(anchor_native.iter())
            .chain(permissionless_shapes.iter())
            .chain(quasar_specific.iter())
            .chain(multi_actor.iter())
            .map(|s| s.to_string())
            .collect(),
        Runtime::QedgenCodegen => quasar_handler_body
            .iter()
            .chain(quasar_specific.iter())
            .chain(multi_actor.iter())
            .map(|s| s.to_string())
            .collect(),
        Runtime::Pinocchio => universal
            .iter()
            .chain(pinocchio_specific.iter())
            .chain(multi_actor.iter())
            .map(|s| s.to_string())
            .collect(),
        Runtime::Unknown => universal.iter().map(|s| s.to_string()).collect(),
    }
}

/// Resolve a Shank handler's source body, run the intent classifier, and
/// return `(intent_tag_str, narrowed_categories)`. The narrowed list is
/// emitted only when the classifier actually drops a category — otherwise
/// the caller's global `applicable_categories` stays authoritative. Both
/// fields are `None` when the body can't be located or no rule matches;
/// a tag whose filter is a no-op (e.g. `TraderGated`) still emits the tag
/// with no narrowing.
fn classify_shank_handler(
    handler_name: &str,
    entry_fn: &str,
    project_root: &Path,
    global: &[String],
) -> (Option<String>, Option<Vec<String>>) {
    let Some((_path, body)) = crate::handler_intent::resolve_handler_body(entry_fn, project_root)
    else {
        return (None, None);
    };
    let tag = crate::handler_intent::classify_handler_body(handler_name, &body);
    let tag_str = tag.map(|t| t.as_str().to_string());
    let narrowed = crate::handler_intent::filter_categories(global, tag);
    if narrowed.len() == global.len() {
        // No-op filter — don't emit a duplicate list.
        return (tag_str, None);
    }
    (tag_str, Some(narrowed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::ParsedHandler;
    use crate::chumsky_adapter::parse_str;

    fn make_handler(name: &str, who: Option<&str>, permissionless: bool) -> ParsedHandler {
        ParsedHandler {
            name: name.to_string(),
            who: who.map(|s| s.to_string()),
            permissionless,
            ..Default::default()
        }
    }

    #[test]
    fn missing_signer_fires_when_no_auth_no_permissionless() {
        let h = make_handler("withdraw", None, false);
        let f = predicate_missing_signer(&h).expect("expected finding");
        assert_eq!(f.handler, "withdraw");
        assert_eq!(f.category_tag, "missing_signer");
    }

    #[test]
    fn missing_signer_silent_when_auth_present() {
        let h = make_handler("withdraw", Some("authority"), false);
        assert!(predicate_missing_signer(&h).is_none());
    }

    #[test]
    fn missing_signer_silent_when_permissionless() {
        let h = make_handler("crank", None, true);
        assert!(predicate_missing_signer(&h).is_none());
    }

    #[test]
    fn arbitrary_cpi_fires_on_writable_token_without_transfers() {
        use crate::check::ParsedHandlerAccount;
        let mut h = make_handler("deposit", Some("user"), false);
        h.accounts.push(ParsedHandlerAccount {
            name: "vault".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: Some("pool".to_string()),
            default_pubkey: None,
            imported_namespace: None,
        });
        let f = predicate_arbitrary_cpi(&h).expect("expected arbitrary_cpi finding");
        assert_eq!(f.category_tag, "arbitrary_cpi");
        assert!(f.spec_silent_on.contains("vault"));
    }

    #[test]
    fn arbitrary_cpi_silent_when_transfers_declared() {
        use crate::check::{ParsedHandlerAccount, ParsedTransfer};
        let mut h = make_handler("deposit", Some("user"), false);
        h.accounts.push(ParsedHandlerAccount {
            name: "vault".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        });
        h.transfers.push(ParsedTransfer {
            from: "src".into(),
            to: "dst".into(),
            amount: Some("amount".into()),
            amount_tree: None,
            authority: Some("user".into()),
        });
        assert!(predicate_arbitrary_cpi(&h).is_none());
    }

    #[test]
    fn arbitrary_cpi_silent_when_no_writable_token() {
        let h = make_handler("crank", None, true);
        assert!(predicate_arbitrary_cpi(&h).is_none());
    }

    #[test]
    fn arbitrary_cpi_silent_on_init_pattern() {
        // Init-via-System: handler with Uninitialized pre-state has
        // writable token accounts as CREATION targets (not transfers).
        // No `transfers` block expected.
        use crate::check::ParsedHandlerAccount;
        let mut h = make_handler("register_market", Some("user"), false);
        h.pre_status = Some("Uninitialized".to_string());
        h.accounts.push(ParsedHandlerAccount {
            name: "base_vault".to_string(),
            is_signer: false,
            is_writable: true,
            is_program: false,
            pda_seeds: None,
            account_type: Some("token".to_string()),
            authority: None,
            default_pubkey: None,
            imported_namespace: None,
        });
        assert!(predicate_arbitrary_cpi(&h).is_none());
    }

    #[test]
    fn arith_predicate_fires_on_wrap() {
        let mut h = make_handler("tick", Some("crank"), false);
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "epoch", "add_wrap", "1",
        ));
        let findings = predicate_arithmetic_overflow_wrapping(&h);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category_tag, "arithmetic_overflow_wrapping");
        assert!(findings[0].spec_silent_on.contains("wrapping"));
    }

    #[test]
    fn arith_predicate_fires_on_saturating() {
        let mut h = make_handler("apply", Some("user"), false);
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "balance", "add_sat", "delta",
        ));
        let findings = predicate_arithmetic_overflow_wrapping(&h);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].spec_silent_on.contains("saturating"));
    }

    #[test]
    fn arith_predicate_silent_on_default_checked() {
        let mut h = make_handler("deposit", Some("user"), false);
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "total", "add", "amount",
        ));
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "fee_pool", "sub", "amount",
        ));
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "balance", "set", "x",
        ));
        assert!(predicate_arithmetic_overflow_wrapping(&h).is_empty());
    }

    #[test]
    fn arith_predicate_fires_per_op() {
        let mut h = make_handler("complex", Some("user"), false);
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "a", "add_wrap", "1",
        ));
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "b", "add_sat", "delta",
        ));
        let findings = predicate_arithmetic_overflow_wrapping(&h);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn lifecycle_predicate_fires_when_state_mutating_no_pre_status() {
        let mut h = make_handler("withdraw", Some("user"), false);
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "balance", "set", "0",
        ));
        let f =
            predicate_lifecycle_one_shot_violation(&h, true).expect("expected lifecycle finding");
        assert_eq!(f.category_tag, "lifecycle_one_shot_violation");
    }

    #[test]
    fn lifecycle_predicate_silent_when_pre_status_declared() {
        let mut h = make_handler("withdraw", Some("user"), false);
        h.pre_status = Some("Active".to_string());
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "balance", "set", "0",
        ));
        assert!(predicate_lifecycle_one_shot_violation(&h, true).is_none());
    }

    #[test]
    fn lifecycle_predicate_silent_when_permissionless() {
        let mut h = make_handler("crank", None, true);
        h.effects
            .push(crate::check::ParsedEffect::from_triple("x", "set", "1"));
        assert!(predicate_lifecycle_one_shot_violation(&h, true).is_none());
    }

    #[test]
    fn lifecycle_predicate_silent_when_spec_has_no_lifecycle() {
        let mut h = make_handler("withdraw", Some("user"), false);
        h.effects.push(crate::check::ParsedEffect::from_triple(
            "balance", "set", "0",
        ));
        assert!(predicate_lifecycle_one_shot_violation(&h, false).is_none());
    }

    #[test]
    fn lifecycle_predicate_silent_when_no_state_mutation() {
        let h = make_handler("read", Some("user"), false);
        assert!(predicate_lifecycle_one_shot_violation(&h, true).is_none());
    }

    #[test]
    fn stable_id_is_stable() {
        let a = stable_id("withdraw", "missing_signer");
        let b = stable_id("withdraw", "missing_signer");
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        let c = stable_id("withdraw", "arbitrary_cpi");
        assert_ne!(a, c);
    }

    #[test]
    fn unbounded_amount_param_fires_on_lower_only_bound() {
        // `requires amount > 0` is a lower bound; doesn't constrain the
        // u64::MAX side. Probe must fire so the auditor escalates.
        let src = r#"spec T
state { pool : U64 }
handler deposit (amount : U64) {
  permissionless
  requires amount > 0 else InvalidAmount
  effect { pool += amount }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        let findings = predicate_unbounded_amount_param(h);
        assert_eq!(findings.len(), 1, "expected one finding: {findings:#?}");
        assert_eq!(findings[0].category_tag, "unbounded_amount_param");
    }

    #[test]
    fn unbounded_amount_param_suppressed_by_upper_bound() {
        // `requires amount <= state.cap` is a real upper bound — suppress.
        let src = r#"spec T
state { pool : U64, cap : U64 }
handler deposit (amount : U64) {
  permissionless
  requires amount <= state.cap else CapExceeded
  effect { pool += amount }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        let findings = predicate_unbounded_amount_param(h);
        assert!(
            findings.is_empty(),
            "upper bound should suppress: {findings:#?}"
        );
    }

    #[test]
    fn unbounded_amount_param_suppressed_by_rhs_form() {
        // `requires state.cap >= amount` — RHS-bounded upper bound.
        let src = r#"spec T
state { pool : U64, cap : U64 }
handler deposit (amount : U64) {
  permissionless
  requires state.cap >= amount else CapExceeded
  effect { pool += amount }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        let findings = predicate_unbounded_amount_param(h);
        assert!(
            findings.is_empty(),
            "RHS-bounded upper should suppress: {findings:#?}"
        );
    }

    #[test]
    fn permissionless_state_writer_fires_on_permissionless_with_effect() {
        let src = r#"spec T
state { counter : U64 }
handler crank {
  permissionless
  effect { counter += 1 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        let f = predicate_permissionless_state_writer(h).expect("expected finding");
        assert_eq!(f.category_tag, "permissionless_state_writer");
    }

    #[test]
    fn permissionless_state_writer_suppressed_when_authd() {
        // Has auth — no permissionless flag — no finding.
        let src = r#"spec T
state { counter : U64 }
handler crank {
  auth admin
  accounts { admin : signer }
  effect { counter += 1 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(predicate_permissionless_state_writer(h).is_none());
    }

    #[test]
    fn permissionless_state_writer_suppressed_when_no_effects() {
        // Permissionless read-only handler — no shared state to grief.
        let src = r#"spec T
state { counter : U64 }
handler ping {
  permissionless
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(predicate_permissionless_state_writer(h).is_none());
    }

    #[test]
    fn init_without_pda_fires_on_init_handler_no_pda() {
        // pre_status `Uninitialized` matches the init shape; the
        // writable account has no pda seeds — collision risk.
        let src = r#"spec T
type State
  | Uninitialized
  | Active of { owner : Pubkey, balance : U64 }

handler initialize : State.Uninitialized -> State.Active {
  auth payer
  accounts {
    payer : signer, writable
    target : writable
  }
  effect { balance := 0 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        let f = predicate_init_without_pda(h, Some("Uninitialized")).expect("expected finding");
        assert_eq!(f.category_tag, "init_without_pda");
    }

    #[test]
    fn init_without_pda_suppressed_when_pda_present() {
        let src = r#"spec T
type State
  | Uninitialized
  | Active of { owner : Pubkey, balance : U64 }

handler initialize : State.Uninitialized -> State.Active {
  auth payer
  accounts {
    payer : signer, writable
    target : writable, pda ["target", payer]
  }
  effect { balance := 0 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(predicate_init_without_pda(h, Some("Uninitialized")).is_none());
    }

    #[test]
    fn init_without_pda_suppressed_when_lifecycle_starts_in_active() {
        // Spec doesn't have an Uninitialized / Empty / Inactive state —
        // not init-shape, no collision risk to flag.
        let src = r#"spec T
type State
  | Active of { owner : Pubkey, count : U64 }
  | Frozen

handler add (i : U8) : State.Active -> State.Active {
  auth admin
  accounts { admin : signer }
  effect { count += 1 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let h = &spec.handlers[0];
        assert!(predicate_init_without_pda(h, Some("Active")).is_none());
    }

    #[test]
    fn stored_field_never_written_fires_on_authd_field_with_no_writer() {
        // The escrow `taker` shape: field declared, `auth taker` reads
        // it (codegen lowers to `has_one = taker`), no handler `effect`
        // writes it → constraint unsatisfiable. CRIT.
        let src = r#"spec Escrow
type State
  | Uninitialized
  | Open of { initializer : Pubkey, taker : Pubkey, amount : U64 }

pda escrow ["escrow", initializer]

handler initialize (deposit : U64) : State.Uninitialized -> State.Open {
  auth initializer
  accounts {
    initializer : signer, writable
    escrow      : writable, pda ["escrow", initializer]
  }
  effect { amount := deposit }
}

handler exchange : State.Open -> State.Open {
  auth taker
  accounts {
    taker : signer, writable
    escrow : writable, pda ["escrow", initializer]
  }
}
"#;
        let spec = parse_str(src).expect("parse");
        let findings = predicate_stored_field_never_written(&spec);
        let taker_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.spec_silent_on.contains("`taker`"))
            .collect();
        assert_eq!(
            taker_findings.len(),
            1,
            "expected one taker finding: {findings:#?}"
        );
        assert_eq!(taker_findings[0].category_tag, "stored_field_never_written");
    }

    #[test]
    fn stored_field_never_written_suppressed_for_pda_seeds() {
        // `initializer` is in the PDA seeds (`pda escrow ["escrow",
        // initializer]`), so codegen binds it implicitly at init.
        // Spec authors don't write an explicit
        // `initializer := initializer.key()` effect.
        let src = r#"spec Escrow
type State
  | Uninitialized
  | Open of { initializer : Pubkey, amount : U64 }

pda escrow ["escrow", initializer]

handler initialize (deposit : U64) : State.Uninitialized -> State.Open {
  auth initializer
  accounts {
    initializer : signer, writable
    escrow      : writable, pda ["escrow", initializer]
  }
  effect { amount := deposit }
}
"#;
        let spec = parse_str(src).expect("parse");
        let findings = predicate_stored_field_never_written(&spec);
        let initializer_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.spec_silent_on.contains("`initializer`"))
            .collect();
        assert!(
            initializer_findings.is_empty(),
            "PDA seed should suppress: {findings:#?}"
        );
    }

    #[test]
    fn stored_field_never_written_suppressed_when_field_unused() {
        // Field declared but never read AND never written — that's the
        // dead-state-field axis, a different concern. This predicate
        // is about read-without-write specifically.
        let src = r#"spec T
type State
  | Active of { unused : Pubkey, counter : U64 }

handler bump : State.Active -> State.Active {
  auth admin
  accounts { admin : signer }
  effect { counter := 0 }
}
"#;
        let spec = parse_str(src).expect("parse");
        let findings = predicate_stored_field_never_written(&spec);
        let unused_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.spec_silent_on.contains("`unused`"))
            .collect();
        assert!(
            unused_findings.is_empty(),
            "unread field should not fire: {findings:#?}"
        );
    }

    #[test]
    fn detect_runtime_classifies_quasar_without_qedgen_markers() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"

[dependencies]
quasar-lang = "0.1"
"#,
        )
        .expect("write");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src").join("lib.rs"), "// no qed markers").expect("write");
        let r = detect_runtime(root);
        assert!(matches!(r, Runtime::Quasar), "expected Quasar, got {r:?}");
    }

    #[test]
    fn detect_runtime_classifies_qedgen_codegen_with_markers() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"

[dependencies]
quasar-lang = "0.1"
"#,
        )
        .expect("write");
        // formal_verification/ alone is enough — one of the three
        // signals `has_qedgen_markers` checks.
        fs::create_dir_all(root.join("formal_verification")).expect("mkdir");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src").join("lib.rs"), "// codegen output").expect("write");
        let r = detect_runtime(root);
        assert!(
            matches!(r, Runtime::QedgenCodegen),
            "expected QedgenCodegen, got {r:?}"
        );
    }

    #[test]
    fn detect_runtime_classifies_pinocchio_from_cargo_dep() {
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"

[dependencies]
pinocchio = "0.6"
pinocchio-token = "0.3"
"#,
        )
        .expect("write");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src").join("lib.rs"), "").expect("write");
        let r = detect_runtime(root);
        assert!(
            matches!(r, Runtime::Pinocchio),
            "expected Pinocchio, got {r:?}"
        );
    }

    #[test]
    fn detect_runtime_pinocchio_preempts_solana_program_dep() {
        // A real Pinocchio program may transitively depend on
        // solana-program. The Pinocchio dep should take precedence.
        use std::fs;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"

[dependencies]
pinocchio = "0.6"
solana-program = "1.18"
"#,
        )
        .expect("write");
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src").join("lib.rs"), "").expect("write");
        let r = detect_runtime(root);
        assert!(
            matches!(r, Runtime::Pinocchio),
            "expected Pinocchio (not Native), got {r:?}"
        );
    }

    #[test]
    fn applicable_categories_for_pinocchio_includes_runtime_specific() {
        let cats = applicable_categories(&Runtime::Pinocchio);
        assert!(
            cats.iter().any(|c| c == "pinocchio_unchecked_amount_arith"),
            "Pinocchio applicable_categories missing unchecked_amount_arith: {:?}",
            cats
        );
        assert!(
            cats.iter().any(|c| c == "pinocchio_stale_safety_comment"),
            "Pinocchio applicable_categories missing stale_safety_comment: {:?}",
            cats
        );
        // Universal categories should still be present.
        assert!(cats.iter().any(|c| c == "missing_signer"));
    }

    #[test]
    fn applicable_categories_for_native_includes_permissionless_shapes() {
        // The per-handler filter only does useful work when these
        // categories are in the global list to begin with.
        let cats = applicable_categories(&Runtime::Native);
        assert!(
            cats.iter().any(|c| c == "permissionless_state_writer"),
            "Native applicable_categories must include permissionless_state_writer: {:?}",
            cats
        );
        assert!(
            cats.iter()
                .any(|c| c == "permissionless_create_account_dos"),
            "Native applicable_categories must include permissionless_create_account_dos: {:?}",
            cats
        );
    }

    #[test]
    fn run_bootstrap_against_shank_fixture_emits_per_handler_narrowing() {
        // End-to-end: the committed fixture exercises three intent
        // shapes (authority_gated / permissionless / trader_gated)
        // across three dispatcher arms. We assert each handler ends
        // up with the right intent tag and that the narrowing filter
        // actually narrows where it should.
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/native-fixtures/shank-dispatcher");
        let out = run_bootstrap(&root).expect("bootstrap must succeed");
        let handlers = out.handlers.expect("expected populated handlers list");
        assert_eq!(handlers.len(), 3, "fixture defines three handlers");

        // 1. InitializeWidget — authority_gated → drops permissionless shapes.
        let init = &handlers[0];
        assert_eq!(init.name, "InitializeWidget");
        assert_eq!(init.intent_tag.as_deref(), Some("authority_gated"));
        let init_cats = init
            .applicable_categories
            .as_ref()
            .expect("authority_gated must narrow");
        assert!(
            !init_cats.iter().any(|c| c == "permissionless_state_writer"),
            "authority_gated must drop permissionless_state_writer: {:?}",
            init_cats
        );
        assert!(
            !init_cats
                .iter()
                .any(|c| c == "permissionless_create_account_dos"),
            "authority_gated must drop permissionless_create_account_dos: {:?}",
            init_cats
        );

        // 2. Tick — permissionless → drops missing_signer.
        let tick = &handlers[1];
        assert_eq!(tick.name, "Tick");
        assert_eq!(tick.intent_tag.as_deref(), Some("permissionless"));
        let tick_cats = tick
            .applicable_categories
            .as_ref()
            .expect("permissionless must narrow");
        assert!(
            !tick_cats.iter().any(|c| c == "missing_signer"),
            "permissionless must drop missing_signer: {:?}",
            tick_cats
        );

        // 3. Close — trader_gated → no narrowing today, but tag still emitted.
        let close = &handlers[2];
        assert_eq!(close.name, "Close");
        assert_eq!(close.intent_tag.as_deref(), Some("trader_gated"));
    }
}
