//! The qedsvm side of `qedgen lift`: a pinned checkout and the tools run inside it.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The qedsvm revision whose `qed-discover` and Lean checkers the lift is tested against
/// (QEDGen/qedsvm#142). `.github/workflows/label-lift.yml` checks out the same revision.
pub const QEDSVM_LIFT_REV: Option<&str> = Some("c94a07da87184105bee7be45dfe8642231ef298f");
pub const QEDSVM_REPO: &str = "https://github.com/QEDGen/qedsvm.git";

pub(crate) trait QedsvmTools {
    fn concept_map(&self, so: &Path, debug: &Path, out: &Path) -> Result<()>;
    fn discover(&self, so: &Path, debug: &Path, concepts: &Path, out: &Path) -> Result<()>;
    fn success_proofs(
        &self,
        bindings: &Path,
        prefix: &str,
        so: &Path,
        out_dir: &Path,
    ) -> Result<()>;
    /// Check the generated modules and return Lean's `#print axioms` output for `theorems`.
    fn check_lean(&self, prefix: &str, dir: &Path, theorems: &[String]) -> Result<String>;
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The checkout's revision, if the lift may use it.
pub(crate) fn check_checkout(dir: &Path, pin: Option<&str>, dev: bool) -> Result<String> {
    let rev = git(dir, &["rev-parse", "HEAD"])?;
    let dirty = !git(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty()
        || !git(dir, &["ls-files", "--others", "--exclude-standard"])?.is_empty();
    if dev {
        return Ok(if dirty { format!("{rev}-dirty") } else { rev });
    }
    let Some(pin) = pin else {
        bail!(
            "no qedsvm revision is pinned for the lift yet; pass --qedsvm-dev with --qedsvm <checkout> to use a development checkout"
        );
    };
    if rev != pin {
        bail!(
            "the qedsvm checkout {} is at {rev}; the lift needs {pin}",
            dir.display()
        );
    }
    if dirty {
        bail!(
            "the qedsvm checkout {} has uncommitted changes",
            dir.display()
        );
    }
    Ok(rev)
}

fn cache_dir() -> Result<PathBuf> {
    if let Ok(ws) = std::env::var("QEDGEN_VALIDATION_WORKSPACE") {
        return Ok(PathBuf::from(ws).join("qedsvm"));
    }
    Ok(crate::project::validate::qedgen_home()?.join("qedsvm"))
}

/// The checkout to use: the given one (checked), else a clone of the pinned revision.
pub(crate) fn resolve_checkout(given: Option<&Path>, dev: bool) -> Result<(PathBuf, String)> {
    if let Some(dir) = given {
        // Absolute, because the Lean check runs with the checkout as its working directory.
        let dir = dir
            .canonicalize()
            .with_context(|| format!("qedsvm checkout {}", dir.display()))?;
        let rev = check_checkout(&dir, QEDSVM_LIFT_REV, dev)?;
        return Ok((dir, rev));
    }
    if dev {
        bail!("--qedsvm-dev needs --qedsvm <checkout>");
    }
    let pin = QEDSVM_LIFT_REV.context(
        "no qedsvm revision is pinned for the lift yet; pass --qedsvm <checkout> --qedsvm-dev",
    )?;
    let dir = cache_dir()?.join(pin);
    ensure_clone(&dir, QEDSVM_REPO, pin)?;
    let rev = check_checkout(&dir, Some(pin), false)?;
    Ok((dir, rev))
}

/// A fresh module tree for one Lean check, inside the checkout (Lean only compiles files
/// under its root). Each run gets its own, so parallel lifts never share files. Removed
/// when dropped.
pub(crate) fn lift_tree(root: &Path) -> Result<tempfile::TempDir> {
    let base = root.join(".lake/qedgen-lift");
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&base)
        .with_context(|| format!("creating a module tree under {}", base.display()))
}

/// A shallow checkout of `rev` from `repo` at `dir`, unless one is already there. It is
/// built in a temporary directory beside `dir` and renamed into place only when complete,
/// so a failed fetch never leaves a `dir` that later runs would take as finished.
pub(crate) fn ensure_clone(dir: &Path, repo: &str, rev: &str) -> Result<()> {
    if dir.exists() {
        return Ok(());
    }
    let parent = dir
        .parent()
        .with_context(|| format!("{} has no parent directory", dir.display()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = tempfile::Builder::new()
        .prefix(".clone-")
        .tempdir_in(parent)
        .with_context(|| format!("creating a clone directory in {}", parent.display()))?;
    git(tmp.path(), &["init", "-q"])?;
    git(tmp.path(), &["fetch", "-q", "--depth", "1", repo, rev])?;
    git(tmp.path(), &["checkout", "-q", "FETCH_HEAD"])?;
    let built = tmp.keep();
    if let Err(e) = std::fs::rename(&built, dir) {
        let _ = std::fs::remove_dir_all(&built);
        // Another run finished the same clone first.
        if dir.exists() {
            return Ok(());
        }
        return Err(e).with_context(|| format!("moving the clone to {}", dir.display()));
    }
    Ok(())
}

pub(crate) struct RealTools {
    pub root: PathBuf,
}

impl RealTools {
    fn cargo(&self, bin: &str, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
        let manifest = self.root.join("qedsvm-rs/Cargo.toml");
        let out = Command::new("cargo")
            .args(["run", "--locked", "-q", "--release", "--manifest-path"])
            .arg(&manifest)
            .args(["-p", "qed-analysis", "--bin", bin, "--"])
            .args(args)
            .output()
            .with_context(|| format!("running {bin}"))?;
        if !out.status.success() {
            bail!(
                "{bin} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    /// `lean` inside the checkout's environment, with the lift's own module tree first on
    /// `LEAN_PATH`, so generated modules never shadow or overwrite the checkout's own.
    fn lean(&self, lib: &Path, root: &Path, args: &[&std::ffi::OsStr]) -> Result<String> {
        let out = Command::new("lake")
            .args([
                "env",
                "sh",
                "-c",
                "LEAN_PATH=\"$QEDGEN_LIFT_LIB:$LEAN_PATH\" exec lean \"$@\"",
                "lean",
            ])
            .arg(format!("--root={}", root.display()))
            .args(args)
            .env("QEDGEN_LIFT_LIB", lib)
            .current_dir(&self.root)
            .output()
            .context("running `lake env lean`")?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            bail!(
                "Lean rejected the generated module:\n{text}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(text)
    }
}

impl QedsvmTools for RealTools {
    fn concept_map(&self, so: &Path, debug: &Path, out: &Path) -> Result<()> {
        let json = self.cargo("qed-concept-map", &[so.as_os_str(), debug.as_os_str()])?;
        std::fs::write(out, json)?;
        Ok(())
    }

    fn discover(&self, so: &Path, debug: &Path, concepts: &Path, out: &Path) -> Result<()> {
        let json = self.cargo(
            "qed-discover",
            &[so.as_os_str(), debug.as_os_str(), concepts.as_os_str()],
        )?;
        std::fs::write(out, json)?;
        Ok(())
    }

    fn success_proofs(
        &self,
        bindings: &Path,
        prefix: &str,
        so: &Path,
        out_dir: &Path,
    ) -> Result<()> {
        self.cargo(
            "qed-success-proofs",
            &[
                "--bindings".as_ref(),
                bindings.as_os_str(),
                "--names".as_ref(),
                prefix.as_ref(),
                so.as_os_str(),
                out_dir.as_os_str(),
            ],
        )?;
        Ok(())
    }

    fn check_lean(&self, prefix: &str, dir: &Path, theorems: &[String]) -> Result<String> {
        let out = Command::new("lake")
            .args(["build", "SVM"])
            .current_dir(&self.root)
            .output()
            .context("running `lake build SVM`")?;
        if !out.status.success() {
            bail!(
                "`lake build SVM` failed in {}:\n{}",
                self.root.display(),
                String::from_utf8_lossy(&out.stdout)
            );
        }
        // Sources in `src/<Prefix>/`, oleans in `lib/<Prefix>/`.
        let tree = lift_tree(&self.root)?;
        let base = tree.path();
        let src = base.join("src");
        let lib = base.join("lib");
        std::fs::create_dir_all(src.join(prefix))?;
        std::fs::create_dir_all(lib.join(prefix))?;
        for module in ["Text", "Success"] {
            let from = dir.join(format!("{module}.lean"));
            let to = src.join(prefix).join(format!("{module}.lean"));
            std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
            let olean = lib.join(prefix).join(format!("{module}.olean"));
            self.lean(
                &lib,
                &src,
                &["-o".as_ref(), olean.as_os_str(), to.as_os_str()],
            )?;
        }
        let audit = src.join(prefix).join("Audit.lean");
        let mut text = format!("import {prefix}.Success\n");
        for t in theorems {
            text.push_str(&format!("#print axioms {t}\n"));
        }
        std::fs::write(&audit, &text)?;
        std::fs::write(dir.join("Audit.lean"), &text)?;
        self.lean(&lib, &src, &[audit.as_os_str()])
    }
}
