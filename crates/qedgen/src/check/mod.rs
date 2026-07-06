//! Spec checking: parsing, the completeness-lint suite, coverage matrix, and
//! code/Kani drift detection. This module is a thin facade — the data model
//! and each stage live in sibling modules, re-exported here so existing
//! `crate::check::<symbol>` paths stay valid.

use anyhow::Result;

mod code_drift;
mod coverage;
mod diagnostics;
mod lints;
mod model;
mod parse;
mod proof_status;

pub use code_drift::*;
pub use coverage::*;
pub(crate) use diagnostics::*;
pub use lints::*;
pub use model::*;
pub use parse::*;
pub use proof_status::*;

#[cfg(test)]
pub(crate) mod test_support;

/// Lint with explicit control over both lock behavior and cache policy.
/// `qedgen check --frozen --no-cache` calls this.
pub fn lint_with_opts(
    spec_path: &std::path::Path,
    lock_mode: crate::qed_lock::LockMode,
    cache_opts: crate::import_resolver::CacheOpts,
) -> Result<Vec<CompletenessWarning>> {
    let spec = parse_spec_file_with_opts(spec_path, lock_mode, cache_opts)?;
    Ok(check_completeness(&spec))
}

/// `qedgen check --anchor-project <path>`: spec handler list vs the
/// existing Anchor program — catches stale specs and uncovered handlers
/// as a CI gate. Renders JSON or the human report; returns true when any
/// finding fired (the caller gates the exit code). Parses the spec with
/// default opts, as this stage always has — folding it into the caller's
/// lock/cache-aware parse would change gating under `--frozen`. Moved
/// out of the dispatch arm (T7c).
pub fn render_anchor_project_report(
    spec_path: &std::path::Path,
    project_path: &std::path::Path,
    json: bool,
) -> Result<bool> {
    let parsed = parse_spec_file(spec_path)?;
    let findings = crate::anchor_check::check_anchor_coverage(&parsed, project_path)?;
    let effect_findings = crate::anchor_check::check_effect_coverage(&parsed, project_path)?;
    if json {
        let payload = serde_json::json!({
            "handler_coverage": findings
                .iter()
                .map(|f| serde_json::json!({
                    "kind": format!("{:?}", f.kind),
                    "handler": f.handler_name,
                    "message": f.message(),
                }))
                .collect::<Vec<_>>(),
            "effect_coverage": effect_findings
                .iter()
                .map(|f| serde_json::json!({
                    "handler": f.handler,
                    "field": f.field,
                    "message": f.message(),
                }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        if findings.is_empty() {
            eprintln!(
                "Anchor cross-check (`{}`) — spec and program handler sets agree.",
                project_path.display()
            );
        } else {
            eprintln!(
                "Anchor cross-check (`{}`) — {} handler-set disagreement(s):",
                project_path.display(),
                findings.len()
            );
            for f in &findings {
                eprintln!("  ! {}", f.message());
            }
        }
        if effect_findings.is_empty() {
            eprintln!(
                "Effect coverage — every spec effect has a matching mutation in the Rust body."
            );
        } else {
            eprintln!(
                "Effect coverage — {} unimplemented effect(s):",
                effect_findings.len()
            );
            for f in &effect_findings {
                eprintln!("  ! {}", f.message());
            }
        }
    }
    Ok(!findings.is_empty() || !effect_findings.is_empty())
}
