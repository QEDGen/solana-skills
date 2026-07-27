use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _tmp: tempfile::TempDir,
    spec: PathBuf,
    program: PathBuf,
}

impl Fixture {
    fn create(with_manifest: bool, lib_rs: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let source_spec =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/descriptor/counter.qedspec");
        let spec = root.join("counter.qedspec");
        std::fs::copy(source_spec, &spec).unwrap();
        let program = root.join("program");
        std::fs::create_dir_all(program.join("src")).unwrap();
        std::fs::write(program.join("src/lib.rs"), lib_rs).unwrap();
        if with_manifest {
            std::fs::write(
                program.join("Cargo.toml"),
                "[package]\nname = \"cli_scaffold\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .unwrap();
        }
        Self {
            _tmp: tmp,
            spec,
            program,
        }
    }

    fn broken_program() -> Self {
        Self::create(true, "pub fn handler() { let _ = MissingGeneratedType; }\n")
    }

    fn without_program_manifest() -> Self {
        Self::create(false, "pub fn handler() {}\n")
    }

    fn program_str(&self) -> &str {
        self.program.to_str().unwrap()
    }

    fn verify(&self, extra_args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_qedgen"));
        command
            .arg("verify")
            .arg("--spec")
            .arg(&self.spec)
            .current_dir(self._tmp.path());
        command.args(extra_args).output().unwrap()
    }

    fn stderr(&self, output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

#[test]
fn flagless_program_auto_runs_scaffold_and_fails_on_broken_rust() {
    let fixture = Fixture::broken_program();
    let out = fixture.verify(&["--program", fixture.program_str()]);
    assert!(!out.status.success());
    assert!(fixture.stderr(&out).contains("[FAIL] scaffold"));
    assert!(fixture.stderr(&out).contains("MissingGeneratedType"));
}

#[test]
fn explicit_backend_does_not_implicitly_run_scaffold() {
    let fixture = Fixture::broken_program();
    let out = fixture.verify(&[
        "--program",
        fixture.program_str(),
        "--lean",
        "--lean-dir",
        "missing-proofs",
    ]);
    assert!(out.status.success(), "{}", fixture.stderr(&out));
    assert!(!fixture.stderr(&out).contains("scaffold"));
}

#[test]
fn scaffold_composes_with_explicit_backend_selection() {
    let fixture = Fixture::broken_program();
    let out = fixture.verify(&[
        "--program",
        fixture.program_str(),
        "--lean",
        "--lean-dir",
        "missing-proofs",
        "--scaffold",
    ]);
    assert!(!out.status.success());
    assert!(fixture.stderr(&out).contains("[FAIL] scaffold"));
}

#[test]
fn fail_fast_reports_only_scaffold_when_it_fails_first() {
    let fixture = Fixture::broken_program();
    let out = fixture.verify(&[
        "--program",
        fixture.program_str(),
        "--lean",
        "--lean-dir",
        "missing-proofs",
        "--scaffold",
        "--fail-fast",
        "--json",
    ]);
    assert!(!out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["backends"].as_array().unwrap().len(), 1);
    assert_eq!(report["backends"][0]["name"], "scaffold");
}

#[test]
fn strict_rejects_an_enabled_scaffold_skip() {
    let fixture = Fixture::without_program_manifest();
    let out = fixture.verify(&["--program", fixture.program_str(), "--scaffold", "--strict"]);
    assert!(!out.status.success());
    assert!(fixture
        .stderr(&out)
        .contains("verify --strict: enabled scaffold backend was skipped"));
}

#[test]
fn skipped_scaffold_is_nonfatal_without_strict() {
    let fixture = Fixture::without_program_manifest();
    let out = fixture.verify(&["--program", fixture.program_str(), "--scaffold"]);
    assert!(out.status.success(), "{}", fixture.stderr(&out));
    assert!(fixture.stderr(&out).contains("[SKIP] scaffold"));
}

#[test]
fn scaffold_without_program_is_a_cli_usage_error() {
    let fixture = Fixture::without_program_manifest();
    let out = fixture.verify(&["--scaffold"]);
    assert!(!out.status.success());
    assert!(fixture.stderr(&out).contains("--program"));
}
