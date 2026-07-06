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

struct CodegenCtx<'a> {
    mir: &'a Mir,
    parsed: &'a ParsedSpec,
    fp: &'a SpecFingerprint,
    spec_path: &'a Path,
    output_dir: &'a Path,
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
        emit_lib(ctx.mir, ctx.parsed, ctx.fp, ctx.output_dir, self.target())
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

    std::fs::create_dir_all(output_dir)?;

    let fp = crate::fingerprint::compute_fingerprint(parsed);
    let ctx = CodegenCtx {
        mir,
        parsed,
        fp: &fp,
        spec_path,
        output_dir,
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
    let hash = fp
        .file_hashes
        .get("Cargo.toml")
        .cloned()
        .unwrap_or_default();
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
fn emit_lib(
    mir: &Mir,
    parsed: &ParsedSpec,
    fp: &crate::fingerprint::SpecFingerprint,
    output_dir: &Path,
    target: Target,
) -> Result<()> {
    use crate::codegen_shared::{to_pascal_case, FrameworkSurface};

    // Pinocchio: dedicated helper emits the no_std entrypoint +
    // byte-dispatch from ParsedSpec.
    if matches!(target, Target::Pinocchio) {
        return crate::codegen_shared::emit_pinocchio_program_lib(parsed, fp, output_dir);
    }

    let surface = FrameworkSurface::for_target(target);
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let lib_path = src_dir.join("lib.rs");
    if lib_path.exists() {
        eprintln!(
            "programs/{}/src/lib.rs already exists — skipping (user-owned). guards.rs regenerated.",
            output_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<program>")
        );
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

    std::fs::write(src_dir.join("lib.rs"), &out)?;
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
    std::fs::write(instr_dir.join("mod.rs"), &mod_out)?;

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
        if handler_path.exists() {
            eprintln!(
                "programs/{}/src/instructions/{}.rs already exists — skipping (user-owned). guards.rs regenerated.",
                output_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("<program>"),
                handler.name
            );
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
        std::fs::write(&handler_path, &out)?;
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
        is_multi_variant_adt_state, map_type_for_target, map_type_pod, to_pascal_case,
        FrameworkSurface,
    };

    // Pinocchio: zeropod zero-copy state via the dedicated helper.
    if matches!(target, Target::Pinocchio) {
        let src_dir = output_dir.join("src");
        std::fs::create_dir_all(&src_dir)?;
        let mut out = String::new();
        crate::codegen_shared::emit_pinocchio_state(parsed, fp, &mut out)?;
        std::fs::write(src_dir.join("state.rs"), &out)?;
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
        let state_name = format!("{}Account", to_pascal_case(&mir.name));
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
        // Flat single-account fallback.
        let state_name = format!("{}Account", to_pascal_case(&mir.name));

        let account_attr = if surface.explicit_account_discriminator {
            "#[account(discriminator = 1)]\n"
        } else {
            "#[account]\n"
        };
        out.push_str(&format!("{}pub struct {} {{\n", account_attr, state_name));

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

    std::fs::write(src_dir.join("state.rs"), &out)?;
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
        std::fs::write(imported_dir.join(format!("{}.rs", local_name)), &out)?;
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
    std::fs::write(imported_dir.join("mod.rs"), mod_out)?;

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

    // R28: runtime PDA verification auto-adds `InvalidPda`. Shares the
    // firing predicate with `generate_guards` so the emitted check and
    // this enum variant can't drift apart.
    let needs_invalid_pda = parsed.handlers.iter().any(|h| {
        let bound: std::collections::HashSet<&str> =
            h.accounts.iter().map(|a| a.name.as_str()).collect();
        h.accounts.iter().any(|acct| {
            let Some(seeds) = &acct.pda_seeds else {
                return false;
            };
            if acct.is_signer {
                return false;
            }
            if crate::codegen_shared::handler_is_init_for(h, &acct.name) {
                return false;
            }
            crate::codegen_shared::r28_pda_check_fires(target, parsed, seeds, &bound)
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

    std::fs::write(src_dir.join("errors.rs"), &out)?;
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
    std::fs::write(src_dir.join("ref_impls.rs"), &out)?;
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

    std::fs::write(src_dir.join("events.rs"), &out)?;
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
    out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(src_dir.join("math.rs"), &out)?;
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
                | Stmt::Abort(_)
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
        emit_lib(&mir, &parsed, &fp, temp, Target::Anchor).expect("emit lib");
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
        emit_lib(&mir, &parsed, &fp, temp, Target::Anchor).expect("emit lib");
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
}
