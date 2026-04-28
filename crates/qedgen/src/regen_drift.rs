use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::Target;

#[derive(Debug, Clone)]
pub struct DriftEntry {
    pub example: String,
    pub path: PathBuf,
    pub kind: DriftKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    Changed,
    MissingGeneratedCounterpart,
}

#[derive(Debug, Default)]
pub struct RegenDriftReport {
    pub checked_examples: usize,
    pub missing_manifests: Vec<PathBuf>,
    pub drift: Vec<DriftEntry>,
}

impl RegenDriftReport {
    pub fn has_issues(&self) -> bool {
        !self.missing_manifests.is_empty() || !self.drift.is_empty()
    }
}

#[derive(Debug)]
struct Example {
    name: String,
    root: PathBuf,
    spec_rel: Option<PathBuf>,
}

pub fn check_examples(examples_root: &Path) -> Result<RegenDriftReport> {
    let examples_root = examples_root
        .canonicalize()
        .with_context(|| format!("resolving examples root {}", examples_root.display()))?;
    let mut report = RegenDriftReport::default();
    let mut examples = Vec::new();

    for entry in std::fs::read_dir(&examples_root)
        .with_context(|| format!("reading {}", examples_root.display()))?
    {
        let entry = entry?;
        let root = entry.path();
        if !root.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let has_manifest = root.join(crate::qed_manifest::MANIFEST_FILENAME).is_file();
        let tracked = has_qed_state(&root) || has_generated_artifacts(&root);

        if tracked && !has_manifest {
            report
                .missing_manifests
                .push(path_relative_to(&root, &examples_root));
            continue;
        }

        if has_manifest {
            examples.push(Example {
                name,
                spec_rel: configured_spec_rel(&root)?,
                root,
            });
        }
    }

    report.missing_manifests.sort();
    examples.sort_by(|a, b| a.name.cmp(&b.name));

    for example in examples {
        report.checked_examples += 1;
        check_example(&example, &mut report)?;
    }

    Ok(report)
}

pub fn print_report(report: &RegenDriftReport) {
    if report.missing_manifests.is_empty() && report.drift.is_empty() {
        eprintln!(
            "Example codegen drift check clean — {} example(s) checked.",
            report.checked_examples
        );
        return;
    }

    if !report.missing_manifests.is_empty() {
        eprintln!("Example manifest coverage failed:");
        for path in &report.missing_manifests {
            eprintln!("  missing qed.toml: {}", path.display());
        }
    }

    if !report.drift.is_empty() {
        eprintln!("Example codegen drift detected:");
        for entry in &report.drift {
            let kind = match entry.kind {
                DriftKind::Changed => "changed",
                DriftKind::MissingGeneratedCounterpart => "missing generated counterpart",
            };
            eprintln!("  {}: {} ({})", entry.example, entry.path.display(), kind);
        }
    }
}

fn check_example(example: &Example, report: &mut RegenDriftReport) -> Result<()> {
    let temp = tempfile::tempdir().context("creating regen-drift tempdir")?;
    let temp_root = temp.path().join("examples/rust").join(&example.name);
    std::fs::create_dir_all(&temp_root)?;
    copy_spec_inputs(&example.root, &temp_root)?;

    let repo_root = example
        .root
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("example should live under <repo>/examples/rust/<name>")?;
    let temp_repo_root = temp.path();
    copy_interfaces(repo_root, temp_repo_root)?;

    let spec_path = match &example.spec_rel {
        Some(rel) => temp_root.join(rel),
        None => temp_root.clone(),
    };

    for (rel, target) in program_outputs(&example.root)? {
        crate::codegen::generate(&spec_path, &temp_root.join(&rel), target)
            .with_context(|| format!("regenerating {} for {}", rel.display(), example.name))?;
    }

    generate_existing_artifacts(&example.root, &temp_root, &spec_path)
        .with_context(|| format!("regenerating verification artifacts for {}", example.name))?;

    for rel in comparable_paths(&example.root, &temp_root)? {
        compare_file(example, &temp_root, &rel, report)?;
    }

    Ok(())
}

fn has_qed_state(root: &Path) -> bool {
    root.join(".qed").is_dir()
}

fn has_generated_artifacts(root: &Path) -> bool {
    root.join("programs/src").is_dir()
        || root.join("src/instructions").is_dir()
        || root.join("formal_verification/Spec.lean").is_file()
        || root.join("tests/kani.rs").is_file()
        || root.join("tests/proptest.rs").is_file()
}

fn configured_spec_rel(root: &Path) -> Result<Option<PathBuf>> {
    let config = root.join(".qed/config.json");
    if !config.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&config)
        .with_context(|| format!("reading {}", config.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", config.display()))?;
    Ok(json.get("spec").and_then(|v| v.as_str()).map(PathBuf::from))
}

fn copy_spec_inputs(src_root: &Path, dst_root: &Path) -> Result<()> {
    copy_if_file(
        src_root.join(crate::qed_manifest::MANIFEST_FILENAME),
        dst_root,
    )?;
    copy_if_file(src_root.join(crate::qed_lock::LOCK_FILENAME), dst_root)?;
    copy_dir_if_exists(&src_root.join(".qed"), &dst_root.join(".qed"))?;
    copy_qedspec_tree(src_root, src_root, dst_root)?;
    Ok(())
}

fn copy_interfaces(repo_root: &Path, temp_repo_root: &Path) -> Result<()> {
    copy_dir_if_exists(
        &repo_root.join("interfaces"),
        &temp_repo_root.join("interfaces"),
    )
}

fn copy_qedspec_tree(base: &Path, current: &Path, dst_root: &Path) -> Result<()> {
    for entry in
        std::fs::read_dir(current).with_context(|| format!("reading {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            if name == ".qed"
                || name == "programs"
                || name == "src"
                || name == "tests"
                || name == "formal_verification"
            {
                continue;
            }
            copy_qedspec_tree(base, &path, dst_root)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("qedspec") {
            let rel = path.strip_prefix(base).unwrap();
            let dst = dst_root.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &dst)
                .with_context(|| format!("copying {} to {}", path.display(), dst.display()))?;
        }
    }
    Ok(())
}

fn copy_if_file(src: PathBuf, dst_root: &Path) -> Result<()> {
    if src.is_file() {
        std::fs::create_dir_all(dst_root)?;
        let dst = dst_root.join(src.file_name().unwrap());
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    }
    Ok(())
}

fn copy_dir_if_exists(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        let out = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_if_exists(&path, &out)?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &out)
                .with_context(|| format!("copying {} to {}", path.display(), out.display()))?;
        }
    }
    Ok(())
}

fn program_outputs(root: &Path) -> Result<Vec<(PathBuf, Target)>> {
    let mut outputs = Vec::new();
    for rel in [PathBuf::from("."), PathBuf::from("programs")] {
        let out_root = root.join(&rel);
        let cargo = out_root.join("Cargo.toml");
        if !cargo.is_file() || !is_generated_file(&cargo)? {
            continue;
        }
        let Some(target) = detect_program_target(&out_root)? else {
            continue;
        };
        outputs.push((rel, target));
    }
    Ok(outputs)
}

fn detect_program_target(out_root: &Path) -> Result<Option<Target>> {
    let cargo = out_root.join("Cargo.toml");
    let cargo_target = if cargo.is_file() {
        target_from_text(&std::fs::read_to_string(&cargo)?)
    } else {
        None
    };

    let lib = out_root.join("src/lib.rs");
    let lib_target = if lib.is_file() {
        target_from_text(&std::fs::read_to_string(&lib)?)
    } else {
        None
    };

    if let (Some(cargo_target), Some(lib_target)) = (cargo_target, lib_target) {
        anyhow::ensure!(
            cargo_target == lib_target,
            "target mismatch in {}: Cargo.toml is {:?}, src/lib.rs is {:?}",
            out_root.display(),
            cargo_target,
            lib_target
        );
    }

    Ok(lib_target.or(cargo_target))
}

fn target_from_text(body: &str) -> Option<Target> {
    if body.contains("anchor-lang") || body.contains("anchor_lang::prelude") {
        Some(Target::Anchor)
    } else if body.contains("quasar-lang") || body.contains("quasar_lang::prelude") {
        Some(Target::Quasar)
    } else {
        None
    }
}

fn generate_existing_artifacts(root: &Path, temp_root: &Path, spec_path: &Path) -> Result<()> {
    if root.join("tests/kani.rs").is_file() {
        crate::kani::generate(spec_path, &temp_root.join("tests/kani.rs"))?;
    }
    if root.join("programs/tests/kani.rs").is_file() {
        crate::kani::generate(spec_path, &temp_root.join("programs/tests/kani.rs"))?;
    }
    if root.join("tests/proptest.rs").is_file() {
        crate::proptest_gen::generate(spec_path, &temp_root.join("tests/proptest.rs"))?;
    }
    if root.join("programs/tests/proptest.rs").is_file() {
        crate::proptest_gen::generate(spec_path, &temp_root.join("programs/tests/proptest.rs"))?;
    }
    if root.join("src/tests.rs").is_file() {
        crate::unit_test::generate(spec_path, &temp_root.join("src/tests.rs"))?;
    }
    if root.join("programs/src/tests.rs").is_file() {
        crate::unit_test::generate(spec_path, &temp_root.join("programs/src/tests.rs"))?;
    }
    if root.join("src/integration_tests.rs").is_file() {
        crate::integration_test::generate(spec_path, &temp_root.join("src/integration_tests.rs"))?;
    }
    if root.join("programs/src/integration_tests.rs").is_file() {
        crate::integration_test::generate(
            spec_path,
            &temp_root.join("programs/src/integration_tests.rs"),
        )?;
    }
    if root.join("formal_verification/Spec.lean").is_file() {
        let parsed = crate::check::parse_spec_file(spec_path)?;
        crate::lean_gen::generate(&parsed, &temp_root.join("formal_verification/Spec.lean"))?;
    }
    Ok(())
}

fn comparable_paths(root: &Path, generated_root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();

    for base in [root, generated_root] {
        for rel in [Path::new("."), Path::new("programs")] {
            let out_root = base.join(rel);
            if out_root.join("Cargo.toml").is_file()
                && is_generated_file(&out_root.join("Cargo.toml"))?
            {
                paths.insert(rel.join("Cargo.toml"));
            }
            let src = out_root.join("src");
            if src.is_dir() {
                collect_program_src(base, &src, &mut paths)?;
            }
        }
    }

    for rel in [
        "tests/kani.rs",
        "programs/tests/kani.rs",
        "tests/proptest.rs",
        "programs/tests/proptest.rs",
        "src/tests.rs",
        "programs/src/tests.rs",
        "src/integration_tests.rs",
        "programs/src/integration_tests.rs",
        // Spec.lean is intentionally NOT compared. Codegen emits it
        // with `sorry` placeholders as TODOs; the agent fills those
        // (and any auxiliary lemmas) directly in the file. Once
        // filled, the file is user-owned — same lifecycle as
        // `instructions/<name>.rs` handler bodies. Drift would
        // otherwise flag every committed proof as a regression.
        // (User-owned proofs may also live alongside in a
        // `Proofs.lean` sibling per `lean_gen.rs:3725`.)
    ] {
        if root.join(rel).is_file() {
            paths.insert(PathBuf::from(rel));
        }
    }

    Ok(paths.into_iter().collect())
}

fn collect_program_src(root: &Path, dir: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_program_src(root, &path, paths)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") || !is_generated_file(&path)? {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        if is_user_owned_generated_file(&rel) {
            continue;
        }
        paths.insert(rel);
    }
    Ok(())
}

fn is_user_owned_generated_file(rel: &Path) -> bool {
    let parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let is_lib = parts.last().is_some_and(|name| name == "lib.rs");
    let is_instruction_body = parts.len() >= 3
        && parts[parts.len() - 3] == "src"
        && parts[parts.len() - 2] == "instructions"
        && parts[parts.len() - 1] != "mod.rs";
    is_lib || is_instruction_body
}

fn compare_file(
    example: &Example,
    temp_root: &Path,
    rel: &Path,
    report: &mut RegenDriftReport,
) -> Result<()> {
    let actual = example.root.join(rel);
    let expected = temp_root.join(rel);
    if !actual.is_file() {
        report.drift.push(DriftEntry {
            example: example.name.clone(),
            path: rel.to_path_buf(),
            kind: DriftKind::MissingGeneratedCounterpart,
        });
        return Ok(());
    }
    if !expected.is_file() {
        report.drift.push(DriftEntry {
            example: example.name.clone(),
            path: rel.to_path_buf(),
            kind: DriftKind::MissingGeneratedCounterpart,
        });
        return Ok(());
    }

    let actual_bytes = std::fs::read(&actual)?;
    let expected_bytes = std::fs::read(&expected)?;
    if actual_bytes != expected_bytes {
        report.drift.push(DriftEntry {
            example: example.name.clone(),
            path: rel.to_path_buf(),
            kind: DriftKind::Changed,
        });
    }
    Ok(())
}

fn is_generated_file(path: &Path) -> Result<bool> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(body.contains("GENERATED BY QEDGEN") || body.contains("Generated from qedspec"))
}

fn path_relative_to(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_missing_manifest_for_tracked_example() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("examples/rust/demo");
        std::fs::create_dir_all(root.join(".qed")).unwrap();
        std::fs::write(root.join("demo.qedspec"), "spec Demo\n").unwrap();

        let report = check_examples(&temp.path().join("examples/rust")).unwrap();
        assert_eq!(report.missing_manifests, vec![PathBuf::from("demo")]);
    }

    #[test]
    fn clean_manifest_only_example_passes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("examples/rust/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("qed.toml"), "[dependencies]\n").unwrap();
        std::fs::write(root.join("demo.qedspec"), "spec Demo\n").unwrap();

        let report = check_examples(&temp.path().join("examples/rust")).unwrap();
        assert_eq!(report.checked_examples, 1);
        assert!(!report.has_issues());
    }

    #[test]
    fn program_target_mismatch_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "# ---- GENERATED BY QEDGEN ----\n[dependencies]\nanchor-lang = \"0.32.1\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "// ---- GENERATED BY QEDGEN ----\nuse quasar_lang::prelude::*;\n",
        )
        .unwrap();

        let err = program_outputs(&root).unwrap_err();
        assert!(err.to_string().contains("target mismatch"));
    }

    #[test]
    fn comparable_paths_skip_user_owned_lib_and_handler_bodies() {
        let temp = tempfile::tempdir().unwrap();
        let actual = temp.path().join("actual");
        let generated = temp.path().join("generated");
        for root in [&actual, &generated] {
            std::fs::create_dir_all(root.join("src/instructions")).unwrap();
            std::fs::write(
                root.join("src/lib.rs"),
                "// ---- GENERATED BY QEDGEN ----\n",
            )
            .unwrap();
            std::fs::write(
                root.join("src/instructions/deposit.rs"),
                "// ---- GENERATED BY QEDGEN ----\n",
            )
            .unwrap();
        }
        std::fs::write(
            generated.join("src/guards.rs"),
            "// ---- GENERATED BY QEDGEN ----\n",
        )
        .unwrap();

        let paths = comparable_paths(&actual, &generated).unwrap();
        assert_eq!(paths, vec![PathBuf::from("src/guards.rs")]);
    }
}
