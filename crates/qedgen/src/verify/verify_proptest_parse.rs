//! Parse `cargo test --release --test proptest` failure output into
//! structured `Counterexample`s. PLAN-v2.16 D2.
//!
//! ## Proptest's failure output (the format we parse)
//!
//! When a proptest property fails, libtest prints something like:
//!
//! ```text
//! running 3 tests
//! test deposit_preserves_pool_solvency ... FAILED
//! test init_pool_preserves_pool_solvency ... ok
//! test withdraw_preserves_pool_solvency ... ok
//!
//! failures:
//!
//! ---- deposit_preserves_pool_solvency stdout ----
//! thread 'deposit_preserves_pool_solvency' panicked at tests/proptest.rs:117:13:
//! Test failed: pool_solvency must hold after deposit; minimal failing input: s = State { total_deposits: 18446744073709551615, total_borrows: 0, interest_rate: 100 }, amount = 1
//!         successes: 0
//!         local rejects: 0
//!         global rejects: 0
//! note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
//!
//! failures:
//!     deposit_preserves_pool_solvency
//!
//! test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured
//! ```
//!
//! The fields we extract per failure:
//! - **harness** — the `---- <name> stdout ----` heading (or fallback to
//!   the `thread '<name>' panicked` line).
//! - **source_location** — `tests/proptest.rs:117:13` from the `panicked at`
//!   line.
//! - **failure_message** — the substring after `Test failed:` and before
//!   `; minimal failing input:`.
//! - **assignments** — parsed from `name = value, name = value, ...` after
//!   `minimal failing input:`. Brace-balanced so `s = State { a: 1, b: 0 }`
//!   stays one assignment.
//!
//! ## Seed (PROPTEST_REGRESSION_FILE)
//!
//! Proptest persists shrunk seeds to
//! `<crate_dir>/proptest-regressions/<test-source>.txt`. We attach the
//! file path + the latest seed line as `seed` so the user can re-run
//! deterministically with `PROPTEST_REGRESSION_FILE=<path> cargo test ...`.
//! Reading the file is `read_seed_for_harness`, called from `verify.rs`
//! after parsing stdout.

use std::path::Path;

use crate::verify_counterexample::{Counterexample, CounterexampleVar};

/// Parse proptest failure stdout into a list of `Counterexample`s, one per
/// failed harness. Returns an empty Vec if the stdout has no
/// `failures:` block (e.g. all tests passed or the binary failed before
/// running tests).
pub fn parse_failures(stdout: &str) -> Vec<Counterexample> {
    let mut out = Vec::new();
    let lines: Vec<&str> = stdout.lines().collect();

    // Find each `---- <name> stdout ----` block. Each one is one failure.
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(harness) = parse_failure_heading(line) {
            // The block continues until the next blank line, the next
            // `---- … stdout ----` heading, or the trailing `failures:` /
            // `test result:` summary.
            let block_end = (i + 1..lines.len())
                .find(|&j| {
                    let l = lines[j];
                    parse_failure_heading(l).is_some()
                        || l.starts_with("failures:")
                        || l.starts_with("test result:")
                })
                .unwrap_or(lines.len());
            let block = &lines[i + 1..block_end];
            out.push(parse_failure_block(harness, block));
            i = block_end;
        } else {
            i += 1;
        }
    }

    out
}

/// Read the latest persisted regression seed for `harness_source` (the
/// `tests/<file>.rs` basename without extension, e.g. `proptest`).
/// Returns `<file_path>::<seed_line>` so the user has both the path
/// (for `PROPTEST_REGRESSION_FILE=`) and the literal seed.
///
/// Returns `None` if the regression directory or file doesn't exist —
/// proptest only writes regressions on failure, so absence is normal.
pub fn read_seed_for_harness(crate_dir: &Path, harness_source: &str) -> Option<String> {
    let path = crate_dir
        .join("proptest-regressions")
        .join(format!("{}.txt", harness_source));
    let content = std::fs::read_to_string(&path).ok()?;
    // The file contains comment lines (`#`) plus seed lines starting with
    // `cc `. Take the last `cc` line — that's the most recently shrunk
    // counterexample.
    let seed_line = content
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("cc "))?;
    Some(format!("{}::{}", path.display(), seed_line.trim()))
}

fn parse_failure_heading(line: &str) -> Option<&str> {
    // Match `---- <name> stdout ----` exactly (libtest's failure block heading).
    let rest = line.strip_prefix("---- ")?;
    let name = rest.strip_suffix(" stdout ----")?;
    Some(name)
}

fn parse_failure_block(harness: &str, lines: &[&str]) -> Counterexample {
    let mut source_location: Option<String> = None;
    let mut failure_message: Option<String> = None;
    let mut assignments: Vec<CounterexampleVar> = Vec::new();

    for line in lines {
        let trimmed = line.trim();

        // `thread '<harness>' panicked at <file>:<line>:<col>:` or
        // `thread '<harness>' panicked at <file>:<line>:`
        if source_location.is_none() {
            if let Some(loc) = trimmed
                .strip_prefix(&format!("thread '{}' panicked at ", harness))
                .and_then(|s| s.strip_suffix(':'))
            {
                source_location = Some(loc.to_string());
                continue;
            }
        }

        // `Test failed: <message>; minimal failing input: <inputs>`
        if let Some(rest) = trimmed.strip_prefix("Test failed: ") {
            let (msg, inputs) = match rest.find("; minimal failing input: ") {
                Some(idx) => (
                    rest[..idx].trim().to_string(),
                    rest[idx + "; minimal failing input: ".len()..].trim(),
                ),
                None => (rest.trim().to_string(), ""),
            };
            failure_message = Some(msg);
            if !inputs.is_empty() {
                assignments = parse_assignments(inputs);
            }
            continue;
        }
    }

    Counterexample {
        harness: harness.to_string(),
        status: "failed".to_string(),
        assignments,
        seed: None, // populated by caller via `read_seed_for_harness`
        failure_message,
        source_location,
    }
}

/// Parse `name = value, name = value, ...` with brace balancing so values
/// like `State { a: 1, b: 2 }` aren't split on inner commas.
fn parse_assignments(input: &str) -> Vec<CounterexampleVar> {
    let mut out = Vec::new();
    let mut depth_brace = 0i32;
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut in_str = false;
    let mut start = 0usize;
    let bytes = input.as_bytes();

    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' if !escaped_at(bytes, i) => in_str = !in_str,
            b'{' if !in_str => depth_brace += 1,
            b'}' if !in_str => depth_brace -= 1,
            b'(' if !in_str => depth_paren += 1,
            b')' if !in_str => depth_paren -= 1,
            b'[' if !in_str => depth_bracket += 1,
            b']' if !in_str => depth_bracket -= 1,
            b',' if !in_str && depth_brace == 0 && depth_paren == 0 && depth_bracket == 0 => {
                if let Some(pair) = split_assignment(&input[start..i]) {
                    out.push(pair);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < input.len() {
        if let Some(pair) = split_assignment(&input[start..]) {
            out.push(pair);
        }
    }
    out
}

fn escaped_at(bytes: &[u8], i: usize) -> bool {
    // Count contiguous backslashes immediately before bytes[i]; odd → escaped.
    let mut count = 0;
    let mut j = i;
    while j > 0 && bytes[j - 1] == b'\\' {
        count += 1;
        j -= 1;
    }
    count % 2 == 1
}

fn split_assignment(s: &str) -> Option<CounterexampleVar> {
    // Find the first `=` not inside a value structure. Since we already
    // top-level split on commas, the first `=` here is the binding.
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let idx = trimmed.find('=')?;
    let name = trimmed[..idx].trim().to_string();
    let value = trimmed[idx + 1..].trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(CounterexampleVar {
        name,
        value,
        line: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Realistic stdout from a single-failure proptest run. Tests the
    /// happy path: heading detected, panic location captured, message +
    /// inputs split correctly, brace-balanced struct value preserved.
    const SINGLE_FAILURE: &str = "
running 3 tests
test init_pool_preserves_pool_solvency ... ok
test withdraw_preserves_pool_solvency ... ok
test deposit_preserves_pool_solvency ... FAILED

failures:

---- deposit_preserves_pool_solvency stdout ----
thread 'deposit_preserves_pool_solvency' panicked at tests/proptest.rs:117:13:
Test failed: pool_solvency must hold after deposit; minimal failing input: s = State { total_deposits: 18446744073709551615, total_borrows: 0, interest_rate: 100 }, amount = 1
        successes: 0
        local rejects: 0
        global rejects: 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    deposit_preserves_pool_solvency

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured
";

    #[test]
    fn parses_single_failure() {
        let cxs = parse_failures(SINGLE_FAILURE);
        assert_eq!(cxs.len(), 1);
        let cx = &cxs[0];
        assert_eq!(cx.harness, "deposit_preserves_pool_solvency");
        assert_eq!(cx.status, "failed");
        assert_eq!(
            cx.source_location.as_deref(),
            Some("tests/proptest.rs:117:13")
        );
        assert_eq!(
            cx.failure_message.as_deref(),
            Some("pool_solvency must hold after deposit")
        );
        assert_eq!(cx.assignments.len(), 2);
        assert_eq!(cx.assignments[0].name, "s");
        assert_eq!(
            cx.assignments[0].value,
            "State { total_deposits: 18446744073709551615, total_borrows: 0, interest_rate: 100 }"
        );
        assert_eq!(cx.assignments[1].name, "amount");
        assert_eq!(cx.assignments[1].value, "1");
        assert!(cx.assignments[0].line.is_none());
    }

    #[test]
    fn parses_multiple_failures() {
        let stdout = "
running 2 tests
test a ... FAILED
test b ... FAILED

failures:

---- a stdout ----
thread 'a' panicked at tests/proptest.rs:10:1:
Test failed: prop A; minimal failing input: x = 1

---- b stdout ----
thread 'b' panicked at tests/proptest.rs:20:1:
Test failed: prop B; minimal failing input: y = 2, z = 3

failures:
    a
    b

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured
";
        let cxs = parse_failures(stdout);
        assert_eq!(cxs.len(), 2);
        assert_eq!(cxs[0].harness, "a");
        assert_eq!(cxs[0].assignments.len(), 1);
        assert_eq!(cxs[1].harness, "b");
        assert_eq!(cxs[1].assignments.len(), 2);
    }

    #[test]
    fn parses_no_failures_when_all_passed() {
        let stdout = "
running 2 tests
test a ... ok
test b ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured
";
        assert!(parse_failures(stdout).is_empty());
    }

    #[test]
    fn parse_assignments_handles_nested_braces() {
        let inputs = "s = State { a: 1, b: 2 }, amount = 100, opt = Some(Foo { x: 0 })";
        let asgs = parse_assignments(inputs);
        assert_eq!(asgs.len(), 3);
        assert_eq!(asgs[0].name, "s");
        assert_eq!(asgs[0].value, "State { a: 1, b: 2 }");
        assert_eq!(asgs[1].name, "amount");
        assert_eq!(asgs[1].value, "100");
        assert_eq!(asgs[2].name, "opt");
        assert_eq!(asgs[2].value, "Some(Foo { x: 0 })");
    }

    #[test]
    fn parse_assignments_handles_strings_with_commas() {
        let inputs = r#"name = "hello, world", count = 42"#;
        let asgs = parse_assignments(inputs);
        assert_eq!(asgs.len(), 2);
        assert_eq!(asgs[0].value, r#""hello, world""#);
        assert_eq!(asgs[1].value, "42");
    }

    #[test]
    fn handles_panic_without_col() {
        // Some toolchain versions / platforms emit `file:line:` without col.
        let stdout = "
running 1 test
test foo ... FAILED

failures:

---- foo stdout ----
thread 'foo' panicked at tests/proptest.rs:42:
Test failed: oops; minimal failing input: x = 1

failures:
    foo

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured
";
        let cxs = parse_failures(stdout);
        assert_eq!(cxs.len(), 1);
        assert_eq!(
            cxs[0].source_location.as_deref(),
            Some("tests/proptest.rs:42")
        );
    }

    #[test]
    fn handles_failure_message_without_minimal_input_marker() {
        // Defensive: if proptest's format ever changes and drops the
        // `minimal failing input:` infix, we still record the message.
        let stdout = "
running 1 test
test foo ... FAILED

failures:

---- foo stdout ----
thread 'foo' panicked at tests/proptest.rs:1:1:
Test failed: just a message

failures:
    foo

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured
";
        let cxs = parse_failures(stdout);
        assert_eq!(cxs.len(), 1);
        assert_eq!(cxs[0].failure_message.as_deref(), Some("just a message"));
        assert!(cxs[0].assignments.is_empty());
    }
}
