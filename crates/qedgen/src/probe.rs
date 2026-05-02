//! `qedgen probe` — spec-coverage gap analyzer.
//!
//! Walks a parsed `.qedspec` and emits structured findings describing
//! categories the spec is silent on. Output is JSON, consumed by the
//! harness-native auditor subagent (CI / non-agent users can read the
//! same JSON directly). The CLI does **not** read implementation source
//! — that's the auditor's job. Predicates here are runtime-agnostic
//! (operate on the spec) by design; per-runtime spec-less predicates
//! live in the auditor SKILL.md.
//!
//! Spec-aware categories: `missing_signer`, `arbitrary_cpi`,
//! `arithmetic_overflow_wrapping`, `lifecycle_one_shot_violation`,
//! `unbounded_amount_param`, `permissionless_state_writer`,
//! `init_without_pda`, `stored_field_never_written`. Each is a
//! *compose-able primitive* — the
//! auditor subagent chains them into kill-chains (see SKILL.md
//! "Compose-with-what cookbook"). Spec-less / impl-side categories
//! (`cpi_param_swap`, `pda_canonical_bump`, `account_type_confusion`,
//! `close_account_redirection`, `oracle_staleness`, etc.) live in
//! the auditor SKILL.md per-runtime predicates — they need source
//! reading the CLI doesn't do.

use anyhow::{anyhow, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::anchor_project::parse_anchor_project;
use crate::check::{parse_spec_file, ParsedHandler, ParsedSpec};

/// Probe output schema version. Bump on incompatible finding-shape changes;
/// the auditor pins against this.
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
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
    /// spec (`auth <field>`, a `requires`/`aborts_if` referencing
    /// `state.<field>`, an `effect` RHS, or a property expression) but
    /// never written by any handler `effect`. On Quasar/Anchor, `auth X`
    /// lowers to `has_one = X`, so an unset Pubkey field makes the
    /// constraint unsatisfiable. On counter-shaped fields, a
    /// `preserved_by all` invariant proves vacuously because the value
    /// is constant. Recurring shape across multisig, escrow, lending,
    /// and percolator audits.
    StoredFieldNeverWritten,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Low used by upcoming categories
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
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
    /// Detection inconclusive — auditor falls back to source-walking.
    Unknown,
}

/// One discovered handler in bootstrap (spec-less) mode. Auditor reads
/// `source_file` to investigate per-handler categories.
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapHandler {
    pub name: String,
    /// Path to the source file containing the handler, relative to
    /// `project_root` if possible. Auditor uses this for Read tool dispatch.
    pub source_file: String,
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

    Ok(ProbeOutput {
        version: SCHEMA_VERSION,
        mode: Mode::SpecAware,
        spec_path: Some(spec_path.display().to_string()),
        project_root: None,
        runtime: None,
        handlers: None,
        applicable_categories: None,
        findings,
    })
}

/// Spec-less probe (the `--bootstrap` mode). Walks a project root,
/// detects runtime, discovers handlers, and emits the work-list envelope
/// the auditor consumes. **The CLI does not investigate handlers in this
/// mode** — that's the auditor's job per the v2.10 architecture
/// (`feedback_audit_as_subagent.md`). The CLI's role is structured
/// dispatch: tell the auditor what runtime, which handlers, and which
/// categories to investigate.
///
/// Per-runtime handler discovery in v2.10:
/// - **Anchor**: `parse_anchor_project` walks the program crate's
///   `lib.rs`, finds the `#[program]` mod, lists its `pub fn`s.
/// - **Native / sBPF / qedgen-codegen**: handler list is left empty;
///   auditor walks source directly via Read+Grep. Future v2.x adds
///   per-runtime discovery as adoption demand justifies.
pub fn run_bootstrap(project_root: &Path) -> Result<ProbeOutput> {
    if !project_root.exists() {
        return Err(anyhow!(
            "project root does not exist: {}",
            project_root.display()
        ));
    }

    let runtime = detect_runtime(project_root);
    let handlers = match runtime {
        // Quasar's `#[program] mod` form is structurally compatible with
        // the Anchor parser — `#[instruction(discriminator = N)]` is an
        // extra attribute that doesn't disturb `pub fn` extraction.
        Runtime::Anchor | Runtime::Quasar | Runtime::QedgenCodegen => {
            discover_anchor_handlers(project_root).unwrap_or_default()
        }
        _ => Vec::new(),
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
    })
}

/// Runtime detection by filesystem heuristics. Order matters: a project
/// with both `Anchor.toml` and `solana-program` dep is Anchor.
fn detect_runtime(root: &Path) -> Runtime {
    // QedgenCodegen wins over Anchor.toml: codegen examples scaffold an
    // `Anchor.toml` for the test harness alongside the actual Quasar
    // program. Without this precedence, a qedgen-codegen scaffold would
    // be misclassified as Anchor and skip the Quasar-specific category
    // overlay (`stored_field_never_written` etc.).
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
        if content.contains("quasar-lang") {
            // Distinguish hand-written Quasar from qedgen-codegen output:
            // codegen leaves a `formal_verification/` dir, a `qed.toml`,
            // or `#[qed(verified)]` markers in source. Without any of
            // those, treat as hand-written Quasar (Anchor-shaped surface
            // plus Quasar-specific shapes).
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

/// Did codegen run against this crate? Three independent signals; any
/// one is sufficient. Used to split `Runtime::Quasar` (hand-written)
/// from `Runtime::QedgenCodegen` when the Cargo dep alone is ambiguous.
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

/// Wrap `anchor_project::parse_anchor_project` to map discovered
/// instructions into `BootstrapHandler` entries. Returns empty vec on
/// failure (auditor falls back to source-walking).
///
/// Handles two layouts:
/// 1. **Program crate root** — `<root>/src/lib.rs` exists. Single
///    `#[program]` mod parsed directly.
/// 2. **Anchor workspace root** — `<root>/programs/*/src/lib.rs`
///    exists. Each child crate is parsed independently and handlers
///    are aggregated. Brownfield users naturally point at workspace
///    roots, so this is the common case.
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
        })
        .collect())
}

/// Categories the auditor should investigate per runtime in spec-less
/// mode. Reflects the v2.10 design table in
/// `docs/prds/PRD-v2.10.md` (runtime coverage section).
fn applicable_categories(runtime: &Runtime) -> Vec<String> {
    let universal = [
        "missing_signer",
        "arbitrary_cpi",
        "arithmetic_overflow_wrapping",
        "lifecycle_one_shot_violation",
    ];
    let anchor_native = ["cpi_param_swap", "pda_canonical_bump"];
    // QedgenCodegen runtime: codegen mechanizes the "universal" categories
    // from the spec, so they don't apply at user-owned handler-body level.
    // What does apply: handler-body-level numeric / lifecycle bugs and the
    // Quasar-specific drift / unanchored-field / bounty-intent shapes added
    // in v2.13.
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
    // Multi-actor / quorum primitive family — added to the v2.15 SKILL.md
    // catalog from the external multisig audit, but the prior release
    // stamped them only as prose. The auditor caught the multisig
    // duplicate-signer CRIT through the escalation rule rather than
    // through a structured `applicable_categories` listing. Surface
    // the family here so the agent walks it as part of the standard
    // category catalog on any program that ships a multi-party state
    // shape.
    let multi_actor = [
        "quorum_dup_inflation",
        "quorum_set_dup_at_init",
        "nonce_absent_action_replay",
        "creator_admin_outside_quorum",
        "signer_set_pinned_to_creator_pda_only",
    ];

    match runtime {
        Runtime::Anchor | Runtime::Native => universal
            .iter()
            .chain(anchor_native.iter())
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
        Runtime::Unknown => universal.iter().map(|s| s.to_string()).collect(),
    }
}

/// Spec-aware predicate: handler has no `auth X` clause and is not marked
/// `permissionless`. Both fields land in `ParsedHandler` from the chumsky
/// adapter (`who: Option<String>`, `permissionless: bool`).
///
/// Mutually-exclusive enforcement (handler can't have both `auth X` and
/// `permissionless`) already lives in `check.rs`; here we just gate on
/// the negative shape.
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
    })
}

/// Spec-aware predicate: handler has a `writable` `token`-typed account
/// (which signals external token state will change) but the spec declares
/// no `transfers { ... }` block and no `call Interface.handler(...)` site.
/// Without a CPI declaration, codegen has nothing to mechanize; the user
/// is left to fill `todo!()` by hand or — worse — the impl emits no
/// transfer at all and silently violates the handler's evident intent.
///
/// Auditor classification (per SKILL.md draft): this is usually a
/// **spec-gap** finding (impl is incomplete or under-specified) rather
/// than a real-vulnerability finding (impl is doing arbitrary CPI). The
/// auditor confirms by reading the handler body for `invoke` /
/// `invoke_signed` calls; if present without spec coverage, escalate to
/// real-vulnerability.
fn predicate_arbitrary_cpi(handler: &ParsedHandler) -> Option<Finding> {
    if handler.has_calls() {
        return None;
    }
    // Init pattern: handler transitioning from a "no-fields" pre-state
    // (Uninitialized / Empty / Inactive) is creating accounts via System
    // Program CPI, not transferring tokens. Writable token accounts in
    // this shape are creation targets, not transfer targets. Suppress
    // the finding — spec-author intent is captured structurally by the
    // lifecycle transition.
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
    })
}

/// Spec-aware predicate: handler uses explicit non-default arithmetic
/// operators (`+=?` / `-=?` wrapping, or `+=!` / `-=!` saturating).
/// Default `+=` / `-=` (v2.7 G3 checked semantics) are silent — they
/// abort on overflow, which is the safe default. The non-default
/// variants are explicit user opt-ins that almost always carry a
/// vulnerability story for amount-shaped fields:
///
/// - **Wrapping** (`+=?` / `-=?`): silent overflow modulo 2^N. Almost
///   always wrong on monetary amounts. Severity: HIGH.
/// - **Saturating** (`+=!` / `-=!`): caps at MAX/MIN. Hides bugs that
///   should propagate as errors. Sometimes legitimate (rate limiters,
///   epoch counters). Severity: MEDIUM.
///
/// Fires once per (field, op) pair on the handler. Auditor SKILL.md
/// classification rules separate "intentional design" (suppress with
/// rationale comment) from "real vulnerability" (change to default `+=`).
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
        });
    }
    out
}

/// Spec-aware predicate: spec models lifecycle states (either via top-level
/// `state ... lifecycle [...]` or per-account-type lifecycle), but this
/// handler declares no `pre_status` AND mutates state in some way
/// (effects / transfers / calls). Without a lifecycle gate, the handler
/// can be invoked in any program state — replay surface, ordering
/// surface, init-after-close surface.
///
/// Suppressed by:
/// - `permissionless` marker (handler is intentionally always-callable)
/// - the spec doesn't model lifecycle at all (stateless program — no gate
///   to declare)
///
/// Auditor classification: usually a spec-gap finding (state machine is
/// modeled but this handler is undeclared). Real-vulnerability if the
/// impl actually has cross-state replay paths the spec is silent on.
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
    })
}

/// Spec-aware predicate: handler takes an integer-shaped parameter that
/// flows into a `transfers.amount` slot or an `effects` value RHS, but no
/// `requires` clause bounds the parameter. The agent should compose this
/// finding with the rest of the handler shape:
///
/// - `+ permissionless` → any caller can pass `u64::MAX`, draining /
///   bricking the system depending on what the param controls.
/// - `+ missing_signer` → any caller can do the above + spoof identity.
/// - `+ arithmetic_overflow_wrapping` on the same field → silent overflow
///   to a wrong post-state (the wrap finding tells you the math is fragile;
///   this one tells you the input is unbounded; together = exploit).
///
/// Detection is intentionally surface-level (substring match on the
/// param name). False positives are acceptable — the auditor reads the
/// impl to confirm. False negatives (missing a bounded param) are worse.
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
        });
    }
    out
}

/// Spec-aware predicate: handler is marked `permissionless` AND has at
/// least one `effects` clause. Permissionless writes to shared state are
/// griefing surface — anyone can call repeatedly, fill the field, contend
/// with the legitimate caller, or chain with another finding to escalate.
///
/// Composes with:
/// - `unbounded_amount_param` → any value griefing
/// - `arithmetic_overflow_wrapping` → cheap overflow trigger
/// - `lifecycle_one_shot_violation` (suppressed by `permissionless` itself,
///   but the chain still applies if the agent finds an undeclared state
///   transition during impl review)
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
    })
}

/// Spec-aware predicate: init-shape handler (matches one of the canonical
/// init-state names) but no writable account in the handler's `accounts`
/// block declares `pda` seeds. Without a PDA, two distinct callers can
/// both target the same canonical address; the second call either fails
/// noisily or — worse — overwrites the first's state.
///
/// Composes with:
/// - `missing_signer` → spoof another user's init by racing them or
///   front-running with attacker-controlled signer/payer
/// - `init_without_is_initialized` (per-runtime auditor predicate) → re-init
///   replay if the impl doesn't guard
///
/// "Init-shape" is matched by `pre_status` ∈ {Uninitialized, Empty,
/// Inactive} — the same convention `predicate_arbitrary_cpi` uses to
/// recognize the init pattern. Specs without those states (e.g., a
/// lifecycle that starts in `Active` because the program runs as a
/// singleton or always-on engine) are out of scope for this probe;
/// init-collision risk only applies to multi-instance programs.
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
    })
}

/// Spec-aware predicate: state field declared on an `account` type but
/// never written by any handler `effect`, while being read somewhere in
/// the spec — `auth <field>`, a `requires` / `aborts_if` referencing
/// `state.<field>`, an effect RHS, or a property expression.
///
/// Reading without writing means downstream codegen lowerings see only
/// the type's default. Two CRIT shapes recur across audits:
/// - `auth <pubkey-field>` lowers to `has_one = <field>` — an unset
///   Pubkey is the zero key, no signer can satisfy the constraint, the
///   handler is unreachable. Caught the multisig `creator` and escrow
///   `taker` shapes.
/// - Counter / accumulator field read by a `preserved_by all` invariant
///   but never updated — invariant proves vacuously because the value
///   is constant. Caught lending's `total_borrows` shape.
///
/// Composes with:
/// - `partial_has_one_chain` (auditor side): even if some `has_one`
///   constraints are present, this field's missing writer makes the
///   chain partial.
/// - `field_chain_missing_root_anchor`: when the never-written field is
///   a stored authority anchor.
fn predicate_stored_field_never_written(spec: &ParsedSpec) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Step 1: collect every field name that any handler `effect` writes.
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for h in &spec.handlers {
        for (field, _, _) in &h.effects {
            written.insert(field.as_str());
        }
    }
    // Fields used as PDA seeds are bound implicitly by codegen at init
    // (the seed value populates the field as part of address derivation).
    // Treat them as written to avoid flagging — spec authors don't write
    // an explicit `initializer := initializer.key()` effect for the
    // canonical `pda X ["X", initializer]` shape.
    for pda in &spec.pdas {
        for seed in &pda.seeds {
            written.insert(seed.as_str());
        }
    }

    // Step 2: for every declared state field that is NOT written,
    // search for readers. Skip fields that are neither written nor
    // read — that's the `write_without_read` lint's complement on
    // the dead-code axis, not what this predicate is about.
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

            // Property expressions (top-level, including `preserved_by all`
            // invariants) are the most common second-source of reads.
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
            });
        }
    }

    findings
}

/// True for the integer-typed DSL types `qedgen probe` reasons about.
/// Matches what `unbounded_amount_param` cares about: scalar quantities
/// that flow into transfer amounts or arithmetic effects.
fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" | "Nat"
    )
}

/// Substring match on word-boundary references to `param` in `value`.
/// Surface-level: catches `param`, `state.x + param`, `wrapping_add(param)`,
/// `param * 2`. Misses obfuscated forms — that's OK; the auditor is the
/// real backstop.
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

/// True when `expr` looks like an *upper* bound on `param`. Lower-only
/// bounds (`amount > 0`) don't suppress the finding — those don't
/// constrain the dangerous side (`u64::MAX`) of an amount param flowing
/// into a transfer. We accept either form:
///
/// - LHS-bounded: `param < X`, `param <= X`, `param ≤ X`
/// - RHS-bounded: `X > param`, `X >= param`, `X ≥ param`
///
/// Equality (`param == X`) also suppresses — fixed value, no overflow
/// surface. Lower-only forms (`param > 0`, `param >= 1`) do NOT suppress.
fn requires_bounds_param(expr: &str, param: &str) -> bool {
    if !param_referenced(expr, param) {
        return false;
    }

    // Equality / inequality fix the param exactly or constrain it from
    // above implicitly. Cheap escape hatch.
    if expr.contains("==") || expr.contains("!=") || expr.contains('\u{2260}') {
        return true;
    }

    // Tokenize-ish: split on whitespace and look for a (lhs, op, rhs)
    // triple where the param is on the bounded side of an inequality.
    // Multi-conjunct expressions (`a > 0 && a < MAX`) are scanned for
    // any bound that satisfies the upper-bound shape.
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

    // Sliding window of length 3.
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
            accounts: vec![],
            transfers: vec![],
            emits: vec![],
            invariants: vec![],
            properties: vec![],
            calls: vec![],
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
}
