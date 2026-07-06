use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.24.0\n";
const GITIGNORE: &str = ".lake/\nbuild/\nlake-packages/\nlean_solana/.lake/\nlean_solana/build/\n";
const QED_DIR: &str = ".qed";

/// Project metadata in `.qed/config.json`. CLI commands resolve the spec
/// path by walking up to the nearest `.qed/`; explicit `--spec` overrides.
#[derive(Serialize, Deserialize)]
pub struct QedConfig {
    pub name: String,
    /// Path to the authored `.qedspec` (file or directory). Relative to
    /// the directory containing `.qed/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Vendored library interfaces dir (e.g. SPL Token), relative to the
    /// directory containing `.qed/`. `qedgen init` writes `.qed/interfaces`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interfaces_dir: Option<String>,
    pub created_at: String,
}

/// Walk up from `start` for a `.qed/config.json`; returns the `.qed/` dir
/// and loaded config, or `None`.
pub fn discover_qed_config(start: &Path) -> Option<(std::path::PathBuf, QedConfig)> {
    let mut current: Option<&Path> = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join(QED_DIR);
        let config_path = candidate.join("config.json");
        if config_path.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&config_path) {
                if let Ok(config) = serde_json::from_str::<QedConfig>(&raw) {
                    return Some((candidate, config));
                }
            }
        }
        current = dir.parent();
    }
    None
}

/// Resolve the spec path. Precedence: explicit `--spec` as-is; else the
/// `spec` field of the nearest `.qed/config.json` (resolved relative to the
/// directory containing `.qed/`); else error.
pub fn resolve_spec_path(explicit: Option<&Path>, cwd: &Path) -> Result<std::path::PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let (qed_dir, config) = discover_qed_config(cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "no --spec given and no .qed/config.json found in {} or any parent — \
             run `qedgen init` or pass `--spec <path>`",
            cwd.display()
        )
    })?;
    let spec_rel = config.spec.ok_or_else(|| {
        anyhow::anyhow!(
            "found {} but it has no `spec` field — \
             edit the config or pass `--spec <path>`",
            qed_dir.join("config.json").display()
        )
    })?;
    // `.qed/` lives inside the project root; spec is relative to that root.
    let project_root = qed_dir.parent().unwrap_or(Path::new("."));
    Ok(project_root.join(spec_rel))
}

/// Check whether `.qed/` exists in the same directory as `spec_path`.
/// `.qed/` is program-level, not repo-level — it lives next to the `.qedspec` file.
pub fn find_qed_dir(spec_path: &Path) -> Option<std::path::PathBuf> {
    let dir = if spec_path.is_file() {
        spec_path.parent()?
    } else {
        spec_path
    };
    let candidate = dir.join(QED_DIR);
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

/// Pick the directory that owns `.qed/`: with `--spec`, anchor to the spec's
/// parent (resolved against `cwd` when relative) — users expect `.qed/` next
/// to the spec, not next to `--output-dir`. Without a spec, fall back to
/// `output_dir.parent()`.
pub fn resolve_program_root(
    spec: Option<&Path>,
    output_dir: &Path,
    cwd: &Path,
) -> std::path::PathBuf {
    if let Some(spec_path) = spec {
        let abs = if spec_path.is_absolute() {
            spec_path.to_path_buf()
        } else {
            cwd.join(spec_path)
        };
        match abs.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => cwd.to_path_buf(),
        }
    } else {
        output_dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.to_path_buf())
    }
}

/// Initialize `.qed/` in `dir` (errors if present). `spec_rel` is the spec
/// path relative to `dir`, written into `config.json` so `check`/`codegen`
/// resolve it without `--spec`; `None` leaves the field empty.
pub fn init_qed_dir(dir: &Path, name: &str, spec_rel: Option<&str>) -> Result<()> {
    let qed_path = dir.join(QED_DIR);
    if qed_path.exists() {
        anyhow::bail!(
            "Already initialized — .qed/ exists in {}\n\
             To reinitialize, remove it first: rm -rf {}",
            dir.display(),
            qed_path.display()
        );
    }
    std::fs::create_dir_all(&qed_path)?;

    let config = QedConfig {
        name: name.to_string(),
        spec: spec_rel.map(|s| s.to_string()),
        // Populated by `qedgen interface --idl <path> --vendor`.
        interfaces_dir: Some(".qed/interfaces".to_string()),
        created_at: chrono_now(),
    };
    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(qed_path.join("config.json"), json)?;

    std::fs::write(
        qed_path.join(".gitignore"),
        "# .qed/ is project metadata — commit config.json and plan/\n",
    )?;

    // .qed/plan/ — agent-maintained ledger, committed by default; subdirs
    // created lazily on first write.
    let plan_path = qed_path.join("plan");
    std::fs::create_dir_all(&plan_path)?;
    std::fs::write(plan_path.join("README.md"), PLAN_README)?;

    Ok(())
}

const PLAN_README: &str = r#"# .qed/plan/

Agent-maintained ledger of what qedgen caught, what it missed, and what
reviewers surfaced after the fact. Committed by default.

## Layout

- `findings/NNN-<slug>.md` — pattern-tagged entries: a probe that fired,
  a reviewer's callout, a gap that surfaced in testing. One pattern per
  file. Reference the pattern, not the incident.
- `sessions/YYYY-MM-DD-<topic>.md` — session summaries written at
  meaningful boundaries (spec finalized, proofs shipped, bug resolved).
  Three fields: what we tried, what worked, what we'd do differently.
- `gaps.md` — running list of "qedgen didn't catch X; Y did" with a
  one-line hypothesis for what lint or harness would've caught it.
- `reviewers.md` — external-review feedback, pattern-tagged.
- `scoping.md` — moments the agent recommended NOT engaging qedgen's
  default flow (target shape doesn't fit, Phase 2 direct, skip-spec,
  forced-shim). Each entry: target shape, why `.qedspec` didn't fit
  structurally, what was recommended instead, and one-line DSL-evolution
  hypothesis. This is the richest signal for extending the DSL —
  capture it **before** walking away, not after.

Subdirectories are created lazily as entries are written.

## What to capture

Capture **patterns**, not business specifics. A good entry names a class
of bug and the shape of the guard that would catch it. A bad entry names
an account, a pubkey, a user, or a dollar value.

Good: *"Generic const parameter flowed into an `as u16` cast without a
force-evaluated compile-time bound — silent wrap at the 65,536th push."*

Bad: *"Alice's vault overflowed when she deposited 2^16 times."*

## Telemetry (future)

This ledger is the seed corpus for future qedgen lints and probes. A
future opt-in `qedgen telemetry push` will upload entries anonymised;
until that command ships, `.qed/plan/` is local-to-your-repo. You
control what leaves: inspect, edit, or delete any entry before
uploading. Scrubbing rules above are the contract.
"#;

/// Timestamp without pulling in chrono.
fn chrono_now() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", d.as_secs())
}

/// Scaffold a formal_verification/ project directory.
pub fn init(
    name: &str,
    output_dir: &Path,
    asm_source: Option<&Path>,
    mathlib: bool,
    program: bool,
) -> Result<()> {
    ensure!(!name.is_empty(), "project name must not be empty");
    ensure!(
        name.chars().all(|c| c.is_alphanumeric() || c == '_'),
        "project name must be alphanumeric (underscores allowed)"
    );

    // Nested-layout footgun: output_dir leaf is `formal_verification` AND its
    // canonicalized parent already ends in `formal_verification/` → a double
    // layer that confuses every downstream tool. Refuse.
    let leaf_is_fv = output_dir.file_name().and_then(|n| n.to_str()) == Some("formal_verification");
    let parent_is_fv = output_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "formal_verification")
        })
        .unwrap_or(false);
    if leaf_is_fv && parent_is_fv {
        anyhow::bail!(
            "refusing to scaffold into {} — the parent directory is already \
             `formal_verification/`. Either run `qedgen init` from the project \
             root, or pass `--output-dir .` to scaffold into the current \
             directory.",
            output_dir.display()
        );
    }

    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    crate::project::update_lean_solana(output_dir, mathlib)?;

    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;

    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;

    let asm_module = if let Some(asm_path) = asm_source {
        let module_name = "Program".to_string();
        let output_file = output_dir.join("Program.lean");
        crate::asm2lean::asm2lean(asm_path, &output_file, Some(&module_name))?;
        eprintln!("Generated {}", output_file.display());
        Some(module_name)
    } else {
        None
    };

    let lakefile = generate_lakefile(name, asm_module.as_deref(), mathlib);
    std::fs::write(output_dir.join("lakefile.lean"), lakefile)?;

    let spec = if program {
        generate_program_spec_skeleton(name)
    } else {
        generate_spec_skeleton(name)
    };
    std::fs::write(output_dir.join("Spec.lean"), spec)?;

    eprintln!("Initialized formal_verification project '{}'", name);
    eprintln!("  {}/", output_dir.display());
    eprintln!("  ├── lakefile.lean");
    eprintln!("  ├── lean-toolchain");
    eprintln!("  ├── lean_solana/        (support library)");
    if asm_module.is_some() {
        eprintln!("  ├── Program.lean");
    }
    eprintln!("  ├── Spec.lean          ← definitions + proofs");
    eprintln!("  └── .gitignore");

    if mathlib {
        advise_mathlib_mode(crate::validate::shared_mathlib_path().as_deref());
    }
    eprintln!();
    eprintln!(
        "Next: edit Spec.lean, then run `lake build` in {}/",
        output_dir.display()
    );

    Ok(())
}

fn generate_lakefile(name: &str, asm_module: Option<&str>, mathlib: bool) -> String {
    let pkg_name = format!("{}Proofs", name);
    let mut s = String::new();

    s.push_str("import Lake\nopen Lake DSL\n\n");
    s.push_str(&format!("package {}\n\n", pkg_name));
    s.push_str("require qedgenSupport from\n  \"./lean_solana\"\n\n");

    if mathlib {
        s.push_str(&mathlib_require_line(
            crate::validate::shared_mathlib_path().as_deref(),
        ));
        s.push('\n');
    }

    if let Some(module) = asm_module {
        s.push_str(&format!(
            "lean_lib {} where\n  roots := #[`{}]\n\n",
            module, module
        ));
    }

    // Both roots: regenerated Spec.lean + user-owned Proofs.lean. Without
    // `Proofs` here, broken or stale theorems sit silently undetected.
    s.push_str("@[default_target]\n");
    s.push_str(&format!(
        "lean_lib {}Spec where\n  roots := #[`Spec, `Proofs]\n",
        capitalize(name)
    ));

    s
}

fn generate_spec_skeleton(name: &str) -> String {
    let cap = capitalize(name);
    format!(
        r#"import QEDGen.Solana.Spec

open QEDGen.Solana.SpecDSL

/-!
# {} Verification Spec

Define the program's state, operations, invariants, and trust boundary here.
This file is the source of truth — proofs must satisfy the properties declared below.
-/

-- Uncomment and fill in your spec:
-- qedspec {} where
--   state
--     owner : Pubkey
--     amount : U64
--
--   operation initialize
--     who: owner
--     when: Uninitialized
--     then: Active
--
--   operation transfer
--     who: owner
--     when: Active
--     then: Active
--     calls: TOKEN_PROGRAM_ID DISC_TRANSFER(source writable, destination writable, authority signer)
--
--   invariant conservation "total tokens preserved"
"#,
        cap, cap
    )
}

fn generate_program_spec_skeleton(name: &str) -> String {
    let cap = capitalize(name);
    format!(
        r#"import QEDGen.Solana.Spec
open QEDGen.Solana.SpecDSL

/-!
# {cap} Verification Spec

This spec drives Rust codegen, Lean proofs, and Kani harnesses.
Edit operations, context blocks, and properties to match your program.
-/

qedspec {cap} where
  program_id: "11111111111111111111111111111111"

  state
    authority : Pubkey
    value : U64

  event InitEvent {{ authority : Pubkey }}

  errors: Unauthorized

  operation initialize
    doc: "Initialize the program state"
    who: authority
    when: Uninitialized
    then: Active
    emits: InitEvent
    context: {{
      authority : Signer, mut
      system_program : Program, System
    }}

  operation update
    doc: "Update the value"
    who: authority
    when: Active
    then: Active
    takes: new_value U64
    guard: "new_value > 0"
    effect: value set new_value
    context: {{
      authority : Signer
    }}

  property bounded "s.value ≤ U64_MAX"
    preserved_by: update
"#,
    )
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

/// Mathlib `require` line for a generated lakefile: local-path when a shared
/// workspace install exists (saves the 8 GB / 15-45 min fetch), git fallback
/// otherwise.
pub(crate) fn mathlib_require_line(shared: Option<&Path>) -> String {
    match shared {
        Some(path) => format!("require mathlib from \"{}\"\n", path.display()),
        None => "require \"leanprover-community\" / \"mathlib\" @ git \"v4.24.0\"\n".to_string(),
    }
}

/// Advise on Mathlib shared-workspace state at init (only with `--mathlib`).
pub(crate) fn advise_mathlib_mode(shared: Option<&Path>) {
    match shared {
        Some(path) => {
            eprintln!(
                "  Mathlib: reusing shared install at {} (subsequent builds are seconds)",
                path.display()
            );
        }
        None => {
            eprintln!(
                "  Mathlib: no shared install found — will fetch fresh (15-45 min first build)"
            );
            eprintln!(
                "  Tip: `qedgen setup --mathlib` once per machine so every future project reuses"
            );
            eprintln!("       the same Mathlib in seconds.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_walks_up_from_nested_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_qed_dir(root, "demo", Some("demo.qedspec")).unwrap();
        let nested = root.join("src/deep/nested");
        std::fs::create_dir_all(&nested).unwrap();
        let (qed_dir, config) = discover_qed_config(&nested).expect("discovery succeeds");
        assert_eq!(qed_dir, root.join(QED_DIR));
        assert_eq!(config.spec.as_deref(), Some("demo.qedspec"));
    }

    #[test]
    fn discover_returns_none_with_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_qed_config(tmp.path()).is_none());
    }

    #[test]
    fn resolve_spec_prefers_explicit_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_qed_dir(root, "demo", Some("from_config.qedspec")).unwrap();
        let explicit = root.join("from_flag.qedspec");
        let resolved = resolve_spec_path(Some(&explicit), root).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn resolve_spec_falls_back_to_config_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_qed_dir(root, "demo", Some("demo.qedspec")).unwrap();
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        let resolved = resolve_spec_path(None, &nested).unwrap();
        assert_eq!(resolved, root.join("demo.qedspec"));
    }

    #[test]
    fn init_scaffolds_plan_directory_with_readme() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_qed_dir(root, "demo", Some("demo.qedspec")).unwrap();
        let plan = root.join(QED_DIR).join("plan");
        assert!(plan.is_dir(), "plan/ directory should be created");
        let readme = plan.join("README.md");
        assert!(readme.is_file(), "plan/README.md should be seeded");
        let body = std::fs::read_to_string(&readme).unwrap();
        assert!(
            body.contains("findings/"),
            "README should describe findings/"
        );
        assert!(body.contains("gaps.md"), "README should describe gaps.md");
        assert!(
            body.contains("scoping.md"),
            "README should describe scoping.md"
        );
    }

    #[test]
    fn mathlib_require_line_uses_local_path_when_shared_available() {
        let shared = std::path::PathBuf::from("/tmp/fake/workspace/.lake/packages/mathlib");
        let line = mathlib_require_line(Some(&shared));
        assert!(
            line.contains("require mathlib from"),
            "should emit local-path require"
        );
        assert!(
            line.contains("/tmp/fake/workspace/.lake/packages/mathlib"),
            "should reference the shared path"
        );
        assert!(
            !line.contains("git"),
            "should not fall back to git when shared path is available"
        );
    }

    #[test]
    fn init_refuses_nested_formal_verification() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonicalize to dereference /var/folders → /private/var/folders on macOS,
        // so our in-function `.parent().canonicalize()` check matches.
        let root = tmp.path().canonicalize().unwrap();
        let existing_fv = root.join("formal_verification");
        std::fs::create_dir_all(&existing_fv).unwrap();
        // Simulate the footgun: running `qedgen init --output-dir ./formal_verification`
        // from inside `formal_verification/` → `output_dir` resolves to
        // `formal_verification/formal_verification/`.
        let nested = existing_fv.join("formal_verification");
        let err = init("demo", &nested, None, false, false).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("refusing to scaffold"),
            "expected refusal message, got: {}",
            msg
        );
        assert!(
            msg.contains("formal_verification"),
            "message should name the offending layout: {}",
            msg
        );
        assert!(
            !nested.exists(),
            "init must not create the nested directory when refusing"
        );
    }

    #[test]
    fn mathlib_require_line_falls_back_to_git_when_shared_absent() {
        let line = mathlib_require_line(None);
        assert!(
            line.contains("mathlib"),
            "should still require mathlib in fallback"
        );
        assert!(
            line.contains("git") || line.contains("leanprover-community"),
            "should emit a git-based require when shared path is absent"
        );
    }

    #[test]
    fn resolve_spec_errors_when_config_lacks_spec_field() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // spec_rel = None → config has no spec pointer.
        init_qed_dir(root, "demo", None).unwrap();
        let err = resolve_spec_path(None, root).unwrap_err().to_string();
        assert!(
            err.contains("no `spec` field"),
            "expected 'no spec field' error, got: {err}"
        );
    }

    #[test]
    fn resolve_program_root_anchors_on_spec_when_given() {
        // `qedgen init --spec pool.qedspec --output-dir /tmp/qedgen_eval`
        // from the project dir: .qed/ must land next to the spec (cwd), not
        // in /tmp.
        let cwd = std::path::Path::new("/home/user/pool-prog");
        let spec = std::path::Path::new("pool.qedspec");
        let output_dir = std::path::Path::new("/tmp/qedgen_eval");

        let root = resolve_program_root(Some(spec), output_dir, cwd);
        assert_eq!(root, cwd);
    }

    #[test]
    fn resolve_program_root_handles_absolute_spec_path() {
        let cwd = std::path::Path::new("/anywhere");
        let spec = std::path::Path::new("/home/user/proj/prog.qedspec");
        let output_dir = std::path::Path::new("/tmp/out");

        let root = resolve_program_root(Some(spec), output_dir, cwd);
        assert_eq!(root, std::path::Path::new("/home/user/proj"));
    }

    #[test]
    fn resolve_program_root_falls_back_to_output_dir_parent_when_no_spec() {
        let cwd = std::path::Path::new("/home/user");
        let output_dir = std::path::Path::new("/home/user/proj/formal_verification");

        let root = resolve_program_root(None, output_dir, cwd);
        assert_eq!(root, std::path::Path::new("/home/user/proj"));
    }
}
