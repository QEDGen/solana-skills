//! `qedgen probe` — spec-coverage gap analyzer.
//!
//! Walks a parsed `.qedspec` and emits JSON findings for categories the
//! spec is silent on, consumed by the harness-native auditor subagent.
//! The CLI does **not** read implementation source — that's the auditor's
//! job. Predicates here are runtime-agnostic compose-able primitives the
//! auditor chains into kill-chains (SKILL.md "Compose-with-what cookbook");
//! spec-less / impl-side categories live in the auditor SKILL.md.

use anyhow::{anyhow, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::anchor_project::parse_anchor_project;
use crate::check::{parse_spec_file, ParsedHandler, ParsedSpec};

/// Probe output schema version. Bump on incompatible finding-shape changes;
/// the auditor pins against this. v2: spec-aware findings carry a
/// `reproducer` (drop-on-fail pipeline) — the field is typed `Option` as a
/// transitional shim, but candidates without a constructible reproducer are
/// dropped, so every emitted finding has one. Spec-less / `--bootstrap`
/// never emits findings.
const SCHEMA_VERSION: u32 = 2;

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
    /// spec (`auth <field>`, `requires`/`aborts_if`, effect RHS, property
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
    pub findings: Vec<Finding>,
    /// Candidate spec clauses derived from findings + runtime signals.
    /// Populated only under `--emit-spec-candidates` (additive — older
    /// consumers ignore it). The auditor reads these to drive the
    /// scaffold-to-spec interview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clusters: Option<Vec<crate::cluster::Cluster>>,
    /// Structural shape of the native dispatcher when detected. Only
    /// `"shank_central_match"` is emitted; other runtimes leave it absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatcher_kind: Option<String>,
}

pub fn run_probe(spec_path: &Path) -> Result<ProbeOutput> {
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

    // Drop-on-fail: every candidate must acquire a concrete reproducer
    // or be silently dropped. No advisory tier.
    let ctx = crate::probe_repro::ReproducerContext::from_spec_path(&spec, spec_path);
    findings.retain_mut(
        |finding| match crate::probe_repro::construct_reproducer(finding, &ctx) {
            Ok(repro) => {
                finding.reproducer = Some(repro);
                true
            }
            Err(_) => false,
        },
    );

    Ok(ProbeOutput {
        version: SCHEMA_VERSION,
        mode: Mode::SpecAware,
        spec_path: Some(spec_path.display().to_string()),
        project_root: None,
        runtime: None,
        handlers: None,
        applicable_categories: None,
        findings,
        clusters: None,
        dispatcher_kind: None,
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
    let (handlers, dispatcher_kind) = match runtime {
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

    Ok(ProbeOutput {
        version: SCHEMA_VERSION,
        mode: Mode::SpecLess,
        spec_path: None,
        project_root: Some(project_root.display().to_string()),
        runtime: Some(runtime),
        handlers: Some(handlers),
        applicable_categories: Some(applicable),
        findings: Vec::new(),
        clusters: None,
        dispatcher_kind,
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

/// Exposed so the Pinocchio dispatcher emits envelopes with the canonical
/// version rather than hard-coding a duplicate.
pub fn schema_version() -> u32 {
    SCHEMA_VERSION
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

/// Spec-aware predicate: handler has no `auth X` clause and is not marked
/// `permissionless`. Mutual-exclusion enforcement (can't have both) lives
/// in `check.rs`; here we only gate on the negative shape.
fn predicate_missing_signer(handler: &ParsedHandler) -> Option<Finding> {
    if handler.who.is_some() || handler.permissionless {
        return None;
    }

    Some(Finding {
        id: stable_id(&handler.name, "missing_signer"),
        category: Category::MissingSigner,
        severity: Severity::Critical,
        handler: handler.name.clone(),
        spec_silent_on: format!(
            "handler `{}` has no `auth` clause and is not marked `permissionless`",
            handler.name
        ),
        suppression_hint: format!(
            "Add `auth <actor>` to handler `{}` — or mark `permissionless` if intentional",
            handler.name
        ),
        investigation_hint: format!(
            "Open the impl for handler `{}`. Confirm authority is `Signer<'info>` (Anchor) \
             or has explicit `is_signer` check (native Rust). Absence is a real vulnerability.",
            handler.name
        ),
        category_tag: "missing_signer".to_string(),
        reproducer: None,
        gated_by: None,
    })
}

/// Spec-aware predicate: handler has a `writable` `token`-typed account
/// but declares no `transfers { ... }` block and no `call
/// Interface.handler(...)` site — codegen has nothing to mechanize, and
/// the impl may silently violate the handler's evident intent.
///
/// Auditor classification: usually a **spec-gap** finding, not a
/// real-vulnerability one. The auditor reads the body for `invoke` /
/// `invoke_signed`; present without spec coverage → escalate to
/// real-vulnerability.
fn predicate_arbitrary_cpi(handler: &ParsedHandler) -> Option<Finding> {
    if handler.has_calls() {
        return None;
    }
    // Init pattern: a handler transitioning from a "no-fields" pre-state
    // (Uninitialized / Empty / Inactive) creates accounts via System CPI —
    // writable token accounts are creation targets, not transfer targets.
    // Suppress: intent is captured structurally by the lifecycle transition.
    if let Some(pre) = handler.pre_status.as_deref() {
        if matches!(pre, "Uninitialized" | "Empty" | "Inactive") {
            return None;
        }
    }
    let writable_token = handler
        .accounts
        .iter()
        .find(|a| a.is_writable && a.account_type.as_deref() == Some("token") && !a.is_program)?;

    Some(Finding {
        id: stable_id(&handler.name, "arbitrary_cpi"),
        category: Category::ArbitraryCpi,
        severity: Severity::High,
        handler: handler.name.clone(),
        spec_silent_on: format!(
            "handler `{}` has writable token account `{}` but declares no `transfers` block or `call` site",
            handler.name, writable_token.name
        ),
        suppression_hint: format!(
            "Add `call Token.transfer(from = <src>, to = <dst>, amount = <amt>, authority = <signer>)` \
             to handler `{}` (the v2.5+ uniform CPI surface) — or the legacy `transfers {{ ... }}` sugar \
             which desugars to the same call. For non-Token CPIs, declare the interface and use \
             `call Interface.handler(...)`. Without one of these, the codegen cannot mechanize the transfer.",
            handler.name
        ),
        investigation_hint: format!(
            "Open the impl for handler `{}`. If the body has `invoke_signed` / `invoke` calls without \
             corresponding spec declarations, this is a real arbitrary-CPI vulnerability. \
             If the body is `todo!()` or empty, this is a spec-gap (impl incomplete).",
            handler.name
        ),
        category_tag: "arbitrary_cpi".to_string(),
        reproducer: None,
        gated_by: None,
    })
}

/// Spec-aware predicate: handler uses explicit non-default arithmetic
/// operators. Default `+=` / `-=` (checked, aborts on overflow) are
/// silent; the opt-in variants almost always carry a vulnerability story
/// on amount-shaped fields:
///
/// - **Wrapping** (`+=?` / `-=?`): silent overflow modulo 2^N — almost
///   always wrong on monetary amounts. HIGH.
/// - **Saturating** (`+=!` / `-=!`): caps at MAX/MIN, hiding bugs that
///   should propagate as errors; sometimes legitimate (rate limiters,
///   epoch counters). MEDIUM.
///
/// Fires once per (field, op) pair. The same pattern surfaces as
/// `wrapping_arithmetic` / `saturating_arithmetic` lints in `qedgen check`
/// (instant structural advisories); this probe finding is the
/// reproducer-bearing version.
fn predicate_arithmetic_overflow_wrapping(handler: &ParsedHandler) -> Vec<Finding> {
    let mut out = Vec::new();
    for (field, op, _value) in &handler.effects {
        let (severity, kind) = match op.as_str() {
            "add_wrap" | "sub_wrap" => (Severity::High, "wrapping"),
            "add_sat" | "sub_sat" => (Severity::Medium, "saturating"),
            _ => continue,
        };

        out.push(Finding {
            id: stable_id(
                &format!("{}::{}::{}", handler.name, field, op),
                "arithmetic_overflow_wrapping",
            ),
            category: Category::ArithmeticOverflowWrapping,
            severity,
            handler: handler.name.clone(),
            spec_silent_on: format!(
                "handler `{}` uses {} arithmetic on `{}` (op `{}`)",
                handler.name, kind, field, op
            ),
            suppression_hint: format!(
                "If the {} semantics are intended, document the invariant inline in the spec. \
                 If not, change the operator to `+=` / `-=` (default checked — aborts on overflow). \
                 Wrap/saturate on amount-shaped fields silently masks bugs.",
                kind
            ),
            investigation_hint: format!(
                "Open the impl for handler `{}`. Confirm the `{}` semantics are deliberate \
                 (e.g., epoch counter wrap, rate limiter saturation). For amount fields, \
                 wrap/saturate is almost always a vulnerability — consult the auditor's \
                 saturating-by-design suppression rules in SKILL.md.",
                handler.name, kind
            ),
            category_tag: "arithmetic_overflow_wrapping".to_string(),
            reproducer: None,
            gated_by: None,
        });
    }
    out
}

/// Spec-aware predicate: spec models lifecycle states but this handler
/// declares no `pre_status` AND mutates state (effects / transfers /
/// calls) — invokable in any program state: replay, ordering, and
/// init-after-close surface. Suppressed by `permissionless` or by specs
/// that don't model lifecycle at all. Usually a spec-gap finding;
/// real-vulnerability if the impl has cross-state replay paths.
fn predicate_lifecycle_one_shot_violation(
    handler: &ParsedHandler,
    spec_models_lifecycle: bool,
) -> Option<Finding> {
    if !spec_models_lifecycle {
        return None;
    }
    if handler.permissionless {
        return None;
    }
    if handler.pre_status.is_some() {
        return None;
    }
    let mutates_state =
        !handler.effects.is_empty() || !handler.transfers.is_empty() || handler.has_calls();
    if !mutates_state {
        return None;
    }

    Some(Finding {
        id: stable_id(&handler.name, "lifecycle_one_shot_violation"),
        category: Category::LifecycleOneShotViolation,
        severity: Severity::Medium,
        handler: handler.name.clone(),
        spec_silent_on: format!(
            "handler `{}` mutates state but declares no lifecycle pre-condition (`pre_status`); \
             spec models lifecycle states elsewhere",
            handler.name
        ),
        suppression_hint: format!(
            "Add a lifecycle clause (`: State.X -> State.Y`) to handler `{}` declaring which \
             state it operates on — or mark `permissionless` if intentionally always-callable.",
            handler.name
        ),
        investigation_hint: format!(
            "Open the impl for handler `{}`. Confirm it cannot be invoked in unintended states \
             (closed account, in-progress proposal, etc.). If reachable from multiple lifecycle \
             states without explicit handling, this is a real replay/ordering vulnerability.",
            handler.name
        ),
        category_tag: "lifecycle_one_shot_violation".to_string(),
        reproducer: None,
        gated_by: None,
    })
}

/// Spec-aware predicate: integer-shaped param flows into a
/// `transfers.amount` slot or an `effects` RHS with no bounding
/// `requires` clause. Composes with:
///
/// - `+ permissionless` → any caller can pass `u64::MAX` (drain / brick).
/// - `+ missing_signer` → the above + identity spoof.
/// - `+ arithmetic_overflow_wrapping` on the same field → fragile math +
///   unbounded input = exploit.
///
/// Detection is intentionally surface-level (word-boundary match on the
/// param name): false positives are acceptable — the auditor confirms in
/// the impl; false negatives are worse.
fn predicate_unbounded_amount_param(handler: &ParsedHandler) -> Vec<Finding> {
    let mut out = Vec::new();
    for (pname, ptype) in &handler.takes_params {
        if !is_integer_type(ptype) {
            continue;
        }

        let used_in_transfer = handler
            .transfers
            .iter()
            .any(|t| t.amount.as_deref() == Some(pname.as_str()));
        let used_in_effect = handler
            .effects
            .iter()
            .any(|(_, _, value)| param_referenced(value, pname));
        if !used_in_transfer && !used_in_effect {
            continue;
        }

        let bounded = handler
            .requires
            .iter()
            .any(|r| requires_bounds_param(&r.lean_expr, pname));
        if bounded {
            continue;
        }

        out.push(Finding {
            id: stable_id(
                &format!("{}::{}", handler.name, pname),
                "unbounded_amount_param",
            ),
            category: Category::UnboundedAmountParam,
            severity: Severity::High,
            handler: handler.name.clone(),
            spec_silent_on: format!(
                "handler `{}` accepts param `{}: {}` used in transfer/effect, \
                 but no `requires` clause bounds it",
                handler.name, pname, ptype
            ),
            suppression_hint: format!(
                "Add a bound: `requires {pname} <= <max> else <ErrorCode>` (or `> 0`, \
                 `< state.<bound>`). If the param is intentionally unbounded \
                 (e.g., admin governance setpoint), suppress with rationale."
            ),
            investigation_hint: format!(
                "Open the impl for handler `{}`. Check whether `{}` flows into \
                 a transfer amount, balance update, or PDA seed. Compose with \
                 `permissionless` and `missing_signer` findings on this same \
                 handler — the combined chain is usually the real vulnerability.",
                handler.name, pname
            ),
            category_tag: "unbounded_amount_param".to_string(),
            reproducer: None,
            gated_by: None,
        });
    }
    out
}

/// Spec-aware predicate: handler is `permissionless` AND has at least one
/// `effects` clause — permissionless writes to shared state are griefing
/// surface. Composes with `unbounded_amount_param` (any-value griefing),
/// `arithmetic_overflow_wrapping` (cheap overflow trigger), and
/// `lifecycle_one_shot_violation` (suppressed by `permissionless` itself,
/// but the chain applies if impl review finds an undeclared transition).
fn predicate_permissionless_state_writer(handler: &ParsedHandler) -> Option<Finding> {
    if !handler.permissionless {
        return None;
    }
    if handler.effects.is_empty() {
        return None;
    }

    let mutated_fields: Vec<&str> = handler.effects.iter().map(|(f, _, _)| f.as_str()).collect();

    Some(Finding {
        id: stable_id(&handler.name, "permissionless_state_writer"),
        category: Category::PermissionlessStateWriter,
        severity: Severity::High,
        handler: handler.name.clone(),
        spec_silent_on: format!(
            "handler `{}` is marked `permissionless` AND mutates state fields: {}",
            handler.name,
            mutated_fields.join(", ")
        ),
        suppression_hint: "Either (a) drop `permissionless` and add `auth <actor>`, or (b) ensure \
             the mutated fields cannot be griefed: per-actor PDAs, rate-limited \
             via cooldown / lifecycle, or bounded by `requires`. If the design is \
             intentional (truly public-callable like a crank), document the \
             griefing-acceptable rationale inline in the spec."
            .to_string(),
        investigation_hint: format!(
            "Open the impl for handler `{}`. The shared fields ({}) are writable \
             by any caller. Look for: missing rate limits, missing cooldowns, \
             unbounded amount params (compose with `unbounded_amount_param`), \
             missing per-actor PDA derivation. The corpus entry \
             `Frontrun the permissionless claim / crank` and Token-2022 \
             `transfer_hook_reentrancy` are common amplifiers.",
            handler.name,
            mutated_fields.join(", ")
        ),
        category_tag: "permissionless_state_writer".to_string(),
        reproducer: None,
        gated_by: None,
    })
}

/// Spec-aware predicate: init-shape handler with no writable account
/// declaring `pda` seeds — two distinct callers can target the same
/// canonical address; the second call fails noisily or overwrites the
/// first's state. Composes with `missing_signer` (front-run another
/// user's init) and the auditor-side `init_without_is_initialized`
/// (re-init replay).
///
/// "Init-shape" = `pre_status` ∈ {Uninitialized, Empty, Inactive}, same
/// convention as `predicate_arbitrary_cpi`. Specs starting in `Active`
/// (singleton / always-on) are out of scope — init-collision risk only
/// applies to multi-instance programs.
fn predicate_init_without_pda(
    handler: &ParsedHandler,
    _initial_state: Option<&str>,
) -> Option<Finding> {
    let pre = handler.pre_status.as_deref()?;
    if !matches!(pre, "Uninitialized" | "Empty" | "Inactive") {
        return None;
    }

    let writable_pda_present = handler
        .accounts
        .iter()
        .any(|a| a.is_writable && a.pda_seeds.is_some());
    if writable_pda_present {
        return None;
    }

    Some(Finding {
        id: stable_id(&handler.name, "init_without_pda"),
        category: Category::InitWithoutPda,
        severity: Severity::High,
        handler: handler.name.clone(),
        spec_silent_on: format!(
            "init-shape handler `{}` (pre_status `{}`) declares no writable PDA — \
             two callers may target the same canonical address",
            handler.name, pre
        ),
        suppression_hint:
            "Add a `pda` seed declaration to the writable account being initialized, \
             scoped to the caller's identity (e.g., `pda [\"<resource>\", payer]`) \
             or the resource's identity (e.g., `pda [\"<resource>\", <id>]`). \
             Without per-caller / per-resource scoping, `init_without_is_initialized` \
             becomes reachable across callers."
                .to_string(),
        investigation_hint: format!(
            "Open the impl for handler `{}`. Check Anchor `#[account(init, ..., \
             seeds = [...])]` on the writable account. If `seeds` is missing or \
             doesn't include the caller pubkey / resource id, this is a real \
             account-collision vulnerability. Compose with `missing_signer` for \
             the full takeover chain.",
            handler.name
        ),
        category_tag: "init_without_pda".to_string(),
        reproducer: None,
        gated_by: None,
    })
}

/// Spec-aware predicate: state field read somewhere in the spec (`auth`,
/// `requires` / `aborts_if`, effect RHS, property expression) but never
/// written by any handler `effect` — downstream codegen sees only the
/// type's default. Two recurring CRIT shapes:
/// - `auth <pubkey-field>` lowers to `has_one = <field>`; an unset Pubkey
///   is the zero key — no signer satisfies it, handler unreachable.
/// - Counter read by a `preserved_by all` invariant but never updated —
///   the invariant proves vacuously.
///
/// Composes with auditor-side `partial_has_one_chain` (missing writer
/// makes the chain partial) and `field_chain_missing_root_anchor` (when
/// the field is a stored authority anchor).
fn predicate_stored_field_never_written(spec: &ParsedSpec) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Step 1: collect every field name that any handler `effect` writes.
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for h in &spec.handlers {
        for (field, _, _) in &h.effects {
            written.insert(field.as_str());
        }
    }
    // PDA-seed fields are bound implicitly by codegen at init; treat as
    // written — spec authors don't write an explicit
    // `initializer := initializer.key()` effect for the canonical
    // `pda X ["X", initializer]` shape.
    for pda in &spec.pdas {
        for seed in &pda.seeds {
            written.insert(seed.as_str());
        }
    }

    // Step 2: for every unwritten state field, search for readers.
    // Fields neither written nor read are the dead-code axis
    // (`write_without_read`'s complement), not this predicate's concern.
    for acct in &spec.account_types {
        for (field, _ty) in &acct.fields {
            if written.contains(field.as_str()) {
                continue;
            }

            let needles = [format!("state.{}", field), format!("s.{}", field)];

            let mut readers: Vec<&str> = Vec::new();
            for h in &spec.handlers {
                let mut is_reader = false;

                // `auth <field>` is a read of the stored Pubkey by the
                // codegen-emitted `has_one = <field>` constraint.
                if h.who.as_deref() == Some(field.as_str()) {
                    is_reader = true;
                }

                // requires clauses (Lean form is the canonical text).
                if !is_reader {
                    for r in &h.requires {
                        if needles.iter().any(|n| r.lean_expr.contains(n.as_str())) {
                            is_reader = true;
                            break;
                        }
                    }
                }

                // legacy guard string + aborts_if (pre-requires DSL).
                if !is_reader {
                    if let Some(g) = &h.guard_str {
                        if needles.iter().any(|n| g.contains(n.as_str())) {
                            is_reader = true;
                        }
                    }
                }
                if !is_reader {
                    for a in &h.aborts_if {
                        if needles.iter().any(|n| a.lean_expr.contains(n.as_str())) {
                            is_reader = true;
                            break;
                        }
                    }
                }

                // effect RHS reads (e.g. `field := s.other_field + 1`).
                if !is_reader {
                    for (_, _, rhs) in &h.effects {
                        if needles.iter().any(|n| rhs.contains(n.as_str())) {
                            is_reader = true;
                            break;
                        }
                    }
                }

                if is_reader {
                    readers.push(h.name.as_str());
                }
            }

            // Top-level property expressions (incl. `preserved_by all`)
            // are the most common second source of reads.
            let mut prop_reads = false;
            for prop in &spec.properties {
                if let Some(expr) = &prop.expression {
                    if needles.iter().any(|n| expr.contains(n.as_str())) {
                        prop_reads = true;
                        break;
                    }
                }
            }

            if readers.is_empty() && !prop_reads {
                continue;
            }

            let primary = readers
                .first()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "_property".to_string());

            let read_summary = if readers.is_empty() {
                "a property expression".to_string()
            } else if readers.len() == 1 {
                format!("handler `{}`", readers[0])
            } else {
                format!("handlers [{}]", readers.join(", "))
            };
            let read_extra = if !readers.is_empty() && prop_reads {
                " and a property expression"
            } else {
                ""
            };

            findings.push(Finding {
                id: stable_id(
                    &format!("{}::{}", acct.name, field),
                    "stored_field_never_written",
                ),
                category: Category::StoredFieldNeverWritten,
                severity: Severity::Critical,
                handler: primary,
                spec_silent_on: format!(
                    "field `{}` declared on `{}` and read by {}{} but never written by any handler `effect`",
                    field, acct.name, read_summary, read_extra
                ),
                suppression_hint: format!(
                    "Either (a) add an `effect` writing `state.{field}` in the appropriate handler — typically the init-shape handler that populates this field at create time — or (b) remove the field from the state declaration if it's truly unused, or (c) initialize it at the declared default if the type's zero value is intentional and document why."
                ),
                investigation_hint: format!(
                    "Open the impl. On Quasar/Anchor, `auth {field}` lowers to `has_one = {field}` — if `state.{field}` is the zero pubkey (default), no signer can satisfy the constraint and the handler is unreachable (escrow `taker` / multisig `creator` shape). On counter-shaped fields read by a `preserved_by all` invariant, the invariant proves vacuously because the field is constant (lending `total_borrows` shape). Look for: pre-deploy state population from migrations, handlers that should write the field but don't, or hand-edits to codegen that diverge from the spec."
                ),
                category_tag: "stored_field_never_written".to_string(),
                reproducer: None,
                gated_by: None,
            });
        }
    }

    findings
}

/// Integer-typed DSL types — the scalar quantities that flow into
/// transfer amounts or arithmetic effects.
fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" | "Nat"
    )
}

/// Word-boundary substring match for `param` in `value`. Surface-level
/// by design — misses obfuscated forms; the auditor is the backstop.
fn param_referenced(value: &str, param: &str) -> bool {
    let bytes = value.as_bytes();
    let pbytes = param.as_bytes();
    let plen = pbytes.len();
    if plen == 0 || bytes.len() < plen {
        return false;
    }
    let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for i in 0..=bytes.len().saturating_sub(plen) {
        if &bytes[i..i + plen] != pbytes {
            continue;
        }
        let prev_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        let next_ok = i + plen == bytes.len() || !is_ident_byte(bytes[i + plen]);
        if prev_ok && next_ok {
            return true;
        }
    }
    false
}

/// True when `expr` looks like an *upper* bound on `param` — LHS-bounded
/// (`param <[=] X`) or RHS-bounded (`X >[=] param`). Equality also
/// suppresses (fixed value, no overflow surface). Lower-only bounds
/// (`param > 0`) do NOT — they don't constrain the dangerous `u64::MAX`
/// side.
fn requires_bounds_param(expr: &str, param: &str) -> bool {
    if !param_referenced(expr, param) {
        return false;
    }

    // Equality / inequality fix the param — cheap escape hatch.
    if expr.contains("==") || expr.contains("!=") || expr.contains('\u{2260}') {
        return true;
    }

    // Whitespace-tokenize and scan (lhs, op, rhs) triples for any
    // upper-bound shape; multi-conjunct exprs (`a > 0 && a < MAX`) count
    // if any conjunct qualifies.
    let normalized = expr
        .replace('\u{2264}', "<=")
        .replace('\u{2265}', ">=")
        .replace("&&", " ")
        .replace("||", " ")
        .replace(" and ", " ")
        .replace(" or ", " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();

    let upper_ops = ["<", "<="];
    let lower_ops = [">", ">="];

    for w in tokens.windows(3) {
        let (lhs, op, rhs) = (w[0], w[1], w[2]);
        // LHS-bounded upper: `param <[=] _`
        if lhs == param && upper_ops.contains(&op) {
            return true;
        }
        // RHS-bounded upper: `_ >[=] param`
        if rhs == param && lower_ops.contains(&op) {
            return true;
        }
    }
    false
}

fn stable_id(handler: &str, category: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(handler.as_bytes());
    hasher.update(b":");
    hasher.update(category.as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash).chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter::parse_str;

    fn make_handler(name: &str, who: Option<&str>, permissionless: bool) -> ParsedHandler {
        ParsedHandler {
            name: name.to_string(),
            doc: None,
            who: who.map(|s| s.to_string()),
            on_account: None,
            pre_status: None,
            post_status: None,
            takes_params: vec![],
            guard_str: None,
            guard_str_rust: None,
            aborts_if: vec![],
            requires: vec![],
            ensures: vec![],
            modifies: None,
            let_bindings: vec![],
            aborts_total: false,
            permissionless,
            effects: vec![],
            effects_rust: vec![],
            effect_on_error: vec![],
            effects_tree: vec![],
            accounts: vec![],
            transfers: vec![],
            emits: vec![],
            invariants: vec![],
            establishes: vec![],
            properties: vec![],
            schema_includes: vec![],
            calls: vec![],
            effect_branches: None,
            abstract_binders: vec![],
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
        h.effects
            .push(("epoch".to_string(), "add_wrap".to_string(), "1".to_string()));
        let findings = predicate_arithmetic_overflow_wrapping(&h);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category_tag, "arithmetic_overflow_wrapping");
        assert!(findings[0].spec_silent_on.contains("wrapping"));
    }

    #[test]
    fn arith_predicate_fires_on_saturating() {
        let mut h = make_handler("apply", Some("user"), false);
        h.effects.push((
            "balance".to_string(),
            "add_sat".to_string(),
            "delta".to_string(),
        ));
        let findings = predicate_arithmetic_overflow_wrapping(&h);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].spec_silent_on.contains("saturating"));
    }

    #[test]
    fn arith_predicate_silent_on_default_checked() {
        let mut h = make_handler("deposit", Some("user"), false);
        h.effects
            .push(("total".to_string(), "add".to_string(), "amount".to_string()));
        h.effects.push((
            "fee_pool".to_string(),
            "sub".to_string(),
            "amount".to_string(),
        ));
        h.effects
            .push(("balance".to_string(), "set".to_string(), "x".to_string()));
        assert!(predicate_arithmetic_overflow_wrapping(&h).is_empty());
    }

    #[test]
    fn arith_predicate_fires_per_op() {
        let mut h = make_handler("complex", Some("user"), false);
        h.effects
            .push(("a".to_string(), "add_wrap".to_string(), "1".to_string()));
        h.effects
            .push(("b".to_string(), "add_sat".to_string(), "delta".to_string()));
        let findings = predicate_arithmetic_overflow_wrapping(&h);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn lifecycle_predicate_fires_when_state_mutating_no_pre_status() {
        let mut h = make_handler("withdraw", Some("user"), false);
        h.effects
            .push(("balance".to_string(), "set".to_string(), "0".to_string()));
        let f =
            predicate_lifecycle_one_shot_violation(&h, true).expect("expected lifecycle finding");
        assert_eq!(f.category_tag, "lifecycle_one_shot_violation");
    }

    #[test]
    fn lifecycle_predicate_silent_when_pre_status_declared() {
        let mut h = make_handler("withdraw", Some("user"), false);
        h.pre_status = Some("Active".to_string());
        h.effects
            .push(("balance".to_string(), "set".to_string(), "0".to_string()));
        assert!(predicate_lifecycle_one_shot_violation(&h, true).is_none());
    }

    #[test]
    fn lifecycle_predicate_silent_when_permissionless() {
        let mut h = make_handler("crank", None, true);
        h.effects
            .push(("x".to_string(), "set".to_string(), "1".to_string()));
        assert!(predicate_lifecycle_one_shot_violation(&h, true).is_none());
    }

    #[test]
    fn lifecycle_predicate_silent_when_spec_has_no_lifecycle() {
        let mut h = make_handler("withdraw", Some("user"), false);
        h.effects
            .push(("balance".to_string(), "set".to_string(), "0".to_string()));
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

pub(crate) mod arithmetic_symbol_probe;
pub(crate) mod cluster;
pub(crate) mod crucible_brownfield;
pub(crate) mod crucible_probe;
pub(crate) mod handler_intent;
pub(crate) mod lifecycle_probe;
pub(crate) mod paired_validator_probe;
pub(crate) mod pinocchio_probe;
pub(crate) mod probe_repro;
pub(crate) mod prompts;
pub(crate) mod ratify;
pub(crate) mod shank_probe;
