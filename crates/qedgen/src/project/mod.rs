use anyhow::Result;
use std::path::{Path, PathBuf};

const LAKEFILE: &str = include_str!("../../templates/lakefile.lean");
const LEAN_TOOLCHAIN: &str = include_str!("../../templates/lean-toolchain");
const MAIN_LEAN: &str = include_str!("../../templates/Main.lean");
const GITIGNORE: &str = include_str!("../../templates/.gitignore");
const README: &str = include_str!("../../templates/README.lean.md");

const SUPPORT_LAKEFILE: &str = include_str!("../../../../lean_solana/lakefile.lean");
const SUPPORT_TOOLCHAIN: &str = include_str!("../../../../lean_solana/lean-toolchain");
const SUPPORT_ROOT: &str = include_str!("../../../../lean_solana/QEDGen.lean");
const SUPPORT_ACCOUNT: &str = include_str!("../../../../lean_solana/QEDGen/Solana/Account.lean");
const SUPPORT_STATE: &str = include_str!("../../../../lean_solana/QEDGen/Solana/State.lean");
const SUPPORT_CPI: &str = include_str!("../../../../lean_solana/QEDGen/Solana/Cpi.lean");
const SUPPORT_VALID: &str = include_str!("../../../../lean_solana/QEDGen/Solana/Valid.lean");
const SUPPORT_ARITHMETIC: &str =
    include_str!("../../../../lean_solana/QEDGen/Solana/Arithmetic.lean");
const SUPPORT_SPEC: &str = include_str!("../../../../lean_solana/QEDGen/Solana/Spec.lean");
// Spec.lean imports QEDGen.Solana.CommandBuilders — it must ship alongside or
// the validation workspace fails to build (issue #71). No further deps.
const SUPPORT_COMMAND_BUILDERS: &str =
    include_str!("../../../../lean_solana/QEDGen/Solana/CommandBuilders.lean");

// The vendored `qedgen-kani-prelude` crate (#182): the soundness-proven,
// dependency-free Kani abstraction library. Embedded so `write_kani_prelude`
// can materialize it next to a program whose generated harness imports it
// (`use qedgen_kani_prelude::…`), mirroring how `lean_solana/` is delivered.
const KANI_PRELUDE_CARGO: &str = include_str!("../../../../kani_prelude/Cargo.toml");
const KANI_PRELUDE_LIB: &str = include_str!("../../../../kani_prelude/src/lib.rs");
const KANI_PRELUDE_README: &str = include_str!("../../../../kani_prelude/README.md");
// Trimmed barrel import — only the modules we embed (no SBPF/Bridge/Guards)
const SUPPORT_SOLANA_BASE: &str = "\
import QEDGen.Solana.Account\n\
import QEDGen.Solana.Cpi\n\
import QEDGen.Solana.State\n\
import QEDGen.Solana.Valid\n\
import QEDGen.Solana.Spec\n";

const SUPPORT_SOLANA_MATHLIB: &str = "\
import QEDGen.Solana.Account\n\
import QEDGen.Solana.Arithmetic\n\
import QEDGen.Solana.Cpi\n\
import QEDGen.Solana.State\n\
import QEDGen.Solana.Valid\n\
import QEDGen.Solana.Spec\n";

/// Mathlib tag for `qedgen init --mathlib` projects. Kept in sync with
/// `lean-toolchain` so `lake update` can't float to a newer Lean.
const MATHLIB_TAG: &str = "v4.30.0";

/// Mathlib `require` stanza for the `lean_solana/` sub-lakefile: local-path
/// when the shared workspace install exists (reuses the pre-built cache),
/// pinned git require otherwise.
fn mathlib_require() -> String {
    match crate::validate::shared_mathlib_path() {
        Some(path) => format!("\nrequire mathlib from \"{}\"\n", path.display()),
        None => format!(
            "\nrequire mathlib from git\n  \
             \"https://github.com/leanprover-community/mathlib4.git\" @ \"{}\"\n",
            MATHLIB_TAG
        ),
    }
}

pub fn setup_lean_project(output_dir: &Path, mathlib: bool) -> Result<()> {
    std::fs::write(output_dir.join("lakefile.lean"), LAKEFILE)?;
    std::fs::write(output_dir.join("lean-toolchain"), LEAN_TOOLCHAIN)?;
    std::fs::write(output_dir.join("Main.lean"), MAIN_LEAN)?;
    std::fs::write(output_dir.join(".gitignore"), GITIGNORE)?;
    std::fs::write(output_dir.join("README.md"), README)?;

    write_lean_solana(output_dir, mathlib)?;

    Ok(())
}

/// Refresh only lean_solana/ — leaves lakefile.lean / lean-toolchain (and
/// the .lake/ build cache) intact.
pub fn update_lean_solana(output_dir: &Path, mathlib: bool) -> Result<()> {
    write_lean_solana(output_dir, mathlib)
}

fn write_lean_solana(output_dir: &Path, mathlib: bool) -> Result<()> {
    let support_dir = output_dir.join("lean_solana");
    std::fs::create_dir_all(&support_dir)?;

    if mathlib {
        let lakefile = format!("{}{}", SUPPORT_LAKEFILE, mathlib_require());
        std::fs::write(support_dir.join("lakefile.lean"), lakefile)?;
    } else {
        std::fs::write(support_dir.join("lakefile.lean"), SUPPORT_LAKEFILE)?;
    }
    std::fs::write(support_dir.join("lean-toolchain"), SUPPORT_TOOLCHAIN)?;
    std::fs::write(support_dir.join("QEDGen.lean"), SUPPORT_ROOT)?;

    let qedgen_dir = support_dir.join("QEDGen");
    std::fs::create_dir_all(&qedgen_dir)?;
    let solana_barrel = if mathlib {
        SUPPORT_SOLANA_MATHLIB
    } else {
        SUPPORT_SOLANA_BASE
    };
    std::fs::write(qedgen_dir.join("Solana.lean"), solana_barrel)?;

    let solana_dir = support_dir.join("QEDGen/Solana");
    std::fs::create_dir_all(&solana_dir)?;
    std::fs::write(solana_dir.join("Account.lean"), SUPPORT_ACCOUNT)?;
    std::fs::write(solana_dir.join("State.lean"), SUPPORT_STATE)?;
    std::fs::write(solana_dir.join("Cpi.lean"), SUPPORT_CPI)?;
    std::fs::write(solana_dir.join("Valid.lean"), SUPPORT_VALID)?;
    std::fs::write(solana_dir.join("Spec.lean"), SUPPORT_SPEC)?;
    std::fs::write(
        solana_dir.join("CommandBuilders.lean"),
        SUPPORT_COMMAND_BUILDERS,
    )?;

    if mathlib {
        std::fs::write(solana_dir.join("Arithmetic.lean"), SUPPORT_ARITHMETIC)?;
    }

    Ok(())
}

/// Vendor the `qedgen-kani-prelude` crate (#182) into `program_dir` as
/// `qedgen_kani_prelude/` so a generated Kani harness in the same package can
/// `use qedgen_kani_prelude::…` (Shape 1 delivery). The crate is
/// dependency-free and `#![cfg(kani)]`, so off-kani it compiles to an empty
/// crate — an unconditional path-dependency on it costs a normal build nothing.
/// Mirrors `write_lean_solana`; idempotent (overwrites in place).
pub fn write_kani_prelude(program_dir: &Path) -> Result<()> {
    let crate_dir = program_dir.join("qedgen_kani_prelude");
    std::fs::create_dir_all(crate_dir.join("src"))?;
    std::fs::write(crate_dir.join("Cargo.toml"), KANI_PRELUDE_CARGO)?;
    std::fs::write(crate_dir.join("src").join("lib.rs"), KANI_PRELUDE_LIB)?;
    std::fs::write(crate_dir.join("README.md"), KANI_PRELUDE_README)?;
    Ok(())
}

/// Deliver `qedgen_kani_prelude` for a generated Kani harness at `harness_path`
/// (#182 Shape-1): materialize the crate in the harness's own package and add a
/// path-dependency on it, so `qedgen_kani_prelude::…` in the harness resolves.
/// The package is the nearest ancestor of `harness_path` with a `Cargo.toml`
/// (handles both `<pkg>/src/kani_impl.rs` in-program placement and a standalone
/// harness crate). No-op with a warning if none is found (e.g. a bare
/// `--kani-impl` before the Rust scaffold exists). Idempotent.
pub fn deliver_kani_prelude_for_harness(harness_path: &Path) -> Result<()> {
    let Some(pkg_dir) = nearest_package_dir(harness_path) else {
        eprintln!(
            "warning: generated Kani harness references `qedgen_kani_prelude` but no \
             Cargo.toml was found above {}; add `qedgen-kani-prelude = {{ path = \
             \"qedgen_kani_prelude\" }}` and run `qedgen codegen --rust` to scaffold it",
            harness_path.display()
        );
        return Ok(());
    };
    write_kani_prelude(&pkg_dir)?;
    ensure_kani_prelude_dep(&pkg_dir.join("Cargo.toml"))
}

/// Nearest ancestor directory of `path` (excluding `path` itself) containing a
/// `Cargo.toml`.
fn nearest_package_dir(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|dir| dir.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// Ensure `cargo_path` depends on the vendored prelude via a `path`
/// dependency. Idempotent (keyed on the crate name); a plain text insert so an
/// existing (brownfield) manifest keeps its comments and formatting. The crate
/// is `#![cfg(kani)]`, so this unconditional dependency compiles to an empty
/// crate off-kani.
fn ensure_kani_prelude_dep(cargo_path: &Path) -> Result<()> {
    let existing = std::fs::read_to_string(cargo_path).unwrap_or_default();
    if existing.contains("qedgen-kani-prelude") {
        return Ok(());
    }
    let dep = "qedgen-kani-prelude = { path = \"qedgen_kani_prelude\" }\n";
    let updated = if let Some(idx) = existing.find("\n[dependencies]\n") {
        let at = idx + "\n[dependencies]\n".len();
        format!("{}{}{}", &existing[..at], dep, &existing[at..])
    } else if let Some(rest) = existing.strip_prefix("[dependencies]\n") {
        format!("[dependencies]\n{dep}{rest}")
    } else {
        let sep = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{existing}{sep}\n[dependencies]\n{dep}")
    };
    std::fs::write(cargo_path, updated)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded `qedgen-kani-prelude` materializes with the dep-free
    /// primitives the generated harness imports and the `#![cfg(kani)]` gate (so
    /// an unconditional dep is free off-kani). It must carry NO `[workspace]`
    /// table: it is delivered nested inside a host harness package that is often
    /// itself a `[workspace]` root, and a second one collides as "multiple
    /// workspace roots in the same workspace".
    #[test]
    fn write_kani_prelude_materializes_the_proven_crate() {
        let tmp = tempfile::tempdir().unwrap();
        write_kani_prelude(tmp.path()).unwrap();
        let base = tmp.path().join("qedgen_kani_prelude");

        let lib = std::fs::read_to_string(base.join("src/lib.rs")).unwrap();
        assert!(lib.contains("pub fn wide_eq_32"));
        assert!(lib.contains("pub fn wide_cmp_32"));
        assert!(lib.contains("pub fn checked_div_i64"));
        assert!(lib.contains("#![cfg(kani)]"));

        let cargo = std::fs::read_to_string(base.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"qedgen-kani-prelude\""));
        // A `[workspace]` TABLE (not the word in a comment) collides with a host
        // package that is itself a workspace root.
        assert!(
            !cargo.lines().any(|l| l.trim() == "[workspace]"),
            "delivered prelude must not declare a [workspace] table (collides with host)"
        );
    }

    /// Embed-list closure gate: every `import QEDGen.Solana.X` inside an
    /// embedded module must resolve to another embedded module (issue #71
    /// shipped because Spec.lean imported a non-embedded CommandBuilders).
    fn embedded_module(name: &str, mathlib: bool) -> Option<&'static str> {
        Some(match name {
            "Account" => SUPPORT_ACCOUNT,
            "State" => SUPPORT_STATE,
            "Cpi" => SUPPORT_CPI,
            "Valid" => SUPPORT_VALID,
            "Spec" => SUPPORT_SPEC,
            "CommandBuilders" => SUPPORT_COMMAND_BUILDERS,
            "Arithmetic" if mathlib => SUPPORT_ARITHMETIC,
            _ => return None,
        })
    }

    fn assert_import_closure(mathlib: bool) {
        let names = if mathlib {
            vec![
                "Account",
                "State",
                "Cpi",
                "Valid",
                "Spec",
                "CommandBuilders",
                "Arithmetic",
            ]
        } else {
            vec![
                "Account",
                "State",
                "Cpi",
                "Valid",
                "Spec",
                "CommandBuilders",
            ]
        };
        for name in &names {
            let content = embedded_module(name, mathlib).unwrap();
            for line in content.lines() {
                let line = line.trim();
                if let Some(dep) = line.strip_prefix("import QEDGen.Solana.") {
                    let dep = dep.split_whitespace().next().unwrap_or(dep);
                    assert!(
                        embedded_module(dep, mathlib).is_some(),
                        "embedded module `{name}.lean` imports `QEDGen.Solana.{dep}`, \
                         which is NOT embedded (mathlib={mathlib}). Add it to \
                         `write_lean_solana` or the setup workspace will fail to build."
                    );
                }
            }
        }
    }

    #[test]
    fn embedded_support_modules_are_import_closed() {
        assert_import_closure(false);
        assert_import_closure(true);
    }
}

// Submodules.
pub(crate) mod consolidate;
pub(crate) mod deps;
pub(crate) mod feedback;
pub(crate) mod fill;
pub(crate) mod init;
pub(crate) mod proofs_bootstrap;
pub(crate) mod qed_lock;
pub(crate) mod qed_manifest;
pub(crate) mod reconcile;
// Lean workspace build harness (`validate_completion`, `setup_workspace`,
// `qedgen_home`) — project-level infrastructure, not spec front-end.
pub(crate) mod validate;
