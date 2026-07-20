//! Anchor/Quasar/Pinocchio program codegen — the sole Rust-codegen path,
//! consuming `mir::Mir` + the originating `ParsedSpec`. Highest blast radius
//! of any qedgen codegen: the output is the program users compile and deploy.
//! Effect-body emission is MIR-direct; the account-constraint / guard /
//! scaffold surface stays `ParsedSpec`-based via [`crate::codegen_shared`]
//! (account/predicate surface, not effect-body `Stmt` IR). Gated by
//! `tests/codegen_snapshot.rs` (text) + `tests/codegen_smoke.rs` (build).

use anyhow::Result;
use std::path::Path;

use crate::check::ParsedSpec;
use crate::fingerprint::SpecFingerprint;
use crate::mir::Mir;
use crate::Target;

/// #288: opt-in regeneration of the user-owned file set. `force`
/// overwrites `src/lib.rs` + `src/instructions/*.rs` wholesale (the
/// regen + re-fill rename workflow); `merge_accounts` rewrites only the
/// `#[derive(Accounts)]` structs inside a user-owned `lib.rs` (Anchor).
/// Both destroy user content, so [`assert_git_recoverable`] gates them:
/// every file they would touch needs a committed, unmodified git
/// baseline first.
#[derive(Clone, Copy, Default)]
pub struct RegenOptions {
    pub force: bool,
    pub merge_accounts: bool,
}

struct CodegenCtx<'a> {
    mir: &'a Mir,
    parsed: &'a ParsedSpec,
    fp: &'a SpecFingerprint,
    spec_path: &'a Path,
    output_dir: &'a Path,
    opts: RegenOptions,
}

/// Per-framework codegen. Every `emit_*` method defaults to the shared
/// `emit_*` free function (dispatched on `self.target()`), so today the three
/// implementors differ only in the `Target` they return. Each method is an
/// intentional override point for upcoming per-target divergence — e.g.
/// Pinocchio zero-copy `State`, Quasar pod layout — where the shared default
/// no longer fits; until then the defaults keep all targets in lockstep.
trait FrameworkCodegen {
    fn target(&self) -> Target;

    fn emit_lib(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_lib(
            ctx.mir,
            ctx.parsed,
            ctx.fp,
            ctx.output_dir,
            self.target(),
            ctx.spec_path,
            ctx.opts,
        )
    }

    fn emit_state(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_state(ctx.mir, ctx.parsed, ctx.fp, ctx.output_dir, self.target())
    }

    fn emit_events(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_events(ctx.mir, ctx.parsed, ctx.fp, ctx.output_dir, self.target())
    }

    fn emit_errors(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_errors(ctx.mir, ctx.parsed, ctx.fp, ctx.output_dir, self.target())
    }

    fn emit_instructions(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_instructions(
            ctx.mir,
            ctx.parsed,
            ctx.fp,
            ctx.spec_path,
            ctx.output_dir,
            self.target(),
            ctx.opts.force,
        )
    }

    fn emit_guards(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        // Guards render the account-constraint surface (signer/writable flags,
        // pda_seeds, variant-payload fields) + per-handler requires/aborts. The
        // emitter is being migrated to read these off `&Mir` (matching the other
        // MIR-direct emitters); `ctx.parsed` stays threaded for the not-yet-
        // lifted reads (helpers, let-bindings) until those land.
        crate::codegen_shared::generate_guards(
            ctx.mir,
            ctx.parsed,
            ctx.fp,
            ctx.output_dir,
            self.target(),
        )
    }

    fn emit_math(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        if crate::codegen_shared::guards_use_math_helpers(ctx.parsed) {
            emit_math(ctx.fp, ctx.output_dir)?;
        }
        Ok(())
    }

    fn emit_ref_impls(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_ref_impls(ctx.mir, ctx.parsed, ctx.fp, ctx.output_dir, self.target())
    }

    fn emit_imported_mirror(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_imported_mirror(ctx.mir, ctx.parsed, ctx.fp, ctx.output_dir, self.target())
    }

    fn emit_cargo_toml(&self, ctx: &CodegenCtx<'_>) -> Result<()> {
        emit_cargo_toml(ctx.mir, ctx.fp, ctx.output_dir, self.target())
    }
}

struct AnchorCodegen;
struct QuasarCodegen;
struct PinocchioCodegen;

impl FrameworkCodegen for AnchorCodegen {
    fn target(&self) -> Target {
        Target::Anchor
    }
}

impl FrameworkCodegen for QuasarCodegen {
    fn target(&self) -> Target {
        Target::Quasar
    }
}

impl FrameworkCodegen for PinocchioCodegen {
    fn target(&self) -> Target {
        Target::Pinocchio
    }
}

/// Generate the program crate under `output_dir`. `spec_path` feeds the
/// instruction emitter's drift stamping.
pub fn generate(
    mir: &Mir,
    parsed: &ParsedSpec,
    spec_path: &Path,
    output_dir: &Path,
    target: Target,
    opts: RegenOptions,
) -> Result<()> {
    if parsed.handlers.is_empty() {
        anyhow::bail!("No handlers found in the spec — is this a valid qedspec file?");
    }

    crate::rust_codegen_util::check_effect_targets(parsed)?;

    if crate::init::find_qed_dir(spec_path).is_none() {
        anyhow::bail!(
            "No .qed/ directory found next to {} — run `qedgen init` first.",
            spec_path.display()
        );
    }

    if opts.merge_accounts && !matches!(target, Target::Anchor) {
        anyhow::bail!(
            "--merge-accounts is Anchor-only: on {:?} the accounts structs live in the \
             user-owned instructions/<name>.rs files, not lib.rs. Use --force to \
             regenerate the user-owned set wholesale.",
            target
        );
    }

    // #288: both destructive modes need git as the recovery path — refuse
    // to overwrite any user-owned file that has no committed, unmodified
    // baseline, BEFORE any sibling artifact is written.
    if opts.force || opts.merge_accounts {
        let mut targets = vec![output_dir.join("src").join("lib.rs")];
        if opts.force {
            for handler in &parsed.handlers {
                targets.push(
                    output_dir
                        .join("src")
                        .join("instructions")
                        .join(format!("{}.rs", handler.name)),
                );
            }
        }
        targets.retain(|p| p.exists());
        assert_git_recoverable(output_dir, &targets)?;
    }

    std::fs::create_dir_all(output_dir)?;

    let fp = crate::fingerprint::compute_fingerprint(parsed);
    let ctx = CodegenCtx {
        mir,
        parsed,
        fp: &fp,
        spec_path,
        output_dir,
        opts,
    };

    match target {
        Target::Anchor => run_framework_codegen(&AnchorCodegen, &ctx)?,
        Target::Quasar => run_framework_codegen(&QuasarCodegen, &ctx)?,
        Target::Pinocchio => run_framework_codegen(&PinocchioCodegen, &ctx)?,
    }

    let file_count = 4
        + parsed.handlers.len()
        + usize::from(!parsed.events.is_empty())
        + usize::from(!parsed.error_codes.is_empty());

    eprintln!("Generated {} files in {}", file_count, output_dir.display());

    Ok(())
}

fn run_framework_codegen(framework: &dyn FrameworkCodegen, ctx: &CodegenCtx<'_>) -> Result<()> {
    framework.emit_lib(ctx)?;
    framework.emit_state(ctx)?;
    framework.emit_events(ctx)?;
    framework.emit_errors(ctx)?;
    framework.emit_instructions(ctx)?;
    framework.emit_guards(ctx)?;
    framework.emit_math(ctx)?;
    framework.emit_ref_impls(ctx)?;
    framework.emit_imported_mirror(ctx)?;
    framework.emit_cargo_toml(ctx)?;
    Ok(())
}

// ----------------------------------------------------------------------
// Sub-generators — Phase 4b ports
// ----------------------------------------------------------------------

/// Emit `Cargo.toml` for the generated program crate. `mir_needs_spl`
/// gates the SPL dependency; an existing on-disk Cargo.toml is merged
/// via `merge_cargo_toml` rather than overwritten.
fn emit_cargo_toml(
    mir: &Mir,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    let fresh = render_cargo_toml(mir, fp, target);
    let path = output_dir.join("Cargo.toml");
    let final_toml = match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => {
            crate::codegen_shared::merge_cargo_toml(&existing, &fresh)
        }
        _ => fresh,
    };
    std::fs::write(path, final_toml)?;
    Ok(())
}

fn render_cargo_toml(
    mir: &Mir,
    fp: &crate::fingerprint::SpecFingerprint,
    target: Target,
) -> String {
    let program_name = mir.name.to_lowercase().replace('_', "-");
    let needs_spl = mir_needs_spl(mir);
    let hash = crate::codegen_shared::fingerprint_hash(fp, "Cargo.toml");
    let qedgen_version = env!("CARGO_PKG_VERSION");

    let mut out = String::new();
    out.push_str(&format!(
        "# ---- GENERATED BY QEDGEN ---- spec-hash:{}\n\n",
        hash
    ));
    out.push_str("[package]\n");
    out.push_str(&format!("name = \"{}\"\n", program_name));
    out.push_str("version = \"0.1.0\"\n");
    out.push_str("edition = \"2021\"\n\n");
    out.push_str("[lib]\n");
    out.push_str("crate-type = [\"cdylib\", \"lib\"]\n\n");
    out.push_str("[features]\n");
    out.push_str("client = []\n");
    out.push_str("debug = []\n\n");
    out.push_str("[dependencies]\n");
    match target {
        Target::Anchor => {
            out.push_str("anchor-lang = \"0.32.1\"\n");
            if needs_spl {
                out.push_str("anchor-spl = \"0.32.1\"\n");
            }
        }
        Target::Quasar => {
            out.push_str("quasar-lang = { version = \"0.0.0\" }\n");
            if needs_spl {
                out.push_str("quasar-spl = { version = \"0.0.0\" }\n");
            }
        }
        Target::Pinocchio => {
            // pinocchio (entrypoint + AccountInfo), pinocchio-pubkey
            // (declare_id!), zeropod (zero-copy state); pinocchio-token
            // only for Token CPIs.
            out.push_str("pinocchio = \"0.8\"\n");
            out.push_str("pinocchio-pubkey = \"0.3\"\n");
            out.push_str("zeropod = \"0.1\"\n");
            if needs_spl {
                out.push_str("pinocchio-token = \"0.3\"\n");
            }
        }
    }
    out.push_str(&format!(
        "qedgen-macros = {{ git = \"https://github.com/qedgen/solana-skills\", tag = \"v{}\" }}\n",
        qedgen_version
    ));

    // The generated proptest harness (tests/proptest.rs) imports
    // `proptest::prelude::*` — ship the dev-dep so the crate compiles
    // out of the box instead of failing on the first `cargo test`.
    out.push_str("\n[dev-dependencies]\n");
    out.push_str("proptest = \"1\"\n");

    // Empty [workspace] keeps the crate out of any parent workspace.
    out.push_str("\n[workspace]\n");

    out
}

/// Emit `src/lib.rs` — the `#[program]` mod with one `pub fn` per handler
/// dispatching to `ctx.accounts.handler(...)`. No-op if `src/lib.rs`
/// already exists (user-owned: stamped imports / extra modules survive
/// regeneration). Falls back to `parsed` for `program_id`, `type_aliases`
/// (Quasar Fin params), per-handler bumps/params/accounts, and the Anchor
/// `#[derive(Accounts)]` emission (`render_handler_accounts_struct`).
/// #253: a skipped user-owned file goes silently stale after spec-level
/// renames — the regenerated siblings (state.rs, guards.rs) pick up the
/// new names while the skipped scaffold keeps the old ones, and the crate
/// stops compiling with nothing pointing at the cause. Detect it at the
/// skip site: generation embedded `spec_hash = "…"` stamps; any stamp not
/// matching a current handler's spec_hash means the file predates the
/// current spec revision.
fn embedded_stamps_stale(existing: &str, current_hashes: &[String]) -> bool {
    let pat = "spec_hash = \"";
    let mut rest = existing;
    while let Some(i) = rest.find(pat) {
        rest = &rest[i + pat.len()..];
        let Some(end) = rest.find('"') else { break };
        if !current_hashes.iter().any(|c| c == &rest[..end]) {
            return true;
        }
        rest = &rest[end..];
    }
    // A file with no stamps (fully hand-rewritten) has nothing to compare.
    false
}

fn warn_stale_skip(what: &str, drift_root: &Path) {
    eprintln!(
        "WARNING: {what} was generated from a DIFFERENT spec revision — its #[qed] \
         spec_hash stamps don't match the current spec. After a spec-level rename \
         the regenerated files (state.rs, guards.rs, harnesses) disagree with it \
         and the crate may not compile. Recover with `qedgen codegen --merge-accounts` \
         (Anchor: regenerates only the #[derive(Accounts)] structs, keeping handler \
         fills) or `qedgen codegen --force` (regenerates the user-owned set wholesale; \
         re-apply fills from git history), or update it by hand; \
         `qedgen check --drift {} ` lists per-handler drift.",
        drift_root.display()
    );
}

/// #288: gate for the destructive regen modes. Every file they would
/// overwrite must be recoverable from git — tracked AND unmodified —
/// otherwise the overwrite silently destroys handler fills with no way
/// back. `git status --porcelain` reports exactly the unrecoverable set
/// (untracked `??`, or any staged/unstaged modification).
fn assert_git_recoverable(output_dir: &Path, files: &[std::path::PathBuf]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let out = std::process::Command::new("git")
        .arg("status")
        .arg("--porcelain")
        .arg("--untracked-files=all")
        .arg("--")
        .args(files)
        .current_dir(output_dir)
        .output();
    let out = match out {
        Ok(out) if out.status.success() => out,
        _ => anyhow::bail!(
            "--force/--merge-accounts need git history as the recovery path, but \
             `git status` failed under {} — is this a git repository? \
             (qedgen is git-native; run `git init` + commit first.)",
            output_dir.display()
        ),
    };
    // `git status` deliberately hides ignored paths. An ignored, untracked
    // user-owned file would therefore produce an empty porcelain report and
    // be overwritten even though git has no baseline to recover. Prove each
    // target is tracked independently before treating a clean status as safe.
    let untracked: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let tracked = std::process::Command::new("git")
                .arg("ls-files")
                .arg("--error-unmatch")
                .arg("--")
                .arg(path)
                .current_dir(output_dir)
                .output()
                .is_ok_and(|out| out.status.success());
            (!tracked).then(|| path.display().to_string())
        })
        .collect();
    if !untracked.is_empty() {
        anyhow::bail!(
            "refusing to overwrite user-owned files that are not tracked by git — \
             ignored and untracked files have no recoverable baseline:\n  {}\n\
             Track and commit them first (`git add -f <file> && git commit`), then re-run.",
            untracked.join("\n  ")
        );
    }
    let dirty: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if !dirty.is_empty() {
        anyhow::bail!(
            "refusing to overwrite user-owned files that have no committed, unmodified \
             git baseline — the overwrite would be unrecoverable:\n  {}\n\
             Commit or stash them first (`git add -A && git commit`), then re-run.",
            dirty.join("\n  ")
        );
    }
    Ok(())
}

fn emit_lib(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
    spec_path: &Path,
    opts: RegenOptions,
) -> Result<()> {
    use crate::codegen_shared::{to_pascal_case, FrameworkSurface};

    // Pinocchio: dedicated helper emits the no_std entrypoint +
    // byte-dispatch from ParsedSpec.
    if matches!(target, Target::Pinocchio) {
        return crate::codegen_shared::emit_pinocchio_program_lib(
            parsed, fp, output_dir, opts.force,
        );
    }

    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let lib_path = src_dir.join("lib.rs");
    if lib_path.exists() && opts.force {
        // #288: recoverability was asserted up front in `generate`.
        eprintln!(
            "regenerating user-owned {} (--force) — previous version is in git history; \
             re-apply handler fills from there.",
            lib_path.display()
        );
    } else if lib_path.exists() {
        // #288: surgical rename recovery — regenerate only the
        // `#[derive(Accounts)]` structs, keep everything else.
        if opts.merge_accounts && matches!(target, Target::Anchor) {
            return merge_accounts_into_lib(mir, parsed, &lib_path);
        }
        eprintln!(
            "programs/{}/src/lib.rs already exists — skipping (user-owned). guards.rs regenerated.",
            output_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<program>")
        );
        // #253: escalate when the skipped file predates the current spec.
        let spec_src = crate::check::read_spec_source(spec_path).unwrap_or_default();
        let current: Vec<String> = parsed
            .handlers
            .iter()
            .filter_map(|h| crate::spec_hash::spec_hash_for_handler(&spec_src, &h.name))
            .collect();
        if let Ok(existing) = std::fs::read_to_string(&lib_path) {
            if embedded_stamps_stale(&existing, &current) {
                warn_stale_skip(&format!("{}", lib_path.display()), output_dir);
            }
        }
        return Ok(());
    }

    let program_name = mir.name.to_lowercase();
    let program_id = parsed
        .program_id
        .as_deref()
        .unwrap_or("11111111111111111111111111111111");

    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/lib.rs",
    ));
    out.push_str(surface.crate_attrs);
    out.push_str(surface.prelude_import);
    out.push('\n');
    out.push_str("mod instructions;\n");
    if matches!(target, Target::Quasar) {
        out.push_str("use instructions::*;\n");
    }

    if !mir.events.is_empty() {
        out.push_str("pub mod events;\n");
    }
    if !mir.errors.variants.is_empty() {
        out.push_str("pub mod errors;\n");
    }
    out.push_str("pub mod state;\n");
    out.push_str("pub mod guards;\n");
    if matches!(target, Target::Pinocchio) {
        out.push_str("#[cfg(kani)]\n");
        out.push_str("extern crate kani;\n");
        out.push_str("#[cfg(kani)]\n");
        out.push_str("mod kani_impl;\n");
    }
    if crate::codegen_shared::guards_use_math_helpers(parsed) {
        out.push_str("pub mod math;\n");
    }
    if !mir.ref_impls.is_empty() {
        out.push_str("pub mod ref_impls;\n");
    }
    if mir
        .imports
        .values()
        .any(|imp| !imp.account_types.is_empty())
    {
        out.push_str("pub mod imported;\n");
    }
    out.push('\n');

    out.push_str(&format!("declare_id!(\"{}\");\n\n", program_id));

    out.push_str("#[program]\n");
    out.push_str(&format!(
        "{} {} {{\n",
        surface.program_mod_vis, program_name
    ));
    out.push_str("    use super::*;\n\n");

    // Iterate `mir.handlers`; the matching `ParsedHandler` supplies
    // bumps / params / Fin-resolution details.
    for (i, handler) in mir.handlers.iter().enumerate() {
        let parsed_handler = parsed
            .handlers
            .iter()
            .find(|h| h.name == handler.name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MIR handler '{}' has no matching ParsedHandler (parser/lowering mismatch)",
                    handler.name
                )
            })?;
        let pascal = to_pascal_case(&handler.name);

        if let Some(ref doc) = handler.doc {
            out.push_str(&format!("    /// {}\n", doc));
        }
        if surface.explicit_handler_discriminator {
            out.push_str(&format!("    #[instruction(discriminator = {})]\n", i));
        }

        let mut params = format!("ctx: {}<{}>", surface.context_type, pascal);

        let needs_fin_cast = |ptype: &str| -> bool {
            if !matches!(target, Target::Quasar) {
                return false;
            }
            let mut resolved = ptype.trim().to_string();
            while let Some((_, rhs)) = parsed.type_aliases.iter().find(|(n, _)| n == &resolved) {
                resolved = rhs.trim().to_string();
            }
            resolved.starts_with("Fin")
        };

        for (pname, ptype) in &parsed_handler.takes_params {
            let rust_ty = if needs_fin_cast(ptype) {
                "u32".to_string()
            } else {
                crate::codegen_shared::map_type_for_target(ptype, parsed, target)?
            };
            params.push_str(&format!(", {}: {}", pname, rust_ty));
        }

        out.push_str(&format!(
            "    pub fn {}({}) -> {} {{\n",
            handler.name, params, surface.handler_result_type
        ));

        let cast_arg = |pname: &str, ptype: &str| -> String {
            if needs_fin_cast(ptype) {
                format!("{} as usize", pname)
            } else {
                pname.to_string()
            }
        };

        if parsed_handler.has_bumps() {
            out.push_str(&format!(
                "        ctx.accounts.handler({}&ctx.bumps)\n",
                parsed_handler
                    .takes_params
                    .iter()
                    .map(|(n, t)| format!("{}, ", cast_arg(n, t)))
                    .collect::<String>()
            ));
        } else {
            out.push_str(&format!(
                "        ctx.accounts.handler({})\n",
                parsed_handler
                    .takes_params
                    .iter()
                    .map(|(n, t)| cast_arg(n, t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push_str("    }\n\n");
    }

    out.push_str("}\n");

    // Anchor: `#[derive(Accounts)]` structs at crate root
    // (`render_handler_accounts_struct` consumes ParsedSpec directly).
    if matches!(target, Target::Anchor) {
        let is_multi = parsed.account_types.len() > 1;
        let default_state_name = format!("{}Account", to_pascal_case(&mir.name));
        out.push('\n');
        out.push_str("// `#[derive(Accounts)]` structs live at the crate root so the\n");
        out.push_str("// Anchor `#[program]` macro can resolve them via `crate::*`.\n");
        out.push_str("// The handler impl blocks live next to the (always-regenerated)\n");
        out.push_str("// guard module in `instructions/<name>.rs`.\n");
        out.push_str("use crate::state::*;\n");
        let has_token = parsed.handlers.iter().any(|h| {
            h.accounts
                .iter()
                .any(|a| a.account_type.as_deref() == Some("token") || a.name == "token_program")
        });
        let has_mint = parsed.handlers.iter().any(|h| {
            h.accounts
                .iter()
                .any(|a| a.account_type.as_deref() == Some("mint"))
        });
        let imports = surface.token_imports(has_token, has_mint);
        if !imports.is_empty() {
            out.push_str(&imports);
        }
        // Render structs first to detect which Anchor wrapper types they
        // reference.
        let mut structs = String::new();
        for handler in &parsed.handlers {
            structs.push('\n');
            structs.push_str(&crate::codegen_shared::render_handler_accounts_struct(
                handler,
                parsed,
                is_multi,
                &default_state_name,
                &surface,
                target,
            ));
        }
        // A user state type (e.g. `type Account = { … }`) glob-imported
        // alongside `anchor_lang::prelude::*` makes the same-named wrapper
        // ambiguous (hard error under deny-by-default
        // `ambiguous_glob_imports`). An explicit `use` outranks globs, so
        // re-import the colliding wrapper(s); scoped to actual collisions.
        const ANCHOR_WRAPPERS: &[&str] = &[
            "Account",
            "Signer",
            "Program",
            "SystemAccount",
            "UncheckedAccount",
            "InterfaceAccount",
            "Interface",
            "Sysvar",
            "AccountLoader",
        ];
        let user_type_names: std::collections::HashSet<&str> = parsed
            .records
            .iter()
            .map(|r| r.name.as_str())
            .chain(parsed.account_types.iter().map(|a| a.name.as_str()))
            .collect();
        let collisions: Vec<&str> = ANCHOR_WRAPPERS
            .iter()
            .copied()
            .filter(|w| user_type_names.contains(*w) && structs.contains(&format!(": {w}<")))
            .collect();
        if !collisions.is_empty() {
            // Single item: no braces (`use a::B;`), matching rustfmt.
            let path = if collisions.len() == 1 {
                collisions[0].to_string()
            } else {
                format!("{{{}}}", collisions.join(", "))
            };
            out.push_str(&format!(
                "// Explicit re-imports: these Anchor wrapper names collide with\n\
                 // same-named `crate::state` types declared in the spec; the\n\
                 // explicit `use` outranks the globs so the wrapper wins.\n\
                 use anchor_lang::prelude::{path};\n"
            ));
        }
        out.push_str(&structs);
    }

    out.push_str("// ---- END GENERATED ----\n");

    crate::codegen_shared::write_generated_file(&src_dir.join("lib.rs"), &out)?;
    Ok(())
}

/// Where a handler's `#[derive(Accounts)]` struct sits inside a
/// user-owned `lib.rs` — or why it can't be merged.
enum StructLocation {
    /// Byte range covering the struct's leading attribute/doc lines
    /// through its closing `}` (inclusive end).
    Found(std::ops::Range<usize>),
    /// A same-named struct exists but doesn't derive `Accounts` —
    /// replacing it would clobber an unrelated user type, and appending
    /// a duplicate name wouldn't compile. Left untouched.
    Foreign,
    Missing,
}

/// Locate `pub struct <name>` in `text` together with its contiguous
/// leading attribute/doc-comment lines. Only a block whose leading
/// attributes contain `#[derive(Accounts…` qualifies as replaceable.
fn locate_accounts_struct(text: &str, name: &str) -> StructLocation {
    let bytes = text.as_bytes();
    let needle = format!("pub struct {name}");
    let mut search_from = 0usize;
    let mut saw_foreign = false;
    while let Some(rel) = text[search_from..].find(&needle) {
        let at = search_from + rel;
        search_from = at + needle.len();
        // Name boundary: reject prefix matches (`Pause` inside `Pause2`).
        match bytes.get(at + needle.len()) {
            Some(b) if b.is_ascii_alphanumeric() || *b == b'_' => continue,
            _ => {}
        }
        // The declaration must start its line (filters `// pub struct …`
        // comment mentions).
        let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if text[line_start..at].trim() != "" {
            continue;
        }
        let Some(open_rel) = text[at..].find('{') else {
            continue;
        };
        let Some(close) = qedgen_hash_core::scan_balanced_block(bytes, at + open_rel) else {
            continue;
        };
        // Walk back over contiguous attribute lines to include
        // `#[derive(Accounts)]`. Deliberately stop at comments: comments are
        // user-owned content and must survive a surgical struct replacement.
        // A comment between the derive and declaration makes the struct
        // ineligible for textual merging rather than risking deletion.
        let mut region_start = line_start;
        // `prev_end` is the index of the '\n' terminating the previous line.
        while let Some(prev_end) = region_start.checked_sub(1) {
            let prev_start = text[..prev_end].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let prev = text[prev_start..prev_end].trim();
            if prev.starts_with("#[") {
                region_start = prev_start;
            } else {
                break;
            }
        }
        if text[region_start..line_start].contains("#[derive(Accounts") {
            return StructLocation::Found(region_start..close + 1);
        }
        saw_foreign = true;
    }
    if saw_foreign {
        StructLocation::Foreign
    } else {
        StructLocation::Missing
    }
}

/// #288 option 3 — surgical rename recovery for a user-owned Anchor
/// `lib.rs`: regenerate every current handler's `#[derive(Accounts)]`
/// struct in place (the Cargo.toml section-merge doctrine applied to
/// Rust items), preserving the `#[program]` mod, handler fills, imports,
/// and anything else the user wrote. Structs with no matching handler
/// (pre-rename leftovers, hand-added instructions) are left in place and
/// reported. The caller has already asserted a clean git baseline.
fn merge_accounts_into_lib(mir: &Mir, parsed: &ParsedSpec, lib_path: &Path) -> Result<()> {
    use crate::codegen_shared::{to_pascal_case, FrameworkSurface};

    let existing = std::fs::read_to_string(lib_path)?;
    let surface = FrameworkSurface::for_target(Target::Anchor);
    let is_multi = parsed.account_types.len() > 1;
    let default_state_name = format!("{}Account", to_pascal_case(&mir.name));

    let fresh: Vec<(String, String)> = parsed
        .handlers
        .iter()
        .map(|h| {
            (
                to_pascal_case(&h.name),
                crate::codegen_shared::render_handler_accounts_struct(
                    h,
                    parsed,
                    is_multi,
                    &default_state_name,
                    &surface,
                    Target::Anchor,
                ),
            )
        })
        .collect();

    let mut merged = existing.clone();
    let mut replaced: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();
    let mut foreign: Vec<&str> = Vec::new();
    for (name, render) in &fresh {
        match locate_accounts_struct(&merged, name) {
            StructLocation::Found(range) => {
                merged.replace_range(range, render.trim_end());
                replaced.push(name);
            }
            StructLocation::Foreign => foreign.push(name),
            StructLocation::Missing => {
                // New (or renamed-to) handler: append before the trailing
                // END marker when present, else at EOF.
                let insertion = format!("\n{}", render);
                match merged.rfind("// ---- END GENERATED ----") {
                    Some(marker) => merged.insert_str(marker, &insertion),
                    None => {
                        if !merged.ends_with('\n') {
                            merged.push('\n');
                        }
                        merged.push_str(&insertion);
                    }
                }
                added.push(name);
            }
        }
    }

    // Orphans: derive(Accounts) structs with no current handler — a
    // renamed handler's old struct, or a hand-added instruction's. Never
    // deleted (the latter is legitimate user code); reported so rename
    // leftovers get cleaned up by hand.
    let current: std::collections::HashSet<&str> = fresh.iter().map(|(n, _)| n.as_str()).collect();
    let mut orphans: Vec<String> = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = merged[from..].find("#[derive(Accounts") {
        let at = from + rel;
        from = at + 1;
        let window_end = (at + 400).min(merged.len());
        if let Some(srel) = merged[at..window_end].find("pub struct ") {
            let name_start = at + srel + "pub struct ".len();
            let name: String = merged[name_start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && !current.contains(name.as_str()) {
                orphans.push(name);
            }
        }
    }

    if merged != existing {
        crate::codegen_shared::write_generated_file(lib_path, &merged)?;
    }
    eprintln!(
        "merged #[derive(Accounts)] structs into user-owned {} (--merge-accounts): \
         {} replaced, {} added; handler fills untouched.",
        lib_path.display(),
        replaced.len(),
        added.len()
    );
    if !foreign.is_empty() {
        eprintln!(
            "warning: not merged (same-named struct without #[derive(Accounts)]): {}",
            foreign.join(", ")
        );
    }
    if !orphans.is_empty() {
        eprintln!(
            "note: structs with no matching spec handler were left in place \
             (delete by hand if they belonged to a renamed handler): {}",
            orphans.join(", ")
        );
    }
    Ok(())
}

/// Emit `src/instructions/mod.rs` + per-handler `<name>.rs` scaffolds.
/// Per-handler files are USER-OWNED — emitted only when missing; mod.rs
/// is always regenerated. Scaffold bodies render from the matching
/// `ParsedHandler`.
fn emit_instructions(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    spec_path: &Path,
    output_dir: &Path,
    target: Target,
    force: bool,
) -> Result<()> {
    use crate::codegen_shared::to_pascal_case;

    let instr_dir = output_dir.join("src").join("instructions");
    std::fs::create_dir_all(&instr_dir)?;

    let is_multi = parsed.account_types.len() > 1;
    let default_state_name = format!("{}Account", to_pascal_case(&mir.name));

    // mod.rs — always regenerated, pure scaffold.
    let mut mod_out = String::new();
    mod_out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/instructions/mod.rs",
    ));
    for handler in &mir.handlers {
        mod_out.push_str(&format!("pub mod {};\n", handler.name));
    }
    // Quasar + Pinocchio re-export their account structs from each
    // `instructions/<name>.rs` (Pinocchio's `guards.rs` resolves `<Pascal>`
    // via `use crate::instructions::*;`); Anchor keeps them in lib.rs at
    // crate root.
    if matches!(target, Target::Quasar | Target::Pinocchio) {
        mod_out.push('\n');
        for handler in &mir.handlers {
            let pascal = to_pascal_case(&handler.name);
            mod_out.push_str(&format!("pub use {}::{};\n", handler.name, pascal));
        }
    }
    mod_out.push_str("// ---- END GENERATED ----\n");
    crate::codegen_shared::write_generated_file(&instr_dir.join("mod.rs"), &mod_out)?;

    // Spec source for spec_hash attributes (single- and multi-file specs).
    let spec_src = crate::check::read_spec_source(spec_path).unwrap_or_default();
    let spec_attr = crate::codegen_shared::relative_spec_path(spec_path, output_dir);

    // Per-handler scaffold files (user-owned — skipped if existing).
    for handler_mir in &mir.handlers {
        let handler = parsed
            .handlers
            .iter()
            .find(|h| h.name == handler_mir.name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MIR handler '{}' has no matching ParsedHandler",
                    handler_mir.name
                )
            })?;

        let handler_path = instr_dir.join(format!("{}.rs", handler.name));
        if handler_path.exists() && force {
            // #288: recoverability was asserted up front in `generate`.
            eprintln!(
                "regenerating user-owned {} (--force) — previous version is in git history; \
                 re-apply the handler fill from there.",
                handler_path.display()
            );
        } else if handler_path.exists() {
            eprintln!(
                "programs/{}/src/instructions/{}.rs already exists — skipping (user-owned). guards.rs regenerated.",
                output_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<program>"),
                handler.name
            );
            // #253: escalate when the skipped file predates the current spec.
            let current: Vec<String> =
                crate::spec_hash::spec_hash_for_handler(&spec_src, &handler.name)
                    .into_iter()
                    .collect();
            if let Ok(existing) = std::fs::read_to_string(&handler_path) {
                if embedded_stamps_stale(&existing, &current) {
                    warn_stale_skip(&format!("{}", handler_path.display()), output_dir);
                }
            }
            continue;
        }

        // Pinocchio uses a dedicated scaffold (struct of &AccountInfo +
        // process_<name> wrapper); the Context-based scaffold doesn't apply.
        let out = if matches!(target, Target::Pinocchio) {
            crate::codegen_shared::render_pinocchio_handler_scaffold(handler, parsed)?
        } else {
            crate::codegen_shared::render_handler_scaffold(
                handler,
                parsed,
                is_multi,
                &default_state_name,
                &spec_src,
                &spec_attr,
                target,
            )?
        };
        crate::codegen_shared::write_generated_file(&handler_path, &out)?;
    }

    Ok(())
}

/// Emit `src/state.rs` — `#[account]` structs for persisted state.
/// Dispatches three shapes:
///   1. **Multi-account**: one `<Name>Account` struct per account_type,
///      with optional `<Name>Status` enum.
///   2. **Multi-variant ADT (Anchor only)**: wrapper-struct + inner-enum
///      pair, with accessors for fields shared across variants.
///   3. **Flat single-account**: `<Name>Account` from `state_fields` with
///      optional bump / status fields + lifecycle `Status` enum.
fn emit_state(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    use crate::codegen_shared::{
        is_multi_variant_adt_state, map_type_for_target, map_type_pod, FrameworkSurface,
    };

    // Pinocchio: zeropod zero-copy state via the dedicated helper.
    if matches!(target, Target::Pinocchio) {
        let src_dir = output_dir.join("src");
        std::fs::create_dir_all(&src_dir)?;
        let mut out = String::new();
        crate::codegen_shared::emit_pinocchio_state(parsed, fp, &mut out)?;
        crate::codegen_shared::write_generated_file(&src_dir.join("state.rs"), &out)?;
        return Ok(());
    }

    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let is_multi = mir.account_states.len() > 1;

    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/state.rs",
    ));
    out.push_str(surface.prelude_import);
    out.push('\n');

    // Records first. Anchor needs Borsh + InitSpace for the outer struct's
    // space calculation; Quasar needs Pod-companion types for zero-copy
    // alignment.
    for record in &parsed.records {
        out.push_str("#[repr(C)]\n");
        let derives = match target {
            Target::Anchor => "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq)]\n",
            _ => "#[derive(Clone, Copy)]\n",
        };
        out.push_str(derives);
        out.push_str(&format!("pub struct {} {{\n", record.name));
        for (fname, ftype) in &record.fields {
            let rust_ty = match target {
                Target::Quasar => map_type_pod(ftype, parsed)?,
                _ => map_type_for_target(ftype, parsed, target)?,
            };
            out.push_str(&format!("    pub {}: {},\n", fname, rust_ty));
        }
        out.push_str("}\n\n");
    }

    if is_multi {
        // pda_ref lives on ParsedAccountType — look up by name.
        for (idx, acct_mir) in mir.account_states.iter().enumerate() {
            let acct = parsed
                .account_types
                .iter()
                .find(|a| a.name == acct_mir.name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "MIR account_state '{}' has no matching ParsedAccountType",
                        acct_mir.name
                    )
                })?;
            let struct_name = format!("{}Account", acct.name);

            let account_attr = if surface.explicit_account_discriminator {
                format!("#[account(discriminator = {})]\n", idx + 1)
            } else {
                "#[account]\n".to_string()
            };
            out.push_str(&account_attr);
            if matches!(target, Target::Anchor) {
                out.push_str("#[derive(InitSpace)]\n");
            }
            out.push_str(&format!("pub struct {} {{\n", struct_name));

            for (fname, ftype) in &acct.fields {
                out.push_str(&format!(
                    "    pub {}: {},\n",
                    fname,
                    map_type_for_target(ftype, parsed, target)?
                ));
            }

            if acct.pda_ref.is_some() && !acct.fields.iter().any(|(n, _)| n == "bump") {
                out.push_str("    pub bump: u8,\n");
            }

            if !acct.lifecycle.is_empty() && !acct.fields.iter().any(|(n, _)| n == "status") {
                out.push_str("    pub status: u8,\n");
            }

            out.push_str("}\n\n");

            if !acct.lifecycle.is_empty() {
                out.push_str(&format!("/// {} lifecycle states.\n", acct.name));
                out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
                out.push_str("#[repr(u8)]\n");
                out.push_str(&format!("pub enum {}Status {{\n", acct.name));
                for (i, state) in acct.lifecycle.iter().enumerate() {
                    out.push_str(&format!("    {} = {},\n", state, i));
                }
                out.push_str("}\n\n");
            }
        }
    } else if is_multi_variant_adt_state(parsed) && matches!(target, Target::Anchor) {
        // Multi-variant ADT: wrapper struct + inner enum + accessors.
        // Shared derivation with the `space =` reference — see
        // `state_struct_name`.
        let state_name = crate::codegen_shared::state_struct_name(parsed, None);
        let inner_name = format!("{}Inner", state_name);
        let acct = &parsed.account_types[0];

        out.push_str("#[account]\n");
        out.push_str("#[derive(InitSpace)]\n");
        out.push_str(&format!("pub struct {} {{\n", state_name));
        out.push_str(&format!("    pub inner: {},\n", inner_name));
        if !parsed.pdas.is_empty() && !parsed.state_fields.iter().any(|(n, _)| n == "bump") {
            out.push_str("    pub bump: u8,\n");
        }
        out.push_str("}\n\n");

        crate::codegen_shared::render_adt_inner_enum(
            &mut out,
            acct,
            &inner_name,
            &format!(
                "/// Variant-payload state for {0}. The Anchor wrapper above\n\
                 /// carries the account discriminator; this enum carries the\n\
                 /// state-machine variant + per-variant payload fields.\n",
                state_name
            ),
            &|fname| {
                format!(
                    "    /// v2.29 Slice B accessor for `{0}`. Panics on variants\n\
                     /// that don't carry the field — guarded against by the\n\
                     /// per-handler lifecycle check that fires before any\n\
                     /// `requires` emission in `crate::guards`.\n",
                    fname
                )
            },
            parsed,
            target,
            /* blank_after_impl */ false,
        )?;
    } else {
        // Flat single-account fallback. Shared derivation with the
        // `space =` reference — see `state_struct_name`.
        let state_name = crate::codegen_shared::state_struct_name(parsed, None);

        let account_attr = if surface.explicit_account_discriminator {
            "#[account(discriminator = 1)]\n"
        } else {
            "#[account]\n"
        };
        out.push_str(account_attr);
        // `InitSpace` on every Anchor account struct, matching the
        // multi-account and ADT branches: an `init` account renders
        // `space = 8 + <T>::INIT_SPACE`, which does not resolve without
        // the derive (E0599, scaffold did not compile).
        if matches!(target, Target::Anchor) {
            out.push_str("#[derive(InitSpace)]\n");
        }
        out.push_str(&format!("pub struct {} {{\n", state_name));

        for (fname, ftype) in &parsed.state_fields {
            out.push_str(&format!(
                "    pub {}: {},\n",
                fname,
                map_type_for_target(ftype, parsed, target)?
            ));
        }

        if !parsed.pdas.is_empty() && !parsed.state_fields.iter().any(|(n, _)| n == "bump") {
            out.push_str("    pub bump: u8,\n");
        }

        if !parsed.lifecycle_states.is_empty()
            && !parsed.state_fields.iter().any(|(n, _)| n == "status")
        {
            out.push_str("    pub status: u8,\n");
        }

        out.push_str("}\n");

        if !parsed.lifecycle_states.is_empty() {
            out.push_str("\n/// Program lifecycle states.\n");
            out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
            out.push_str("#[repr(u8)]\n");
            out.push_str("pub enum Status {\n");
            for (i, state) in parsed.lifecycle_states.iter().enumerate() {
                out.push_str(&format!("    {} = {},\n", state, i));
            }
            out.push_str("}\n");
        }
    }

    out.push_str("// ---- END GENERATED ----\n");

    crate::codegen_shared::write_generated_file(&src_dir.join("state.rs"), &out)?;
    Ok(())
}

/// Emit `src/imported/<ns>.rs` mirror files + `src/imported/mod.rs`
/// re-export aggregator. Iterates `mir.imports` (BTreeMap — deterministic
/// order). `Inline` origins have no source artifact and never produce a
/// mirror; Tier-0 stubs (bundled SPL/System/Metaplex) have empty
/// `account_types` and are skipped entirely.
fn emit_imported_mirror(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    if !mir
        .imports
        .values()
        .any(|imp| !imp.account_types.is_empty())
    {
        return Ok(());
    }

    let (prelude_import, explicit_account_discriminator): (&str, bool) = match target {
        Target::Anchor => ("use anchor_lang::prelude::*;\n", false),
        Target::Quasar => ("use quasar_lang::prelude::*;\n", true),
        // Pinocchio mirrors need a zeropod decode shape that isn't emitted
        // yet; fail cleanly.
        Target::Pinocchio => anyhow::bail!(
            "imported account-type mirrors are not yet supported for the \
             Pinocchio target. Inline the interface's account types into the \
             spec, or generate this program for the Anchor or Quasar target."
        ),
    };

    let src_dir = output_dir.join("src");
    let imported_dir = src_dir.join("imported");
    std::fs::create_dir_all(&imported_dir)?;

    for (local_name, imp) in &mir.imports {
        if imp.account_types.is_empty() {
            continue;
        }
        let dep_key = match &imp.origin {
            crate::mir::ImportOrigin::Builtin(k) | crate::mir::ImportOrigin::File(k) => k.clone(),
            crate::mir::ImportOrigin::Inline => {
                // No source artifact; already gated by the empty
                // account_types check above. Skip defensively.
                continue;
            }
        };

        let mut out = String::new();
        let file_rel = format!("src/imported/{}.rs", local_name);
        out.push_str(&crate::codegen_shared::marker("DO NOT EDIT", fp, &file_rel));
        out.push_str(&format!(
            "//! v2.29 Slice H mirror of `{0}`'s account types\n\
             //! (sourced from dep `{1}`).\n\
             //!\n\
             //! Hand-editing is unsafe: every `qedgen codegen` regenerates\n\
             //! this file from the imported `.qedspec`'s `type` declarations.\n\
             //! To change a field, change the imported spec and re-resolve.\n\n",
            local_name, dep_key,
        ));
        out.push_str(prelude_import);
        out.push('\n');

        // Records — declared first so account_types can reference them.
        for record in &imp.records {
            out.push_str("#[repr(C)]\n");
            let derives = match target {
                Target::Anchor => "#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq)]\n",
                _ => "#[derive(Clone, Copy)]\n",
            };
            out.push_str(derives);
            out.push_str(&format!("pub struct {} {{\n", record.name));
            for (fname, ftype) in &record.fields {
                let rust_ty = crate::codegen_shared::map_type_for_target(ftype, parsed, target)?;
                out.push_str(&format!("    pub {}: {},\n", fname, rust_ty));
            }
            out.push_str("}\n\n");
        }

        // Account types — flat struct or multi-variant wrapper+inner enum,
        // mirroring `emit_state`'s dispatch shape.
        for (idx, acct) in imp.account_types.iter().enumerate() {
            let is_multi_variant = acct.variants.len() > 1;
            let account_attr = if explicit_account_discriminator {
                format!("#[account(discriminator = {})]\n", idx + 1)
            } else {
                "#[account]\n".to_string()
            };

            if !is_multi_variant {
                out.push_str(&format!("{}pub struct {} {{\n", account_attr, acct.name));
                for (fname, ftype) in &acct.fields {
                    let rust_ty =
                        crate::codegen_shared::map_type_for_target(ftype, parsed, target)?;
                    out.push_str(&format!("    pub {}: {},\n", fname, rust_ty));
                }
                if !acct.lifecycle.is_empty() && !acct.fields.iter().any(|(n, _)| n == "status") {
                    out.push_str("    pub status: u8,\n");
                }
                out.push_str("}\n\n");

                if !acct.lifecycle.is_empty() {
                    out.push_str(&format!(
                        "/// {} lifecycle states (mirrored from `{}`).\n",
                        acct.name, dep_key
                    ));
                    out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
                    out.push_str("#[repr(u8)]\n");
                    out.push_str(&format!("pub enum {}Status {{\n", acct.name));
                    for (i, state) in acct.lifecycle.iter().enumerate() {
                        out.push_str(&format!("    {} = {},\n", state, i));
                    }
                    out.push_str("}\n\n");
                }
                continue;
            }

            // Multi-variant ADT: wrapper struct + inner enum.
            let inner_name = format!("{}Inner", acct.name);
            out.push_str(&format!("{}pub struct {} {{\n", account_attr, acct.name));
            out.push_str(&format!("    pub inner: {},\n", inner_name));
            out.push_str("}\n\n");

            crate::codegen_shared::render_adt_inner_enum(
                &mut out,
                acct,
                &inner_name,
                &format!(
                    "/// Variant-payload state for `{0}` (mirrored from `{1}`).\n",
                    acct.name, dep_key
                ),
                &|fname| {
                    format!(
                        "    /// v2.29 Slice H accessor for `{0}`. Panics on variants\n\
                         /// that don't carry the field — the per-handler lifecycle\n\
                         /// check at the top of each `crate::guards::*` fn prevents\n\
                         /// the panic arm from being reached at runtime.\n",
                        fname
                    )
                },
                parsed,
                target,
                /* blank_after_impl */ true,
            )?;
        }

        out.push_str("// ---- END GENERATED ----\n");
        crate::codegen_shared::write_generated_file(
            &imported_dir.join(format!("{}.rs", local_name)),
            &out,
        )?;
    }

    // mod.rs re-export aggregator.
    let mut mod_out = String::new();
    mod_out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/imported/mod.rs",
    ));
    mod_out.push_str("//! v2.29 Slice H — re-exports for imported namespace mirrors.\n\n");
    mod_out.push_str("#![allow(non_snake_case)]\n\n");
    for (local_name, imp) in &mir.imports {
        if imp.account_types.is_empty() {
            continue;
        }
        mod_out.push_str(&format!("pub mod {};\n", local_name));
    }
    mod_out.push_str("\n// ---- END GENERATED ----\n");
    crate::codegen_shared::write_generated_file(&imported_dir.join("mod.rs"), &mod_out)?;

    Ok(())
}

/// Emit `src/errors.rs` — `#[error_code] pub enum <Name>Error`. The
/// `needs_lifecycle` / `needs_invalid_pda` augmentation predicates walk
/// `parsed` compound shapes with no direct MIR equivalent yet.
fn emit_errors(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    if mir.errors.variants.is_empty() {
        return Ok(());
    }
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let prelude_import = match target {
        Target::Anchor => "use anchor_lang::prelude::*;\n",
        Target::Quasar => "use quasar_lang::prelude::*;\n",
        // Pinocchio has no `#[error_code]` macro — plain enum + a
        // hand-written `From<…> for ProgramError` (emitted below).
        Target::Pinocchio => "use pinocchio::program_error::ProgramError;\n",
    };

    let error_name = format!("{}Error", crate::codegen_shared::to_pascal_case(&mir.name));

    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/errors.rs",
    ));
    out.push_str(prelude_import);
    out.push('\n');

    // R26: a non-init lifecycle pre-status auto-adds `InvalidLifecycle`.
    let needs_lifecycle = parsed.handlers.iter().any(|h| {
        let pre = h.pre_status.as_deref().unwrap_or("");
        let is_init = matches!(pre, "Uninitialized" | "Empty");
        !pre.is_empty() && !is_init
    });

    // R28: runtime PDA verification auto-adds `InvalidPda`. Both this error
    // declaration and guard emission consume the account plan's SeedPlan, so
    // the variant cannot drift from the generated check.
    let needs_invalid_pda = !matches!(target, Target::Pinocchio)
        && parsed.handlers.iter().any(|h| {
            let state_acct = crate::codegen_shared::resolve_handler_state_account(h, parsed);
            h.accounts.iter().any(|acct| {
                let is_state = state_acct.map(|sa| sa.name == acct.name).unwrap_or(false);
                let plan =
                    crate::codegen_shared::AccountPlan::derive(acct, h, target, parsed, is_state);
                matches!(plan.seeds, crate::codegen_shared::SeedPlan::Runtime)
            })
        });

    let mut codes: Vec<String> = mir.errors.variants.clone();
    if needs_lifecycle && !codes.iter().any(|c| c == "InvalidLifecycle") {
        codes.push("InvalidLifecycle".to_string());
    }
    if needs_invalid_pda && !codes.iter().any(|c| c == "InvalidPda") {
        codes.push("InvalidPda".to_string());
    }

    if matches!(target, Target::Pinocchio) {
        // Pinocchio: plain `#[repr(u32)]` enum + `From<…> for ProgramError`
        // (guards/handlers convert via `ProgramError::from(<Enum>::<V>)`).
        out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n#[repr(u32)]\n");
        out.push_str(&format!("pub enum {} {{\n", error_name));
        for (i, code) in codes.iter().enumerate() {
            out.push_str(&format!("    {} = {},\n", code, i));
        }
        out.push_str("}\n\n");
        out.push_str(&format!(
            "impl From<{0}> for ProgramError {{\n    fn from(e: {0}) -> Self {{\n        ProgramError::Custom(e as u32)\n    }}\n}}\n",
            error_name
        ));
    } else {
        out.push_str("#[error_code]\n");
        out.push_str(&format!("pub enum {} {{\n", error_name));
        for (i, code) in codes.iter().enumerate() {
            out.push_str(&format!("    {} = {},\n", code, i));
        }
        out.push_str("}\n");
    }
    out.push_str("// ---- END GENERATED ----\n");

    crate::codegen_shared::write_generated_file(&src_dir.join("errors.rs"), &out)?;
    Ok(())
}

/// Emit `src/ref_impls.rs` — one `pub fn` per declared `ref_impl`.
/// Param/return types flow through `map_type_for_target` against `parsed`
/// (it consumes raw DSL strings, not MIR `Ty`).
fn emit_ref_impls(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    if mir.ref_impls.is_empty() {
        return Ok(());
    }
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/ref_impls.rs",
    ));
    out.push_str(
        "//! Reference implementations (from qedspec `ref_impl` declarations).\n\
         //! Pure expressions — no state mutation, no side effects.\n\
         //! Generated alongside guards.rs so `requires` / `ensures` clauses\n\
         //! and user handler bodies can call them by name.\n\n",
    );
    out.push_str("#![allow(dead_code, clippy::too_many_arguments)]\n\n");
    for r in &mir.ref_impls {
        let params = r
            .params
            .iter()
            .map(|(n, t)| {
                let ty = crate::codegen_shared::map_type_for_target(t, parsed, target)
                    .unwrap_or_else(|_| t.clone());
                format!("{}: {}", n, ty)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let ret = crate::codegen_shared::map_type_for_target(&r.return_type, parsed, target)
            .unwrap_or_else(|_| r.return_type.clone());
        if let Some(doc) = &r.doc {
            for line in doc.lines() {
                out.push_str(&format!("/// {}\n", line.trim_start_matches("///").trim()));
            }
        }
        out.push_str(&format!(
            "#[inline]\npub fn {}({}) -> {} {{\n    {}\n}}\n\n",
            r.name, params, ret, r.rust_body
        ));
    }
    out.push_str("// ---- END GENERATED ----\n");
    crate::codegen_shared::write_generated_file(&src_dir.join("ref_impls.rs"), &out)?;
    Ok(())
}

/// Emit `src/events.rs` — one `#[event]` struct per declared event.
/// Field types come from a parallel `parsed.events` lookup because
/// `map_type_for_target` consumes raw DSL strings, not MIR `Ty`.
fn emit_events(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    if mir.events.is_empty() {
        return Ok(());
    }
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let prelude_import: &str = match target {
        Target::Anchor => "use anchor_lang::prelude::*;\n",
        Target::Quasar => "use quasar_lang::prelude::*;\n",
        // Pinocchio has no event framework — plain data structs the
        // program serializes and logs itself; no prelude to import.
        Target::Pinocchio => {
            "// Pinocchio has no event macro — these are plain data structs.\n\
             // Serialize + emit them yourself (e.g. via the `sol_log_data` syscall).\n"
        }
    };

    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/events.rs",
    ));
    out.push_str(prelude_import);
    out.push('\n');

    for (i, event) in mir.events.iter().enumerate() {
        let parsed_event = parsed
            .events
            .iter()
            .find(|e| e.name == event.name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MIR event '{}' has no matching ParsedEvent (parser/lowering mismatch)",
                    event.name
                )
            })?;

        match target {
            Target::Anchor => out.push_str("#[event]\n"),
            Target::Quasar => out.push_str(&format!("#[event(discriminator = {})]\n", i + 1)),
            Target::Pinocchio => out.push_str("#[derive(Clone)]\n"),
        }
        out.push_str(&format!("pub struct {} {{\n", event.name));
        for (fname, ftype) in &parsed_event.fields {
            out.push_str(&format!(
                "    pub {}: {},\n",
                fname,
                crate::codegen_shared::map_type_for_target(ftype, parsed, target)?
            ));
        }
        out.push_str("}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");

    crate::codegen_shared::write_generated_file(&src_dir.join("events.rs"), &out)?;
    Ok(())
}

/// Emit `src/math.rs` — fixed-point helpers for spec-derived guards /
/// properties. Fully deterministic; the only data input is the
/// fingerprint hash in the marker banner.
fn emit_math(fp: &crate::fingerprint::SpecFingerprint, output_dir: &Path) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    let mut out = String::new();
    out.push_str(&crate::codegen_shared::marker(
        "DO NOT EDIT",
        fp,
        "src/math.rs",
    ));
    out.push_str("//! Fixed-point math helpers used by spec-derived guards and properties.\n\n");
    out.push_str("#![allow(dead_code)]\n\n");
    out.push_str(
        "/// Floor of `(a * b) / d`. Returns `0` if `d == 0` (caller must guard).\n\
/// Uses saturating multiplication as a safe approximation; specs that need\n\
/// exact u256-width fixed-point math should pin a checked widening crate\n\
/// once the spec language exposes one.\n\
#[inline]\n\
pub fn mul_div_floor_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 {\n\
        return 0;\n\
    }\n\
    a.saturating_mul(b) / d\n\
}\n\n",
    );
    out.push_str(
        "/// Ceiling of `(a * b) / d`. Same caveats as `mul_div_floor_u128`.\n\
#[inline]\n\
pub fn mul_div_ceil_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 {\n\
        return 0;\n\
    }\n\
    let prod = a.saturating_mul(b);\n\
    if prod % d == 0 {\n\
        prod / d\n\
    } else {\n\
        (prod / d).saturating_add(1)\n\
    }\n\
}\n",
    );
    out.push_str(
        "/// Nearest `(a * b) / d`, with exact halves rounded upward.\n\
#[inline]\n\
pub fn mul_div_round_half_up_u128(a: u128, b: u128, d: u128) -> u128 {\n\
    if d == 0 { return 0; }\n\
    let prod = a.saturating_mul(b);\n\
    let q = prod / d;\n\
    let r = prod % d;\n\
    let threshold = d / 2 + d % 2;\n\
    if r >= threshold { q.saturating_add(1) } else { q }\n\
}\n",
    );
    out.push_str("// ---- END GENERATED ----\n");
    crate::codegen_shared::write_generated_file(&src_dir.join("math.rs"), &out)?;
    Ok(())
}

/// True when the generated Cargo.toml needs the target's SPL crate.
fn mir_needs_spl(mir: &Mir) -> bool {
    use crate::mir::{AccountKind, Stmt};

    for handler in &mir.handlers {
        if handler
            .accounts
            .iter()
            .any(|a| matches!(a.kind, AccountKind::Token | AccountKind::Mint))
        {
            return true;
        }
        // Both the `transfers { … }` sugar and `call Token.transfer(...)`
        // lower to `Stmt::TokenTransfer`.
        for stmt in &handler.body.stmts {
            match stmt {
                Stmt::TokenTransfer { .. } => return true,
                Stmt::Cpi { target, .. } if target.0 == "Token" => return true,
                Stmt::Cpi { .. }
                | Stmt::RequireOrAbort { .. }
                | Stmt::VariantPromote { .. }
                | Stmt::Assign { .. }
                | Stmt::CheckedAdd { .. }
                | Stmt::CheckedSub { .. }
                | Stmt::WrapAdd { .. }
                | Stmt::WrapSub { .. }
                | Stmt::SatAdd { .. }
                | Stmt::SatSub { .. }
                | Stmt::Branch { .. }
                | Stmt::Emit { .. } => {}
            }
        }
    }
    false
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;
    use std::path::Path;

    #[test]
    fn embedded_stamps_stale_detects_spec_revision_drift() {
        let current = vec!["aaaaaaaaaaaaaaaa".to_string()];
        let fresh = r#"#[qed(scaffold, spec_hash = "aaaaaaaaaaaaaaaa")] fn h() {}"#;
        assert!(
            !embedded_stamps_stale(fresh, &current),
            "matching stamp is fresh"
        );
        let stale = r#"#[qed(scaffold, spec_hash = "bbbbbbbbbbbbbbbb")] fn h() {}"#;
        assert!(
            embedded_stamps_stale(stale, &current),
            "mismatched stamp is stale"
        );
        assert!(
            !embedded_stamps_stale("fn main() {}", &current),
            "no stamps → nothing to compare"
        );
    }

    fn parse_spec(src: &str) -> (Mir, ParsedSpec) {
        let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = crate::mir::lower(&parsed);
        (mir, parsed)
    }

    fn spec_with_account(account: &str) -> String {
        format!(
            r#"spec Renamer
program_id "11111111111111111111111111111111"
type State | Active of {{ total : U64 }}
handler poke : State.Active -> State.Active {{
  accounts {{ {account} : writable }}
  effect {{ Active.total += 1 }}
}}
"#
        )
    }

    /// #288 --merge-accounts: an account rename regenerates the struct's
    /// fields in place while user content elsewhere in lib.rs survives.
    #[test]
    fn merge_accounts_updates_renamed_fields_preserving_user_content() {
        let (mir_a, parsed_a) = parse_spec(&spec_with_account("vault"));
        let fp = crate::fingerprint::compute_fingerprint(&parsed_a);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        emit_lib(
            &mir_a,
            &parsed_a,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions::default(),
        )
        .expect("emit lib");

        // Simulate user ownership: edits outside and immediately above the
        // Accounts struct. Both must survive the surgical replacement.
        let lib_path = temp.join("src/lib.rs");
        let lib = std::fs::read_to_string(&lib_path).expect("lib.rs");
        assert!(lib.contains("pub vault:"), "baseline has vault field");
        std::fs::write(
            &lib_path,
            lib.replace("use super::*;", "use super::*; // USER-KEPT")
                .replace(
                    "#[derive(Accounts)]\npub struct Poke",
                    "// USER-STRUCT-DOC\n#[derive(Accounts)]\npub struct Poke",
                ),
        )
        .expect("user edit");

        // Spec-level rename vault → treasury, then merge.
        let (mir_b, parsed_b) = parse_spec(&spec_with_account("treasury"));
        merge_accounts_into_lib(&mir_b, &parsed_b, &lib_path).expect("merge");

        let merged = std::fs::read_to_string(&lib_path).expect("merged lib.rs");
        assert!(
            merged.contains("pub treasury:") && !merged.contains("pub vault:"),
            "struct fields must pick up the rename; got:\n{merged}"
        );
        assert!(
            merged.contains("// USER-KEPT"),
            "user content outside the structs must survive the merge; got:\n{merged}"
        );
        assert!(
            merged.contains("// USER-STRUCT-DOC\n#[derive(Accounts)]\npub struct Poke"),
            "user comments adjacent to the struct must survive the merge; got:\n{merged}"
        );
    }

    #[test]
    fn destructive_regen_rejects_ignored_untracked_files() {
        use std::process::Command;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).expect("mk src");
        std::fs::write(root.join(".gitignore"), "src/lib.rs\n").expect("gitignore");
        std::fs::write(root.join("src/lib.rs"), "// user-owned fill\n").expect("lib");

        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .expect("git init")
            .success());
        assert!(Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(root)
            .status()
            .expect("git add")
            .success());
        assert!(Command::new("git")
            .args([
                "-c",
                "user.name=qedgen-test",
                "-c",
                "user.email=qedgen@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ])
            .current_dir(root)
            .status()
            .expect("git commit")
            .success());

        let err = assert_git_recoverable(root, &[root.join("src/lib.rs")])
            .expect_err("ignored, untracked files have no git recovery baseline");
        assert!(
            err.to_string().contains("not tracked by git"),
            "error must explain the missing recovery baseline: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.rs")).expect("lib survives"),
            "// user-owned fill\n"
        );
    }

    /// #288 --merge-accounts: a handler rename appends the new struct and
    /// leaves the old one in place (reported, never deleted — it may be a
    /// hand-added instruction's).
    #[test]
    fn merge_accounts_appends_new_struct_keeps_orphan() {
        let (mir_a, parsed_a) = parse_spec(&spec_with_account("vault"));
        let fp = crate::fingerprint::compute_fingerprint(&parsed_a);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        emit_lib(
            &mir_a,
            &parsed_a,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions::default(),
        )
        .expect("emit lib");
        let lib_path = temp.join("src/lib.rs");

        // Handler rename poke → jab.
        let renamed = spec_with_account("vault").replace("poke", "jab");
        let (mir_b, parsed_b) = parse_spec(&renamed);
        merge_accounts_into_lib(&mir_b, &parsed_b, &lib_path).expect("merge");

        let merged = std::fs::read_to_string(&lib_path).expect("merged lib.rs");
        assert!(
            merged.contains("pub struct Jab"),
            "renamed handler's struct must be appended; got:\n{merged}"
        );
        assert!(
            merged.contains("pub struct Poke"),
            "orphan struct is left in place for the user to delete; got:\n{merged}"
        );
    }

    /// A same-named struct WITHOUT #[derive(Accounts)] is user territory:
    /// never replaced, never duplicated.
    #[test]
    fn locate_accounts_struct_shapes() {
        let text = "#[derive(Accounts)]\npub struct Pause<'info> {\n    pub a: u8,\n}\n\npub struct Pause2 {\n    pub b: u8,\n}\n";
        assert!(matches!(
            locate_accounts_struct(text, "Pause"),
            StructLocation::Found(_)
        ));
        assert!(matches!(
            locate_accounts_struct(text, "Pause2"),
            StructLocation::Foreign
        ));
        assert!(matches!(
            locate_accounts_struct(text, "Missing"),
            StructLocation::Missing
        ));
        // Comment mentions don't count as declarations.
        let commented = "// pub struct Ghost { }\n";
        assert!(matches!(
            locate_accounts_struct(commented, "Ghost"),
            StructLocation::Missing
        ));
    }

    /// #288 --force: an existing user-owned lib.rs is regenerated instead
    /// of skipped (the git-recoverability guard lives in `generate`, not
    /// here).
    #[test]
    fn force_regenerates_existing_lib() {
        let (mir, parsed) = parse_spec(&spec_with_account("vault"));
        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        let lib_path = temp.join("src/lib.rs");
        std::fs::write(&lib_path, "// stale user-owned content\n").expect("seed lib");

        emit_lib(
            &mir,
            &parsed,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions {
                force: true,
                merge_accounts: false,
            },
        )
        .expect("emit lib");
        let lib = std::fs::read_to_string(&lib_path).expect("lib.rs");
        assert!(
            lib.contains("#[program]") && !lib.contains("stale user-owned content"),
            "--force must regenerate over the existing file; got:\n{lib}"
        );
    }

    fn lower_fixture(rel_path: &str) -> (Mir, ParsedSpec) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/qedgen/ under repo root");
        let spec_path = root.join(rel_path);
        let parsed = check::parse_spec_file(&spec_path).expect("fixture parses");
        let mir = crate::mir::lower(&parsed);
        (mir, parsed)
    }

    #[test]
    fn cargo_toml_ships_proptest_dev_dep() {
        // The generated tests/proptest.rs imports proptest::prelude::* —
        // without the dev-dep the crate fails its first `cargo test`.
        let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        for target in [Target::Anchor, Target::Quasar, Target::Pinocchio] {
            let toml = render_cargo_toml(&mir, &fp, target);
            let dev = toml.split("[dev-dependencies]").nth(1).unwrap_or_else(|| {
                panic!(
                    "{:?} Cargo.toml missing [dev-dependencies]:\n{toml}",
                    target
                )
            });
            assert!(
                dev.contains("proptest = \"1\""),
                "{:?} dev-dependencies must carry proptest; got:\n{toml}",
                target
            );
        }
    }

    #[test]
    fn phase_4a_scaffold_loads() {
        // Smoke: a real spec round-trips into MIR + parsed without
        // panicking; the rendering integration tests live in the
        // snapshot suite.
        let (mir, parsed) = lower_fixture("examples/rust/escrow/escrow.qedspec");
        assert!(!parsed.handlers.is_empty(), "escrow has handlers");
        assert!(!mir.state.variants.is_empty(), "escrow has state variants");
    }

    /// Regression: `type Account = { … }` collides with the Anchor
    /// `Account<'info, _>` wrapper under glob imports; `emit_lib` must emit
    /// an explicit `use anchor_lang::prelude::Account;` so the wrapper
    /// wins. Non-colliding specs get no such line.
    #[test]
    fn anchor_lib_disambiguates_state_type_colliding_with_prelude_wrapper() {
        let src = r#"spec Coll
program_id "11111111111111111111111111111111"
type Account = { x : U64 }
type State | Active of { total : U64 }
handler poke : State.Active -> State.Active {
  accounts { vault : writable }
  effect { Active.total += 1 }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = crate::mir::lower(&parsed);
        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        emit_lib(
            &mir,
            &parsed,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions::default(),
        )
        .expect("emit lib");
        let lib = std::fs::read_to_string(temp.join("src/lib.rs")).expect("lib.rs");
        assert!(
            lib.contains(": Account<"),
            "the state account field should use the Anchor `Account<>` wrapper; got:\n{lib}"
        );
        assert!(
            lib.contains("use anchor_lang::prelude::Account;"),
            "colliding state type `Account` must force an explicit prelude re-import; got:\n{lib}"
        );
    }

    /// v2.46 (Bug 5) — a single-ADT `State.Uninitialized -> State.Active`
    /// handler over a PDA must emit `#[account(init, payer, space = 8 +
    /// <Program>Account::INIT_SPACE, …)]`. The type-qualified pre-state
    /// (`State.`) previously set `on_account = Some("State")`, and the
    /// name heuristic then rejected a PDA named anything but `state`, so
    /// init/payer/space never emitted (and the space struct name resolved
    /// to the non-existent `StateAccount`).
    #[test]
    fn anchor_init_emits_for_type_qualified_single_adt_pda() {
        let src = r#"spec Vault
program_id "11111111111111111111111111111111"
type State
  | Uninitialized
  | Active of { admin : Pubkey, balance : U64 }
type Error | Bad
pda vault ["vault", admin]
handler initialize : State.Uninitialized -> State.Active {
  accounts {
    admin : signer, writable
    vault : writable, pda ["vault", admin]
    system_program : program
  }
  effect { state := .Active { admin := admin, balance := 0 } }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = crate::mir::lower(&parsed);
        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        emit_lib(
            &mir,
            &parsed,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions::default(),
        )
        .expect("emit lib");
        let lib = std::fs::read_to_string(temp.join("src/lib.rs")).expect("lib.rs");
        // The vault line must carry init/payer/space with the correct
        // wrapper struct name (`VaultAccount`, not `StateAccount`).
        let vault_line = lib
            .lines()
            .find(|l| l.contains("seeds = [b\"vault\""))
            .unwrap_or_else(|| panic!("no vault #[account] line in:\n{lib}"));
        assert!(
            vault_line.contains("init")
                && vault_line.contains("payer = admin")
                && vault_line.contains("space = 8 + VaultAccount::INIT_SPACE"),
            "init/payer/space must emit with the VaultAccount wrapper; got:\n{vault_line}"
        );
        assert!(
            !lib.contains("StateAccount::INIT_SPACE"),
            "space must not reference the non-existent StateAccount; got:\n{lib}"
        );
    }

    /// A non-init handler on the same PDA stays `mut` (init only on the
    /// Uninitialized→X transition).
    #[test]
    fn anchor_non_init_handler_keeps_mut_not_init() {
        let src = r#"spec Vault
program_id "11111111111111111111111111111111"
type State
  | Uninitialized
  | Active of { admin : Pubkey, balance : U64 }
type Error | Bad | MathOverflow
pda vault ["vault", admin]
handler deposit (amount : U64) : State.Active -> State.Active {
  accounts {
    admin : signer
    vault : writable, pda ["vault", admin]
  }
  effect { balance += amount }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = crate::mir::lower(&parsed);
        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        emit_lib(
            &mir,
            &parsed,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions::default(),
        )
        .expect("emit lib");
        let lib = std::fs::read_to_string(temp.join("src/lib.rs")).expect("lib.rs");
        let vault_line = lib
            .lines()
            .find(|l| l.contains("seeds = [b\"vault\""))
            .expect("vault line");
        assert!(
            vault_line.contains("mut") && !vault_line.contains("init"),
            "non-init handler must keep mut, not init; got:\n{vault_line}"
        );
    }

    /// The disambiguation is scoped: a spec with no prelude-colliding type
    /// gets no explicit re-import line.
    #[test]
    fn anchor_lib_no_disambiguation_without_collision() {
        let src = r#"spec NoColl
program_id "11111111111111111111111111111111"
type State | Active of { total : U64 }
handler poke : State.Active -> State.Active {
  accounts { vault : writable }
  effect { Active.total += 1 }
}
"#;
        let parsed = crate::chumsky_adapter::parse_str(src).expect("parse");
        let mir = crate::mir::lower(&parsed);
        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");
        emit_lib(
            &mir,
            &parsed,
            &fp,
            temp,
            Target::Anchor,
            Path::new("unused.qedspec"),
            RegenOptions::default(),
        )
        .expect("emit lib");
        let lib = std::fs::read_to_string(temp.join("src/lib.rs")).expect("lib.rs");
        assert!(
            !lib.contains("use anchor_lang::prelude::{")
                && !lib.contains("use anchor_lang::prelude::Account;"),
            "no collision → no explicit wrapper re-import; got:\n{lib}"
        );
    }

    #[test]
    fn pinocchio_events_emit_plain_struct_no_event_macro() {
        let (mir, parsed) = lower_fixture(
            "crates/qedgen/tests/fixtures/pinocchio-fixtures/vault-greenfield/vault.qedspec",
        );
        assert!(!mir.events.is_empty(), "vault-greenfield declares an event");

        let fp = crate::fingerprint::compute_fingerprint(&parsed);
        let tmp = tempfile::tempdir().expect("tempdir");
        let temp = tmp.path();
        std::fs::create_dir_all(temp.join("src")).expect("mk src");

        emit_events(&mir, &parsed, &fp, temp, Target::Pinocchio)
            .expect("Pinocchio events must emit, not panic");

        let rendered = std::fs::read_to_string(temp.join("src/events.rs")).expect("events.rs");

        assert!(
            rendered.contains("pub struct Withdrawn"),
            "event struct must be emitted; got:\n{rendered}"
        );
        assert!(
            rendered.contains("#[derive(Clone)]"),
            "Pinocchio events are plain derive-Clone structs; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("#[event"),
            "Pinocchio has no #[event] macro; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("anchor_lang") && !rendered.contains("quasar_lang"),
            "no Anchor/Quasar prelude leakage; got:\n{rendered}"
        );
    }

    /// v2.29 Slice H — when a spec's `imported_namespaces` carries an
    /// account type, codegen emits `src/imported/<ns>.rs` with the
    /// mirrored struct plus a `src/imported/mod.rs` re-exporter.
    /// Bundled-stub-only imports leave the map empty and the mirror
    /// dir is never created.
    #[test]
    fn imported_namespace_emits_local_mirror() {
        use crate::check::{ImportedNamespace, ParsedAccountType};

        let mut spec = ParsedSpec {
            program_name: "ConsumerProgram".into(),
            ..ParsedSpec::default()
        };
        spec.account_types.push(ParsedAccountType {
            name: "Consumer".into(),
            fields: vec![("balance".into(), "U64".into())],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        });
        // Inject an imported namespace by hand (the resolver path is
        // exercised by check.rs tests; this test focuses on the
        // codegen-side mirror emission).
        let imported = ImportedNamespace {
            dep_key: "foreign_dep".into(),
            account_types: vec![ParsedAccountType {
                name: "ForeignState".into(),
                fields: vec![
                    ("admin".into(), "Pubkey".into()),
                    ("counter".into(), "U64".into()),
                ],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![],
            }],
            records: vec![],
        };
        spec.imported_namespaces.insert("Foreign".into(), imported);

        let mir = crate::mir::lower(&spec);
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(out_dir.join("src")).unwrap();

        emit_imported_mirror(&mir, &spec, &fp, &out_dir, Target::Anchor)
            .expect("imported mirror generation should succeed");

        let ns_file = out_dir.join("src/imported/Foreign.rs");
        let body =
            std::fs::read_to_string(&ns_file).expect("namespace mirror file should be written");
        assert!(
            body.contains("pub struct ForeignState"),
            "expected `ForeignState` mirror struct; got:\n{body}"
        );
        assert!(
            body.contains("pub admin: Pubkey,"),
            "expected `admin: Pubkey` field; got:\n{body}"
        );
        assert!(
            body.contains("#[account]"),
            "expected `#[account]` attr (Anchor target); got:\n{body}"
        );

        let mod_file = out_dir.join("src/imported/mod.rs");
        let mod_body =
            std::fs::read_to_string(&mod_file).expect("imported mod.rs should be written");
        assert!(
            mod_body.contains("pub mod Foreign;"),
            "expected `pub mod Foreign;` re-export; got:\n{mod_body}"
        );
    }

    /// v2.29 Slice H — multi-variant imported account types lower to
    /// the wrapper-struct + inner-enum shape and emit accessor
    /// methods on the inner enum (mirrors `emit_state`'s Slice B
    /// accessor work).
    #[test]
    fn imported_multi_variant_namespace_emits_accessors() {
        use crate::check::{ImportedNamespace, ParsedAccountType, ParsedVariant};

        let mut spec = ParsedSpec {
            program_name: "Consumer".into(),
            ..ParsedSpec::default()
        };
        spec.account_types.push(ParsedAccountType {
            name: "Local".into(),
            fields: vec![("x".into(), "U64".into())],
            lifecycle: vec![],
            pda_ref: None,
            variants: vec![],
        });
        let imported = ImportedNamespace {
            dep_key: "amm_dep".into(),
            account_types: vec![ParsedAccountType {
                name: "Pool".into(),
                fields: vec![],
                lifecycle: vec![],
                pda_ref: None,
                variants: vec![
                    ParsedVariant {
                        name: "Open".into(),
                        fields: vec![
                            ("admin".into(), "Pubkey".into()),
                            ("balance".into(), "U64".into()),
                        ],
                    },
                    ParsedVariant {
                        name: "Closed".into(),
                        fields: vec![("admin".into(), "Pubkey".into())],
                    },
                ],
            }],
            records: vec![],
        };
        spec.imported_namespaces.insert("AMM".into(), imported);

        let mir = crate::mir::lower(&spec);
        let fp = crate::fingerprint::compute_fingerprint(&spec);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().join("programs");
        std::fs::create_dir_all(out_dir.join("src")).unwrap();

        emit_imported_mirror(&mir, &spec, &fp, &out_dir, Target::Anchor)
            .expect("imported mirror generation should succeed");

        let body = std::fs::read_to_string(out_dir.join("src/imported/AMM.rs"))
            .expect("AMM mirror file should be written");
        assert!(
            body.contains("pub struct Pool"),
            "expected wrapper struct; got:\n{body}"
        );
        assert!(
            body.contains("pub inner: PoolInner,"),
            "expected `inner: PoolInner` field; got:\n{body}"
        );
        assert!(
            body.contains("pub enum PoolInner"),
            "expected inner enum; got:\n{body}"
        );
        // `admin` exists in both variants — accessor emitted, no
        // panic arm because the match exhausts.
        assert!(
            body.contains("pub fn admin(&self) -> &Pubkey"),
            "expected `admin` accessor; got:\n{body}"
        );
        // `balance` only in Open — accessor emits with a panic arm.
        assert!(
            body.contains("pub fn balance(&self) -> &u64"),
            "expected `balance` accessor; got:\n{body}"
        );
        assert!(
            body.contains("PoolInner::balance() called on a variant without `balance`"),
            "expected panic message for missing variant; got:\n{body}"
        );
    }

    /// #305 regression: the `space = 8 + <T>::INIT_SPACE` attribute and
    /// the state-struct emission are rendered at different sites, and
    /// diverged two ways on a single-account spec whose ADT name differs
    /// from the program name:
    /// - the space target read the ADT name (`VaultAccount`) while the
    ///   struct was named after the program (`RuntimeVaultAccount`) —
    ///   E0433, undeclared type;
    /// - the flat branch emitted `#[account]` without
    ///   `#[derive(InitSpace)]`, so `INIT_SPACE` did not resolve — E0599.
    ///
    /// Asserts the invariant rather than either spelling: whatever struct
    /// `space =` names must be the struct that is emitted, and it must
    /// carry the derive.
    #[test]
    fn init_space_target_matches_the_emitted_state_struct() {
        const SRC: &str = r#"spec RuntimeVault

type Vault
  | Uninitialized
  | Active of {
      owner : Pubkey,
      total : U64,
    }

type Error
  | InvalidAmount

pda vault ["vault", owner]

handler open : Vault.Uninitialized -> Vault.Active {
  auth owner
  accounts {
    owner          : signer, writable
    vault          : writable, pda ["vault", owner]
    system_program : program
  }
  effect {
    owner := owner.pubkey
    total := 0
  }
}
"#;
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("vault.qedspec");
        std::fs::write(&spec_path, SRC).unwrap();
        std::fs::create_dir_all(dir.path().join(".qed")).unwrap();

        let spec = crate::check::parse_spec_file(&spec_path).expect("parse");
        let mir = crate::mir::lower(&spec);
        let out_dir = dir.path().join("programs");
        generate(
            &mir,
            &spec,
            &spec_path,
            &out_dir,
            Target::Anchor,
            RegenOptions::default(),
        )
        .expect("anchor codegen should succeed");

        let state = std::fs::read_to_string(out_dir.join("src/state.rs")).expect("state.rs");
        let lib = std::fs::read_to_string(out_dir.join("src/lib.rs")).expect("lib.rs");

        // Pull the struct named by `space = 8 + <T>::INIT_SPACE`.
        let marker = "space = 8 + ";
        let start = lib
            .find(marker)
            .map(|i| i + marker.len())
            .expect("init account renders a space attribute");
        let space_target = lib[start..]
            .split("::INIT_SPACE")
            .next()
            .expect("space target is an INIT_SPACE reference")
            .to_string();

        // The named struct must be the one actually emitted...
        assert!(
            state.contains(&format!("pub struct {space_target} {{")),
            "space target `{space_target}` names no emitted struct:\n{state}"
        );
        // ...and it must derive InitSpace, or `INIT_SPACE` does not resolve.
        assert!(
            state.contains("#[derive(InitSpace)]"),
            "Anchor state struct must derive InitSpace:\n{state}"
        );
        // Guard the specific historical spelling too, so a regression is
        // reported as a name mismatch rather than a bare missing struct.
        assert_eq!(
            space_target, "RuntimeVaultAccount",
            "single-account spec names the wrapper after the program, not the ADT"
        );
    }
}
