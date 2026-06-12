//! Parse `cargo kani --tests` failure output into structured `Counterexample`s.
//!
//! ## Expected output shape (brittle external contract)
//!
//! A failing harness prints a CBMC-derived report like:
//!
//! ```text
//! Checking harness probe_overflow_transfer...
//! CBMC 6.4.0 (cbmc-6.4.0)
//! ...
//!
//! ** Results:
//! tests/kani.rs function probe_overflow_transfer
//! Check 1: probe_overflow_transfer.assertion.1
//!          - Status: FAILURE
//!          - Description: "assertion failed: post == pre.checked_add(amount).unwrap_or(0)"
//!          - Location: tests/kani.rs:42:5 in function probe_overflow_transfer
//!
//! Check 2: probe_overflow_transfer.overflow.1
//!          - Status: SUCCESS
//!          - Description: "arithmetic overflow on u64 + in pre.wrapping_add(amount)"
//!          - Location: tests/kani.rs:40:13 in function probe_overflow_transfer
//!
//! Counterexample:
//!
//! State 1: tests/kani.rs:38:13 in function probe_overflow_transfer
//! ----------------------------------------------------
//!   pre = 18446744073709551615ul (00000000 00000000 ... 11111111)
//!
//! State 2: tests/kani.rs:39:13 in function probe_overflow_transfer
//! ----------------------------------------------------
//!   amount = 1ul (00000000 ... 00000001)
//!
//! State 3: tests/kani.rs:40:13 in function probe_overflow_transfer
//! ----------------------------------------------------
//!   post = 0ul
//!
//! ** 1 of 2 failed (1 unreachable)
//! VERIFICATION:- FAILED
//! ```
//!
//! Extracted per failed harness:
//! - **harness** — from `Checking harness <name>...`.
//! - **failure_message** — `- Description:` of the first FAILURE check.
//! - **source_location** — `- Location:` of that check (`file:line:col`,
//!   dropping ` in function ...`).
//! - **assignments** — one `CounterexampleVar` per `<var> = <value>` line in
//!   the `Counterexample:` section; `line` comes from the enclosing
//!   `State N: <file>:<line>:<col>` header. The CBMC binary-rep suffix
//!   `(00000000 ...)` is stripped from values.
//!
//! Human-readable output is parsed (not `-Z output-format=json`) because the
//! JSON mode is unstable and its schema unpinned.
//!
//! Not captured: SUCCESS checks, build noise, and FAILURE checks past the
//! first per harness (the full state trace still lands in `assignments`).

use crate::verify_counterexample::{Counterexample, CounterexampleVar};

/// One `Counterexample` per failed harness; empty if no `Checking harness ...`
/// block reached `VERIFICATION:- FAILED`.
pub fn parse_failures(stdout: &str) -> Vec<Counterexample> {
    let mut out = Vec::new();
    let lines: Vec<&str> = stdout.lines().collect();

    let mut i = 0;
    while i < lines.len() {
        // Harness block: from `Checking harness X...` to the next heading (or EOF).
        let Some(harness) = parse_checking_heading(lines[i]) else {
            i += 1;
            continue;
        };
        let block_end = (i + 1..lines.len())
            .find(|&j| parse_checking_heading(lines[j]).is_some())
            .unwrap_or(lines.len());
        let block = &lines[i + 1..block_end];

        // Failed only if `VERIFICATION:- FAILED` appears; SUCCESS / UNDETERMINED
        // produce no finding.
        let failed = block.iter().any(|l| l.contains("VERIFICATION:- FAILED"));
        if failed {
            out.push(parse_harness_block(harness, block));
        }
        i = block_end;
    }

    out
}

fn parse_checking_heading(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("Checking harness ")?
        .strip_suffix("...")
}

fn parse_harness_block(harness: &str, lines: &[&str]) -> Counterexample {
    let mut failure_message: Option<String> = None;
    let mut source_location: Option<String> = None;
    let mut assignments: Vec<CounterexampleVar> = Vec::new();

    // First `- Status: FAILURE`, then its surrounding Description / Location.
    let failure_idx = lines.iter().position(|l| {
        let t = l.trim();
        t.starts_with("- Status: FAILURE") || t.starts_with("Status: FAILURE")
    });
    if let Some(idx) = failure_idx {
        // Description / Location appear in any order near the Status line; scan ±5.
        let lo = idx.saturating_sub(5);
        let hi = (idx + 6).min(lines.len());
        for line in &lines[lo..hi] {
            let t = line.trim();
            if let Some(d) = t
                .strip_prefix("- Description: ")
                .or_else(|| t.strip_prefix("Description: "))
            {
                failure_message = Some(strip_surrounding_quotes(d).to_string());
            } else if let Some(loc) = t
                .strip_prefix("- Location: ")
                .or_else(|| t.strip_prefix("Location: "))
            {
                source_location = Some(strip_in_function_suffix(loc).to_string());
            }
        }
    }

    // Counterexample section: `State N: <file>:<line>:<col>` headers, each
    // followed by `  <var> = <value>` lines, until the `** ...` summary.
    let mut state_line: Option<u32> = None;
    let mut in_counterexample = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "Counterexample:" {
            in_counterexample = true;
            continue;
        }
        if !in_counterexample {
            continue;
        }
        if trimmed.starts_with("** ") || trimmed.starts_with("VERIFICATION:") {
            break;
        }
        if let Some(line_num) = parse_state_header(trimmed) {
            state_line = Some(line_num);
            continue;
        }
        if let Some((name, value)) = parse_assignment(line) {
            assignments.push(CounterexampleVar {
                name,
                value,
                line: state_line,
            });
        }
    }

    Counterexample {
        harness: harness.to_string(),
        status: "failed".to_string(),
        assignments,
        seed: None, // Kani is deterministic; no seed concept
        failure_message,
        source_location,
    }
}

/// Line number from `State N: <file>:<line>:<col> in function ...`;
/// `None` for divider lines and other formats.
fn parse_state_header(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("State ")?;
    let colon_idx = rest.find(": ")?;
    let loc = &rest[colon_idx + 2..];
    // `loc` is `<file>:<line>:<col>[ in function ...]`.
    let loc = strip_in_function_suffix(loc);
    let parts: Vec<&str> = loc.rsplitn(3, ':').collect();
    // parts[0] = col, parts[1] = line, parts[2] = file
    if parts.len() < 3 {
        return None;
    }
    parts[1].trim().parse::<u32>().ok()
}

fn parse_assignment(line: &str) -> Option<(String, String)> {
    // Assignments are indented `  <var> = <value>`; leading whitespace required
    // to avoid matching other lines containing `=`.
    if !line.starts_with(' ') && !line.starts_with('\t') {
        return None;
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let eq_idx = trimmed.find(" = ")?;
    let name = trimmed[..eq_idx].trim().to_string();
    let raw_value = trimmed[eq_idx + 3..].trim();
    // Strip trailing CBMC binary-rep `(00000000 ...)` — but only when the tail
    // looks like a bit pattern, so struct-constructor parens survive.
    let value = if let Some(paren_idx) = raw_value.rfind(" (") {
        let suffix = &raw_value[paren_idx + 2..];
        if looks_like_cbmc_bits(suffix) {
            raw_value[..paren_idx].trim().to_string()
        } else {
            raw_value.to_string()
        }
    } else {
        raw_value.to_string()
    };
    if name.is_empty() {
        return None;
    }
    Some((name, value))
}

fn looks_like_cbmc_bits(s: &str) -> bool {
    // CBMC bit patterns: only 0/1/whitespace before the closing paren.
    let s = s.trim_end_matches(')').trim();
    !s.is_empty() && s.chars().all(|c| c == '0' || c == '1' || c.is_whitespace())
}

fn strip_in_function_suffix(s: &str) -> &str {
    s.trim()
        .split(" in function ")
        .next()
        .unwrap_or(s.trim())
        .trim()
}

fn strip_surrounding_quotes(s: &str) -> &str {
    let trimmed = s.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical Kani 0.67-shape cargo-kani output: one failure, full trace.
    const SINGLE_FAILURE: &str = "
Kani Rust Verifier 0.67.0 (cargo plugin)
Building project for harness `probe_overflow_transfer`...
Compiling probe-test v0.1.0
    Finished `kani` profile [optimized] target(s) in 0.34s

Checking harness probe_overflow_transfer...
CBMC 6.4.0 (cbmc-6.4.0)
CBMC version 6.4.0 (cbmc-6.4.0) 64-bit aarch64-apple-darwin

** Results:
tests/kani.rs function probe_overflow_transfer
Check 1: probe_overflow_transfer.assertion.1
         - Status: FAILURE
         - Description: \"assertion failed: post == pre.checked_add(amount).unwrap_or(0)\"
         - Location: tests/kani.rs:42:5 in function probe_overflow_transfer

Check 2: probe_overflow_transfer.overflow.1
         - Status: SUCCESS
         - Description: \"arithmetic overflow on u64 + in pre.wrapping_add(amount)\"
         - Location: tests/kani.rs:40:13 in function probe_overflow_transfer

Counterexample:

State 1: tests/kani.rs:38:13 in function probe_overflow_transfer
----------------------------------------------------
  pre = 18446744073709551615ul (1111111111111111111111111111111111111111111111111111111111111111)

State 2: tests/kani.rs:39:13 in function probe_overflow_transfer
----------------------------------------------------
  amount = 1ul (0000000000000000000000000000000000000000000000000000000000000001)

State 3: tests/kani.rs:40:13 in function probe_overflow_transfer
----------------------------------------------------
  post = 0ul

** 1 of 2 failed (1 unreachable)
VERIFICATION:- FAILED

Verification Time: 0.123s

Summary:
Verification failed for - probe_overflow_transfer

Complete - 0 successfully verified harnesses, 1 failures, 1 total.
";

    #[test]
    fn parses_single_failure() {
        let cxs = parse_failures(SINGLE_FAILURE);
        assert_eq!(cxs.len(), 1);
        let cx = &cxs[0];
        assert_eq!(cx.harness, "probe_overflow_transfer");
        assert_eq!(cx.status, "failed");
        assert_eq!(
            cx.failure_message.as_deref(),
            Some("assertion failed: post == pre.checked_add(amount).unwrap_or(0)")
        );
        assert_eq!(cx.source_location.as_deref(), Some("tests/kani.rs:42:5"));
        assert_eq!(cx.assignments.len(), 3);
        assert_eq!(cx.assignments[0].name, "pre");
        assert_eq!(cx.assignments[0].value, "18446744073709551615ul");
        assert_eq!(cx.assignments[0].line, Some(38));
        assert_eq!(cx.assignments[1].name, "amount");
        assert_eq!(cx.assignments[1].value, "1ul");
        assert_eq!(cx.assignments[1].line, Some(39));
        assert_eq!(cx.assignments[2].name, "post");
        assert_eq!(cx.assignments[2].value, "0ul");
        assert_eq!(cx.assignments[2].line, Some(40));
        // Kani is deterministic — no seed.
        assert!(cx.seed.is_none());
    }

    #[test]
    fn skips_successful_harnesses() {
        let stdout = "
Checking harness probe_lifecycle_safe...
** Results:
tests/kani.rs function probe_lifecycle_safe
Check 1: probe_lifecycle_safe.assertion.1
         - Status: SUCCESS
         - Description: \"some property\"
         - Location: tests/kani.rs:10:5 in function probe_lifecycle_safe

** 0 of 1 failed
VERIFICATION:- SUCCESSFUL

Complete - 1 successfully verified harnesses, 0 failures, 1 total.
";
        assert!(parse_failures(stdout).is_empty());
    }

    #[test]
    fn parses_two_harnesses_one_fail_one_pass() {
        let stdout = "
Checking harness probe_a...
** Results:
tests/kani.rs function probe_a
Check 1: probe_a.assertion.1
         - Status: SUCCESS
         - Description: \"a-prop\"
         - Location: tests/kani.rs:5:5 in function probe_a

** 0 of 1 failed
VERIFICATION:- SUCCESSFUL

Checking harness probe_b...
** Results:
tests/kani.rs function probe_b
Check 1: probe_b.assertion.1
         - Status: FAILURE
         - Description: \"b-prop broke\"
         - Location: tests/kani.rs:20:5 in function probe_b

Counterexample:

State 1: tests/kani.rs:15:13 in function probe_b
----------------------------------------------------
  x = 42ul

** 1 of 1 failed
VERIFICATION:- FAILED

Complete - 1 successfully verified harnesses, 1 failures, 2 total.
";
        let cxs = parse_failures(stdout);
        assert_eq!(cxs.len(), 1);
        assert_eq!(cxs[0].harness, "probe_b");
        assert_eq!(cxs[0].failure_message.as_deref(), Some("b-prop broke"));
        assert_eq!(cxs[0].assignments.len(), 1);
        assert_eq!(cxs[0].assignments[0].name, "x");
        assert_eq!(cxs[0].assignments[0].value, "42ul");
    }

    #[test]
    fn strips_cbmc_binary_representation() {
        let line = "  flag = 1u8 (00000001)";
        let (name, value) = parse_assignment(line).unwrap();
        assert_eq!(name, "flag");
        assert_eq!(value, "1u8");
    }

    #[test]
    fn preserves_struct_constructor_parens() {
        // `Foo(123)` is not a CBMC bit pattern; the parens must survive.
        let line = "  a = Foo(123)";
        let (name, value) = parse_assignment(line).unwrap();
        assert_eq!(name, "a");
        assert_eq!(value, "Foo(123)");
    }

    #[test]
    fn ignores_non_indented_lines_with_equals() {
        // Banner lines can contain `=`; non-indented lines must be ignored.
        let line = "CBMC version 6.4.0 (cbmc-6.4.0) 64-bit";
        assert!(parse_assignment(line).is_none());
    }

    #[test]
    fn handles_no_counterexample_section() {
        // Timeout/undetermined failures have no Counterexample section;
        // still emit the harness with empty assignments.
        let stdout = "
Checking harness probe_timeout...
** Results:
tests/kani.rs function probe_timeout
Check 1: probe_timeout.assertion.1
         - Status: FAILURE
         - Description: \"verification timed out\"
         - Location: tests/kani.rs:5:5 in function probe_timeout

** 1 of 1 failed
VERIFICATION:- FAILED
";
        let cxs = parse_failures(stdout);
        assert_eq!(cxs.len(), 1);
        assert_eq!(cxs[0].harness, "probe_timeout");
        assert_eq!(
            cxs[0].failure_message.as_deref(),
            Some("verification timed out")
        );
        assert!(cxs[0].assignments.is_empty());
    }

    #[test]
    fn parse_state_header_extracts_line_number() {
        assert_eq!(
            parse_state_header("State 1: tests/kani.rs:42:5 in function foo"),
            Some(42)
        );
        assert_eq!(parse_state_header("State 7: src/lib.rs:100:1"), Some(100));
        assert_eq!(parse_state_header("---------"), None);
        assert_eq!(parse_state_header("State 1:"), None);
    }
}
