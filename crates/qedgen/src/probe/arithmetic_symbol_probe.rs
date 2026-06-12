//! Arithmetic-symbol catalog probes — v2.22 Slice 1.
//!
//! Runtime-agnostic source scanners that fire on arithmetic operators
//! whose *symbol* (not result) is the bug. Closes the subscriptions
//! bench's two firm-High misses (CAN-H1 `saturating_sub`, CAN-H3
//! `checked_sub`) — both arithmetic operators that are correct in
//! isolation; the bug is the failure-mode interaction with the
//! surrounding control flow.
//!
//! ## Rules in this module
//!
//! - **`silent_success_arithmetic`** (S1.1, HIGH) — `saturating_sub` /
//!   `saturating_add` on a timestamp-shape receiver whose result gates
//!   a non-trivial effect. The 0-or-MAX boundary value silently opens
//!   a fund-flow gate that should have stayed closed.
//!
//! - **`graceful_error_as_dos`** (S1.2, HIGH) — `checked_sub` /
//!   `checked_add` / `checked_mul` in an init / create path where
//!   `Err` propagation permanently bricks a deterministic PDA.
//!   The operator is correct in isolation; the bug is the failure-mode
//!   interaction with the address's permanence. Closes CAN-H3 on the
//!   subscriptions bench.
//!
//! - **`unchecked_arith_with_fund_flow`** (S1.3, LOW) — unchecked
//!   `<ident> * <literal>` / `+ <literal>` / `- <literal>` inside a
//!   function whose body also contains a token / system CPI. The
//!   arithmetic is locally safe today under upstream bounds but the
//!   local site makes no explicit invariant claim — preventive
//!   recommendation (use `checked_*`). Closes CAN-I3 on the
//!   subscriptions bench.
//!
//! ## Why source-scan, not spec
//!
//! These bugs fire on the deployed Rust source, not on `.qedspec`
//! state. Mirrors `pinocchio_probe::scan_program`'s shape — walk
//! `*.rs` under the project root, regex-match the pattern, emit
//! `Finding`s into the same envelope as the spec-aware probes.
//!
//! ## Reproducers
//!
//! Each finding ships a `Reproducer::MolluskPrompt` (same template
//! mechanism the Pinocchio probes use) pointing at a per-rule markdown
//! under `references/probes/arithmetic_symbol/<rule>.md`. The agent
//! fills the litesvm test body using the cited handler, account,
//! and timestamp source. Time-to-fired-repro target: ≤ 20 min per
//! finding.

use anyhow::Result;
use regex::Regex;
use std::path::{Path, PathBuf};

use crate::probe::{Category, Finding, Reproducer, Severity};

/// Entry point: walk `<root>/src/**/*.rs` and emit findings. Returns
/// an empty vec when no matches surface (no errors — the absence of a
/// match is informational, not a failure).
pub fn scan_program(project_root: &Path) -> Result<Vec<Finding>> {
    let src_dir = project_root.join("src");
    if !src_dir.exists() {
        // No `src/` — not a Rust crate root. Defer to the caller's
        // bootstrap envelope; this probe has nothing to say.
        return Ok(Vec::new());
    }
    let rs_files = collect_rust_files(&src_dir)?;
    let mut findings = Vec::new();
    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(project_root)
            .unwrap_or(file)
            .to_path_buf();
        findings.extend(scan_silent_success_arithmetic(&rel, &source));
        findings.extend(scan_graceful_error_as_dos(&rel, &source));
        findings.extend(scan_unchecked_arith_with_fund_flow(&rel, &source));
    }
    Ok(findings)
}

/// S1.1 — `silent_success_arithmetic` scanner. Pattern criteria:
///
/// 1. A call site of `saturating_sub` or `saturating_add`.
/// 2. The receiver matches a timestamp-shape pattern (`current_ts`,
///    `now`, `Clock::get()?.unix_timestamp`, `clock.slot`, `slot`,
///    `epoch`, `block_height`, or an identifier ending in `_ts` / `_secs`).
/// 3. The site is inside a function whose body, in the lines AFTER
///    the call, contains a comparison (`>=`, `>`) gating an
///    `if`/`else` branch — the canonical "elapsed time opens a gate"
///    shape.
///
/// False-positive guard: the receiver-type check rejects calls on
/// counter / amount values that happen to use `saturating_sub` for
/// fee accounting. Only timestamp-shape receivers fire.
///
/// For v2.22 first ship the scanner emits one finding per call site
/// (not per gated branch). Iteration on bench feedback may tighten
/// the gate-detection step in v2.22.x.
pub(crate) fn scan_silent_success_arithmetic(rel_file: &Path, source: &str) -> Vec<Finding> {
    // Receiver pattern: identifier or `Clock::get()?.unix_timestamp` /
    // `*deref` of one of those. We accept up to ~64 chars of receiver
    // to keep the regex tractable; longer expressions (chained
    // method calls) won't match, which is a deliberate
    // false-negative.
    //
    // The body group `(?P<recv>...)` captures the receiver text so we
    // can re-check the timestamp-shape predicate at filter time.
    let call_re = Regex::new(
        r"(?m)\b(?P<recv>\*?[\w\.\?\(\)\:]{1,64})\.(?P<op>saturating_sub|saturating_add)\s*\(",
    )
    .expect("static regex compiles");

    let mut out = Vec::new();
    for caps in call_re.captures_iter(source) {
        let m = caps.get(0).unwrap();
        let recv = caps.name("recv").unwrap().as_str();
        if !is_timestamp_shape(recv) {
            continue;
        }
        let line = byte_offset_to_line(source, m.start());
        let fn_name = enclosing_fn_name(source, m.start());
        // Look at the next ~400 chars of source for the gating
        // comparison shape. This is the "elapsed >= threshold opens
        // a non-trivial effect" tell.
        let window = &source[m.end()..source.len().min(m.end() + 400)];
        if !window_has_gating_comparison(window) {
            continue;
        }

        let finding_id = make_id(rel_file, line, "silent_success_arithmetic");
        let mut subs = std::collections::BTreeMap::new();
        subs.insert("FILE".to_string(), rel_file.display().to_string());
        subs.insert("LINE".to_string(), line.to_string());
        subs.insert("RECEIVER".to_string(), recv.to_string());
        subs.insert(
            "OPERATOR".to_string(),
            caps.name("op").unwrap().as_str().to_string(),
        );
        subs.insert(
            "FN".to_string(),
            fn_name.clone().unwrap_or_else(|| "<unknown>".into()),
        );

        out.push(Finding {
            id: finding_id.clone(),
            category: Category::SilentSuccessArithmetic,
            severity: Severity::High,
            handler: fn_name.unwrap_or_else(|| "<unknown>".into()),
            spec_silent_on: format!(
                "`{}.{}(...)` at {}:{} returns the boundary value (0 / MAX) \
                 when the conceptual operation underflows. The downstream \
                 `>=` comparison fires for both 'no time elapsed' and 'an \
                 undefined amount of negative time elapsed' — collapsing \
                 two semantically distinct states.",
                recv,
                caps.name("op").unwrap().as_str(),
                rel_file.display(),
                line
            ),
            suppression_hint: "Replace `saturating_*` with an explicit underflow check: \
                 `if current_ts < start_ts { return Err(...) }`. The early \
                 return makes the 'time hasn't elapsed' branch distinguishable \
                 from the 'time has elapsed' branch."
                .to_string(),
            investigation_hint: format!(
                "Trace the gated effect downstream of the comparison at \
                 {}:{}. Confirm whether the boundary value (0 / MAX) is \
                 ever a valid input or always a bug-condition signal. \
                 If the gate touches funds (transfer / mint / state \
                 advance), this is a fund-flow leak.",
                rel_file.display(),
                line
            ),
            category_tag: "silent_success_arithmetic".to_string(),
            reproducer: Some(Reproducer::MolluskPrompt {
                template_path:
                    "references/probes/arithmetic_symbol/silent_success_arithmetic.md#reproducer"
                        .to_string(),
                substitutions: subs,
                repro_path: format!(".qed/probes/arithmetic_symbol/{}/repro.rs", finding_id),
            }),
            gated_by: None,
        });
    }
    out
}

/// S1.2 — `graceful_error_as_dos` scanner. Pattern criteria:
///
/// 1. A call site of `checked_sub` / `checked_add` / `checked_mul`.
/// 2. The enclosing fn name contains `init` / `create` / `initialize`
///    (case-insensitive) — the lifecycle handlers that materialise a
///    deterministic address.
/// 3. The fn body OR signature signals PDA / seed-driven derivation:
///    - body contains `find_program_address`, OR
///    - signature contains `seeds:` / `&[Seed` / `&Seed<`, OR
///    - body contains `invoke_signed(` (signed CPI implies a PDA
///      derivation upstream).
/// 4. The operator's `Err` arm exits the function via `?` (the most
///    common shape) or `return Err(...)`.
///
/// Surfaces as HIGH severity — the failure mode bricks a permanent
/// address every caller subsequently hits.
///
/// False-positive guard: when none of the PDA / seed signals fire, the
/// arithmetic is treated as user-funded (a user can retry with
/// corrected inputs) and the finding is suppressed.
pub(crate) fn scan_graceful_error_as_dos(rel_file: &Path, source: &str) -> Vec<Finding> {
    let call_re = Regex::new(r"\.(?P<op>checked_sub|checked_add|checked_mul)\s*\(")
        .expect("static regex compiles");

    let mut out = Vec::new();
    for caps in call_re.captures_iter(source) {
        let m = caps.get(0).unwrap();
        let op = caps.name("op").unwrap().as_str();
        let line = byte_offset_to_line(source, m.start());
        let Some(fn_name) = enclosing_fn_name(source, m.start()) else {
            continue;
        };
        if !is_init_shape(&fn_name) {
            continue;
        }
        let fn_body = enclosing_fn_body(source, m.start());
        if !body_signals_pda(&fn_body) {
            continue;
        }
        // Confirm the Err arm exits: look in the ~120 chars after the
        // call for `?` or `return Err`. The propagation can chain
        // through `.ok_or(...)` / `.ok_or_else(...)`.
        let window = &source[m.end()..source.len().min(m.end() + 160)];
        if !window.contains('?') && !window.contains("return Err") {
            continue;
        }

        let finding_id = make_id(rel_file, line, "graceful_error_as_dos");
        let mut subs = std::collections::BTreeMap::new();
        subs.insert("FILE".to_string(), rel_file.display().to_string());
        subs.insert("LINE".to_string(), line.to_string());
        subs.insert("OPERATOR".to_string(), op.to_string());
        subs.insert("FN".to_string(), fn_name.clone());

        out.push(Finding {
            id: finding_id.clone(),
            category: Category::GracefulErrorAsDos,
            severity: Severity::High,
            handler: fn_name.clone(),
            spec_silent_on: format!(
                "`{}` at {}:{} inside `{}` propagates `Err` via `?` on \
                 a PDA-init path. The PDA's seeds are deterministic; nobody \
                 holds its private key; if the operator returns `None` on the \
                 first call, every subsequent call hits the same failure — \
                 the address is permanently locked.",
                op,
                rel_file.display(),
                line,
                fn_name
            ),
            suppression_hint: "Distinguish 'attacker pre-funded the PDA' from 'genuine \
                 arithmetic underflow' explicitly. For pre-fund DoS, accept \
                 the existing lamports and skip the transfer (idempotent \
                 init). For genuine overflow on attacker-controlled inputs, \
                 reject earlier in the handler via a `requires`-style \
                 precondition check."
                .to_string(),
            investigation_hint: format!(
                "Read `{}` around line {}. Confirm: (a) the touched account \
                 reaches a `find_program_address` / signed CPI, so the \
                 address is deterministic; (b) the `Err` propagation has no \
                 alternate path — every caller hits the same operator. \
                 Then derive an attack: pre-fund the PDA with `lamports + 1` \
                 to force the underflow, observe permanent init failure.",
                rel_file.display(),
                line
            ),
            category_tag: "graceful_error_as_dos".to_string(),
            reproducer: Some(Reproducer::MolluskPrompt {
                template_path:
                    "references/probes/arithmetic_symbol/graceful_error_as_dos.md#reproducer"
                        .to_string(),
                substitutions: subs,
                repro_path: format!(".qed/probes/arithmetic_symbol/{}/repro.rs", finding_id),
            }),
            gated_by: None,
        });
    }
    out
}

/// S1.3 — `unchecked_arith_with_fund_flow` scanner. Pattern criteria:
///
/// 1. A bare `*` / `+` / `-` BinOp shape `<ident_path> <op>
///    <numeric_literal>` (e.g. `period_hours * 3600`, `slot + 100`).
///    The literal-on-RHS restriction is the v2.22 first-ship guard
///    that keeps false-positive volume tractable; the bench's
///    canonical CAN-I3 site matches this shape exactly.
/// 2. The enclosing fn body contains a CPI signal: `Transfer`,
///    `MintTo`, `invoke(`, `invoke_signed(`, `cpi::`, `token::`,
///    `system_program::`. The signal discriminates "arithmetic that
///    crosses into fund flow" from "arithmetic on book-keeping
///    counters" — the former is what the rule targets.
/// 3. The same line is not already inside a `checked_*` /
///    `saturating_*` call (those are already correctly defensive).
///
/// Surfaces as LOW severity — the recommendation is preventive
/// (`checked_*`) and most sites are safe today under upstream
/// bounds. The bench surfaces the pattern so the audit subagent can
/// triage and confirm the bound holds.
pub(crate) fn scan_unchecked_arith_with_fund_flow(rel_file: &Path, source: &str) -> Vec<Finding> {
    // `<ident_or_path> <space> [*+-] <space> <int_literal>`. The path
    // can include dots, indexes, and `_`. We accept up to ~48 chars to
    // keep matching tractable. Integer literals carry optional
    // underscores (`3_600`) and optional Rust type suffix (`100u64`).
    let bin_re = Regex::new(
        r"(?P<lhs>[A-Za-z_][\w\.\[\]]{0,48})\s*(?P<op>[*+\-])\s*(?P<rhs>\d[\d_]*(?:u\d{1,3}|i\d{1,3}|usize|isize)?)\b",
    )
    .expect("static regex compiles");

    let mut out = Vec::new();
    let mut seen_lines = std::collections::BTreeSet::new();
    for caps in bin_re.captures_iter(source) {
        let m = caps.get(0).unwrap();
        let lhs = caps.name("lhs").unwrap().as_str();
        let op = caps.name("op").unwrap().as_str();
        let rhs = caps.name("rhs").unwrap().as_str();
        // Skip patterns that aren't really arithmetic on user values:
        // numeric-only LHS (e.g. `1 - 2`), single-character LHS likely
        // to be `i + 1` index math (deliberate false negative for
        // v2.22 — bench evidence didn't surface index-arithmetic
        // findings).
        if lhs.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if lhs.len() < 3 {
            continue;
        }
        // Skip lifetime suffixes / pointer-like patterns.
        if lhs.contains("'") {
            continue;
        }
        // Skip if the operator is `-` and the LHS looks like a generic
        // bound (`T -> U`) or a pointer (`&-`); the heuristic is that
        // `<` or `>` adjacent to the match flags non-arithmetic shapes.
        let surrounding_start = m.start().saturating_sub(2);
        let surrounding = &source[surrounding_start..m.end()];
        if surrounding.contains("->") || surrounding.contains("<-") {
            continue;
        }
        // Reject sites already inside a `checked_*` / `saturating_*` /
        // `wrapping_*` call — the operator family is part of the
        // method name and the literal is a closing paren away. Look
        // at the ~80 chars preceding the match for any of those.
        let before_start = m.start().saturating_sub(80);
        let before = &source[before_start..m.start()];
        if before.contains("checked_")
            || before.contains("saturating_")
            || before.contains("wrapping_")
            || before.contains("overflowing_")
        {
            continue;
        }
        let line = byte_offset_to_line(source, m.start());
        // Comment-line guard: if the line starts (after whitespace)
        // with `//`, or if a `//` precedes the match on the same line,
        // skip. Comments routinely contain shapes like `// Token-2022`
        // that match the BinOp regex but are obviously not real
        // arithmetic.
        if line_is_commented(source, m.start()) {
            continue;
        }
        // Dedupe by (line, lhs) — multi-statement lines (e.g. macro
        // arg lists) trigger the regex multiple times.
        if !seen_lines.insert((line, lhs.to_string())) {
            continue;
        }
        let Some(fn_name) = enclosing_fn_name(source, m.start()) else {
            continue;
        };
        // Skip test fns: `fn test_*`, `fn *_tests`, `fn it_*` (the
        // common test-naming conventions). Inline `#[cfg(test)]
        // mod tests { ... }` blocks live in the same file as
        // production code, so the file-level filter in
        // `collect_rust_files` doesn't catch them.
        if is_test_fn_name(&fn_name) {
            continue;
        }
        let fn_body = enclosing_fn_body(source, m.start());
        if !body_signals_cpi(&fn_body) {
            continue;
        }

        let finding_id = make_id(rel_file, line, "unchecked_arith_with_fund_flow");
        let mut subs = std::collections::BTreeMap::new();
        subs.insert("FILE".to_string(), rel_file.display().to_string());
        subs.insert("LINE".to_string(), line.to_string());
        subs.insert("LHS".to_string(), lhs.to_string());
        subs.insert("OPERATOR".to_string(), op.to_string());
        subs.insert("RHS".to_string(), rhs.to_string());
        subs.insert("FN".to_string(), fn_name.clone());

        let suggested_op = match op {
            "*" => "checked_mul",
            "+" => "checked_add",
            "-" => "checked_sub",
            _ => "checked_<op>",
        };

        out.push(Finding {
            id: finding_id.clone(),
            category: Category::UncheckedArithWithFundFlow,
            severity: Severity::Low,
            handler: fn_name.clone(),
            spec_silent_on: format!(
                "`{} {} {}` at {}:{} inside `{}` uses bare arithmetic where the \
                 surrounding handler dispatches a CPI. The operation is locally \
                 safe today under upstream bounds on `{}`, but the local code \
                 makes no explicit invariant claim — if the upstream bound ever \
                 loosens, the operator wraps and the fund-flow effect proceeds \
                 on a corrupted value.",
                lhs,
                op,
                rhs,
                rel_file.display(),
                line,
                fn_name,
                lhs
            ),
            suppression_hint: format!(
                "Replace `{lhs} {op} {rhs}` with \
                 `{lhs}.{suggested_op}({rhs}).ok_or(/* explicit error */)?`. \
                 The explicit error path documents the local bound assumption \
                 and survives upstream changes that loosen `{lhs}`'s range."
            ),
            investigation_hint: format!(
                "Trace `{lhs}`'s upstream bound. If the bound is enforced at a \
                 distance (e.g. `MAX_X` constant elsewhere, a `requires` clause \
                 in a sibling handler), confirm whether the local code path is \
                 robust against the bound loosening. Otherwise, switch to the \
                 checked variant."
            ),
            category_tag: "unchecked_arith_with_fund_flow".to_string(),
            reproducer: Some(Reproducer::MolluskPrompt {
                template_path:
                    "references/probes/arithmetic_symbol/unchecked_arith_with_fund_flow.md#reproducer"
                        .to_string(),
                substitutions: subs,
                repro_path: format!(".qed/probes/arithmetic_symbol/{}/repro.rs", finding_id),
            }),
            gated_by: None,
        });
    }
    out
}

/// Test-fn name predicate. Skips inline `#[cfg(test)] mod tests {
/// fn test_* / fn it_* / fn *_test }` patterns that
/// `collect_rust_files`'s directory filter doesn't catch (those tests
/// live in the same file as the production code).
fn is_test_fn_name(fn_name: &str) -> bool {
    let lower = fn_name.to_ascii_lowercase();
    lower.starts_with("test_")
        || lower.starts_with("it_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
}

/// Comment-line predicate. Walks backward from the offset to the line
/// start; returns true when a `//` appears before the offset on the
/// same line. Stripping block comments (`/* ... */`) is out of scope
/// for v2.22 — the bench evidence so far doesn't need it.
fn line_is_commented(source: &str, offset: usize) -> bool {
    let bytes = source.as_bytes();
    let mut i = offset.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    let line_prefix = &source[i..offset.min(source.len())];
    if let Some(idx) = line_prefix.find("//") {
        // Confirm `//` isn't inside a string literal earlier on the
        // line. Counting double-quotes is a rough approximation.
        let before = &line_prefix[..idx];
        let quote_count = before.chars().filter(|c| *c == '"').count();
        quote_count % 2 == 0
    } else {
        false
    }
}

/// True when the function body invokes a token / system CPI — the
/// discriminator for "arithmetic that crosses into fund flow." Also
/// accepts helper-function calls whose name suggests transfer / mint
/// dispatch (`transfer_with_delegate`, `mint_to_user`, ...) because
/// the CAN-I3 site dispatches through a `transfer_with_delegate`
/// helper rather than constructing the CPI directly.
fn body_signals_cpi(body: &str) -> bool {
    if body.contains("invoke(")
        || body.contains("invoke_signed(")
        || body.contains("Transfer ")
        || body.contains("Transfer {")
        || body.contains("MintTo ")
        || body.contains("MintTo {")
        || body.contains("Burn ")
        || body.contains("Burn {")
        || body.contains("cpi::")
        || body.contains("token::transfer")
        || body.contains("token::mint_to")
        || body.contains("system_program::transfer")
    {
        return true;
    }
    // Helper-function dispatch: `transfer_with_delegate(...)`,
    // `mint_to_user(...)`. Anchor / native programs commonly factor
    // the CPI behind a `<verb>_<descriptor>` helper.
    let helper_re =
        Regex::new(r"\b(?:transfer|mint|burn|withdraw|deposit|approve|revoke)_[a-z_]+\s*\(")
            .expect("static regex compiles");
    helper_re.is_match(body)
}

/// Walk forward from `offset` to find the body of the enclosing fn —
/// the text between the next `{` after the fn signature and its
/// matching `}`. Used by `scan_graceful_error_as_dos` to check
/// PDA/seed signals across the whole fn, not just the call site.
fn enclosing_fn_body(source: &str, offset: usize) -> String {
    // Locate the enclosing `fn ... (` in source[..offset]; then track
    // the first `{` after that point and its matching `}`.
    let head = &source[..offset.min(source.len())];
    let fn_re = Regex::new(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*\s*[<\(]").expect("static regex");
    let Some(fn_match) = fn_re.find_iter(head).last() else {
        return String::new();
    };
    let bytes = source.as_bytes();
    let mut i = fn_match.start();
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return String::new();
    }
    let body_start = i + 1;
    let mut depth: i32 = 1;
    let mut j = body_start;
    while j < bytes.len() && depth > 0 {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return source[body_start..j].to_string();
        }
        j += 1;
    }
    source[body_start..].to_string()
}

/// True when the fn name suggests a lifecycle-initialisation handler.
/// Case-insensitive substring match on the canonical naming
/// conventions (`init`, `create`, `initialize`).
fn is_init_shape(fn_name: &str) -> bool {
    let lower = fn_name.to_ascii_lowercase();
    lower == "init"
        || lower == "create"
        || lower == "initialize"
        || lower.starts_with("init_")
        || lower.starts_with("create_")
        || lower.starts_with("initialize_")
        || lower.ends_with("_init")
        || lower.ends_with("_create")
        || lower.ends_with("_initialize")
        || lower.contains("_init_")
        || lower.contains("_create_")
        || lower.contains("_initialize_")
}

/// True when the function body or signature signals PDA / seed-driven
/// derivation — the discriminator for "the address is deterministic and
/// nobody holds the private key."
fn body_signals_pda(body: &str) -> bool {
    body.contains("find_program_address")
        || body.contains("invoke_signed")
        || body.contains("Pubkey::create_program_address")
        || body.contains("seeds:")
        || body.contains("&[Seed")
        || body.contains("&Seed<")
        || body.contains("&[&[u8]]")
}

/// Receiver-shape predicate. Returns true for identifiers and
/// expressions that look like a Solana timestamp / clock value.
fn is_timestamp_shape(recv: &str) -> bool {
    let r = recv.trim();
    let r = r.strip_prefix('*').unwrap_or(r);
    // Common identifier patterns.
    let known = [
        "current_ts",
        "current_time",
        "now",
        "now_ts",
        "ts",
        "clock_ts",
        "unix_timestamp",
        "slot",
        "epoch",
        "block_height",
        "current_slot",
        "current_epoch",
    ];
    if known.contains(&r) {
        return true;
    }
    // Suffix shapes: `_ts`, `_secs`, `_at`, `_time`, `_timestamp`.
    let id = r
        .rsplit('.')
        .next()
        .unwrap_or(r)
        .trim_start_matches('*')
        .trim_end_matches('?');
    if id.ends_with("_ts")
        || id.ends_with("_secs")
        || id.ends_with("_seconds")
        || id.ends_with("_time")
        || id.ends_with("_timestamp")
        || id.ends_with("_slot")
        || id.ends_with("_epoch")
    {
        return true;
    }
    // Clock accessor patterns.
    if r.contains("Clock::get()") && r.contains("unix_timestamp") {
        return true;
    }
    if r.contains(".unix_timestamp")
        || r.contains(".slot")
        || r.contains(".epoch")
        || r.contains(".block_height")
    {
        return true;
    }
    false
}

/// Window-after-call check: does the next chunk of source contain a
/// comparison shape we associate with "elapsed time opens a gate"?
///
/// Conservative: looks for `>=` or `>` followed (within a short
/// window) by `{` (block body) or `return` (early return inverted to
/// guard). Misses sophisticated re-binding patterns; future bench
/// evidence tightens this.
fn window_has_gating_comparison(window: &str) -> bool {
    let cmp = Regex::new(r">=|>").expect("static regex");
    if let Some(m) = cmp.find(window) {
        let after = &window[m.end()..window.len().min(m.end() + 120)];
        return after.contains('{')
            || after.contains("return ")
            || after.contains("Ok(")
            || after.contains("Err(");
    }
    false
}

/// Resolve the byte offset to a 1-indexed line number.
fn byte_offset_to_line(source: &str, offset: usize) -> u32 {
    let prefix = &source[..offset.min(source.len())];
    1 + prefix.chars().filter(|c| *c == '\n').count() as u32
}

/// Walk backward from the given offset to find the nearest enclosing
/// `fn <name>(...)` or `fn <name><...>(...)`. Returns the function
/// name; falls back to `None` when the offset isn't inside a function.
/// The `[<\(]` terminator captures both bare fns and generic fns
/// (`fn init<'a, T>`, the CAN-H3 shape).
fn enclosing_fn_name(source: &str, offset: usize) -> Option<String> {
    let head = &source[..offset.min(source.len())];
    let re = Regex::new(r"fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<\(]").expect("static regex");
    re.captures_iter(head).last().map(|c| c[1].to_string())
}

/// Stable id: hash of (file, line, rule). Matches the Pinocchio probe
/// shape so suppression files are uniform across rule families.
fn make_id(rel_file: &Path, line: u32, rule: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(rel_file.display().to_string().as_bytes());
    h.update(b":");
    h.update(line.to_string().as_bytes());
    h.update(b":");
    h.update(rule.as_bytes());
    let id = format!("{:x}", h.finalize());
    id[..16].to_string()
}

fn collect_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| matches!(n, "target" | ".git" | "node_modules" | "tests" | ".qed"))
        {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn fires_on_canonical_subscriptions_can_h1_shape() {
        // Mirrors transfer_validation.rs:61 from the subscriptions
        // bench — the CAN-H1 firm-High miss.
        let src = r#"
fn process_transfer(ctx: Context, current_ts: i64) -> Result<()> {
    let time_since_start = current_ts.saturating_sub(*current_period_start_ts);
    if time_since_start >= period_length {
        // period advancement — gives the merchant fresh budget
        advance_period(ctx)?;
    }
    Ok(())
}
"#;
        let findings = scan_silent_success_arithmetic(&p("transfer.rs"), src);
        assert_eq!(findings.len(), 1, "expected 1 finding, got {findings:#?}");
        let f = &findings[0];
        assert_eq!(f.category_tag, "silent_success_arithmetic");
        assert!(matches!(f.severity, Severity::High));
        assert_eq!(f.handler, "process_transfer");
        assert!(matches!(
            f.reproducer,
            Some(Reproducer::MolluskPrompt { .. })
        ));
    }

    #[test]
    fn ignores_saturating_sub_on_non_timestamp_receiver() {
        // Counter difference is a legitimate use of saturating_sub and
        // shouldn't flag.
        let src = r#"
fn deduct_fee(balance: u64, fee: u64) -> u64 {
    if balance.saturating_sub(fee) > 0 {
        balance - fee
    } else {
        0
    }
}
"#;
        let findings = scan_silent_success_arithmetic(&p("fees.rs"), src);
        assert!(
            findings.is_empty(),
            "balance/fee receiver should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn fires_on_clock_get_unix_timestamp_receiver() {
        let src = r#"
fn check_expiry(ctx: Context) -> Result<()> {
    let elapsed = Clock::get()?.unix_timestamp.saturating_sub(start_ts);
    if elapsed >= duration {
        return Err(Expired.into());
    }
    Ok(())
}
"#;
        let findings = scan_silent_success_arithmetic(&p("expiry.rs"), src);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn fires_on_suffix_named_timestamp() {
        let src = r#"
fn advance(state: &mut State, last_seen_ts: i64) -> Result<()> {
    let delta = last_seen_ts.saturating_sub(state.previous_ts);
    if delta >= MIN_INTERVAL {
        state.advance();
    }
    Ok(())
}
"#;
        let findings = scan_silent_success_arithmetic(&p("advance.rs"), src);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn does_not_fire_without_gating_comparison() {
        // Saturating_sub on timestamp but the result is logged, not
        // gated — no fund-flow leak shape.
        let src = r#"
fn log_elapsed(current_ts: i64, start_ts: i64) {
    let elapsed = current_ts.saturating_sub(start_ts);
    msg!("elapsed: {}", elapsed);
}
"#;
        let findings = scan_silent_success_arithmetic(&p("log.rs"), src);
        assert!(
            findings.is_empty(),
            "no gating comparison should NOT fire; got {findings:#?}"
        );
    }

    // ───────────────────────────────────────────────────────────────
    // S1.2 — graceful_error_as_dos tests
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn fires_on_canonical_subscriptions_can_h3_shape() {
        // Mirrors the multi_delegator helpers/program.rs:47-49 site —
        // the CAN-H3 firm-High miss. PDA-init path with `checked_sub`
        // whose Err propagates via `?`.
        let src = r#"
fn init<'a, T: Sized>(
    payer: &AccountView,
    account: &AccountView,
    seeds: &[Seed<'a>],
    space: usize,
) -> ProgramResult {
    let lamports = Rent::get()?.try_minimum_balance(space)?;
    let signer = [Signer::from(seeds)];

    if account.lamports() == 0 {
        // happy path
    } else {
        let required_lamports = lamports
            .checked_sub(account.lamports())
            .ok_or(ArithmeticUnderflow)?;
        if required_lamports > 0 {
            Transfer { from: payer, to: account, lamports: required_lamports }
                .invoke()?;
        }
        Allocate { account, space: space as u64 }.invoke_signed(&signer)?;
    }
    Ok(())
}
"#;
        let findings = scan_graceful_error_as_dos(&p("program.rs"), src);
        assert_eq!(
            findings.len(),
            1,
            "expected 1 graceful_error_as_dos finding, got {findings:#?}"
        );
        let f = &findings[0];
        assert_eq!(f.category_tag, "graceful_error_as_dos");
        assert!(matches!(f.severity, Severity::High));
        assert_eq!(f.handler, "init");
        assert!(matches!(
            f.reproducer,
            Some(Reproducer::MolluskPrompt { .. })
        ));
    }

    #[test]
    fn fires_on_create_named_fn_with_find_program_address() {
        let src = r#"
fn create_subscription(ctx: Context, amount: u64) -> Result<()> {
    let (pda, bump) = Pubkey::find_program_address(&[b"sub", ctx.user.key.as_ref()], &ctx.program.key);
    let cost = amount.checked_sub(BASE_FEE).ok_or(Underflow)?;
    msg!("cost: {}", cost);
    Ok(())
}
"#;
        let findings = scan_graceful_error_as_dos(&p("create.rs"), src);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].handler, "create_subscription");
    }

    #[test]
    fn ignores_checked_sub_outside_init_fn() {
        // Non-init / non-create fn — even with a PDA signal — shouldn't
        // fire. The rule is specifically about lifecycle-init paths
        // whose failure permanently bricks the address.
        let src = r#"
fn transfer(ctx: Context, amount: u64) -> Result<()> {
    let (_pda, _bump) = Pubkey::find_program_address(&[b"x"], &ctx.program.key);
    let remaining = ctx.balance.checked_sub(amount).ok_or(Underflow)?;
    Ok(())
}
"#;
        let findings = scan_graceful_error_as_dos(&p("transfer.rs"), src);
        assert!(
            findings.is_empty(),
            "non-init fn should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_init_fn_without_pda_signal() {
        // `init`-named but no PDA / seeds / invoke_signed in the body —
        // user-funded account, retryable, suppressed.
        let src = r#"
fn init_user(ctx: Context, balance: u64) -> Result<()> {
    let remaining = balance.checked_sub(MIN_BALANCE).ok_or(Underflow)?;
    ctx.user.balance = remaining;
    Ok(())
}
"#;
        let findings = scan_graceful_error_as_dos(&p("init_user.rs"), src);
        assert!(
            findings.is_empty(),
            "init fn without PDA signal should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn fires_on_invoke_signed_in_init_body() {
        // No find_program_address but invoke_signed indicates a PDA
        // derivation upstream. Still fires.
        let src = r#"
fn initialize(payer: &AccountView, pda: &AccountView, bump: u8) -> ProgramResult {
    let required = MIN_LAMPORTS.checked_sub(pda.lamports()).ok_or(Underflow)?;
    let signer = [Signer::from(&[b"acc", &[bump]])];
    Transfer { from: payer, to: pda, lamports: required }.invoke_signed(&signer)?;
    Ok(())
}
"#;
        let findings = scan_graceful_error_as_dos(&p("init.rs"), src);
        assert_eq!(findings.len(), 1);
    }

    // ───────────────────────────────────────────────────────────────
    // S1.3 — unchecked_arith_with_fund_flow tests
    // ───────────────────────────────────────────────────────────────

    #[test]
    fn fires_on_canonical_subscriptions_can_i3_shape() {
        // Mirrors transfer_subscription.rs:61 — the CAN-I3 site.
        let src = r#"
fn process_transfer(ctx: Context) -> Result<()> {
    let period_length_s = plan.data.period_hours * 3600;
    Transfer { from: ctx.user, to: ctx.dest, lamports: 1000 }.invoke()?;
    Ok(())
}
"#;
        let findings = scan_unchecked_arith_with_fund_flow(&p("transfer.rs"), src);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.category_tag, "unchecked_arith_with_fund_flow");
        assert!(matches!(f.severity, Severity::Low));
        assert_eq!(f.handler, "process_transfer");
    }

    #[test]
    fn ignores_checked_mul_in_same_fn() {
        let src = r#"
fn process_safe(ctx: Context) -> Result<()> {
    let period_length_s = plan.data.period_hours.checked_mul(3600).ok_or(Overflow)?;
    Transfer { from: ctx.user, to: ctx.dest, lamports: 1000 }.invoke()?;
    Ok(())
}
"#;
        let findings = scan_unchecked_arith_with_fund_flow(&p("safe.rs"), src);
        assert!(
            findings.is_empty(),
            "checked_mul should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_arithmetic_without_cpi_signal() {
        // No CPI in the fn body — book-keeping arithmetic, not
        // fund-flow.
        let src = r#"
fn compute_only(period_hours: u32) -> u32 {
    let period_seconds = period_hours * 3600;
    period_seconds
}
"#;
        let findings = scan_unchecked_arith_with_fund_flow(&p("compute.rs"), src);
        assert!(
            findings.is_empty(),
            "no CPI in body should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_short_lhs_index_arithmetic() {
        // `i + 1` index arithmetic is a deliberate false negative for
        // v2.22 — short LHS skipped.
        let src = r#"
fn loop_through(ctx: Context, items: &[u64]) -> Result<()> {
    for i in 0..items.len() {
        let next = i + 1;
        if next < items.len() {
            Transfer { lamports: items[next] }.invoke()?;
        }
    }
    Ok(())
}
"#;
        let findings = scan_unchecked_arith_with_fund_flow(&p("loop.rs"), src);
        assert!(
            findings.is_empty(),
            "short-LHS index math should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn timestamp_shape_predicate_recognises_common_idents() {
        assert!(is_timestamp_shape("current_ts"));
        assert!(is_timestamp_shape("now"));
        assert!(is_timestamp_shape("start_ts"));
        assert!(is_timestamp_shape("clock.slot"));
        assert!(is_timestamp_shape("Clock::get()?.unix_timestamp"));
        assert!(is_timestamp_shape("*current_period_start_ts"));
        assert!(!is_timestamp_shape("balance"));
        assert!(!is_timestamp_shape("amount"));
        assert!(!is_timestamp_shape("fee_lamports"));
    }
}
