# Scaffold Verification Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `qedgen verify` compile the selected generated program crate through a first-class `scaffold` backend.

**Architecture:** A focused `verify::scaffold` runner executes `cargo check --tests` and returns the existing `BackendReport` type. The CLI registers it before the model/proof backends, auto-enables it for flagless `verify --program`, persists it through existing evidence serialization, and makes strict mode reject a skipped enabled scaffold check.

**Tech Stack:** Rust 2021, clap, Cargo subprocesses, serde JSON reports, tempfile-backed tests.

## Global Constraints

- `--scaffold` requires `--program`.
- Flagless `verify --program <crate>` auto-enables scaffold compilation.
- Explicit backend flags remain exact: `--program <crate> --kani` does not imply scaffold.
- Run `cargo check --tests` in the selected program directory.
- Scaffold compilation is recorded evidence but never authorizes `#[qed(verified)]`.
- Ordinary skips are nonfatal; `verify --strict` fails when an enabled scaffold backend is skipped.
- Do not add scaffold compilation to the semantic backend-obligation manifest.
- Do not add dependencies or Cargo policy flags such as `--locked` or `--offline`.
- Preserve the existing behavior of verify invocations without `--program`.

---

### Task 1: Implement the scaffold runner

**Files:**
- Create: `crates/qedgen/src/verify/scaffold.rs`
- Modify: `crates/qedgen/src/verify/mod.rs`
- Test: `crates/qedgen/src/verify/scaffold.rs`

**Interfaces:**
- Consumes: `super::{BackendReport, BackendStatus}`.
- Produces: `pub(super) fn run(program_dir: Option<&Path>) -> BackendReport`.
- Produces for tests: private `run_with_cargo(program_dir: Option<&Path>, cargo_bin: &OsStr) -> BackendReport`.

- [ ] **Step 1: Register an empty scaffold module and write failing runner tests**

Add `pub(crate) mod scaffold;` beside the other verify modules in
`crates/qedgen/src/verify/mod.rs`. Create `scaffold.rs` with test helpers and
the following tests before defining `run_with_cargo`:

```rust
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
        assert!(
            report.detail.unwrap().contains(
                tmp.path().join("Cargo.toml").to_string_lossy().as_ref()
            )
        );
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
```

- [ ] **Step 2: Run the new tests and verify RED**

Run:

```bash
cargo test -p qedgen-solana-skills verify::scaffold::tests -- --nocapture
```

Expected: compilation fails because `run_with_cargo` is not defined.

- [ ] **Step 3: Implement the minimal scaffold runner**

Add these functions above the test module:

```rust
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
        Ok(out) if out.status.success() => BackendReport::passed(
            "scaffold",
            start,
            Some("cargo check --tests passed".into()),
        ),
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
```

- [ ] **Step 4: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p qedgen-solana-skills verify::scaffold::tests -- --nocapture
```

Expected: 4 passed, 0 failed.

- [ ] **Step 5: Run formatting and commit**

Run:

```bash
cargo fmt --check
git diff --check
git add crates/qedgen/src/verify/mod.rs crates/qedgen/src/verify/scaffold.rs
git commit -m "feat(verify): add scaffold compilation runner"
```

### Task 2: Wire backend selection, ordering, strictness, and evidence

**Files:**
- Modify: `crates/qedgen/src/cli.rs`
- Modify: `crates/qedgen/src/run.rs`
- Modify: `crates/qedgen/src/verify/mod.rs`
- Create: `crates/qedgen/tests/scaffold_verify_cli.rs`
- Test: `crates/qedgen/tests/scaffold_verify_cli.rs`
- Test: `crates/qedgen/src/verify/evidence.rs`

**Interfaces:**
- Consumes: `verify::scaffold::run`.
- Extends: `VerifyOpts` with `scaffold: bool` and `program_dir: Option<PathBuf>`.
- Produces: CLI flag `--scaffold`, requiring `--program`.
- Produces: `pub fn strict_scaffold_skip(report: &VerifyReport, scaffold_enabled: bool) -> Option<&BackendReport>`.

- [ ] **Step 1: Write black-box CLI tests before adding the flag**

Create `crates/qedgen/tests/scaffold_verify_cli.rs`. Stage
`crates/qedgen/tests/fixtures/descriptor/counter.qedspec` into a
git-initialized temp directory, and create `program/Cargo.toml` plus
`program/src/lib.rs`. Drive `env!("CARGO_BIN_EXE_qedgen")` with this fixture:

```rust
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
        let source_spec = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/descriptor/counter.qedspec");
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
        Self::create(
            true,
            "pub fn handler() { let _ = MissingGeneratedType; }\n",
        )
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
```

Add these tests:

```rust
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
    let out = fixture.verify(&[
        "--program",
        fixture.program_str(),
        "--scaffold",
        "--strict",
    ]);
    assert!(!out.status.success());
    assert!(
        fixture.stderr(&out).contains(
            "verify --strict: enabled scaffold backend was skipped"
        )
    );
}

#[test]
fn skipped_scaffold_is_nonfatal_without_strict() {
    let fixture = Fixture::without_program_manifest();
    let out = fixture.verify(&[
        "--program",
        fixture.program_str(),
        "--scaffold",
    ]);
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
```

Also extend the evidence test helper in `verify/evidence.rs` with a report
containing a passing `scaffold` backend:

```rust
let program = tmp_program(&spec);
let r = report(vec![("scaffold", BackendStatus::Passed)]);
let e = build(&spec, Some(&program), &r, false, None, None).unwrap();
assert_eq!(e.backends[0].name, "scaffold");
assert!(!e.backends[0].implementation_bound);
assert!(!e.implementation_verified);
assert!(e.program_hash.is_some());
```

This pins that the source hash is retained while compilation alone remains
ineligible for `stamp`.

- [ ] **Step 2: Run CLI tests and verify RED**

Run:

```bash
cargo test -p qedgen-solana-skills --test scaffold_verify_cli -- --nocapture
```

Expected: failures because clap does not recognize `--scaffold`, flagless
`--program` does not compile the crate, and no scaffold report is emitted.

- [ ] **Step 3: Add the CLI flag and backend fields**

In `Commands::Verify`, add after `program`:

```rust
/// Compile the selected program crate with `cargo check --tests`.
/// Requires `--program`. With no explicit backend flags, supplying
/// `--program` enables this backend automatically.
#[arg(long, requires = "program")]
scaffold: bool,
```

Extend `VerifyOpts`:

```rust
pub scaffold: bool,
pub program_dir: Option<PathBuf>,
```

Register the backend first:

```rust
struct ScaffoldBackend;

impl VerifyBackend for ScaffoldBackend {
    fn enabled(&self, opts: &VerifyOpts) -> bool {
        opts.scaffold
    }

    fn run(&self, opts: &VerifyOpts) -> BackendReport {
        scaffold::run(opts.program_dir.as_deref())
    }
}
```

Change the runner array to:

```rust
let runners: [&dyn VerifyBackend; 5] = [
    &ScaffoldBackend,
    &ProptestBackend,
    &KaniBackend,
    &LeanBackend,
    &MiriBackend,
];
```

- [ ] **Step 4: Implement exact/default selection and strict skip gating**

Destructure `scaffold` in `run.rs`. Include it in every
`any_backend_flag`/`any_flag` expression in the Verify arm. Compute the
effective default once, then construct opts:

```rust
let any_flag = scaffold || proptest || kani || lean || miri;
let scaffold_enabled = if any_flag { scaffold } else { program.is_some() };

let opts = if any_flag {
    verify::VerifyOpts {
        spec: spec.clone(),
        scaffold: scaffold_enabled,
        program_dir: program.clone(),
        proptest,
        proptest_path,
        kani,
        kani_path,
        lean,
        lean_dir,
        miri,
        fail_fast,
        project_root: project_root.clone(),
    }
} else {
    verify::VerifyOpts {
        spec: spec.clone(),
        scaffold: scaffold_enabled,
        program_dir: program.clone(),
        proptest: proptest_path.exists(),
        proptest_path,
        kani: kani_path.exists(),
        kani_path,
        lean: lean_dir.join("lakefile.lean").exists()
            || lean_dir.join("lakefile.toml").exists(),
        lean_dir,
        miri: miri_default,
        fail_fast,
        project_root: project_root.clone(),
    }
};
```

Use the same `scaffold_enabled` value when binding evidence to source. Rename
the local `evidence_program` variable to `source_bound_program` and add
scaffold to its path predicate:

```rust
let scaffold_matches =
    scaffold_enabled && program_root.join("Cargo.toml").is_file();
let source_matches = scaffold_matches || kani_matches || miri_matches;
source_matches
```

Pass `source_bound_program` to `record_verify_evidence`. This retains the
selected source hash for a scaffold run while leaving the evidence builder in
control of semantic stamp eligibility.

Add to `verify/mod.rs`:

```rust
pub fn strict_scaffold_skip(
    report: &VerifyReport,
    scaffold_enabled: bool,
) -> Option<&BackendReport> {
    if !scaffold_enabled {
        return None;
    }
    report.backends.iter().find(|backend| {
        backend.name == "scaffold"
            && matches!(backend.status, BackendStatus::Skipped)
    })
}
```

After rendering and evidence recording, before the existing semantic
obligation strict gate, add:

```rust
if strict {
    if let Some(skipped) = verify::strict_scaffold_skip(&report, opts.scaffold) {
        eprintln!(
            "verify --strict: enabled scaffold backend was skipped — {}",
            skipped.detail.as_deref().unwrap_or("no reason reported")
        );
        std::process::exit(1);
    }
}
```

Do not add `"scaffold"` to the implementation-bound arms in
`verify::evidence::build`; the existing fallback must remain false.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p qedgen-solana-skills verify::scaffold -- --nocapture
cargo test -p qedgen-solana-skills verify::evidence -- --nocapture
cargo test -p qedgen-solana-skills --test scaffold_verify_cli -- --nocapture
```

Expected: all focused tests pass.

- [ ] **Step 6: Run formatting, lint the diff, and commit**

Run:

```bash
cargo fmt --check
git diff --check
git add crates/qedgen/src/cli.rs crates/qedgen/src/run.rs crates/qedgen/src/verify/mod.rs crates/qedgen/src/verify/evidence.rs crates/qedgen/tests/scaffold_verify_cli.rs
git commit -m "feat(verify): compile selected program scaffolds"
```

### Task 3: Document scaffold verification

**Files:**
- Modify: `references/cli/validation.md`
- Modify: `README.md`
- Modify: `crates/qedgen/src/cli.rs`

**Interfaces:**
- Documents the `--scaffold` flag and flagless `--program` auto-selection.
- Documents that scaffold compilation is not semantic or stamp-authorizing evidence.

- [ ] **Step 1: Update the verify reference**

In `references/cli/validation.md`, add this example near the existing verify
examples:

```markdown
# Compile the generated program crate as a verify backend.
$QEDGEN verify --spec my_program.qedspec --program ./programs/my_program --scaffold
```

Add this row to the verify flags table:

```markdown
| `--scaffold` | bool | auto with flagless `--program` | Run `cargo check --tests` in the `--program` crate. Requires `--program`; explicit backend selection remains exact. Compilation proves buildability, not semantic conformance, and cannot authorize `stamp`. |
```

Extend the `--strict` row to state that an enabled but skipped scaffold
backend also gates strict verification.

- [ ] **Step 2: Update the README workflow**

Near the primary `qedgen verify` example, add:

```markdown
When `--program <crate>` is supplied without explicit backend flags, verify
also runs the `scaffold` backend (`cargo check --tests`) so generated Rust
compile failures surface inside QEDGen. Use `--scaffold` to combine that check
with an explicitly selected backend. A scaffold pass proves buildability, not
spec conformance, and cannot authorize `qedgen stamp`.
```

Ensure the clap help on `program`, `scaffold`, and the Verify command summary
uses the same terms: “program crate”, “cargo check --tests”, and
“buildability, not semantic conformance”.

- [ ] **Step 3: Run documentation and CLI drift checks**

Run:

```bash
bash scripts/check-readme-drift.sh
cargo run -p qedgen-solana-skills -- verify --help
npm test
```

Expected: all commands exit 0, and help lists `--scaffold` as requiring
`--program`.

- [ ] **Step 4: Commit documentation**

Run:

```bash
git diff --check
git add README.md references/cli/validation.md crates/qedgen/src/cli.rs
git commit -m "docs(verify): explain scaffold compilation checks"
```

### Task 4: Verify the complete implementation

**Files:**
- Verify only; modify files solely to correct failures found by these gates.

**Interfaces:**
- Confirms the complete branch satisfies issue #364 and repository gates.

- [ ] **Step 1: Run focused acceptance tests**

Run:

```bash
cargo test -p qedgen-solana-skills --test scaffold_verify_cli -- --nocapture
cargo test -p qedgen-solana-skills verify::scaffold -- --nocapture
cargo test -p qedgen-solana-skills verify::evidence -- --nocapture
```

Expected: all tests pass.

- [ ] **Step 2: Run the full Rust suite**

Run:

```bash
cargo test
```

Expected: exit 0 with zero failed tests.

- [ ] **Step 3: Run lint and package checks**

Run:

```bash
cargo clippy -- -D warnings
npm test
git diff --check origin/main...HEAD
```

Expected: all commands exit 0.

- [ ] **Step 4: Confirm branch contents**

Run:

```bash
git status --short --branch
git log --oneline --reverse origin/main..HEAD
git diff --stat origin/main...HEAD
```

Expected: a clean `feat/364-scaffold-verify` worktree containing only the
design, implementation, tests, and documentation for issue #364.
