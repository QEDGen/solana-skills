//! Paired-validator asymmetry probe — v2.22 Slice 2.
//!
//! Detects validator-shape sites across multiple files / handlers that
//! apply distinct accept-domains to the same logical field. The
//! canonical bench miss (subscriptions CAN-M1 / M2 / L2 / L3) is
//! `expiry_ts` being treated as "past expiry" by one validator and
//! "never expires" by another. Users following the docs for one path
//! get a hard rejection on the other — the mismatch is a sentinel-
//! semantics drift across handlers.
//!
//! ## Rule shape
//!
//! Two-stage:
//!
//! 1. **Per-file validator extraction.** Walk every `*.rs` under
//!    `<root>/src/**`; for each `if <cond> { return Err(...) }`
//!    pattern in the file, capture the condition text and identify
//!    every "field-like" identifier referenced inside it. A
//!    "field-like" ident matches a suffix pattern (`_ts`, `_secs`,
//!    `_amount`, `_id`, `_count`, ...) or is prefixed by `self.`.
//!
//! 2. **Cross-file pairwise comparison.** Group validator sites by
//!    field name. When a field appears in 2+ sites with distinct
//!    *normalized* condition shapes (whitespace-stripped, `self.`-
//!    stripped), emit a `PairedValidatorInputDomainMismatch` finding
//!    listing all distinct shapes and the sites they appear at.
//!
//! Severity is MEDIUM (a sentinel mismatch is rarely a one-shot
//! drain; usually a usability bug or a sentinel-semantics drift the
//! audit subagent triages into HIGH / suppress per the PRD's
//! escalation rules).
//!
//! ## False-positive guards
//!
//! - **Test-fn filter** (reuses `is_test_fn_name` from
//!   `arithmetic_symbol_probe`): inline `#[cfg(test)] mod tests` fns
//!   don't contribute validator sites.
//! - **Comment guard** (reuses `line_is_commented`): `//` comments
//!   on the same line are skipped.
//! - **Field name allowlist**: only fields whose name matches a
//!   suffix list (typically time / amount / id / count / status
//!   shapes) contribute. Drops the noise from generic ident-references
//!   in conditions (`let`, `mut`, etc.).
//!
//! See PRD-v2.22 §S2.1.

use anyhow::Result;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::probe::{Category, Finding, Reproducer, Severity};

#[derive(Debug, Clone)]
struct ValidatorSite {
    rel_file: PathBuf,
    line: u32,
    fn_name: String,
    raw_cond: String,
    normalized_cond: String,
}

/// Entry point: walk `<root>/src/**/*.rs`, collect validator sites,
/// then run the pairwise comparison. Returns one `Finding` per field
/// that has 2+ distinct validator shapes.
pub fn scan_program(project_root: &Path) -> Result<Vec<Finding>> {
    let src_dir = project_root.join("src");
    if !src_dir.exists() {
        return Ok(Vec::new());
    }
    let rs_files = collect_rust_files(&src_dir)?;
    // (field_name -> list of validator sites that reference it).
    let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
    for file in &rs_files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(project_root)
            .unwrap_or(file)
            .to_path_buf();
        for (field, site) in extract_validator_sites(&rel, &source) {
            by_field.entry(field).or_default().push(site);
        }
    }
    Ok(emit_findings(&by_field))
}

/// Pure text scan, isolated for unit-testability. Returns `(field,
/// site)` pairs — a single condition can register under multiple
/// field names (`expiry_ts != 0 && current_ts > expiry_ts`
/// contributes both `expiry_ts` and `current_ts`).
fn extract_validator_sites(rel_file: &Path, source: &str) -> Vec<(String, ValidatorSite)> {
    // The `(?s)` flag lets `.` match newlines so multi-line conditions
    // resolve in one shot. The lookahead-style `\s*\{[^{]*?return\s+Err`
    // ties the `if` to a Err-returning body (filtering out
    // non-validator if-chains).
    let validator_re = Regex::new(r"(?s)\bif\s+(?P<cond>[^{]+?)\s*\{\s*return\s+Err")
        .expect("static regex compiles");

    if is_cfg_kani_file(source) {
        return Vec::new();
    }

    let mut out: Vec<(String, ValidatorSite)> = Vec::new();
    for caps in validator_re.captures_iter(source) {
        let m = caps.get(0).unwrap();
        let cond = caps.name("cond").unwrap().as_str();
        let line = byte_offset_to_line(source, m.start());
        if line_is_commented(source, m.start()) {
            continue;
        }
        let Some((fn_start, fn_name)) = enclosing_fn_start_and_name(source, m.start()) else {
            continue;
        };
        if is_test_fn_name(&fn_name) {
            continue;
        }
        if has_cfg_kani_attr_before(source, fn_start) {
            continue;
        }
        // Skip plain "early return" patterns that aren't validator
        // shapes — e.g. `if balance == 0 { return Err(...) }` where
        // `balance` doesn't match a field-like name (the rule is
        // bench-tuned to time / amount / id / count fields, where the
        // mismatch surface lives).
        let fields = field_like_idents_in(cond);
        if fields.is_empty() {
            continue;
        }
        let normalized = normalize_condition(cond);
        if is_membership_check(&normalized) {
            continue;
        }
        let site = ValidatorSite {
            rel_file: rel_file.to_path_buf(),
            line,
            fn_name,
            raw_cond: cond.trim().to_string(),
            normalized_cond: normalized,
        };
        for f in fields {
            out.push((f, site.clone()));
        }
    }
    out
}

/// Group validator sites by field, then for each field with 2+
/// distinct shapes emit a single finding listing the shape set and
/// every site participating in the mismatch.
fn emit_findings(by_field: &BTreeMap<String, Vec<ValidatorSite>>) -> Vec<Finding> {
    let mut out = Vec::new();
    for (field, sites) in by_field {
        if sites.len() < 2 {
            continue;
        }
        // Distinct shapes via BTreeSet for stable ordering.
        let mut shapes: std::collections::BTreeMap<String, Vec<&ValidatorSite>> =
            std::collections::BTreeMap::new();
        for s in sites {
            shapes.entry(s.normalized_cond.clone()).or_default().push(s);
        }
        if shapes.len() < 2 {
            continue;
        }
        if shapes_are_base_guard_refinements(&shapes) {
            continue;
        }
        // Skip cases where every "shape" is a near-trivial echo —
        // e.g. the same field tested against `0` in multiple places.
        // The v2.22 first ship is conservative: emit one finding per
        // field, narrative listing each distinct shape and its sites.
        let summary = render_shape_summary(field, &shapes);

        let primary = sites.first().expect("non-empty per outer check");
        let finding_id = make_id(&primary.rel_file, primary.line, field);

        let mut subs = std::collections::BTreeMap::new();
        subs.insert("FIELD".to_string(), field.clone());
        subs.insert("SHAPES".to_string(), summary.clone());
        // Pick the first two distinct shapes as the headline sites
        // for the markdown template — the agent reads the full list
        // out of `subs["SHAPES"]`.
        let mut shape_iter = shapes.values();
        if let Some(first) = shape_iter.next() {
            if let Some(s) = first.first() {
                subs.insert(
                    "SITE_A".to_string(),
                    format!("{}:{} ({})", s.rel_file.display(), s.line, s.fn_name),
                );
                subs.insert("CONDITION_A".to_string(), s.raw_cond.clone());
            }
        }
        if let Some(second) = shape_iter.next() {
            if let Some(s) = second.first() {
                subs.insert(
                    "SITE_B".to_string(),
                    format!("{}:{} ({})", s.rel_file.display(), s.line, s.fn_name),
                );
                subs.insert("CONDITION_B".to_string(), s.raw_cond.clone());
            }
        }

        out.push(Finding {
            id: finding_id.clone(),
            category: Category::PairedValidatorInputDomainMismatch,
            severity: Severity::Medium,
            handler: primary.fn_name.clone(),
            spec_silent_on: format!(
                "Field `{field}` is gated by {} distinct validator shapes across \
                 the program. Sentinel semantics drift across handlers: users \
                 following the docs for one path may hit a hard rejection on the \
                 other.\n\n{summary}",
                shapes.len()
            ),
            suppression_hint:
                "Either: (1) align the validators on a single semantics — pick \
                 the stricter shape and apply everywhere; (2) document the \
                 sentinel explicitly (`// 0 means \"never expires\"`) and audit \
                 every validator for compliance; (3) split into two distinct \
                 fields if the semantics are truly different (e.g. \
                 `expiry_ts: Option<i64>` vs `i64`)."
                    .to_string(),
            investigation_hint: format!(
                "For each of the {} shapes, identify the sentinel value (often \
                 `0`) and whether the validator accepts or rejects it. The \
                 mismatch is HIGH severity when (a) the sentinel is documented \
                 as having a special meaning and (b) exactly one validator \
                 honors it. MEDIUM when the difference is in tolerance / \
                 operator strictness with no sentinel semantics in play.",
                shapes.len()
            ),
            category_tag: "paired_validator_input_domain_mismatch".to_string(),
            reproducer: Some(Reproducer::MolluskPrompt {
                template_path:
                    "references/probes/arithmetic_symbol/paired_validator_input_domain_mismatch.md#reproducer"
                        .to_string(),
                substitutions: subs,
                repro_path: format!(".qed/probes/paired_validator/{finding_id}/repro.rs"),
            }),
            gated_by: None,
        });
    }
    out
}

fn is_membership_check(normalized_cond: &str) -> bool {
    normalized_cond.contains(".contains(") || normalized_cond.contains("contains(")
}

fn shapes_are_base_guard_refinements(
    shapes: &std::collections::BTreeMap<String, Vec<&ValidatorSite>>,
) -> bool {
    shapes.keys().any(|base| {
        shapes.keys().all(|shape| {
            shape == base
                || shape
                    .split("||")
                    .map(|clause| clause.trim())
                    .any(|clause| clause == base)
        })
    })
}

/// Render a human-readable summary of the shape set: one line per
/// distinct shape, with the sites it appears at. Goes into
/// `spec_silent_on` so the finding speaks for itself even without the
/// markdown template.
fn render_shape_summary(
    field: &str,
    shapes: &std::collections::BTreeMap<String, Vec<&ValidatorSite>>,
) -> String {
    let mut s = format!("Field `{field}` validators:\n");
    for (shape, sites) in shapes {
        s.push_str(&format!("  • `{shape}`\n"));
        for site in sites {
            s.push_str(&format!(
                "      at {}:{} (in `{}`)\n",
                site.rel_file.display(),
                site.line,
                site.fn_name
            ));
        }
    }
    s.trim_end().to_string()
}

/// Tokenize the condition and return every field-like identifier.
/// "Field-like" matches one of the canonical state-shape suffixes;
/// generic locals (`x`, `tmp`, `result`) are filtered out.
fn field_like_idents_in(cond: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    // Identifier-or-path; accept `self.` and `*` prefix.
    let ident_re = Regex::new(r"(?:self\.)?[A-Za-z_][A-Za-z0-9_]*").expect("static regex");
    for m in ident_re.find_iter(cond) {
        let raw = m.as_str().trim_start_matches("self.");
        // Take the rightmost segment in case of nested paths.
        let last = raw.rsplit('.').next().unwrap_or(raw);
        if is_field_like(last) {
            seen.insert(last.to_string());
        }
    }
    seen.into_iter().collect()
}

fn is_field_like(name: &str) -> bool {
    // Denylist of well-known time-source / clock idents. These match
    // the `_ts` suffix but are validator *arguments* (the clock value
    // the validator compares against), not the *field* whose accept-
    // domain we're checking for drift. Without this filter, `current_ts`
    // surfaces as a high-noise "8 distinct shapes" finding because it
    // appears on the RHS of nearly every time-comparison in the
    // program. The bench evidence on subscriptions confirms it's
    // pure noise.
    let denylist = [
        "current_ts",
        "current_time",
        "now",
        "now_ts",
        "clock_ts",
        "unix_timestamp",
        "current_slot",
        "current_epoch",
    ];
    if denylist.contains(&name) {
        return false;
    }
    let suffixes = [
        "_ts",
        "_at",
        "_secs",
        "_seconds",
        "_amount",
        "_lamports",
        "_balance",
        "_id",
        "_count",
        "_status",
        "_state",
        "_bump",
        "_authority",
        "_owner",
        "_mint",
        "_program",
        "_total",
        "_limit",
        "_threshold",
        "_hours",
        "_period",
        "_length",
        "_duration",
        "_expiry",
        "_start",
        "_end",
        "_deadline",
        "_index",
    ];
    suffixes.iter().any(|s| name.ends_with(s))
}

/// Normalize a condition for shape comparison: strip whitespace,
/// strip `self.`, collapse `&& ` chains into a sorted set of atoms so
/// that `a && b` and `b && a` compare equal. Keeps the result
/// deterministic across runs.
fn normalize_condition(cond: &str) -> String {
    let stripped = cond.trim().replace("self.", "");
    // Split on `&&` first so multi-clause validators (`expiry_ts != 0
    // && current_ts > expiry_ts`) compare as sets rather than as
    // ordered sequences.
    let mut clauses: Vec<String> = stripped
        .split("&&")
        .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|c| !c.is_empty())
        .collect();
    clauses.sort();
    clauses.join(" && ")
}

// ─────────────────────────────────────────────────────────────────────
// Shared helpers (copied from arithmetic_symbol_probe; v2.22 keeps
// them duplicated so the two modules can evolve independently. v2.23
// may factor them into a `probe_text_utils` shim once a third
// source-scanner module joins them).
// ─────────────────────────────────────────────────────────────────────

fn byte_offset_to_line(source: &str, offset: usize) -> u32 {
    let prefix = &source[..offset.min(source.len())];
    1 + prefix.chars().filter(|c| *c == '\n').count() as u32
}

fn enclosing_fn_start_and_name(source: &str, offset: usize) -> Option<(usize, String)> {
    let head = &source[..offset.min(source.len())];
    let re = Regex::new(r"fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[<\(]").expect("static regex");
    re.captures_iter(head)
        .last()
        .map(|c| (c.get(0).expect("full fn match").start(), c[1].to_string()))
}

fn is_cfg_kani_file(source: &str) -> bool {
    source
        .lines()
        .take_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("#![")
                || trimmed.starts_with("#[")
        })
        .any(is_inner_cfg_kani_line)
}

fn has_cfg_kani_attr_before(source: &str, offset: usize) -> bool {
    let mut lines_before_fn: Vec<&str> = source[..offset.min(source.len())].lines().collect();
    while let Some(line) = lines_before_fn.pop() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("#[") {
            if is_cfg_kani_attr_line(trimmed) {
                return true;
            }
            continue;
        }
        break;
    }
    false
}

fn is_inner_cfg_kani_line(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.starts_with("#![cfg(kani")
}

fn is_cfg_kani_attr_line(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.starts_with("#[cfg(kani")
}

fn is_test_fn_name(fn_name: &str) -> bool {
    let lower = fn_name.to_ascii_lowercase();
    lower.starts_with("test_")
        || lower.starts_with("it_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
}

fn line_is_commented(source: &str, offset: usize) -> bool {
    let bytes = source.as_bytes();
    let mut i = offset.min(bytes.len());
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    let line_prefix = &source[i..offset.min(source.len())];
    if let Some(idx) = line_prefix.find("//") {
        let before = &line_prefix[..idx];
        let quote_count = before.chars().filter(|c| *c == '"').count();
        quote_count % 2 == 0
    } else {
        false
    }
}

fn make_id(rel_file: &Path, line: u32, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(rel_file.display().to_string().as_bytes());
    h.update(b":");
    h.update(line.to_string().as_bytes());
    h.update(b":paired_validator:");
    h.update(key.as_bytes());
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
    fn fires_on_canonical_subscriptions_expiry_ts_mismatch() {
        // Mirrors the subscriptions CAN-M1 shape across two files.
        // `create_fixed_delegation::validate` REJECTS expiry_ts == 0
        // (it's less than `current_time - drift`).
        // `transfer_validation::validate_fixed_transfer` carves out
        // expiry_ts == 0 as "never expires".
        let create_src = r#"
impl CreateFixedDelegationData {
    pub fn validate(&self, current_time: i64) -> Result<(), Error> {
        if self.expiry_ts < current_time.saturating_sub(TIME_DRIFT_ALLOWED_SECS) {
            return Err(Error::ExpiryInPast);
        }
        Ok(())
    }
}
"#;
        let transfer_src = r#"
pub fn validate_fixed_transfer(expiry_ts: i64, current_ts: i64) -> Result<(), Error> {
    if expiry_ts != 0 && current_ts > expiry_ts {
        return Err(Error::DelegationExpired);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("create_fixed_delegation.rs"), create_src) {
            by_field.entry(f).or_default().push(s);
        }
        for (f, s) in extract_validator_sites(&p("transfer_validation.rs"), transfer_src) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        let expiry_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.spec_silent_on.contains("`expiry_ts`"))
            .collect();
        assert!(
            !expiry_findings.is_empty(),
            "expected at least one expiry_ts finding; got {findings:#?}"
        );
        let f = expiry_findings[0];
        assert_eq!(f.category_tag, "paired_validator_input_domain_mismatch");
        assert!(matches!(f.severity, Severity::Medium));
    }

    #[test]
    fn ignores_single_validator_with_no_pair() {
        let src = r#"
fn validate(expiry_ts: i64, current_ts: i64) -> Result<(), Error> {
    if expiry_ts < current_ts {
        return Err(Error::Expired);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("v.rs"), src) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        assert!(
            findings.is_empty(),
            "single validator should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_test_fns() {
        // Two validator shapes but both inside test fns — suppressed.
        let src = r#"
fn test_a() -> Result<(), Error> {
    if some_amount > LIMIT {
        return Err(Error::Over);
    }
    Ok(())
}
fn test_b() -> Result<(), Error> {
    if some_amount == 0 {
        return Err(Error::Zero);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("t.rs"), src) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        assert!(
            findings.is_empty(),
            "test fns should NOT contribute, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_base_guard_plus_or_refinement() {
        let src = r#"
fn parse(lane_count: u8) -> Result<(), Error> {
    if lane_count == 0 {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn require_lane(lane_count: u8, lane_id: u8) -> Result<(), Error> {
    if lane_count == 0 || lane_id >= lane_count {
        return Err(Error::Invalid);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("state.rs"), src) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        assert!(
            findings.is_empty(),
            "base guard plus lane-id refinement should NOT fire, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_membership_checks_for_count_fields() {
        let src = r#"
fn write_account(mint_count: usize) -> Result<(), Error> {
    if mint_count == 0 || mint_count > MAX_ALLOWED_MINTS {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn require_allowed_mint(mint_count: usize, mint: Pubkey) -> Result<(), Error> {
    if mint_count == 0 || mint_count > MAX_ALLOWED_MINTS {
        return Err(Error::Invalid);
    }
    if !allowed_mints[..mint_count].contains(&mint) {
        return Err(Error::Invalid);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("state.rs"), src) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        assert!(
            findings.is_empty(),
            "membership check should NOT become mint_count domain drift, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_cfg_kani_validator_functions() {
        let src = r#"
#[cfg(not(kani))]
fn read_account_runtime(config_owner: &Pubkey, program_id: &Pubkey) -> Result<(), Error> {
    if config_owner != program_id {
        return Err(Error::WrongOwner);
    }
    Ok(())
}

#[cfg(kani)]
fn read_account_kani(config_owner: &Pubkey, program_id: &Pubkey) -> Result<(), Error> {
    if !pubkey_eq_kani(config_owner, program_id) {
        return Err(Error::WrongOwner);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("state.rs"), src) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        assert!(
            findings.is_empty(),
            "cfg(kani) proof-model helpers should NOT pair with runtime validators, got {findings:#?}"
        );
    }

    #[test]
    fn ignores_inner_cfg_kani_files() {
        let src = r#"
#![cfg(kani)]

fn proof_model(owner: &Pubkey, program_id: &Pubkey) -> Result<(), Error> {
    if !pubkey_eq_kani(owner, program_id) {
        return Err(Error::WrongOwner);
    }
    Ok(())
}
"#;
        let sites = extract_validator_sites(&p("kani_impl.rs"), src);
        assert!(
            sites.is_empty(),
            "inner cfg(kani) files should not contribute production validator sites"
        );
    }

    #[test]
    fn field_like_predicate_recognises_canonical_suffixes() {
        assert!(is_field_like("expiry_ts"));
        assert!(is_field_like("amount_per_period"));
        assert!(is_field_like("end_ts"));
        assert!(is_field_like("period_hours"));
        assert!(is_field_like("max_count"));
        assert!(!is_field_like("x"));
        assert!(!is_field_like("tmp"));
        assert!(!is_field_like("result"));
    }

    #[test]
    fn normalize_condition_treats_and_clauses_as_set() {
        let a = normalize_condition("expiry_ts != 0 && current_ts > expiry_ts");
        let b = normalize_condition("current_ts > expiry_ts && expiry_ts != 0");
        assert_eq!(a, b);
    }

    #[test]
    fn normalize_condition_strips_self_prefix() {
        let a = normalize_condition("self.expiry_ts < threshold");
        let b = normalize_condition("expiry_ts < threshold");
        assert_eq!(a, b);
    }

    #[test]
    fn pairs_two_distinct_shapes_for_same_field() {
        let src_a = r#"
fn validate_a(expiry_ts: i64, current_ts: i64) -> Result<(), Error> {
    if expiry_ts < current_ts {
        return Err(Error::Past);
    }
    Ok(())
}
"#;
        let src_b = r#"
fn validate_b(expiry_ts: i64, current_ts: i64) -> Result<(), Error> {
    if expiry_ts != 0 && current_ts > expiry_ts {
        return Err(Error::Past);
    }
    Ok(())
}
"#;
        let mut by_field: BTreeMap<String, Vec<ValidatorSite>> = BTreeMap::new();
        for (f, s) in extract_validator_sites(&p("a.rs"), src_a) {
            by_field.entry(f).or_default().push(s);
        }
        for (f, s) in extract_validator_sites(&p("b.rs"), src_b) {
            by_field.entry(f).or_default().push(s);
        }
        let findings = emit_findings(&by_field);
        // expiry_ts gets a finding (2 shapes), current_ts also (2
        // shapes — `current_ts` appears in both with different
        // condition contexts).
        let expiry = findings
            .iter()
            .find(|f| f.spec_silent_on.contains("`expiry_ts`"));
        assert!(expiry.is_some(), "expected expiry_ts finding");
    }
}
