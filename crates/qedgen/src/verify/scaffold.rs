use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use super::BackendReport;

pub(super) fn run(program_dir: Option<&Path>) -> BackendReport {
    run_with_cargo(program_dir, OsStr::new("cargo"))
}

fn run_with_cargo(program_dir: Option<&Path>, cargo_bin: &OsStr) -> BackendReport {
    let start = Instant::now();
    let Some(program_dir) = program_dir else {
        return BackendReport::skipped(
            "scaffold",
            start,
            Some("program crate not supplied (pass `--program <crate>`)".into()),
        );
    };
    let manifest = program_dir.join("Cargo.toml");
    if !manifest.is_file() {
        return BackendReport::skipped(
            "scaffold",
            start,
            Some(format!("Cargo.toml not found at {}", manifest.display())),
        );
    }

    match Command::new(cargo_bin)
        .args(["check", "--tests"])
        .current_dir(program_dir)
        .output()
    {
        Ok(out) if out.status.success() => {
            BackendReport::passed("scaffold", start, Some("cargo check --tests passed".into()))
        }
        Ok(out) => BackendReport::failed(
            "scaffold",
            start,
            Some(summarize_failure(&out.stdout, &out.stderr)),
        ),
        Err(error) => BackendReport::skipped(
            "scaffold",
            start,
            Some(format!(
                "Cargo is unavailable ({error}); install Cargo or add it to PATH"
            )),
        ),
    }
}

fn summarize_failure(stdout: &[u8], stderr: &[u8]) -> String {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lines: Vec<&str> = combined.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with("error"))
        .unwrap_or_else(|| lines.len().saturating_sub(20));
    let mut selected: Vec<&str> = lines.iter().skip(start).take(24).copied().collect();
    if let Some(final_line) = lines
        .iter()
        .rev()
        .find(|line| line.contains("could not compile"))
        .copied()
    {
        if !selected.contains(&final_line) {
            selected.push(final_line);
        }
    }
    format!("cargo check --tests failed\n{}", selected.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::BackendStatus;
    use std::ffi::OsStr;

    fn write_crate(dir: &Path, lib_rs: &str) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"scaffold_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), lib_rs).unwrap();
    }

    #[test]
    fn valid_program_crate_passes() {
        let tmp = tempfile::tempdir().unwrap();
        write_crate(tmp.path(), "pub fn handler() {}\n");
        let report = run_with_cargo(Some(tmp.path()), OsStr::new("cargo"));
        assert_eq!(report.status, BackendStatus::Passed);
        assert_eq!(report.name, "scaffold");
    }

    #[test]
    fn rustc_failure_is_attached_to_report() {
        let tmp = tempfile::tempdir().unwrap();
        write_crate(
            tmp.path(),
            "pub fn handler() { let _ = MissingGeneratedType; }\n",
        );
        let report = run_with_cargo(Some(tmp.path()), OsStr::new("cargo"));
        assert_eq!(report.status, BackendStatus::Failed);
        let detail = report.detail.unwrap();
        assert!(detail.contains("cargo check --tests failed"), "{detail}");
        assert!(detail.contains("MissingGeneratedType"), "{detail}");
        assert!(detail.contains("src/lib.rs"), "{detail}");
    }

    #[test]
    fn missing_manifest_skips_with_exact_path() {
        let tmp = tempfile::tempdir().unwrap();
        let report = run_with_cargo(Some(tmp.path()), OsStr::new("cargo"));
        assert_eq!(report.status, BackendStatus::Skipped);
        assert!(report
            .detail
            .unwrap()
            .contains(tmp.path().join("Cargo.toml").to_string_lossy().as_ref()));
    }

    #[test]
    fn unavailable_cargo_skips_with_path_hint() {
        let tmp = tempfile::tempdir().unwrap();
        write_crate(tmp.path(), "pub fn handler() {}\n");
        let report = run_with_cargo(
            Some(tmp.path()),
            OsStr::new("definitely-not-a-real-cargo-binary"),
        );
        assert_eq!(report.status, BackendStatus::Skipped);
        assert!(report.detail.unwrap().contains("Cargo is unavailable"));
    }
}
