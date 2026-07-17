//! Invariant hypothesizer — spec elicitation, Phase 1 (PRD:
//! `docs/design/spec-elicitation-prd.md` §6.1).
//!
//! Consumes evidence the probe already computes (discovered handlers,
//! IDL signer flags, intent tags, handler bodies) and emits
//! program-specific `InvariantHypothesis` records: claim + evidence +
//! payoff + confidence + how-to-lower. The binary owns only the
//! deterministic, evidence-anchored classes (D3); deep cross-procedure
//! hypothesis formation stays with the agent.
//!
//! Precision rule (the make-or-break): **no evidence anchor → no
//! hypothesis.** A handler name alone never fires a detector; every
//! hypothesis cites at least one source- or IDL-derived anchor beyond
//! naming. Five right beats thirty speculative.
//!
//! Phase 1 shipped the two highest-prior classes (Authorization,
//! Lifecycle/init-once); Phase 5 adds Arithmetic-bound (a held bound
//! check lifted from the body), Conservation (paired operations with no
//! supply-changing flows), CPI-integrity (a pinned SPL-token transfer
//! with resolved direction), Unwired-guard-as-question (a #240
//! dead-guard candidate flipped into "you named this check but never
//! wired it — should it hold?"), and State-machine (an IDL-declared
//! status enum lifted into the spec's `type State`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::cluster::Confidence;
use super::handler_intent;
use super::BootstrapHandler;

/// Hypothesis class — mirrors the PRD §6.3 lowering table rows shipped so
/// far. Extend alongside a lowering contract, never ahead of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisClass {
    Authorization,
    LifecycleInitOnce,
    ArithmeticBound,
    Conservation,
    CpiIntegrity,
    UnwiredGuard,
    StateMachine,
}

impl HypothesisClass {
    pub fn as_str(self) -> &'static str {
        match self {
            HypothesisClass::Authorization => "authorization",
            HypothesisClass::LifecycleInitOnce => "lifecycle_init_once",
            HypothesisClass::ArithmeticBound => "arithmetic_bound",
            HypothesisClass::Conservation => "conservation",
            HypothesisClass::CpiIntegrity => "cpi_integrity",
            HypothesisClass::UnwiredGuard => "unwired_guard",
            HypothesisClass::StateMachine => "state_machine",
        }
    }
}

/// One citable reason the detector believes the claim. `detail` is
/// human-readable; `source` names the file (or IDL) the evidence came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceAnchor {
    /// Anchor kind: `idl_signer_flag`, `authority_comparison`,
    /// `authority_assert_helper`, `init_guard_in_body`,
    /// `anchor_init_constraint`.
    pub kind: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Deterministic lowering recipe `ratify` executes on accept. Absent =
/// the claim is confirmable but not mechanically lowerable yet; ratify
/// reports `confirmed, not executable` instead of inserting a placeholder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HypothesisLowering {
    /// Insert `auth <signer_account>` into the handler body.
    AuthClause { signer_account: String },
    /// Ensure the handler declares an init-shaped `: State.<pre> ->
    /// State.<post>` transition (variants resolved against the skeleton's
    /// `type State` at ratify time).
    LifecycleTransition,
    /// Insert `requires <param> <op> <bound> else <error>` into the
    /// handler body (and declare the error variant if missing).
    RequiresBound {
        param: String,
        /// `<=` or `<` — mirrors the check found in the body.
        op: String,
        bound: String,
        error: String,
    },
    /// Insert `transfers { from <from> to <to> [amount <amount>]
    /// [authority <authority>] }` into the handler body.
    TransfersClause {
        from: String,
        to: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        amount: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        authority: Option<String>,
    },
    /// Replace the skeleton's placeholder `type State` variants with the
    /// program's real status-enum variants (positional `State.<old>` →
    /// `State.<new>` reference rewrite). Applied before any transition
    /// lowering so lifecycle edges resolve against the real variants.
    StateAdtRewrite { variants: Vec<String> },
}

/// A confirmable, evidence-anchored claim about what this program appears
/// to guarantee. The unit of the in-harness elicitation interview; on
/// `accept` ratify lowers it to an executable `.qedspec` clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantHypothesis {
    /// Deterministic hash of `(class, handler)` — stable across runs on an
    /// unchanged program, so answer sets survive re-probing.
    pub id: String,
    pub class: HypothesisClass,
    pub handler: String,
    /// The claim, phrased about *this* program (not a generic template).
    pub claim: String,
    pub evidence: Vec<EvidenceAnchor>,
    /// What confirming buys — the conversion copy (§6.4). Names only the
    /// strongest honestly-available assurance.
    pub payoff: String,
    /// Strongest available backend for this claim as detected, e.g.
    /// `checking` / `proptest (model-tested)` / `impl-kani`.
    pub backend: String,
    /// Assurance level (§3.1) the clause earns immediately on ratify:
    /// always `checking` at emission; stronger levels require running a
    /// backend and are never claimed here.
    pub assurance: String,
    pub confidence: Confidence,
    /// Lowering recipe; `None` = confirmed answers stay in the dossier as
    /// `confirmed, not executable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lowering: Option<HypothesisLowering>,
}

/// Phase-0 funnel instrumentation: hypothesis supply counts, by class and
/// confidence. Ratify-side outcome counts live in
/// `elicitation-outcome.json`; joined on `run_id` they measure conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecReadiness {
    pub hypotheses_total: usize,
    pub by_class: BTreeMap<String, usize>,
    pub by_confidence: BTreeMap<String, usize>,
    /// Hypotheses carrying a mechanical lowering recipe.
    pub lowerable: usize,
}

pub fn spec_readiness(hypotheses: &[InvariantHypothesis]) -> SpecReadiness {
    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_confidence: BTreeMap<String, usize> = BTreeMap::new();
    for h in hypotheses {
        *by_class.entry(h.class.as_str().to_string()).or_default() += 1;
        let conf = match h.confidence {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        };
        *by_confidence.entry(conf.to_string()).or_default() += 1;
    }
    SpecReadiness {
        hypotheses_total: hypotheses.len(),
        by_class,
        by_confidence,
        lowerable: hypotheses.iter().filter(|h| h.lowering.is_some()).count(),
    }
}

/// Per-handler IDL analysis facts the hypothesizer consumes beyond the
/// signer/writable flags already riding on `handlers[]` (Phase 4: the
/// IDL is an evidence *source*, folding in what `spec --idl` used to
/// turn into a TODO shell). Keyed by snake_case instruction name.
///
/// `has_one` relations surface as `relations` on an account in Anchor
/// IDLs; Codama IDLs (and Anchor programs without `has_one`) omit the
/// key, so an empty map means "unknown", never "no relationship".
fn idl_has_one_by_handler(project_root: &Path) -> BTreeMap<String, Vec<(String, String)>> {
    let mut map = BTreeMap::new();
    let Ok(Some((idl_path, _))) = super::idl_overlay::discover_idl(project_root) else {
        return map;
    };
    let Ok((_, analyses)) = crate::idl::parse_idl(&idl_path) else {
        return map;
    };
    for a in analyses {
        if !a.has_one_relations.is_empty() {
            map.insert(to_snake(&a.name), a.has_one_relations);
        }
    }
    map
}

/// camelCase / PascalCase → snake_case (IDL instruction names vs
/// source-discovered handler names).
fn to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Run the evidence-anchored detectors over the discovered handlers and
/// investigation candidates. Deterministic: output order is (class,
/// handler) and IDs hash the same pair, so re-runs on an unchanged
/// program are byte-identical.
pub fn hypothesize(
    project_root: &Path,
    handlers: &[BootstrapHandler],
    candidates: &[super::Candidate],
) -> Vec<InvariantHypothesis> {
    let has_one = idl_has_one_by_handler(project_root);
    let mut out = Vec::new();
    for h in handlers {
        let relations = has_one.get(&to_snake(&h.name)).map(Vec::as_slice);
        if let Some(hyp) = detect_authorization(project_root, h, relations) {
            out.push(hyp);
        }
        if let Some(hyp) = detect_lifecycle_init_once(project_root, h) {
            out.push(hyp);
        }
        if let Some(hyp) = detect_arithmetic_bound(project_root, h) {
            out.push(hyp);
        }
        if let Some(hyp) = detect_cpi_integrity(project_root, h) {
            out.push(hyp);
        }
    }
    out.extend(detect_conservation(project_root));
    out.extend(unwired_guard_hypotheses(candidates));
    if let Some(hyp) = detect_state_machine(project_root) {
        out.push(hyp);
    }
    out.sort_by(|a, b| {
        a.class
            .as_str()
            .cmp(b.class.as_str())
            .then(a.handler.cmp(&b.handler))
    });
    out
}

/// Rank for the human-readable summary: high confidence first, then
/// lowerable before not-yet-executable, then the stable (class, handler)
/// order.
pub fn ranked(hypotheses: &[InvariantHypothesis]) -> Vec<&InvariantHypothesis> {
    let conf_rank = |c: Confidence| match c {
        Confidence::High => 0,
        Confidence::Medium => 1,
        Confidence::Low => 2,
    };
    let mut v: Vec<&InvariantHypothesis> = hypotheses.iter().collect();
    v.sort_by_key(|h| (conf_rank(h.confidence), h.lowering.is_none(), h.id.clone()));
    v
}

/// The default-on stderr tail of every spec-less probe run (§6.4): ranked,
/// human-readable, with the payoff and the honest assurance level beside
/// every claim. JSON on stdout is unchanged for agent consumers.
pub fn render_summary(hypotheses: &[InvariantHypothesis]) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "\n{} invariant hypothes{} about this program — confirm the ones that match \
         your intent; each becomes an executable spec clause with its assurance \
         level shown explicitly.\n\n",
        hypotheses.len(),
        if hypotheses.len() == 1 { "is" } else { "es" }
    ));
    for (i, h) in ranked(hypotheses).into_iter().enumerate() {
        let conf = match h.confidence {
            Confidence::High => "high",
            Confidence::Medium => "med ",
            Confidence::Low => "low ",
        };
        let class_label = match h.class {
            HypothesisClass::Authorization => "AUTH     ",
            HypothesisClass::LifecycleInitOnce => "LIFECYCLE",
            HypothesisClass::ArithmeticBound => "ARITH    ",
            HypothesisClass::Conservation => "CONSERVE ",
            HypothesisClass::CpiIntegrity => "CPI      ",
            HypothesisClass::UnwiredGuard => "GUARD    ",
            HypothesisClass::StateMachine => "STATEMACH",
        };
        s.push_str(&format!(
            "  H{} {} · {} {}\n",
            i + 1,
            class_label,
            conf,
            h.claim
        ));
        for e in &h.evidence {
            let src = e
                .source
                .as_deref()
                .map(|p| format!(" ({})", p))
                .unwrap_or_default();
            s.push_str(&format!("       evidence:  {}{}\n", e.detail, src));
        }
        s.push_str(&format!("       payoff:    {}\n", h.payoff));
        s.push_str(&format!("       backend:   {}\n", h.backend));
        s.push_str(&format!("       id:        {}\n", h.id));
    }
    s.push_str(
        "\n  Answer in the conversation (accept / reject / BUG per hypothesis); the \
         agent writes answers.json and runs `qedgen ratify --audit-dir <dir>` to \
         apply confirmed hypotheses as executable clauses.\n",
    );
    s
}

/// Deterministic hypothesis ID: `h-<8hex>-<class>-<handler>`. Hashes class
/// + handler only — evidence details change as the program evolves, the
///
/// hypothesis *identity* does not (same contract as cluster IDs).
fn hypothesis_id(class: HypothesisClass, handler: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"qedgen-hypothesis-v1\n");
    hasher.update(class.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(handler.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    format!("h-{}-{}-{}", &hex[..8], class.as_str(), handler)
}

// ── Authorization ─────────────────────────────────────────────────────

/// §6.3 Authorization contract: one unambiguous signer plus a binding to
/// the stored authority (body key-equality / assert helper, an IDL
/// `has_one` relation naming the signer, or a framework-enforced
/// authority-named signer flag). Abstains on multiple plausible signers,
/// permissionless intent, or name-only evidence.
fn detect_authorization(
    project_root: &Path,
    handler: &BootstrapHandler,
    has_one_relations: Option<&[(String, String)]>,
) -> Option<InvariantHypothesis> {
    if handler.intent_tag.as_deref() == Some("permissionless") {
        return None;
    }

    let signers: Vec<String> = handler
        .idl_accounts
        .as_ref()
        .map(|accs| {
            accs.iter()
                .filter(|a| a.signer)
                .map(|a| a.name.clone())
                .collect()
        })
        .unwrap_or_default();
    // Multiple plausible signers → ambiguous authority; abstain.
    if signers.len() > 1 {
        return None;
    }
    let signer = signers.first().cloned();

    let mut evidence = Vec::new();
    if let Some(name) = &signer {
        evidence.push(EvidenceAnchor {
            kind: "idl_signer_flag".to_string(),
            detail: format!("`{}` is the instruction's only declared signer", name),
            source: Some(handler.source_file.clone()),
        });
    }

    // Body-level binding to a stored authority — the strongest anchor.
    if let Some((path, body)) = resolve_body(project_root, handler) {
        if let Some(rule) = handler_intent::authority_evidence(&body) {
            evidence.push(EvidenceAnchor {
                kind: rule.to_string(),
                detail: match rule {
                    "authority_comparison" => {
                        "handler body compares a signer key against a stored authority field"
                            .to_string()
                    }
                    _ => "handler body calls an authority-assert helper".to_string(),
                },
                source: Some(path),
            });
        }
    }

    // IDL `has_one` relation whose target is the signer — Anchor
    // enforces `<account>.<signer> == <signer>.key()`, the exact
    // stored-authority binding the contract asks for (legacy-Anchor IDLs
    // only; absent = unknown).
    if let (Some(signer_name), Some(relations)) = (signer.as_deref(), has_one_relations) {
        if let Some((account, _)) = relations.iter().find(|(_, rel)| rel == signer_name) {
            evidence.push(EvidenceAnchor {
                kind: "idl_has_one_binding".to_string(),
                detail: format!(
                    "IDL `has_one` relation binds `{}.{}` to the `{}` signer",
                    account, signer_name, signer_name
                ),
                source: Some(handler.source_file.clone()),
            });
        }
    }

    let has_stored_binding = evidence.iter().any(|e| {
        e.kind == "authority_comparison"
            || e.kind == "authority_assert_helper"
            || e.kind == "idl_has_one_binding"
    });
    let authority_named_signer = signer
        .as_deref()
        .map(|n| {
            let n = n.to_ascii_lowercase();
            ["authority", "admin", "manager", "owner", "delegate"]
                .iter()
                .any(|kw| n.contains(kw))
        })
        .unwrap_or(false);

    // Precision gate: a signer flag alone is a gate, not an *authority*
    // binding; a name prior alone is not evidence. Require a stored
    // binding (body or has_one), or an authority-named enforced signer.
    let confidence = match (has_stored_binding, &signer, authority_named_signer) {
        (true, Some(_), _) => Confidence::High,
        (true, None, _) => Confidence::Medium,
        (false, Some(_), true) => Confidence::Medium,
        _ => return None,
    };

    let subject = signer
        .clone()
        .unwrap_or_else(|| "the authority".to_string());
    let claim = format!(
        "`{}` requires the caller to be the stored authority (signer: {}).",
        handler.name, subject
    );
    let lowering = signer
        .clone()
        .map(|signer_account| HypothesisLowering::AuthClause { signer_account });
    let backend = if lowering.is_some() {
        "checking on ratify; impl-bound backends (impl-Kani / runtime reproducer) \
         where the handler shape is source-bindable"
            .to_string()
    } else {
        "checking once the signer account is named (confirmed, not executable until then)"
            .to_string()
    };

    Some(InvariantHypothesis {
        id: hypothesis_id(HypothesisClass::Authorization, &handler.name),
        class: HypothesisClass::Authorization,
        handler: handler.name.clone(),
        claim,
        evidence,
        payoff: "confirming makes unauthorized calls a checkable spec violation \
                 (auth guard generated in every backend)"
            .to_string(),
        backend,
        assurance: "checking".to_string(),
        confidence,
        lowering,
    })
}

// ── Lifecycle / init-once ─────────────────────────────────────────────

/// §6.3 Lifecycle contract: an init handler plus an identifiable one-shot
/// discriminator — an init guard in the body (`is_initialized` /
/// already-initialized error) or an Anchor `init` account constraint in
/// the handler's file. Name-only endpoints abstain.
fn detect_lifecycle_init_once(
    project_root: &Path,
    handler: &BootstrapHandler,
) -> Option<InvariantHypothesis> {
    let name = handler.name.to_ascii_lowercase();
    let init_named = name.starts_with("init")
        || name.starts_with("create")
        || name.contains("initialize")
        || handler
            .entry_fn
            .as_deref()
            .map(|f| f.contains("init"))
            .unwrap_or(false);
    if !init_named {
        return None;
    }

    let mut evidence = Vec::new();

    if let Some((path, body)) = resolve_body(project_root, handler) {
        if let Some(guard) = init_guard_in_body(&body) {
            evidence.push(EvidenceAnchor {
                kind: "init_guard_in_body".to_string(),
                detail: format!("handler body guards re-initialization via `{}`", guard),
                source: Some(path),
            });
        }
    }

    if let Some(rel_file) = anchor_init_constraint(project_root, handler) {
        evidence.push(EvidenceAnchor {
            kind: "anchor_init_constraint".to_string(),
            detail: "Anchor `#[account(init, …)]` constraint creates the state account \
                     (fails on re-run by construction)"
                .to_string(),
            source: Some(rel_file),
        });
    }

    // Precision gate: the handler *name* is the trigger, never the
    // evidence — abstain without a guard/constraint anchor.
    if evidence.is_empty() {
        return None;
    }
    let confidence = if evidence.len() >= 2 {
        Confidence::High
    } else {
        Confidence::Medium
    };

    Some(InvariantHypothesis {
        id: hypothesis_id(HypothesisClass::LifecycleInitOnce, &handler.name),
        class: HypothesisClass::LifecycleInitOnce,
        handler: handler.name.clone(),
        claim: format!(
            "`{}` initializes its state exactly once — re-invocation on an \
             already-initialized account is rejected.",
            handler.name
        ),
        evidence,
        payoff: "confirming models init as a one-shot state transition; \
                 re-initialization becomes a checkable violation"
            .to_string(),
        backend: "checking on ratify; the transition is model-testable under \
                  generated proptests (model-tested)"
            .to_string(),
        assurance: "checking".to_string(),
        confidence,
        lowering: Some(HypothesisLowering::LifecycleTransition),
    })
}

// ── Arithmetic bound ──────────────────────────────────────────────────

/// §6.3 Arithmetic-bound contract: a specific parameter, bound
/// expression, and error path — lifted from a bound check the body
/// *already enforces* (`require!(param <= X, Err)` or
/// `if param > X { return Err(..) }`). A held invariant, confirmed,
/// becomes a spec clause. Abstains when no check site exists — a bound
/// is never guessed from a type width or a parameter name.
fn detect_arithmetic_bound(
    project_root: &Path,
    handler: &BootstrapHandler,
) -> Option<InvariantHypothesis> {
    let args: Vec<String> = handler
        .idl_args
        .as_ref()?
        .iter()
        .map(|a| a.name.clone())
        .collect();
    if args.is_empty() {
        return None;
    }
    let (path, body) = resolve_body(project_root, handler)?;
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();

    let re_require = regex::Regex::new(
        r"require!\(([A-Za-z_][A-Za-z0-9_]*)(<=|<)([A-Za-z0-9_.:]+),([A-Za-z0-9_:]+)\)",
    )
    .expect("static regex");
    let re_if = regex::Regex::new(r"if([A-Za-z_][A-Za-z0-9_]*)>([A-Za-z0-9_.:]+)\{returnErr")
        .expect("static regex");

    // (param, op, bound, error, matched-shape)
    let hit: Option<(String, String, String, String, &'static str)> = re_require
        .captures_iter(&unspaced)
        .find(|c| args.contains(&c[1].to_string()))
        .map(|c| {
            let err = c[4].rsplit("::").next().unwrap_or(&c[4]).to_string();
            (
                c[1].to_string(),
                c[2].to_string(),
                c[3].to_string(),
                err,
                "require!",
            )
        })
        .or_else(|| {
            re_if
                .captures_iter(&unspaced)
                .find(|c| args.contains(&c[1].to_string()))
                .map(|c| {
                    (
                        c[1].to_string(),
                        "<=".to_string(),
                        c[2].to_string(),
                        "InvalidAmount".to_string(),
                        "if-return-Err",
                    )
                })
        });
    let (param, op, bound, error, shape) = hit?;

    // Lowering only when the bound is spec syntax as-is: an integer
    // literal (underscores stripped) — a Rust path/const does not resolve
    // in a skeleton and would just bounce off the ratify gate.
    let is_literal = bound.chars().all(|c| c.is_ascii_digit() || c == '_')
        && bound.chars().next().is_some_and(|c| c.is_ascii_digit());
    let lowering = is_literal.then(|| HypothesisLowering::RequiresBound {
        param: param.clone(),
        op: op.clone(),
        bound: bound.replace('_', ""),
        error: error.clone(),
    });
    let confidence = if lowering.is_some() {
        Confidence::High
    } else {
        Confidence::Medium
    };
    Some(InvariantHypothesis {
        id: hypothesis_id(HypothesisClass::ArithmeticBound, &handler.name),
        class: HypothesisClass::ArithmeticBound,
        handler: handler.name.clone(),
        claim: format!(
            "`{}` bounds `{}` ({} {}) and rejects with `{}`.",
            handler.name, param, op, bound, error
        ),
        evidence: vec![EvidenceAnchor {
            kind: "bound_check_in_body".to_string(),
            detail: format!(
                "body enforces `{} {} {}` via {} — a held invariant lifted into a question",
                param, op, bound, shape
            ),
            source: Some(path),
        }],
        payoff: "confirming makes the input bound an executable precondition — \
                 out-of-domain inputs become a checkable rejection in every backend"
            .to_string(),
        backend: if lowering.is_some() {
            "checking on ratify; a source-bound reject harness can raise it to \
             implementation-verified"
                .to_string()
        } else {
            "checking once the bound expression is spec-representable \
             (confirmed, not executable until then)"
                .to_string()
        },
        assurance: "checking".to_string(),
        confidence,
        lowering,
    })
}

// ── Conservation ──────────────────────────────────────────────────────

/// §6.3 Conservation contract: a paired forward/reverse operation with no
/// supply-changing flow anywhere in the scanner's inventory. Abstains the
/// moment any issuance/destruction-shaped flow exists — supply-changing
/// operations not ruled out means no conservation claim. Lowering stays
/// with the agent (the total expression must be bound to real state);
/// confirmed hypotheses report `confirmed, not executable`.
fn detect_conservation(project_root: &Path) -> Vec<InvariantHypothesis> {
    let Ok(facts) = super::domain_extract::extract_program(project_root) else {
        return Vec::new();
    };
    let supply_changing = facts.asset_flows.iter().any(|f| {
        matches!(
            f.flow_shape,
            super::domain_extract::FlowShape::Issuance
                | super::domain_extract::FlowShape::Destruction
        )
    });
    if supply_changing {
        return Vec::new();
    }
    facts
        .paired_operations
        .iter()
        .filter(|p| !p.left_handlers.is_empty() && !p.right_handlers.is_empty())
        .map(|p| {
            let pair_key = format!("{}+{}", p.left_operation, p.right_operation);
            let handlers: Vec<String> = p
                .left_handlers
                .iter()
                .chain(p.right_handlers.iter())
                .cloned()
                .collect();
            let mut evidence: Vec<EvidenceAnchor> = p
                .evidence
                .iter()
                .map(|span| EvidenceAnchor {
                    kind: "paired_operation".to_string(),
                    detail: format!(
                        "{} / {} pair ({})",
                        p.left_operation, p.right_operation, p.relationship
                    ),
                    source: Some(format!("{}:{}", span.path, span.start_line)),
                })
                .collect();
            evidence.push(EvidenceAnchor {
                kind: "no_supply_changing_flows".to_string(),
                detail: "source scan found no mint/burn-shaped flows alongside the pair"
                    .to_string(),
                source: None,
            });
            InvariantHypothesis {
                id: hypothesis_id(HypothesisClass::Conservation, &pair_key),
                class: HypothesisClass::Conservation,
                handler: pair_key,
                claim: format!(
                    "Total value is conserved across {} (paired `{}`/`{}`; no \
                     supply-changing operations detected).",
                    handlers.join(" + "),
                    p.left_operation,
                    p.right_operation
                ),
                evidence,
                payoff: "confirming yields a conservation property over the spec model \
                         (`total == old(total)` preserved by the pair)"
                    .to_string(),
                backend: "proptest over the spec model (model-tested) once the total \
                          expression is bound; implementation binding is runtime-dependent"
                    .to_string(),
                assurance: "checking".to_string(),
                confidence: Confidence::Medium,
                lowering: None,
            }
        })
        .collect()
}

// ── CPI integrity ─────────────────────────────────────────────────────

/// §6.3 CPI-integrity contract: a resolved callee identity (SPL Token,
/// pinned by an Anchor `Program` account / `anchor_spl::token` path) AND
/// a resolved account-role mapping (an Anchor `Transfer { from, to,
/// authority }` struct literal). Abstains when either is unresolved.
fn detect_cpi_integrity(
    project_root: &Path,
    handler: &BootstrapHandler,
) -> Option<InvariantHypothesis> {
    let (path, body) = resolve_body(project_root, handler)?;
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();

    // Callee pin — the program identity must be resolved, not caller-supplied.
    let token_program_account = handler
        .idl_accounts
        .as_ref()
        .map(|accs| accs.iter().any(|a| a.name.contains("token_program")))
        .unwrap_or(false);
    let token_path_in_body = unspaced.contains("anchor_spl::token")
        || unspaced.contains("token::transfer(")
        || unspaced.contains("spl_token::");
    if !(token_program_account || token_path_in_body) {
        return None;
    }

    // Direction — the `Transfer { … }` struct literal names the roles.
    let start = unspaced.find("Transfer{")?;
    let block = &unspaced[start + "Transfer{".len()..];
    let block = &block[..block.find('}')?];
    let role = |field: &str| -> Option<String> {
        let seg = block.split(&format!("{field}:")).nth(1)?;
        let seg = seg.split(',').next().unwrap_or(seg);
        // `ctx.accounts.<name>.to_account_info()` → `<name>`; a bare
        // simple ident passes through.
        if let Some(after) = seg.split("accounts.").nth(1) {
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        } else if seg.chars().all(|c| c.is_alphanumeric() || c == '_') && !seg.is_empty() {
            Some(seg.to_string())
        } else {
            None
        }
    };
    let from = role("from")?;
    let to = role("to")?;
    let authority = role("authority");

    // Amount: the second top-level argument of the `transfer(ctx, amount)`
    // call, when it is a simple ident.
    let amount = unspaced.find("transfer(").and_then(|i| {
        let rest = &unspaced[i + "transfer(".len()..];
        let mut depth = 0usize;
        let mut last_arg_start = 0usize;
        for (j, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    let arg = &rest[last_arg_start..j];
                    return (arg.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && arg
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphabetic() || c == '_'))
                    .then(|| arg.to_string());
                }
                ')' => depth -= 1,
                ',' if depth == 0 => last_arg_start = j + 1,
                _ => {}
            }
        }
        None
    });

    let mut evidence = vec![EvidenceAnchor {
        kind: "cpi_transfer_roles".to_string(),
        detail: format!(
            "`Transfer {{ from: {}, to: {}{} }}` struct literal resolves the account roles",
            from,
            to,
            authority
                .as_deref()
                .map(|a| format!(", authority: {a}"))
                .unwrap_or_default()
        ),
        source: Some(path),
    }];
    if token_program_account {
        evidence.push(EvidenceAnchor {
            kind: "cpi_callee_pinned".to_string(),
            detail: "`token_program` account in the instruction's IDL accounts pins the callee"
                .to_string(),
            source: Some(handler.source_file.clone()),
        });
    }

    let confidence = if token_program_account && authority.is_some() {
        Confidence::High
    } else {
        Confidence::Medium
    };
    Some(InvariantHypothesis {
        id: hypothesis_id(HypothesisClass::CpiIntegrity, &handler.name),
        class: HypothesisClass::CpiIntegrity,
        handler: handler.name.clone(),
        claim: format!(
            "`{}` moves tokens from `{}` to `{}` via SPL Token{}.",
            handler.name,
            from,
            to,
            authority
                .as_deref()
                .map(|a| format!(" (authority `{a}`)"))
                .unwrap_or_default()
        ),
        evidence,
        payoff: "confirming pins the CPI's direction in the spec — a swapped source/\
                 destination or re-routed authority becomes a checkable violation"
            .to_string(),
        backend: "checking on ratify; implementation level requires a runtime/source-bound \
                  harness"
            .to_string(),
        assurance: "checking".to_string(),
        confidence,
        lowering: Some(HypothesisLowering::TransfersClause {
            from,
            to,
            amount,
            authority,
        }),
    })
}

// ── Unwired guard as question ─────────────────────────────────────────

/// §6.1's sixth class: a #240 dead-guard candidate flipped into a
/// confirm/deny question. The evidence is the definition site itself;
/// there is nothing to lower — an `accept` answer means "the check is
/// intended and missing", which ratify routes to a missing-enforcement
/// finding (elicitation as bug-catcher), and a `reject` records a dead
/// variant to delete.
fn unwired_guard_hypotheses(candidates: &[super::Candidate]) -> Vec<InvariantHypothesis> {
    candidates
        .iter()
        .filter(|c| c.category_tag == "unwired_error_variant")
        .map(|c| InvariantHypothesis {
            id: hypothesis_id(HypothesisClass::UnwiredGuard, &c.handler),
            class: HypothesisClass::UnwiredGuard,
            handler: c.handler.clone(),
            claim: format!(
                "Error variant `{}` names a check the program never enforces — should \
                 the guard it describes hold?",
                c.handler
            ),
            evidence: vec![EvidenceAnchor {
                kind: "unwired_error_variant".to_string(),
                detail: c.spec_silent_on.clone(),
                source: None,
            }],
            payoff: "confirming files a missing-enforcement finding at the unguarded \
                     path's impact ceiling; rejecting records a dead variant to delete"
                .to_string(),
            backend: "source-bound reproducer once the unguarded path is identified".to_string(),
            assurance: "checking".to_string(),
            confidence: Confidence::Medium,
            lowering: None,
        })
        .collect()
}

// ── State machine ─────────────────────────────────────────────────────

/// The bench-miss class (M-01): when the program's own IDL declares a
/// status enum that a state struct carries, the spec's `type State`
/// should be that machine — not the two-variant placeholder. Fires only
/// when exactly ONE such enum exists (ambiguous state representation →
/// abstain, per the lifecycle rules).
fn detect_state_machine(project_root: &Path) -> Option<InvariantHypothesis> {
    let (idl_path, _) = super::idl_overlay::discover_idl(project_root)
        .ok()
        .flatten()?;
    let (idl, _) = crate::idl::parse_idl(&idl_path).ok()?;
    let referenced: Vec<(&str, Vec<String>)> = idl
        .types
        .iter()
        .filter(|t| t.ty.kind == "enum" && t.ty.variants.len() >= 2)
        .filter(|e| {
            idl.types.iter().any(|t| {
                t.ty.kind == "struct"
                    && t.ty
                        .fields
                        .iter()
                        .any(|f| f.ty.to_string().contains(&format!("\"{}\"", e.name)))
            })
        })
        .map(|e| {
            (
                e.name.as_str(),
                e.ty.variants.iter().map(|v| v.name.clone()).collect(),
            )
        })
        .collect();
    // Exactly one status enum, or the state representation is ambiguous.
    let [(name, variants)] = referenced.as_slice() else {
        return None;
    };
    let rel_idl = idl_path
        .strip_prefix(project_root)
        .unwrap_or(&idl_path)
        .display()
        .to_string();
    Some(InvariantHypothesis {
        id: hypothesis_id(HypothesisClass::StateMachine, name),
        class: HypothesisClass::StateMachine,
        handler: name.to_string(),
        claim: format!(
            "The program's lifecycle is a state machine over `{}` ({}) — the spec's \
             `type State` should carry these variants and state-writing handlers \
             should declare their transitions.",
            name,
            variants.join(" | ")
        ),
        evidence: vec![EvidenceAnchor {
            kind: "idl_status_enum".to_string(),
            detail: format!(
                "enum `{}` is declared in the IDL and carried by a state struct field",
                name
            ),
            source: Some(rel_idl),
        }],
        payoff: "confirming lifts the real status enum into the spec's `type State`, \
                 making every lifecycle transition (and its gaps) expressible and \
                 checkable"
            .to_string(),
        backend: "checking on ratify; per-handler transitions become model-testable \
                  under generated proptests"
            .to_string(),
        assurance: "checking".to_string(),
        confidence: Confidence::Medium,
        lowering: Some(HypothesisLowering::StateAdtRewrite {
            variants: variants.clone(),
        }),
    })
}

/// One-shot guard shapes in a handler body (whitespace-insensitive; bodies
/// may come through `quote`-rendered token streams).
fn init_guard_in_body(body: &str) -> Option<&'static str> {
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    [
        "is_initialized",
        "AccountAlreadyInitialized",
        "already_initialized",
        "AlreadyInUse",
        "AlreadyInitialized",
        "discriminator",
        "discriminant",
    ]
    .into_iter()
    .find(|needle| unspaced.contains(needle))
}

/// Does the file defining this handler carry an Anchor `#[account(init,
/// …)]` constraint (standalone `init`, not `init_if_needed` — the latter
/// is exactly *not* one-shot)? File-scoped on purpose: precise
/// struct-to-handler mapping stays with the agent (D3).
fn anchor_init_constraint(project_root: &Path, handler: &BootstrapHandler) -> Option<String> {
    let path = project_root.join(&handler.source_file);
    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let unspaced: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let has_init = unspaced.contains("#[account(init,") || unspaced.contains("#[account(init)]");
    let only_if_needed = !has_init && unspaced.contains("#[account(init_if_needed");
    if has_init && !only_if_needed {
        Some(handler.source_file.clone())
    } else {
        None
    }
}

/// Resolve the handler's body text: prefer the handler's own source file
/// (works for Anchor workspaces where bodies live outside `<root>/src`),
/// fall back to the project-wide walk keyed on `entry_fn`/name.
fn resolve_body(project_root: &Path, handler: &BootstrapHandler) -> Option<(String, String)> {
    let fn_name = handler.entry_fn.as_deref().unwrap_or(&handler.name);
    let direct = project_root.join(&handler.source_file);
    if direct.extension().and_then(|e| e.to_str()) == Some("rs") {
        if let Ok(source) = std::fs::read_to_string(&direct) {
            if let Ok(syntax) = syn::parse_file(&source) {
                if let Some(body) = handler_intent::find_fn_body_in_items(&syntax.items, fn_name) {
                    return Some((handler.source_file.clone(), body));
                }
            }
        }
    }
    handler_intent::resolve_handler_body(fn_name, project_root)
        .map(|(path, body)| (path.display().to_string(), body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::idl_overlay::IdlAccountMeta;

    fn handler(name: &str) -> BootstrapHandler {
        BootstrapHandler {
            name: name.to_string(),
            source_file: "src/lib.rs".to_string(),
            enum_variant: None,
            entry_fn: None,
            line: None,
            applicable_categories: None,
            intent_tag: None,
            idl_accounts: None,
            idl_args: None,
            discovered_via: None,
        }
    }

    fn signer(name: &str) -> IdlAccountMeta {
        IdlAccountMeta {
            name: name.to_string(),
            signer: true,
            writable: false,
        }
    }

    fn tmp_project(tag: &str, lib_rs: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("qedgen-hypothesize-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), lib_rs).unwrap();
        dir
    }

    #[test]
    fn authority_comparison_plus_single_signer_fires_high() {
        let root = tmp_project(
            "auth-high",
            r#"pub fn set_fee(a: u64) -> Result<(), ()> {
    if signer.key != state.authority { return Err(()); }
    state.fee = a;
    Ok(())
}
"#,
        );
        let mut h = handler("set_fee");
        h.idl_accounts = Some(vec![signer("authority")]);
        let hyps = hypothesize(&root, &[h], &[]);
        assert_eq!(hyps.len(), 1);
        let hyp = &hyps[0];
        assert_eq!(hyp.class, HypothesisClass::Authorization);
        assert!(matches!(hyp.confidence, Confidence::High));
        assert!(matches!(
            hyp.lowering,
            Some(HypothesisLowering::AuthClause { ref signer_account }) if signer_account == "authority"
        ));
        assert!(hyp
            .evidence
            .iter()
            .any(|e| e.kind == "authority_comparison"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn signer_flag_alone_without_authority_name_abstains() {
        // A trader-signer with no stored-authority binding is a gate, not
        // an authority claim — precision rule says abstain.
        let root = tmp_project(
            "auth-abstain",
            r#"pub fn place_order(sz: u64) -> Result<(), ()> {
    book.push(sz);
    Ok(())
}
"#,
        );
        let mut h = handler("place_order");
        h.idl_accounts = Some(vec![signer("trader")]);
        let hyps = hypothesize(&root, &[h], &[]);
        assert!(hyps.is_empty(), "{hyps:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn multiple_signers_abstain() {
        let root = tmp_project(
            "auth-multi",
            r#"pub fn co_sign(x: u64) -> Result<(), ()> {
    if a.key != state.authority { return Err(()); }
    Ok(())
}
"#,
        );
        let mut h = handler("co_sign");
        h.idl_accounts = Some(vec![signer("authority"), signer("co_authority")]);
        let hyps = hypothesize(&root, &[h], &[]);
        assert!(hyps.is_empty(), "{hyps:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn permissionless_intent_abstains() {
        let root = tmp_project("auth-perm", "pub fn crank() {}\n");
        let mut h = handler("crank");
        h.intent_tag = Some("permissionless".to_string());
        h.idl_accounts = Some(vec![signer("admin")]);
        assert!(hypothesize(&root, &[h], &[]).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn authority_named_enforced_signer_without_body_is_medium() {
        // Body unresolvable (no such fn) but the single signer is
        // authority-named — medium-confidence claim, still lowerable.
        let root = tmp_project("auth-med", "// no handlers here\n");
        let mut h = handler("update_config");
        h.idl_accounts = Some(vec![signer("admin")]);
        let hyps = hypothesize(&root, &[h], &[]);
        assert_eq!(hyps.len(), 1);
        assert!(matches!(hyps[0].confidence, Confidence::Medium));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn idl_has_one_binding_fires_high_without_authority_name() {
        // The signer `payer` is not authority-named and the body is
        // unresolvable — signer-flag-alone would abstain. The IDL's
        // `has_one` relation IS the stored-authority binding (Phase 4:
        // IDL as evidence source), so the hypothesis fires High.
        let root = tmp_project("auth-hasone", "// no source bodies\n");
        std::fs::write(
            root.join("idl.json"),
            r#"{
                "metadata": { "name": "vault" },
                "instructions": [{
                    "name": "setFee",
                    "accounts": [
                        { "name": "payer", "signer": true, "writable": false },
                        { "name": "vault", "writable": true, "relations": ["payer"] }
                    ],
                    "args": []
                }]
            }"#,
        )
        .unwrap();
        let mut h = handler("set_fee");
        h.idl_accounts = Some(vec![signer("payer")]);
        let hyps = hypothesize(&root, &[h], &[]);
        assert_eq!(hyps.len(), 1, "{hyps:?}");
        assert!(matches!(hyps[0].confidence, Confidence::High));
        assert!(hyps[0]
            .evidence
            .iter()
            .any(|e| e.kind == "idl_has_one_binding"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_named_handler_without_guard_abstains() {
        // Name-only endpoints must not fire (PRD §6.3 abstain rule).
        let root = tmp_project(
            "life-abstain",
            r#"pub fn initialize(x: u64) -> Result<(), ()> {
    state.x = x;
    Ok(())
}
"#,
        );
        let hyps = hypothesize(&root, &[handler("initialize")], &[]);
        assert!(hyps.is_empty(), "{hyps:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_guard_in_body_fires_lifecycle() {
        let root = tmp_project(
            "life-guard",
            r#"pub fn initialize(x: u64) -> Result<(), ()> {
    if state.is_initialized { return Err(()); }
    state.x = x;
    Ok(())
}
"#,
        );
        let hyps = hypothesize(&root, &[handler("initialize")], &[]);
        assert_eq!(hyps.len(), 1);
        assert_eq!(hyps[0].class, HypothesisClass::LifecycleInitOnce);
        assert!(matches!(
            hyps[0].lowering,
            Some(HypothesisLowering::LifecycleTransition)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn anchor_init_constraint_fires_lifecycle() {
        let root = tmp_project(
            "life-anchor",
            r#"#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init, payer = admin, space = 128)]
    pub vault: Account<'info, Vault>,
}
pub fn initialize(ctx: Context<Initialize>) -> Result<()> { Ok(()) }
"#,
        );
        let hyps = hypothesize(&root, &[handler("initialize")], &[]);
        assert_eq!(hyps.len(), 1);
        assert!(hyps[0]
            .evidence
            .iter()
            .any(|e| e.kind == "anchor_init_constraint"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_if_needed_does_not_count_as_one_shot() {
        let root = tmp_project(
            "life-ifneeded",
            r#"#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(init_if_needed, payer = admin, space = 128)]
    pub vault: Account<'info, Vault>,
}
pub fn initialize(ctx: Context<Initialize>) -> Result<()> { Ok(()) }
"#,
        );
        let hyps = hypothesize(&root, &[handler("initialize")], &[]);
        assert!(hyps.is_empty(), "{hyps:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn arg(name: &str) -> crate::probe::idl_overlay::IdlArgMeta {
        crate::probe::idl_overlay::IdlArgMeta {
            name: name.to_string(),
            ty: "U64".to_string(),
        }
    }

    #[test]
    fn arithmetic_bound_lifts_require_check() {
        let root = tmp_project(
            "arith-require",
            r#"pub fn set_cap(cap: u64) -> Result<(), ()> {
    require!(cap <= 1_000_000, VaultError::CapTooHigh);
    state.cap = cap;
    Ok(())
}
"#,
        );
        let mut h = handler("set_cap");
        h.idl_args = Some(vec![arg("cap")]);
        let hyps = hypothesize(&root, &[h], &[]);
        let arith: Vec<_> = hyps
            .iter()
            .filter(|h| h.class == HypothesisClass::ArithmeticBound)
            .collect();
        assert_eq!(arith.len(), 1, "{hyps:?}");
        assert!(matches!(arith[0].confidence, Confidence::High));
        assert!(matches!(
            &arith[0].lowering,
            Some(HypothesisLowering::RequiresBound { param, op, bound, error })
                if param == "cap" && op == "<=" && bound == "1000000" && error == "CapTooHigh"
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn arithmetic_bound_without_check_site_abstains() {
        // A u64 arg with no bound check anywhere — never guess from the
        // type width or the name.
        let root = tmp_project(
            "arith-abstain",
            r#"pub fn set_cap(cap: u64) -> Result<(), ()> {
    state.cap = cap;
    Ok(())
}
"#,
        );
        let mut h = handler("set_cap");
        h.idl_args = Some(vec![arg("cap")]);
        let hyps = hypothesize(&root, &[h], &[]);
        assert!(
            !hyps
                .iter()
                .any(|h| h.class == HypothesisClass::ArithmeticBound),
            "{hyps:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn arithmetic_bound_non_literal_rhs_has_no_lowering() {
        let root = tmp_project(
            "arith-nonliteral",
            r#"pub fn set_cap(cap: u64) -> Result<(), ()> {
    require!(cap <= state.limit, VaultError::CapTooHigh);
    Ok(())
}
"#,
        );
        let mut h = handler("set_cap");
        h.idl_args = Some(vec![arg("cap")]);
        let hyps = hypothesize(&root, &[h], &[]);
        let arith: Vec<_> = hyps
            .iter()
            .filter(|h| h.class == HypothesisClass::ArithmeticBound)
            .collect();
        assert_eq!(arith.len(), 1);
        assert!(arith[0].lowering.is_none());
        assert!(matches!(arith[0].confidence, Confidence::Medium));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cpi_integrity_resolves_transfer_roles() {
        let root = tmp_project(
            "cpi-roles",
            r#"pub fn payout(amount: u64) -> Result<(), ()> {
    let cpi = CpiContext::new(
        ctx.accounts.token_program.to_account_info(),
        Transfer {
            from: ctx.accounts.vault_ta.to_account_info(),
            to: ctx.accounts.user_ta.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        },
    );
    anchor_spl::token::transfer(cpi, amount)?;
    Ok(())
}
"#,
        );
        let mut h = handler("payout");
        h.idl_accounts = Some(vec![crate::probe::idl_overlay::IdlAccountMeta {
            name: "token_program".to_string(),
            signer: false,
            writable: false,
        }]);
        let hyps = hypothesize(&root, &[h], &[]);
        let cpi: Vec<_> = hyps
            .iter()
            .filter(|h| h.class == HypothesisClass::CpiIntegrity)
            .collect();
        assert_eq!(cpi.len(), 1, "{hyps:?}");
        assert!(matches!(cpi[0].confidence, Confidence::High));
        assert!(matches!(
            &cpi[0].lowering,
            Some(HypothesisLowering::TransfersClause { from, to, amount, authority })
                if from == "vault_ta" && to == "user_ta"
                    && amount.as_deref() == Some("amount")
                    && authority.as_deref() == Some("vault")
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cpi_integrity_unresolved_direction_abstains() {
        // Token CPI present but no Transfer struct literal — direction
        // unresolved → abstain (PRD §6.3).
        let root = tmp_project(
            "cpi-abstain",
            r#"pub fn payout(amount: u64) -> Result<(), ()> {
    spl_token::instruction::transfer_via_helper(amount)?;
    Ok(())
}
"#,
        );
        let hyps = hypothesize(&root, &[handler("payout")], &[]);
        assert!(
            !hyps
                .iter()
                .any(|h| h.class == HypothesisClass::CpiIntegrity),
            "{hyps:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn conservation_fires_on_pair_without_supply_changes() {
        let root = tmp_project(
            "conserve",
            r#"pub fn process_deposit(amount: u64) -> Result<(), ()> {
    state.total = state.total.checked_add(amount).unwrap();
    Ok(())
}
pub fn process_withdraw(amount: u64) -> Result<(), ()> {
    state.total = state.total.checked_sub(amount).unwrap();
    Ok(())
}
"#,
        );
        let hyps = hypothesize(&root, &[], &[]);
        let cons: Vec<_> = hyps
            .iter()
            .filter(|h| h.class == HypothesisClass::Conservation)
            .collect();
        assert_eq!(cons.len(), 1, "{hyps:?}");
        assert!(cons[0].lowering.is_none());
        assert!(cons[0]
            .evidence
            .iter()
            .any(|e| e.kind == "no_supply_changing_flows"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unwired_guard_candidates_become_questions() {
        let root = tmp_project("unwired", "// empty\n");
        let candidate = crate::probe::Candidate {
            category: crate::probe::Category::UnwiredErrorVariant,
            category_tag: "unwired_error_variant".to_string(),
            handler: "CapTooHigh".to_string(),
            spec_silent_on: "defined at src/error.rs:12".to_string(),
            suppression_hint: String::new(),
            investigation_hint: String::new(),
            reason: String::new(),
            repro_harness: None,
        };
        let hyps = hypothesize(&root, &[], &[candidate]);
        assert_eq!(hyps.len(), 1);
        assert_eq!(hyps[0].class, HypothesisClass::UnwiredGuard);
        assert_eq!(hyps[0].handler, "CapTooHigh");
        assert!(hyps[0].lowering.is_none());
        assert!(hyps[0].evidence[0].detail.contains("src/error.rs:12"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn state_machine_fires_on_single_referenced_status_enum() {
        let root = tmp_project("statemach", "// empty\n");
        std::fs::write(
            root.join("idl.json"),
            r#"{
                "metadata": { "name": "vault" },
                "instructions": [],
                "types": [
                    { "name": "VaultStatus", "type": { "kind": "enum", "variants": [
                        { "name": "Uninitialized" }, { "name": "Active" }, { "name": "Frozen" }
                    ] } },
                    { "name": "Vault", "type": { "kind": "struct", "fields": [
                        { "name": "status", "type": { "defined": "VaultStatus" } },
                        { "name": "cap", "type": "u64" }
                    ] } }
                ]
            }"#,
        )
        .unwrap();
        let hyps = hypothesize(&root, &[], &[]);
        let sm: Vec<_> = hyps
            .iter()
            .filter(|h| h.class == HypothesisClass::StateMachine)
            .collect();
        assert_eq!(sm.len(), 1, "{hyps:?}");
        assert!(matches!(
            &sm[0].lowering,
            Some(HypothesisLowering::StateAdtRewrite { variants })
                if variants == &["Uninitialized", "Active", "Frozen"]
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn state_machine_ambiguous_enums_abstain() {
        let root = tmp_project("statemach-ambig", "// empty\n");
        std::fs::write(
            root.join("idl.json"),
            r#"{
                "metadata": { "name": "vault" },
                "instructions": [],
                "types": [
                    { "name": "StatusA", "type": { "kind": "enum", "variants": [
                        { "name": "X" }, { "name": "Y" } ] } },
                    { "name": "StatusB", "type": { "kind": "enum", "variants": [
                        { "name": "P" }, { "name": "Q" } ] } },
                    { "name": "S1", "type": { "kind": "struct", "fields": [
                        { "name": "a", "type": { "defined": "StatusA" } } ] } },
                    { "name": "S2", "type": { "kind": "struct", "fields": [
                        { "name": "b", "type": { "defined": "StatusB" } } ] } }
                ]
            }"#,
        )
        .unwrap();
        let hyps = hypothesize(&root, &[], &[]);
        assert!(
            !hyps
                .iter()
                .any(|h| h.class == HypothesisClass::StateMachine),
            "{hyps:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ids_are_stable_and_readiness_counts() {
        let root = tmp_project(
            "stable",
            r#"pub fn initialize(x: u64) -> Result<(), ()> {
    if state.is_initialized { return Err(()); }
    Ok(())
}
"#,
        );
        let a = hypothesize(&root, &[handler("initialize")], &[]);
        let b = hypothesize(&root, &[handler("initialize")], &[]);
        assert_eq!(a[0].id, b[0].id);
        assert!(a[0].id.starts_with("h-"));
        let readiness = spec_readiness(&a);
        assert_eq!(readiness.hypotheses_total, 1);
        assert_eq!(readiness.lowerable, 1);
        assert_eq!(readiness.by_class.get("lifecycle_init_once"), Some(&1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn summary_renders_evidence_payoff_backend() {
        let root = tmp_project(
            "summary",
            r#"pub fn initialize(x: u64) -> Result<(), ()> {
    if state.is_initialized { return Err(()); }
    Ok(())
}
"#,
        );
        let hyps = hypothesize(&root, &[handler("initialize")], &[]);
        let s = render_summary(&hyps);
        assert!(s.contains("LIFECYCLE"));
        assert!(s.contains("evidence:"));
        assert!(s.contains("payoff:"));
        assert!(s.contains("backend:"));
        assert!(s.contains(&hyps[0].id));
        let _ = std::fs::remove_dir_all(&root);
    }
}
