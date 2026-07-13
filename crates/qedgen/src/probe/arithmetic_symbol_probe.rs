//! Arithmetic-symbol catalog probes.
//!
//! Runtime-agnostic source scanners for arithmetic operators whose *symbol*
//! (not result) is the bug — correct in isolation, broken by the failure-mode
//! interaction with surrounding control flow: `silent_success_arithmetic`
//! (HIGH), `graceful_error_as_dos` (HIGH), `unchecked_arith_with_fund_flow`
//! (LOW). These bugs live in the deployed Rust source, not `.qedspec` state:
//! walk `*.rs` under the project root, regex-match, emit `Finding`s into the
//! same envelope as the spec-aware probes. Each finding ships a
//! `Reproducer::MolluskPrompt` pointing at a per-rule markdown under
//! `references/probes/arithmetic_symbol/<rule>.md`.

use anyhow::Result;
use regex::Regex;
use std::path::Path;

use crate::probe::scan_util::{
    self, byte_offset_to_line, enclosing_fn_body, floor_char_boundary, is_test_fn_name,
    line_is_commented, make_id,
};
use crate::probe::{Category, Finding, Reproducer, Severity};

/// Entry point: walk `<root>/src/**/*.rs` and emit findings. No matches
/// is an empty vec, not an error.
pub fn scan_program(project_root: &Path) -> Result<Vec<Finding>> {
    let src_dir = project_root.join("src");
    if !src_dir.exists() {
        // No `src/` — not a Rust crate root; nothing to scan.
        return Ok(Vec::new());
    }
    let rs_files = crate::fs_walk::collect_rs_files(&src_dir, crate::fs_walk::DEFAULT_SKIP_DIRS);
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

/// `silent_success_arithmetic` (HIGH): the 0-or-MAX boundary value of
/// `saturating_*` silently opens a gate that should have stayed closed.
/// Pattern criteria:
///
/// 1. A call site of `saturating_sub` or `saturating_add`.
/// 2. The receiver is timestamp-shaped (see `is_timestamp_shape`).
/// 3. The lines AFTER the call contain a `>=` / `>` comparison gating a
///    branch — the canonical "elapsed time opens a gate" shape.
///
/// False-positive guard: only timestamp-shape receivers fire; counter /
/// amount values using `saturating_sub` for fee accounting are rejected.
/// Emits one finding per call site (not per gated branch).
pub(crate) fn scan_silent_success_arithmetic(rel_file: &Path, source: &str) -> Vec<Finding> {
    // Receiver: identifier / `Clock::get()?.unix_timestamp` / `*deref`,
    // capped at ~64 chars to keep the regex tractable — longer chained
    // expressions are a deliberate false-negative. `recv` is re-checked
    // by the timestamp-shape predicate at filter time.
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
        // The "elapsed >= threshold opens an effect" tell must appear
        // within the next ~400 chars.
        let window = &source[m.end()..floor_char_boundary(source, m.end() + 400)];
        if !window_has_gating_comparison(window) {
            continue;
        }

        let finding_id = make_id(rel_file, line, Category::SilentSuccessArithmetic.tag());
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
            category_tag: Category::SilentSuccessArithmetic.tag().to_string(),
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

/// `graceful_error_as_dos` (HIGH): `Err` propagation on a PDA-init path
/// permanently bricks a deterministic address every caller subsequently
/// hits. Pattern criteria:
///
/// 1. A call site of `checked_sub` / `checked_add` / `checked_mul`.
/// 2. The enclosing fn name contains `init` / `create` / `initialize`
///    (case-insensitive) — the handlers that materialise a deterministic
///    address.
/// 3. The fn signals PDA / seed-driven derivation: `find_program_address`,
///    `seeds:` / `&[Seed` / `&Seed<`, or `invoke_signed(` (signed CPI
///    implies a PDA derivation upstream).
/// 4. The operator's `Err` arm exits via `?` or `return Err(...)`.
///
/// False-positive guard: without a PDA / seed signal the arithmetic is
/// treated as user-funded (retryable with corrected inputs) and suppressed.
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
        // Err arm must exit: `?` or `return Err` within ~160 chars
        // (propagation may chain through `.ok_or(_else)`).
        let window = &source[m.end()..floor_char_boundary(source, m.end() + 160)];
        if !window.contains('?') && !window.contains("return Err") {
            continue;
        }

        let finding_id = make_id(rel_file, line, Category::GracefulErrorAsDos.tag());
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
            category_tag: Category::GracefulErrorAsDos.tag().to_string(),
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

/// `unchecked_arith_with_fund_flow` (LOW): bare arithmetic in a handler
/// that dispatches a CPI. Pattern criteria:
///
/// 1. A bare `*` / `+` / `-` BinOp `<ident_path> <op> <numeric_literal>`
///    (e.g. `period_hours * 3600`). The literal-on-RHS restriction keeps
///    false-positive volume tractable.
/// 2. The enclosing fn body contains a CPI signal — discriminates
///    "arithmetic that crosses into fund flow" (the target) from
///    "arithmetic on book-keeping counters".
/// 3. The site is not already inside a `checked_*` / `saturating_*` call
///    (those are already correctly defensive).
///
/// LOW severity — the recommendation is preventive (`checked_*`); most
/// sites are safe today under upstream bounds. The audit subagent triages
/// and confirms the bound holds.
pub(crate) fn scan_unchecked_arith_with_fund_flow(rel_file: &Path, source: &str) -> Vec<Finding> {
    // `<ident_or_path> [*+-] <int_literal>`; path may include dots /
    // indexes, capped at ~48 chars. Literals may carry underscores
    // (`3_600`) and a type suffix (`100u64`).
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
        // Skip non-user-value arithmetic: numeric-only LHS (`1 - 2`);
        // short LHS is likely `i + 1` index math (deliberate false
        // negative).
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
        // Adjacent `->` / `<-` flags non-arithmetic shapes (return
        // types, pointer-like patterns).
        let surrounding_start = floor_char_boundary(source, m.start().saturating_sub(2));
        let surrounding = &source[surrounding_start..m.end()];
        if surrounding.contains("->") || surrounding.contains("<-") {
            continue;
        }
        // Reject sites already inside a `checked_*` / `saturating_*` /
        // `wrapping_*` call (check the ~80 preceding chars).
        let before_start = floor_char_boundary(source, m.start().saturating_sub(80));
        let before = &source[before_start..m.start()];
        if before.contains("checked_")
            || before.contains("saturating_")
            || before.contains("wrapping_")
            || before.contains("overflowing_")
        {
            continue;
        }
        let line = byte_offset_to_line(source, m.start());
        // Skip commented lines — comments routinely contain shapes like
        // `// Token-2022` that match the BinOp regex.
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
        // Skip inline `#[cfg(test)]` test fns — same file as production
        // code, so `collect_rust_files`'s directory filter misses them.
        if is_test_fn_name(&fn_name) {
            continue;
        }
        let fn_body = enclosing_fn_body(source, m.start());
        if !body_signals_cpi(&fn_body) {
            continue;
        }

        let finding_id = make_id(rel_file, line, Category::UncheckedArithWithFundFlow.tag());
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
            category_tag: Category::UncheckedArithWithFundFlow.tag().to_string(),
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

/// True when the fn body invokes a token / system CPI — the discriminator
/// for "arithmetic that crosses into fund flow". Also accepts helper calls
/// named like transfer / mint dispatch (`transfer_with_delegate`, ...) —
/// programs commonly factor the CPI behind a `<verb>_<descriptor>` helper.
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
    let helper_re =
        Regex::new(r"\b(?:transfer|mint|burn|withdraw|deposit|approve|revoke)_[a-z_]+\s*\(")
            .expect("static regex compiles");
    helper_re.is_match(body)
}

/// Fn name suggests a lifecycle-init handler (`init` / `create` /
/// `initialize`, case-insensitive).
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

/// PDA / seed-driven derivation signal — the discriminator for "the
/// address is deterministic and nobody holds the private key".
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

/// "Elapsed time opens a gate" check: `>=` / `>` followed shortly by `{`
/// or `return`. Conservative — misses re-binding patterns (deliberate).
fn window_has_gating_comparison(window: &str) -> bool {
    let cmp = Regex::new(r">=|>").expect("static regex");
    if let Some(m) = cmp.find(window) {
        let after = &window[m.end()..floor_char_boundary(window, m.end() + 120)];
        return after.contains('{')
            || after.contains("return ")
            || after.contains("Ok(")
            || after.contains("Err(");
    }
    false
}

/// Nearest enclosing fn name (None when not inside a fn).
fn enclosing_fn_name(source: &str, offset: usize) -> Option<String> {
    scan_util::enclosing_fn_start_and_name(source, offset).map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn fires_on_canonical_subscriptions_can_h1_shape() {
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

    #[test]
    fn fires_on_canonical_subscriptions_can_h3_shape() {
        // PDA-init path with `checked_sub` whose Err propagates via `?`.
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
        // Non-init fn, even with a PDA signal — the rule is specifically
        // about init paths whose failure permanently bricks the address.
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

    #[test]
    fn fires_on_canonical_subscriptions_can_i3_shape() {
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
        // `i + 1` index math: short LHS is a deliberate false negative.
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

    /// #187: a multi-byte char (`—`) just before a match made the
    /// backward context window (`m.start() - 2`) split the char and panic
    /// the slice. The exact sanitized repro from the issue — must scan
    /// without panicking (and the commented line yields no finding).
    #[test]
    fn utf8_boundary_before_match_does_not_panic() {
        let src = "#![no_std]\n\n// \u{2014} amount + 1\npub fn generic_handler(amount: u64) -> u64 {\n    amount\n}\n";
        let findings = scan_unchecked_arith_with_fund_flow(&p("lib.rs"), src);
        assert!(
            findings.is_empty(),
            "commented shape must not fire; got {findings:#?}"
        );
    }

    /// #187 (forward windows): multi-byte chars downstream of a match must
    /// not panic the `end + 400` / `end + 160` / `end + 120` context
    /// slices. Three paddings guarantee at least one window edge lands
    /// mid-char regardless of match length.
    #[test]
    fn utf8_boundary_after_match_does_not_panic() {
        for pad in 0..3usize {
            let src = format!(
                "fn process(current_ts: i64) -> Result<(), ()> {{\n    \
                 let elapsed = current_ts.saturating_sub(start_ts);\n    \
                 if elapsed >= period {{ advance()?; }}\n    \
                 let total = balance_amount + 100;\n{}{}\n}}\n",
                " ".repeat(pad),
                "\u{2014}".repeat(300),
            );
            // Exercise all three scanners over the same dash-heavy source.
            let _ = scan_silent_success_arithmetic(&p("a.rs"), &src);
            let _ = scan_graceful_error_as_dos(&p("a.rs"), &src);
            let _ = scan_unchecked_arith_with_fund_flow(&p("a.rs"), &src);
        }
    }
}
