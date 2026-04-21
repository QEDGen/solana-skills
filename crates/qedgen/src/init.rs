use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.24.0\n";
const GITIGNORE: &str = ".lake/\nbuild/\nlake-packages/\nlean_solana/.lake/\nlean_solana/build/\n";
const QED_DIR: &str = ".qed";

/// Persistent project metadata stored in `.qed/config.json`.
///
/// `.qed/` pins the project layout: CLI commands resolve the spec path by
/// walking up from the current directory, finding the nearest `.qed/`, and
/// reading this file. Users can still pass `--spec <path>` explicitly;
/// the config is the fallback.
#[derive(Serialize, Deserialize)]
pub struct QedConfig {
    pub name: String,
    /// Path to the authored `.qedspec` (file or directory). Relative to
    /// the directory containing `.qed/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    /// Directory for vendored library interfaces (e.g. SPL Token).
    /// Relative to the directory containing `.qed/`. Defaults to
    /// `.qed/interfaces` when written by `qedgen init`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interfaces_dir: Option<String>,
    pub created_at: String,
}

/// Walk upward from `start` looking for a `.qed/config.json`. Returns the
/// discovered `.qed/` directory and the loaded config, or `None` if no
/// ancestor has one.
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

/// Resolve the spec path a CLI command should operate on.
///
/// Precedence:
/// 1. `--spec <path>` passed explicitly — returned as-is.
/// 2. `.qed/config.json spec` field discovered by walking up from `cwd`
///    (the config's spec is resolved relative to the directory containing
///    `.qed/`).
/// 3. Neither — a helpful error pointing at the two options.
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

/// Initialize `.qed/` in the given directory. Returns error if already initialized.
/// `dir` should be the program root — the directory where the `.qedspec` lives.
///
/// `spec_rel` is the spec path *relative to `dir`* — the pointer written into
/// `config.json` so `qedgen check`/`codegen` can resolve it without `--spec`.
/// Pass `None` to leave the field empty (users will need to pass `--spec`
/// explicitly until they edit the config).
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
        // Vendored library interfaces (e.g. SPL Token) land here when the
        // user runs `qedgen interface --idl <path> --vendor`.
        interfaces_dir: Some(".qed/interfaces".to_string()),
        created_at: chrono_now(),
    };
    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(qed_path.join("config.json"), json)?;

    // Add .qed/ gitignore for internal state (config.json is committed)
    std::fs::write(
        qed_path.join(".gitignore"),
        "# .qed/ is project metadata — commit config.json and plan/\n",
    )?;

    // Scaffold .qed/plan/ — agent-maintained ledger of session findings,
    // gap reports, and reviewer feedback. Committed by default. Subdirs
    // (findings/, sessions/) are created lazily when first written.
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

/// Simple ISO-8601 timestamp without pulling in chrono.
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
    quasar: bool,
) -> Result<()> {
    ensure!(!name.is_empty(), "project name must not be empty");
    ensure!(
        name.chars().all(|c| c.is_alphanumeric() || c == '_'),
        "project name must be alphanumeric (underscores allowed)"
    );

    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    // Write lean_solana/ support library (embedded in binary)
    crate::project::update_lean_solana(output_dir, mathlib)?;

    // Lean toolchain
    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;

    // .gitignore
    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;

    // If --asm, run asm2lean first
    let asm_module = if let Some(asm_path) = asm_source {
        let module_name = "Program".to_string();
        let output_file = output_dir.join("Program.lean");
        crate::asm2lean::asm2lean(asm_path, &output_file, Some(&module_name))?;
        eprintln!("Generated {}", output_file.display());
        Some(module_name)
    } else {
        None
    };

    // lakefile.lean
    let lakefile = generate_lakefile(name, asm_module.as_deref(), mathlib);
    std::fs::write(output_dir.join("lakefile.lean"), lakefile)?;

    // Spec.lean skeleton
    let spec = if quasar {
        generate_quasar_spec_skeleton(name)
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
        s.push_str("require \"leanprover-community\" / \"mathlib\" @ git \"v4.24.0\"\n\n");
    }

    // asm2lean-generated program module
    if let Some(module) = asm_module {
        s.push_str(&format!(
            "lean_lib {} where\n  roots := #[`{}]\n\n",
            module, module
        ));
    }

    // Spec library (definitions + proofs in one file)
    s.push_str("@[default_target]\n");
    s.push_str(&format!(
        "lean_lib {}Spec where\n  roots := #[`Spec]\n",
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

fn generate_quasar_spec_skeleton(name: &str) -> String {
    let cap = capitalize(name);
    format!(
        r#"import QEDGen.Solana.Spec
open QEDGen.Solana.SpecDSL

/-!
# {cap} Verification Spec

This spec drives Quasar codegen, Lean proofs, and Kani harnesses.
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
        // Don't init — no .qed/ anywhere.
        assert!(discover_qed_config(tmp.path()).is_none());
    }

    #[test]
    fn resolve_spec_prefers_explicit_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_qed_dir(root, "demo", Some("from_config.qedspec")).unwrap();
        // Explicit --spec should win over discovery.
        let explicit = root.join("from_flag.qedspec");
        let resolved = resolve_spec_path(Some(&explicit), root).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn resolve_spec_falls_back_to_config_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        init_qed_dir(root, "demo", Some("demo.qedspec")).unwrap();
        // No explicit --spec → discovery resolves via config, relative to
        // the directory containing .qed/.
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
        assert!(body.contains("findings/"), "README should describe findings/");
        assert!(body.contains("gaps.md"), "README should describe gaps.md");
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
}
