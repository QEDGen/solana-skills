//! Structured elicitation answers + hypothesis lowering (PRD
//! `docs/design/spec-elicitation-prd.md` §6.2/§6.3).
//!
//! The interview is in-harness: the agent asks, the user answers in the
//! conversation, and the agent writes `answers.json` into the audit
//! working set — `{hypothesis_or_cluster_id → accept | reject | bug +
//! note}`. No user-edited markdown round-trip. `ratify` consumes this
//! file (falling back to the legacy `interview.md` when absent) and
//! lowers each *confirmed, lowerable* hypothesis to real `.qedspec`
//! syntax, validating with the parser + completeness lints after every
//! application. A confirmed hypothesis that cannot be lowered without
//! placeholders is reported `confirmed, not executable` — never a
//! misleading comment.
//!
//! User confirmation, not detector confidence, controls activation: only
//! answered-accept hypotheses are lowered; unconfirmed ones are never
//! active.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::hypothesize::{HypothesisLowering, InvariantHypothesis};
use super::prompts::Choice;

/// `answers.json` — the structured answer set the agent writes after the
/// in-harness interview. IDs address hypotheses (`h-…`) and legacy
/// clusters (`c-…`) uniformly.
#[derive(Debug, Deserialize)]
pub struct AnswerSet {
    #[serde(default)]
    pub run_id: Option<String>,
    pub answers: Vec<StructuredAnswer>,
}

#[derive(Debug, Deserialize)]
pub struct StructuredAnswer {
    pub id: String,
    /// `accept` | `narrow` (clusters only) | `reject` | `bug`.
    pub decision: String,
    #[serde(default)]
    pub note: String,
}

impl StructuredAnswer {
    pub fn choice(&self) -> Option<Choice> {
        match self.decision.as_str() {
            "accept" => Some(Choice::Accept),
            "narrow" => Some(Choice::Narrow),
            "reject" => Some(Choice::Reject),
            "bug" => Some(Choice::Bug),
            _ => None,
        }
    }
}

pub fn read_answers(path: &Path) -> Result<AnswerSet> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// `hypotheses.json` — written by the probe (see
/// `probe::write_elicitation_artifacts`), read back for lowering.
#[derive(Debug, Deserialize)]
pub struct HypothesesDoc {
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub generated_at_unix: Option<u64>,
    #[serde(default)]
    pub hypotheses: Vec<InvariantHypothesis>,
}

pub fn read_hypotheses(path: &Path) -> Result<HypothesesDoc> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Per-hypothesis ratification outcome, persisted to
/// `elicitation-outcome.json` (Phase-0 conversion instrumentation).
#[derive(Debug, Clone, Serialize)]
pub struct HypothesisOutcome {
    pub id: String,
    pub handler: String,
    pub decision: String,
    /// `lowered` — executable clause injected; `already_modeled` — the
    /// spec already carries the clause; `confirmed_not_executable` —
    /// accepted but no placeholder-free lowering exists (stays in the
    /// dossier); `rejected` / `bug` — routed to scoping / findings.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result of attempting one lowering against the current spec text.
pub enum LoweringResult {
    /// New spec text with the clause injected.
    Applied(String),
    /// The spec already models the claim; nothing to inject.
    AlreadyModeled,
    /// No placeholder-free lowering possible against this skeleton.
    NotExecutable(String),
}

/// Apply one hypothesis lowering to the spec text. Purely textual (every
/// spec emitter in this codebase builds concrete syntax and re-parses to
/// self-validate); the caller owns the parse + lint gate and reverts on
/// failure.
pub fn apply_lowering(
    spec_text: &str,
    hypothesis: &InvariantHypothesis,
    lowering: &HypothesisLowering,
) -> LoweringResult {
    match lowering {
        HypothesisLowering::AuthClause { signer_account } => {
            apply_auth_clause(spec_text, hypothesis, signer_account)
        }
        HypothesisLowering::LifecycleTransition => {
            apply_lifecycle_transition(spec_text, hypothesis)
        }
        HypothesisLowering::RequiresBound {
            param,
            op,
            bound,
            error,
        } => apply_requires_bound(spec_text, hypothesis, param, op, bound, error),
        HypothesisLowering::TransfersClause {
            from,
            to,
            amount,
            authority,
        } => apply_transfers_clause(
            spec_text,
            hypothesis,
            from,
            to,
            amount.as_deref(),
            authority.as_deref(),
        ),
        HypothesisLowering::StateAdtRewrite { variants } => {
            apply_state_adt_rewrite(spec_text, variants)
        }
    }
}

/// Insert `requires <param> <op> <bound> else <error>` into the handler
/// body, declaring the error variant when the spec doesn't carry it yet.
fn apply_requires_bound(
    spec_text: &str,
    hypothesis: &InvariantHypothesis,
    param: &str,
    op: &str,
    bound: &str,
    error: &str,
) -> LoweringResult {
    let Some(block) = handler_block(spec_text, &hypothesis.handler) else {
        return LoweringResult::NotExecutable(format!(
            "handler `{}` not present in the skeleton",
            hypothesis.handler
        ));
    };
    let clause = format!("requires {} {} {} else {}", param, op, bound, error);
    let body = &spec_text[block.body_start..block.body_end];
    if body.lines().any(|l| l.trim() == clause) {
        return LoweringResult::AlreadyModeled;
    }
    let mut out = String::with_capacity(spec_text.len() + 96);
    out.push_str(&spec_text[..block.body_start]);
    out.push_str(&format!(
        "  // provenance: hypothesis {}\n  {}\n",
        hypothesis.id, clause
    ));
    out.push_str(&spec_text[block.body_start..]);
    LoweringResult::Applied(ensure_error_variant(&out, error))
}

/// Insert a `transfers { … }` clause into the handler body.
fn apply_transfers_clause(
    spec_text: &str,
    hypothesis: &InvariantHypothesis,
    from: &str,
    to: &str,
    amount: Option<&str>,
    authority: Option<&str>,
) -> LoweringResult {
    let Some(block) = handler_block(spec_text, &hypothesis.handler) else {
        return LoweringResult::NotExecutable(format!(
            "handler `{}` not present in the skeleton",
            hypothesis.handler
        ));
    };
    let body = &spec_text[block.body_start..block.body_end];
    if body.contains("transfers {") {
        return LoweringResult::AlreadyModeled;
    }
    let mut inner = format!("from {} to {}", from, to);
    if let Some(a) = amount {
        inner.push_str(&format!(" amount {}", a));
    }
    if let Some(a) = authority {
        inner.push_str(&format!(" authority {}", a));
    }
    let mut out = String::with_capacity(spec_text.len() + 96);
    out.push_str(&spec_text[..block.body_start]);
    out.push_str(&format!(
        "  // provenance: hypothesis {}\n  transfers {{ {} }}\n",
        hypothesis.id, inner
    ));
    out.push_str(&spec_text[block.body_start..]);
    LoweringResult::Applied(out)
}

/// Replace the skeleton's `type State` variants with the program's real
/// status-enum variants. Existing references are preserved by exact name or
/// by an unambiguous lifecycle-shaped mapping (`Init` → `Uninitialized`,
/// `Active` → `Open`). Unrelated variants are never remapped by position: an
/// ambiguous reference makes the lowering non-executable instead of silently
/// changing the spec's meaning. Runs before any transition lowering.
fn apply_state_adt_rewrite(spec_text: &str, variants: &[String]) -> LoweringResult {
    if variants.len() < 2 {
        return LoweringResult::NotExecutable(
            "a state machine needs at least two variants".to_string(),
        );
    }
    let old = state_variants(spec_text);
    if old.is_empty() {
        return LoweringResult::NotExecutable(
            "spec has no `type State` ADT to rewrite".to_string(),
        );
    }
    if old == variants {
        return LoweringResult::AlreadyModeled;
    }

    let mut reference_map = std::collections::BTreeMap::new();
    for old_v in &old {
        let needle = format!("State.{old_v}");
        if !spec_text.contains(&needle) {
            continue;
        }
        let Some(new_v) = map_state_variant(old_v, variants) else {
            return LoweringResult::NotExecutable(format!(
                "cannot safely map existing `State.{old_v}` reference into the confirmed \
                 variants ({}) — preserve the variant name or provide an explicit transition \
                 mapping",
                variants.join(" | ")
            ));
        };
        reference_map.insert(old_v.clone(), new_v.to_string());
    }

    // Rewrite the `type State` block (same-line or multi-line form).
    let mut out = String::with_capacity(spec_text.len() + 64);
    let mut in_state = false;
    let mut replaced = false;
    for line in spec_text.split_inclusive('\n') {
        let trimmed = line.trim();
        if !replaced && trimmed.starts_with("type State") {
            in_state = true;
            out.push_str("type State\n");
            for v in variants {
                out.push_str(&format!("  | {}\n", v));
            }
            replaced = true;
            continue;
        }
        if in_state {
            if trimmed.starts_with('|') {
                continue; // old variant lines, superseded
            }
            in_state = false;
        }
        out.push_str(line);
    }

    let state_ref = regex::Regex::new(r"State\.([A-Za-z_][A-Za-z0-9_]*)\b")
        .expect("static state reference regex");
    out = state_ref
        .replace_all(&out, |caps: &regex::Captures<'_>| {
            reference_map
                .get(&caps[1])
                .map(|v| format!("State.{v}"))
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned();
    LoweringResult::Applied(out)
}

fn map_state_variant<'a>(old: &str, variants: &'a [String]) -> Option<&'a str> {
    if let Some(exact) = variants.iter().find(|v| v.as_str() == old) {
        return Some(exact);
    }
    let old_lower = old.to_ascii_lowercase();
    let class = if looks_uninitialized_variant(&old_lower) {
        looks_uninitialized_variant as fn(&str) -> bool
    } else if looks_active_variant(&old_lower) {
        looks_active_variant as fn(&str) -> bool
    } else {
        return None;
    };
    let mut matches = variants.iter().filter(|v| class(&v.to_ascii_lowercase()));
    let only = matches.next()?;
    matches.next().is_none().then_some(only.as_str())
}

fn looks_uninitialized_variant(lower: &str) -> bool {
    lower.contains("uninit")
        || lower == "init"
        || lower.contains("empty")
        || lower.contains("created")
}

fn looks_active_variant(lower: &str) -> bool {
    lower == "active"
        || (lower.contains("initialized") && !lower.contains("uninitialized"))
        || lower == "open"
}

/// Ensure `type Error` declares `variant`; append it to the existing
/// block (or create the block before the first handler) when missing.
fn ensure_error_variant(spec_text: &str, variant: &str) -> String {
    // Already declared anywhere in an Error ADT?
    let mut in_error = false;
    for line in spec_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("type Error") {
            in_error = true;
            if rest
                .split('|')
                .skip(1)
                .any(|p| variant_name(p).as_deref() == Some(variant))
            {
                return spec_text.to_string();
            }
            continue;
        }
        if in_error {
            if let Some(rest) = trimmed.strip_prefix('|') {
                if variant_name(rest).as_deref() == Some(variant) {
                    return spec_text.to_string();
                }
            } else if !trimmed.is_empty() {
                in_error = false;
            }
        }
    }

    // Append to the existing block: after its last `| …` line.
    let mut out = String::with_capacity(spec_text.len() + 32);
    let mut in_error = false;
    let mut inserted = false;
    for line in spec_text.split_inclusive('\n') {
        let trimmed = line.trim();
        if !inserted && trimmed.starts_with("type Error") {
            in_error = true;
            // Same-line form: `type Error | A | B` → extend in place.
            if trimmed.contains('|') && !trimmed.ends_with("type Error") {
                out.push_str(line.trim_end_matches('\n'));
                out.push_str(&format!(" | {}\n", variant));
                inserted = true;
                in_error = false;
                continue;
            }
            out.push_str(line);
            continue;
        }
        if in_error && !inserted && !trimmed.starts_with('|') {
            out.push_str(&format!("  | {}\n", variant));
            inserted = true;
            in_error = false;
        }
        out.push_str(line);
    }
    if in_error && !inserted {
        // `type Error` block ran to EOF.
        out.push_str(&format!("  | {}\n", variant));
        inserted = true;
    }
    if inserted {
        return out;
    }

    // No `type Error` at all: declare one before the first handler.
    let mut out = String::with_capacity(spec_text.len() + 48);
    let block = format!("type Error\n  | {}\n\n", variant);
    let mut placed = false;
    for line in spec_text.split_inclusive('\n') {
        if !placed && line.trim_start().starts_with("handler ") {
            out.push_str(&block);
            placed = true;
        }
        out.push_str(line);
    }
    if !placed {
        out.push_str(&block);
    }
    out
}

/// Insert `auth <signer>` as the first clause of the handler body.
fn apply_auth_clause(
    spec_text: &str,
    hypothesis: &InvariantHypothesis,
    signer: &str,
) -> LoweringResult {
    let Some(block) = handler_block(spec_text, &hypothesis.handler) else {
        return LoweringResult::NotExecutable(format!(
            "handler `{}` not present in the skeleton",
            hypothesis.handler
        ));
    };
    let body = &spec_text[block.body_start..block.body_end];
    if body.lines().any(|l| {
        let t = l.trim();
        t.starts_with("auth ") || t == "permissionless"
    }) {
        return LoweringResult::AlreadyModeled;
    }
    // The injected clause supersedes the skeleton's auth placeholder line.
    let body_without_placeholder: String = body
        .split_inclusive('\n')
        .filter(|l| l.trim() != "// TODO: auth <signer>")
        .collect();
    let mut out = String::with_capacity(spec_text.len() + 64);
    out.push_str(&spec_text[..block.body_start]);
    out.push_str(&format!(
        "  // provenance: hypothesis {}\n  auth {}\n",
        hypothesis.id, signer
    ));
    out.push_str(&body_without_placeholder);
    out.push_str(&spec_text[block.body_end..]);
    LoweringResult::Applied(out)
}

/// Ensure the handler declares an init-shaped `: State.<pre> ->
/// State.<post>` transition, resolving variant names against the spec's
/// own `type State` ADT.
fn apply_lifecycle_transition(spec_text: &str, hypothesis: &InvariantHypothesis) -> LoweringResult {
    let Some(block) = handler_block(spec_text, &hypothesis.handler) else {
        return LoweringResult::NotExecutable(format!(
            "handler `{}` not present in the skeleton",
            hypothesis.handler
        ));
    };
    let decl = &spec_text[block.decl_start..block.body_start];
    let decl_head = decl.split('{').next().unwrap_or(decl).to_string();
    if let Some((pre, post)) = existing_transition(&decl_head) {
        if pre != post {
            // A real (state-changing) transition already models the edge.
            return LoweringResult::AlreadyModeled;
        }
        // Placeholder self-loop (`State.Init -> State.Init`, the Anchor
        // skeleton default) — a self-loop cannot express init-once; fall
        // through and rewrite it.
    }
    let variants = state_variants(spec_text);
    let Some((pre, post)) = resolve_init_transition(&variants) else {
        return LoweringResult::NotExecutable(
            "spec has no `type State` ADT with two variants to anchor the transition".to_string(),
        );
    };
    let Some(brace_offset) = decl.find('{') else {
        return LoweringResult::NotExecutable(format!(
            "handler `{}` declaration has no body to annotate",
            hypothesis.handler
        ));
    };
    // Strip any existing (self-loop) annotation before re-annotating.
    let head_full = decl[..brace_offset].trim_end();
    let head = head_full
        .split_once(" : State.")
        .map(|(h, _)| h)
        .unwrap_or(head_full)
        .trim_end();
    let mut out = String::with_capacity(spec_text.len() + 48);
    out.push_str(&spec_text[..block.decl_start]);
    out.push_str(&format!("{} : State.{} -> State.{} {{", head, pre, post));
    out.push_str(&spec_text[block.decl_start + brace_offset + 1..]);
    LoweringResult::Applied(out)
}

/// Extract `(pre, post)` from an existing `: A -> B` handler annotation.
/// Anchored on the `->` (a `:` also appears in param annotations like
/// `(cap : U64)`).
fn existing_transition(decl_head: &str) -> Option<(String, String)> {
    let (before, after) = decl_head.split_once("->")?;
    let pre = before.rsplit(':').next()?.trim();
    let post = after.trim();
    (!pre.is_empty() && !post.is_empty()).then(|| (pre.to_string(), post.to_string()))
}

struct HandlerBlock {
    /// Byte offset of the `handler` keyword.
    decl_start: usize,
    /// Byte offset just past the opening `{`'s newline (start of the body's
    /// first line).
    body_start: usize,
    /// Byte offset of the body's closing line.
    body_end: usize,
}

/// Locate a handler's declaration and body in spec text. Assumes the
/// emitted-skeleton shape every producer in this repo uses: `handler
/// <name> …` on one line ending with `{`, body closed by a line whose
/// trimmed content is `}`.
fn handler_block(spec_text: &str, name: &str) -> Option<HandlerBlock> {
    let mut offset = 0usize;
    let mut decl_start = None;
    let mut body_start = None;
    for line in spec_text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if decl_start.is_none() {
            if let Some(rest) = trimmed.strip_prefix("handler ") {
                let found = rest
                    .trim_start()
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("");
                if found == name && line.contains('{') {
                    decl_start = Some(offset + (line.len() - line.trim_start().len()));
                    body_start = Some(offset + line.len());
                }
            }
        } else if line.trim() == "}" {
            return Some(HandlerBlock {
                decl_start: decl_start?,
                body_start: body_start?,
                body_end: offset,
            });
        }
        offset += line.len();
    }
    None
}

/// Collect the `type State` ADT's variant names — same-line (`type State |
/// A | B`) and following-lines (`  | A`) forms.
fn state_variants(spec_text: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let mut in_state = false;
    for line in spec_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("type State") {
            in_state = true;
            for piece in rest.split('|').skip(1) {
                if let Some(v) = variant_name(piece) {
                    variants.push(v);
                }
            }
            continue;
        }
        if in_state {
            if let Some(rest) = trimmed.strip_prefix('|') {
                if let Some(v) = variant_name(rest) {
                    variants.push(v);
                }
            } else if !trimmed.is_empty() {
                break;
            }
        }
    }
    variants
}

fn variant_name(piece: &str) -> Option<String> {
    let name: String = piece
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Pick (pre, post) variants for an init transition: an
/// uninitialized-looking variant leads, an active/initialized-looking one
/// lands; fall back to declaration order.
fn resolve_init_transition(variants: &[String]) -> Option<(String, String)> {
    if variants.len() < 2 {
        return None;
    }
    let pre = variants
        .iter()
        .find(|v| looks_uninitialized_variant(&v.to_ascii_lowercase()))
        .cloned()
        .unwrap_or_else(|| variants[0].clone());
    let post = variants
        .iter()
        .find(|v| **v != pre && looks_active_variant(&v.to_ascii_lowercase()))
        .cloned()
        .or_else(|| variants.iter().find(|v| **v != pre).cloned())?;
    Some((pre, post))
}

/// Parse + completeness gate: `Ok(error_lint_count)` when the text parses,
/// `Err` otherwise. Ratify commits a lowering only when the candidate
/// parses and introduces no new Error-severity lints over the baseline.
pub fn validate_spec_text(spec_text: &str) -> Result<usize> {
    let parsed = crate::chumsky_adapter::parse_str(spec_text)?;
    let warnings = crate::check::check_completeness(&parsed);
    Ok(warnings
        .iter()
        .filter(|w| matches!(w.severity, crate::check::Severity::Error))
        .count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::hypothesize::HypothesisClass;

    fn hyp(handler: &str, lowering: Option<HypothesisLowering>) -> InvariantHypothesis {
        InvariantHypothesis {
            id: format!("h-test-{}", handler),
            class: HypothesisClass::Authorization,
            handler: handler.to_string(),
            claim: String::new(),
            evidence: Vec::new(),
            payoff: String::new(),
            backend: String::new(),
            assurance: "checking".to_string(),
            confidence: crate::cluster::Confidence::High,
            lowering,
        }
    }

    const SKELETON: &str = "spec Demo\n\ntype State\n  | Init\n  | Active\n\ntype Error\n  | InvalidArgument\n  | Unauthorized\n\nhandler set_fee {\n  // accounts, requires, effect, transfers — filled by interview\n}\n\nhandler initialize {\n  // accounts, requires, effect, transfers — filled by interview\n}\n";

    #[test]
    fn auth_clause_injects_and_validates() {
        let h = hyp("set_fee", None);
        let LoweringResult::Applied(out) = apply_auth_clause(SKELETON, &h, "admin") else {
            panic!("expected Applied");
        };
        assert!(out.contains("auth admin"));
        assert!(out.contains("provenance: hypothesis h-test-set_fee"));
        validate_spec_text(&out).expect("lowered spec must parse");
    }

    #[test]
    fn auth_clause_is_idempotent() {
        let h = hyp("set_fee", None);
        let LoweringResult::Applied(once) = apply_auth_clause(SKELETON, &h, "admin") else {
            panic!("expected Applied");
        };
        assert!(matches!(
            apply_auth_clause(&once, &h, "admin"),
            LoweringResult::AlreadyModeled
        ));
    }

    #[test]
    fn auth_clause_missing_handler_is_not_executable() {
        let h = hyp("nonexistent", None);
        assert!(matches!(
            apply_auth_clause(SKELETON, &h, "admin"),
            LoweringResult::NotExecutable(_)
        ));
    }

    #[test]
    fn lifecycle_transition_annotates_bare_handler() {
        let h = hyp("initialize", None);
        let LoweringResult::Applied(out) = apply_lifecycle_transition(SKELETON, &h) else {
            panic!("expected Applied");
        };
        assert!(
            out.contains("handler initialize : State.Init -> State.Active {"),
            "{out}"
        );
        validate_spec_text(&out).expect("lowered spec must parse");
    }

    #[test]
    fn lifecycle_transition_existing_annotation_already_modeled() {
        let spec = SKELETON.replace(
            "handler initialize {",
            "handler initialize : State.Init -> State.Active {",
        );
        let h = hyp("initialize", None);
        assert!(matches!(
            apply_lifecycle_transition(&spec, &h),
            LoweringResult::AlreadyModeled
        ));
    }

    #[test]
    fn lifecycle_without_state_adt_is_not_executable() {
        let spec = "spec Demo\n\nhandler initialize {\n  // x\n}\n";
        let h = hyp("initialize", None);
        assert!(matches!(
            apply_lifecycle_transition(spec, &h),
            LoweringResult::NotExecutable(_)
        ));
    }

    #[test]
    fn lifecycle_transition_rewrites_placeholder_self_loop() {
        // Anchor-skeleton shape: parameterized handler with a placeholder
        // self-loop — a self-loop cannot express init-once, so the
        // lowering rewrites it (the param's `:` must not confuse the
        // transition parse).
        let spec = "spec Demo\n\ntype State\n  | Init\n  | Active\n\ntype Error\n  | Unauthorized\n\nhandler initialize (cap : U64) : State.Init -> State.Init {\n  // TODO: requires\n}\n";
        let h = hyp("initialize", None);
        let LoweringResult::Applied(out) = apply_lifecycle_transition(spec, &h) else {
            panic!("expected Applied on a self-loop placeholder");
        };
        assert!(
            out.contains("handler initialize (cap : U64) : State.Init -> State.Active {"),
            "{out}"
        );
        validate_spec_text(&out).expect("rewritten spec must parse");
    }

    #[test]
    fn requires_bound_injects_and_declares_error_variant() {
        let h = hyp("set_fee", None);
        let LoweringResult::Applied(out) =
            apply_requires_bound(SKELETON, &h, "fee", "<=", "1000000", "CapTooHigh")
        else {
            panic!("expected Applied");
        };
        assert!(
            out.contains("requires fee <= 1000000 else CapTooHigh"),
            "{out}"
        );
        // The error variant was appended to the existing Error block.
        assert!(out.contains("| CapTooHigh"), "{out}");
        validate_spec_text(&out).expect("lowered spec must parse");
        // Idempotent on re-application.
        assert!(matches!(
            apply_requires_bound(&out, &h, "fee", "<=", "1000000", "CapTooHigh"),
            LoweringResult::AlreadyModeled
        ));
    }

    #[test]
    fn requires_bound_existing_error_variant_not_duplicated() {
        let h = hyp("set_fee", None);
        let LoweringResult::Applied(out) =
            apply_requires_bound(SKELETON, &h, "fee", "<=", "100", "Unauthorized")
        else {
            panic!("expected Applied");
        };
        assert_eq!(out.matches("| Unauthorized").count(), 1, "{out}");
        validate_spec_text(&out).expect("lowered spec must parse");
    }

    #[test]
    fn transfers_clause_injects_and_parses() {
        let h = hyp("set_fee", None);
        let LoweringResult::Applied(out) = apply_transfers_clause(
            SKELETON,
            &h,
            "vault_ta",
            "user_ta",
            Some("amount"),
            Some("vault"),
        ) else {
            panic!("expected Applied");
        };
        assert!(
            out.contains("transfers { from vault_ta to user_ta amount amount authority vault }"),
            "{out}"
        );
        validate_spec_text(&out).expect("lowered spec must parse");
        assert!(matches!(
            apply_transfers_clause(&out, &h, "vault_ta", "user_ta", None, None),
            LoweringResult::AlreadyModeled
        ));
    }

    #[test]
    fn state_adt_rewrite_replaces_variants_and_refs() {
        // Skeleton with annotated handlers referencing the placeholder
        // variants — the rewrite must keep them parsing (positional map).
        let spec = "spec Demo\n\ntype State\n  | Init\n  | Active\n\ntype Error\n  | Unauthorized\n\nhandler initialize : State.Init -> State.Active {\n  // x\n}\n";
        let new: Vec<String> = ["Uninitialized", "Open", "Frozen"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let LoweringResult::Applied(out) = apply_state_adt_rewrite(spec, &new) else {
            panic!("expected Applied");
        };
        assert!(out.contains("| Uninitialized"), "{out}");
        assert!(out.contains("| Frozen"), "{out}");
        assert!(!out.contains("| Init\n"), "{out}");
        // Positional: Init → Uninitialized, Active → Open.
        assert!(
            out.contains("handler initialize : State.Uninitialized -> State.Open {"),
            "{out}"
        );
        validate_spec_text(&out).expect("rewritten spec must parse");
        assert!(matches!(
            apply_state_adt_rewrite(&out, &new),
            LoweringResult::AlreadyModeled
        ));
    }

    #[test]
    fn state_adt_rewrite_preserves_existing_variant_by_name_not_position() {
        let spec = "spec Demo\n\ntype State\n  | Init\n  | Active\n\ntype Error\n  | Unauthorized\n\nhandler tick : State.Active -> State.Active {\n  // x\n}\n";
        let new: Vec<String> = ["Uninitialized", "Pending", "Active"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let LoweringResult::Applied(out) = apply_state_adt_rewrite(spec, &new) else {
            panic!("expected Applied");
        };
        assert!(
            out.contains("handler tick : State.Active -> State.Active {"),
            "{out}"
        );
        assert!(!out.contains("State.Pending -> State.Pending"), "{out}");
    }

    #[test]
    fn state_adt_rewrite_rejects_unrelated_reference_without_mapping() {
        let spec = "spec Demo\n\ntype State\n  | Draft\n  | Live\n\ntype Error\n  | Unauthorized\n\nhandler publish : State.Draft -> State.Live {\n  // x\n}\n";
        let new: Vec<String> = ["Pending", "Active"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let LoweringResult::NotExecutable(reason) = apply_state_adt_rewrite(spec, &new) else {
            panic!("expected NotExecutable");
        };
        assert!(reason.contains("cannot safely map existing `State.Draft`"));
    }

    #[test]
    fn state_variants_same_line_and_multiline() {
        assert_eq!(
            state_variants("type State | Uninitialized | Active\n"),
            vec!["Uninitialized", "Active"]
        );
        assert_eq!(
            state_variants("type State\n  | Init\n  | Active of { x : U64 }\n\nother"),
            vec!["Init", "Active"]
        );
    }

    #[test]
    fn resolve_transition_prefers_semantic_names() {
        let vs: Vec<String> = ["Active", "Uninitialized"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            resolve_init_transition(&vs),
            Some(("Uninitialized".to_string(), "Active".to_string()))
        );
    }
}
