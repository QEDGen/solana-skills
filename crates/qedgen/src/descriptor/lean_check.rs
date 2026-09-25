//! Type-check qedlift's emitted modules with Lean before a discharge is called
//! `verified` (#406).
//!
//! The modules import each other as `Generated.<Name>`. They are copied into a
//! scratch `Generated/` directory and compiled in import order with the Lake
//! project's `LEAN_PATH`. The scratch output directory comes first on that
//! path, so an older build of the same module name in the project cannot
//! shadow the fresh module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

/// Outcome of compiling the emitted modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeanResult {
    Passed,
    Failed(String),
}

/// The nearest directory at or above `start` that holds a Lake project file.
pub(crate) fn find_lake_project(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| is_lake_project(d))
        .map(Path::to_path_buf)
}

pub(crate) fn is_lake_project(dir: &Path) -> bool {
    dir.join("lakefile.lean").is_file() || dir.join("lakefile.toml").is_file()
}

/// Compile `modules` (qedlift's emitted `.lean` files) against `project`.
/// Errors mean the check could not run (no `lake`, bad project); a Lean
/// error or a `sorry` is `LeanResult::Failed`.
pub(crate) fn check_modules(project: &Path, modules: &[PathBuf]) -> Result<LeanResult> {
    if !is_lake_project(project) {
        bail!(
            "{} is not a Lake project (no lakefile.lean or lakefile.toml)",
            project.display()
        );
    }
    let lean_path = lake_stdout(project, &["env", "printenv", "LEAN_PATH"])?;
    let prefix = lake_stdout(project, &["env", "lean", "--print-prefix"])?;
    let lean =
        Path::new(prefix.trim())
            .join("bin")
            .join(if cfg!(windows) { "lean.exe" } else { "lean" });

    let scratch = tempfile::tempdir().context("create scratch dir for the Lean check")?;
    let src = scratch.path().join("src");
    let olean = scratch.path().join("olean");
    std::fs::create_dir_all(src.join("Generated"))?;
    std::fs::create_dir_all(olean.join("Generated"))?;

    let mut sources = BTreeMap::new();
    for m in modules {
        let stem = m
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("module path has no file stem: {}", m.display()))?
            .to_string();
        let text =
            std::fs::read_to_string(m).with_context(|| format!("reading {}", m.display()))?;
        std::fs::write(src.join("Generated").join(format!("{stem}.lean")), &text)?;
        sources.insert(stem, text);
    }

    let mut search = vec![olean.clone()];
    search.extend(std::env::split_paths(lean_path.trim()));
    let search = std::env::join_paths(search).context("building LEAN_PATH")?;

    for stem in import_order(&sources)? {
        let out = Command::new(&lean)
            .current_dir(&src)
            .env("LEAN_PATH", &search)
            .arg("-o")
            .arg(olean.join("Generated").join(format!("{stem}.olean")))
            .arg(Path::new("Generated").join(format!("{stem}.lean")))
            .output()
            .with_context(|| format!("running {}", lean.display()))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            return Ok(LeanResult::Failed(format!(
                "Generated.{stem}: lean exited {}\n{}",
                out.status,
                tail(&text, 20)
            )));
        }
        if uses_sorry(&text) {
            return Ok(LeanResult::Failed(format!(
                "Generated.{stem}: a declaration uses `sorry`"
            )));
        }
    }
    Ok(LeanResult::Passed)
}

/// Run `lake <args>` in `project` with `LEAN_PATH` cleared, so the result is
/// the project's own path.
fn lake_stdout(project: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("lake")
        .args(args)
        .current_dir(project)
        .env_remove("LEAN_PATH")
        .output()
        .map_err(|e| anyhow!("could not run `lake` (is elan installed?): {e}"))?;
    if !out.status.success() {
        bail!(
            "`lake {}` failed in {}:\n{}",
            args.join(" "),
            project.display(),
            tail(&String::from_utf8_lossy(&out.stderr), 20)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Order modules so each comes after the `Generated.*` modules it imports.
/// Imports outside the emitted set come from the project and are ignored.
fn import_order(sources: &BTreeMap<String, String>) -> Result<Vec<String>> {
    let deps: BTreeMap<&str, BTreeSet<&str>> = sources
        .iter()
        .map(|(stem, text)| {
            let ds = text
                .lines()
                .filter_map(|l| l.trim().strip_prefix("import Generated."))
                .map(str::trim)
                .filter(|d| sources.contains_key(*d))
                .collect();
            (stem.as_str(), ds)
        })
        .collect();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut order = Vec::new();
    while order.len() < deps.len() {
        let ready: Vec<&str> = deps
            .iter()
            .filter(|(s, ds)| !done.contains(*s) && ds.iter().all(|d| done.contains(d)))
            .map(|(s, _)| *s)
            .collect();
        if ready.is_empty() {
            bail!("emitted modules import each other in a cycle");
        }
        for s in ready {
            done.insert(s);
            order.push(s.to_string());
        }
    }
    Ok(order)
}

/// Lean reports `sorry` as a warning and still exits 0. Older toolchains quote
/// with `'`, newer ones with a backtick.
fn uses_sorry(output: &str) -> bool {
    output.contains("declaration uses 'sorry'") || output.contains("declaration uses `sorry`")
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_order_puts_dependencies_first() {
        let mut s = BTreeMap::new();
        s.insert(
            "ARefinement".to_string(),
            "import SVM.X\nimport Generated.ZTracedLifted\n".to_string(),
        );
        s.insert("ZTracedLifted".to_string(), "import SVM.Y\n".to_string());
        assert_eq!(
            import_order(&s).unwrap(),
            vec!["ZTracedLifted".to_string(), "ARefinement".to_string()]
        );
    }

    #[test]
    fn import_order_ignores_project_modules_and_rejects_cycles() {
        let mut s = BTreeMap::new();
        s.insert("A".to_string(), "import Generated.Other\n".to_string());
        assert_eq!(import_order(&s).unwrap(), vec!["A".to_string()]);

        s.insert("A".to_string(), "import Generated.B\n".to_string());
        s.insert("B".to_string(), "import Generated.A\n".to_string());
        assert!(import_order(&s).is_err());
    }

    #[test]
    fn detects_sorry_warning_in_both_quote_styles() {
        assert!(uses_sorry("x.lean:1:8: warning: declaration uses `sorry`"));
        assert!(uses_sorry("x.lean:1:8: warning: declaration uses 'sorry'"));
        assert!(!uses_sorry("-- no sorry here, just a comment"));
    }

    #[test]
    fn finds_nearest_lake_project() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let nested = proj.join("formal_verification").join("discharge");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(proj.join("lakefile.lean"), "").unwrap();
        assert_eq!(find_lake_project(&nested), Some(proj.clone()));
        assert!(is_lake_project(&proj));
        assert!(!is_lake_project(&nested));
    }
}
