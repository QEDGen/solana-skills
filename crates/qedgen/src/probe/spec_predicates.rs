//! Spec-aware probe predicates — the runtime-agnostic, compose-able
//! primitives the auditor chains into kill-chains (SKILL.md
//! "Compose-with-what cookbook"). Each `predicate_*` inspects the parsed
//! spec only (never implementation source) and returns [`Finding`]s for
//! categories the spec is silent on. Split out of `probe/mod.rs`; the
//! enumerator there drives them per handler.

use super::{Category, Finding, Severity};
use crate::check::{ParsedHandler, ParsedSpec};
use sha2::{Digest, Sha256};

/// Spec-aware predicate: handler has no `auth X` clause and is not marked
/// `permissionless`. Mutual-exclusion enforcement (can't have both) lives
/// in `check.rs`; here we only gate on the negative shape.
pub(crate) fn predicate_missing_signer(handler: &ParsedHandler) -> Option<Finding> {
    if handler.who.is_some() || handler.permissionless {
        return None;
    }

    Some(Finding {
        id: stable_id(&handler.name, Category::MissingSigner.tag()),
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
        category_tag: Category::MissingSigner.tag().to_string(),
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
pub(crate) fn predicate_arbitrary_cpi(handler: &ParsedHandler) -> Option<Finding> {
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
        id: stable_id(&handler.name, Category::ArbitraryCpi.tag()),
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
        category_tag: Category::ArbitraryCpi.tag().to_string(),
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
pub(crate) fn predicate_arithmetic_overflow_wrapping(handler: &ParsedHandler) -> Vec<Finding> {
    let mut out = Vec::new();
    for eff in &handler.effects {
        let (field, op) = (&eff.field, &eff.op);
        let (severity, kind) = match op.as_str() {
            "add_wrap" | "sub_wrap" => (Severity::High, "wrapping"),
            "add_sat" | "sub_sat" => (Severity::Medium, "saturating"),
            _ => continue,
        };

        out.push(Finding {
            id: stable_id(
                &format!("{}::{}::{}", handler.name, field, op),
                Category::ArithmeticOverflowWrapping.tag(),
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
            category_tag: Category::ArithmeticOverflowWrapping.tag().to_string(),
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
pub(crate) fn predicate_lifecycle_one_shot_violation(
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
        id: stable_id(&handler.name, Category::LifecycleOneShotViolation.tag()),
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
        category_tag: Category::LifecycleOneShotViolation.tag().to_string(),
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
pub(crate) fn predicate_unbounded_amount_param(handler: &ParsedHandler) -> Vec<Finding> {
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
            .any(|e| param_referenced(&e.value, pname));
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
                Category::UnboundedAmountParam.tag(),
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
            category_tag: Category::UnboundedAmountParam.tag().to_string(),
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
pub(crate) fn predicate_permissionless_state_writer(handler: &ParsedHandler) -> Option<Finding> {
    if !handler.permissionless {
        return None;
    }
    if handler.effects.is_empty() {
        return None;
    }

    let mutated_fields: Vec<&str> = handler.effects.iter().map(|e| e.field.as_str()).collect();

    Some(Finding {
        id: stable_id(&handler.name, Category::PermissionlessStateWriter.tag()),
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
        category_tag: Category::PermissionlessStateWriter.tag().to_string(),
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
pub(crate) fn predicate_init_without_pda(
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
        id: stable_id(&handler.name, Category::InitWithoutPda.tag()),
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
        category_tag: Category::InitWithoutPda.tag().to_string(),
        reproducer: None,
        gated_by: None,
    })
}

/// Spec-aware predicate: state field read somewhere in the spec (`auth`,
/// `requires`, effect RHS, property expression) but never
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
pub(crate) fn predicate_stored_field_never_written(spec: &ParsedSpec) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Step 1: collect every field name that any handler `effect` writes.
    let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for h in &spec.handlers {
        for eff in &h.effects {
            written.insert(eff.field.as_str());
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

                // effect RHS reads (e.g. `field := s.other_field + 1`).
                if !is_reader {
                    for eff in &h.effects {
                        if needles.iter().any(|n| eff.value.contains(n.as_str())) {
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
                    Category::StoredFieldNeverWritten.tag(),
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
                category_tag: Category::StoredFieldNeverWritten.tag().to_string(),
                reproducer: None,
                gated_by: None,
            });
        }
    }

    findings
}

/// Integer-typed DSL types — the scalar quantities that flow into
/// transfer amounts or arithmetic effects.
pub(crate) fn is_integer_type(ty: &str) -> bool {
    matches!(
        ty,
        "U8" | "U16" | "U32" | "U64" | "U128" | "I8" | "I16" | "I32" | "I64" | "I128" | "Nat"
    )
}

/// Word-boundary substring match for `param` in `value`. Surface-level
/// by design — misses obfuscated forms; the auditor is the backstop.
pub(crate) fn param_referenced(value: &str, param: &str) -> bool {
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
pub(crate) fn requires_bounds_param(expr: &str, param: &str) -> bool {
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

pub(crate) fn stable_id(handler: &str, category: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(handler.as_bytes());
    hasher.update(b":");
    hasher.update(category.as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash).chars().take(8).collect()
}
