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
//! v2.10 initial cut: `missing_signer`, `arbitrary_cpi`,
//! `arithmetic_overflow_wrapping`, and `lifecycle_one_shot_violation`.
//! Remaining categories (`cpi_param_swap`, `pda_canonical_bump`) lean
//! more heavily on spec-less / impl-side analysis (per the manual-audit
//! calibration); their spec-aware predicates are weak. Land alongside
//! the auditor SKILL.md per-runtime predicates rather than here.

use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::check::{parse_spec_file, ParsedHandler};

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
#[allow(dead_code)] // SpecLess used by --bootstrap mode landing later in v2.10
pub enum Mode {
    SpecAware,
    SpecLess,
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
    pub spec_path: String,
    pub mode: Mode,
    pub findings: Vec<Finding>,
}

pub fn run_probe(spec_path: &Path) -> Result<ProbeOutput> {
    let spec = parse_spec_file(spec_path)?;
    let spec_models_lifecycle = !spec.lifecycle_states.is_empty()
        || spec.account_types.iter().any(|a| !a.lifecycle.is_empty());
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
    }

    Ok(ProbeOutput {
        version: SCHEMA_VERSION,
        spec_path: spec_path.display().to_string(),
        mode: Mode::SpecAware,
        findings,
    })
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
            "Add `transfers {{ from <src> to <dst> amount <amt> authority <signer> }}` to handler `{}` — \
             or a `call Interface.handler(...)` site if invoking a non-Token CPI. \
             Without one of these, the codegen cannot mechanize the transfer.",
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
}
