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
