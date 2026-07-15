//! Evidence-based Crucible crash triage (#229).
//!
//! The previous classifier (`crucible_probe::categorize_crash`) inferred the
//! crash class from the *last action* in the recorded sequence — "last action
//! succeeded, so a post-action assert must have fired → invariant violation".
//! That heuristic misclassifies any crash whose violating action is not the
//! last one (stateful chains routinely trip an invariant mid-sequence and then
//! run more actions), and it has no way to name *which* invariant fired.
//!
//! Crucible actually hands us the answer. On replay, the generated harness
//! prints a stable marker to stdout (`crucible-fuzz-macro`, `modes.rs` replay
//! block — identical across the crucible revs we build against):
//!
//! ```text
//! [FUZZ_FINDING] crash:<crash_id> reproduces:<true|false> summary:<message>
//! ```
//!
//! `<message>` is the `fuzz_assert!` message. QEDGen's harness emits
//! `"invariant <name> violated"` / `"property <name> violated"`
//! (`crucible_gen`), the protocol-guard suite emits its own per-guard text
//! (lamport inflation, ownership takeover, …), and a bare `fuzz_assert!(cond)`
//! emits `"Assertion failed: <cond> at <file>:<line>"`.
//!
//! This module is the pure parse + classify core: it turns replay stdout into
//! a [`ReplayEvidence`] and a [`CrashClass`]. It shells nothing, so it is
//! unit-testable against the exact marker strings. The wiring that runs the
//! replay lives in `crucible_probe`.

use crate::probe::Severity;

/// The `[FUZZ_FINDING]` marker parsed from a single replay's stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayEvidence {
    /// `crash_<16-hex>` — libafl hash of the input bytes; the harness's own id.
    pub crash_id: String,
    /// Whether the crash reproduced under replay. `false` means the recorded
    /// crash did not re-fire — no reproducible evidence, so it must NOT become
    /// a finding.
    pub reproduces: bool,
    /// The `fuzz_assert!` message (everything after `summary:`), verbatim.
    pub summary: String,
}

/// Crash class derived from replay evidence — the replacement for
/// `categorize_crash`'s `(Severity, &'static str)` return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashClass {
    pub severity: Severity,
    /// Stable snake_case tag for `Finding.category_tag` / dedupe.
    pub tag: &'static str,
    /// The specific invariant or property name, when the message named one
    /// (`"invariant <name> violated"` → `Some("<name>")`). This is the signal
    /// the last-action heuristic could never produce; it also lets dedupe tell
    /// two distinct invariants tripped by the same handler apart.
    pub detail: Option<String>,
}

/// Parse the `[FUZZ_FINDING]` marker out of a replay's stdout. Returns `None`
/// if no marker line is present (e.g. the harness crashed before printing, or
/// the crucible output format drifted — the caller then falls back rather than
/// fabricating a class).
pub fn parse_fuzz_finding(stdout: &str) -> Option<ReplayEvidence> {
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("[FUZZ_FINDING]"))?;
    // Format: `[FUZZ_FINDING] crash:<id> reproduces:<bool> summary:<msg...>`.
    // `summary:` is last and its value can contain spaces, so split it off the
    // tail first, then tokenize the fixed-shape prefix.
    let (prefix, summary) = line.split_once(" summary:")?;
    let crash_id = token_after(prefix, "crash:")?;
    let reproduces = match token_after(prefix, "reproduces:")?.as_str() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    Some(ReplayEvidence {
        crash_id,
        reproduces,
        summary: summary.trim().to_string(),
    })
}

/// Extract the whitespace-delimited value following `key` in `haystack`.
fn token_after(haystack: &str, key: &str) -> Option<String> {
    let start = haystack.find(key)? + key.len();
    let rest = &haystack[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Classify a reproduced crash from its `fuzz_assert!` summary message.
///
/// Only call this when `evidence.reproduces` is true — a non-reproducing crash
/// has no class (it is dropped, not classified). An unrecognized message maps
/// to `unclassified_crash`: replay confirmed a real violation, but we can't
/// name its class from the marker alone — an explicit, honest fallback rather
/// than a guess.
pub fn classify_reproduced(summary: &str) -> CrashClass {
    // QEDGen spec-linked assertions carry the invariant/property name.
    if let Some(name) = between(summary, "invariant ", " violated") {
        return CrashClass {
            severity: Severity::High,
            tag: "invariant_violation",
            detail: Some(name),
        };
    }
    if let Some(name) = between(summary, "property ", " violated") {
        return CrashClass {
            severity: Severity::High,
            tag: "property_violation",
            detail: Some(name),
        };
    }
    // Protocol-guard suite (crucible_gen::emit_guard_*). Each guard's message
    // opens with a stable phrase; match on that. These are genuine
    // conservation/authority breaks → High.
    let guard = [
        ("lamport inflation", "lamport_inflation"),
        ("ownership takeover", "ownership_takeover"),
        ("discriminator change", "discriminator_change"),
        ("unscrubbed close", "unscrubbed_close"),
        ("rent-exemption lost", "rent_exemption_lost"),
        ("realloc data leak", "realloc_data_leak"),
        ("token inflation", "token_inflation"),
    ]
    .into_iter()
    .find(|(phrase, _)| summary.starts_with(phrase));
    if let Some((_, tag)) = guard {
        return CrashClass {
            severity: Severity::High,
            tag,
            detail: None,
        };
    }
    // Bare `fuzz_assert!(cond)` with no custom message. Real, but lower-signal
    // than a named invariant.
    if summary.starts_with("Assertion failed:") {
        return CrashClass {
            severity: Severity::Medium,
            tag: "assertion_failure",
            detail: None,
        };
    }
    // Reproduced, but the message matches no known shape.
    CrashClass {
        severity: Severity::Medium,
        tag: "unclassified_crash",
        detail: None,
    }
}

/// The substring strictly between `open` and the first `close` after it, or
/// `None` if the framing isn't present.
fn between(haystack: &str, open: &str, close: &str) -> Option<String> {
    let start = haystack.find(open)? + open.len();
    let rest = &haystack[start..];
    let end = rest.find(close)?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Marker strings below are byte-for-byte what `crucible-fuzz-macro`
    // (`modes.rs` replay block) prints; keep them verbatim so this test also
    // guards against us drifting from the crucible contract.

    #[test]
    fn parses_reproducing_invariant_marker() {
        let stdout = "[REPLAY] Input size: 128 bytes\n\
             [FUZZ_FINDING] crash:crash_00000000deadbeef reproduces:true summary:invariant conservation violated\n\
             [INVARIANT] crash_00000000deadbeef: invariant conservation violated\n";
        let ev = parse_fuzz_finding(stdout).expect("marker present");
        assert_eq!(ev.crash_id, "crash_00000000deadbeef");
        assert!(ev.reproduces);
        assert_eq!(ev.summary, "invariant conservation violated");
    }

    #[test]
    fn parses_non_reproducing_marker() {
        let stdout = "[FUZZ_FINDING] crash:crash_0000000000000001 reproduces:false summary:did not reproduce\n";
        let ev = parse_fuzz_finding(stdout).unwrap();
        assert!(!ev.reproduces);
    }

    #[test]
    fn absent_marker_is_none() {
        assert!(parse_fuzz_finding("[REPLAY] Input size: 0 bytes\n").is_none());
    }

    #[test]
    fn classifies_named_invariant_with_detail() {
        let c = classify_reproduced("invariant conservation violated");
        assert_eq!(c.tag, "invariant_violation");
        assert_eq!(c.detail.as_deref(), Some("conservation"));
        assert_eq!(c.severity, Severity::High);
    }

    #[test]
    fn classifies_named_property_with_detail() {
        let c = classify_reproduced("property no_dilution violated");
        assert_eq!(c.tag, "property_violation");
        assert_eq!(c.detail.as_deref(), Some("no_dilution"));
    }

    #[test]
    fn classifies_protocol_guard_messages() {
        // Verbatim prefixes from crucible_gen::emit_guard_*.
        assert_eq!(
            classify_reproduced("lamport inflation in escrow_vault: tracked total 5 -> 9").tag,
            "lamport_inflation"
        );
        assert_eq!(
            classify_reproduced("ownership takeover on vault in claim: A -> B").tag,
            "ownership_takeover"
        );
        assert_eq!(
            classify_reproduced("token inflation for mint M in mint_to: 100 -> 200").tag,
            "token_inflation"
        );
    }

    #[test]
    fn bare_assertion_is_medium() {
        let c = classify_reproduced("Assertion failed: x < y at src/main.rs:42");
        assert_eq!(c.tag, "assertion_failure");
        assert_eq!(c.severity, Severity::Medium);
    }

    #[test]
    fn unknown_reproduced_message_is_unclassified() {
        let c = classify_reproduced("some novel host panic we don't recognize");
        assert_eq!(c.tag, "unclassified_crash");
        assert_eq!(c.detail, None);
    }

    /// The #229 regression: a marker that names an EARLIER invariant while the
    /// recorded sequence's last action succeeded. The last-action heuristic
    /// would call this a generic `invariant_violation` with no name; evidence
    /// classification names the actual invariant. (Mirrors the committed
    /// `real-crucible-crash.meta.json` fixture, whose last action is a
    /// successful `withdraw`.)
    #[test]
    fn earlier_action_marker_wins_over_last_action() {
        let stdout = "[FUZZ_FINDING] crash:crash_00000000cafef00d reproduces:true summary:invariant escrow_conservation violated\n";
        let ev = parse_fuzz_finding(stdout).unwrap();
        assert!(ev.reproduces);
        let c = classify_reproduced(&ev.summary);
        assert_eq!(c.tag, "invariant_violation");
        assert_eq!(c.detail.as_deref(), Some("escrow_conservation"));
    }
}
