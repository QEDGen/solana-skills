//! Locate an Anchor crate's `lib.rs`, find its `#[program] pub mod`, and list
//! each `pub fn` as a discovered `Instruction`. The `#[program]` mod's pub fns
//! are the one universal source of instruction names — real programs vary
//! wildly in where handler bodies live, but `lib.rs` always holds the
//! `#[program]` mod. Forwarder resolution (instruction → actual handler body)
//! lives in `anchor_resolver.rs`.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// One discovered instruction. Manual `Debug` prints just the name —
/// `syn::ItemFn` lacks `Debug` without the heavy `extra-traits` feature.
#[derive(Clone)]
pub struct Instruction {
    /// Pub fn ident inside the `#[program]` mod — the ABI name a
    /// `.qedspec` handler block should match.
    pub name: String,
    /// Full `pub fn` item; carries the body the forwarder resolver inspects.
    pub program_fn: syn::ItemFn,
}

impl std::fmt::Debug for Instruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instruction")
            .field("name", &self.name)
            .finish()
    }
}

/// A user's Anchor program crate, as discovered via syn-based parsing.
#[derive(Clone, Debug)]
pub struct AnchorProject {
    /// Absolute path to the program's `lib.rs`.
    pub lib_rs_path: PathBuf,
    /// Name of the `#[program] pub mod <name>` block.
    pub program_mod_name: String,
    /// Every `pub fn` inside the `#[program]` mod, in source order.
    pub instructions: Vec<Instruction>,
}

/// Parse the Anchor project rooted at `program_root` (the dir holding the
/// program's `Cargo.toml`). Errors clearly on: missing `src/lib.rs`, Rust
/// parse failure, no `#[program]` mod, or an instruction-less program mod.
pub fn parse_anchor_project(program_root: &Path) -> Result<AnchorProject> {
    let lib_rs = program_root.join("src").join("lib.rs");
    if !lib_rs.is_file() {
        anyhow::bail!(
            "Anchor program crate must have `src/lib.rs` — not found at {}",
            lib_rs.display()
        );
    }
    let source = std::fs::read_to_string(&lib_rs)
        .with_context(|| format!("reading {}", lib_rs.display()))?;
    parse_lib_rs(&lib_rs, &source)
}

/// Parse a lib.rs source string; exposed for tests (production callers go
/// through `parse_anchor_project`).
pub fn parse_lib_rs(lib_rs_path: &Path, source: &str) -> Result<AnchorProject> {
    let file: syn::File = syn::parse_str(source).map_err(|e| {
        anyhow!(
            "failed to parse {} as Rust source: {}",
            lib_rs_path.display(),
            e
        )
    })?;

    let program_mod = find_program_mod(&file).ok_or_else(|| {
        anyhow!(
            "no `#[program] pub mod <name>` block in {}.\n\
             qedgen's brownfield adapter (`qedgen adapt`) is Anchor-only — \
             non-Anchor (raw / native) Solana programs are not supported.",
            lib_rs_path.display(),
        )
    })?;

    let instructions = collect_instructions(program_mod);
    if instructions.is_empty() {
        anyhow::bail!(
            "the `#[program] pub mod {}` in {} declares no `pub fn` instructions — \
             nothing to adapt.",
            program_mod.ident,
            lib_rs_path.display(),
        );
    }

    Ok(AnchorProject {
        lib_rs_path: lib_rs_path.to_path_buf(),
        program_mod_name: program_mod.ident.to_string(),
        instructions,
    })
}

/// Find `#[program] pub mod <name>`. Matched by the attribute path's last
/// segment, so qualified forms (`#[anchor_lang::program]`) also work.
fn find_program_mod(file: &syn::File) -> Option<&syn::ItemMod> {
    for item in &file.items {
        if let syn::Item::Mod(item_mod) = item {
            if has_program_attr(&item_mod.attrs) {
                return Some(item_mod);
            }
        }
    }
    None
}

fn has_program_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "program")
    })
}

/// Every `pub fn` directly inside the `#[program]` mod — Anchor's convention
/// puts instruction handlers at the mod's top level; all else is skipped.
fn collect_instructions(item_mod: &syn::ItemMod) -> Vec<Instruction> {
    let Some((_brace, items)) = &item_mod.content else {
        return Vec::new();
    };
    let mut instructions = Vec::new();
    for item in items {
        if let syn::Item::Fn(item_fn) = item {
            if matches!(item_fn.vis, syn::Visibility::Public(_)) {
                instructions.push(Instruction {
                    name: item_fn.sig.ident.to_string(),
                    program_fn: item_fn.clone(),
                });
            }
        }
    }
    instructions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<AnchorProject> {
        parse_lib_rs(Path::new("/test/lib.rs"), src)
    }

    #[test]
    fn finds_program_mod_with_pub_fns() {
        // Anchor scaffold-style forwarders.
        let src = r#"
            use anchor_lang::prelude::*;

            declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

            #[program]
            pub mod my_escrow {
                use super::*;

                pub fn initialize(ctx: Context<Initialize>, amount: u64) -> Result<()> {
                    instructions::initialize::handler(ctx, amount)
                }

                pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
                    instructions::cancel::handler(ctx)
                }
            }

            #[derive(Accounts)]
            pub struct Initialize<'info> {
                #[account(mut)]
                pub authority: Signer<'info>,
            }
        "#;
        let project = parse(src).unwrap();
        assert_eq!(project.program_mod_name, "my_escrow");
        assert_eq!(project.instructions.len(), 2);
        assert_eq!(project.instructions[0].name, "initialize");
        assert_eq!(project.instructions[1].name, "cancel");
    }

    #[test]
    fn errors_when_no_program_mod() {
        let src = r#"
            // Plain Rust crate — no #[program] anywhere.
            pub mod helpers {
                pub fn util() {}
            }
        "#;
        let err = parse(src).unwrap_err().to_string();
        assert!(err.contains("no `#[program] pub mod"), "got: {err}");
        assert!(
            err.contains("Anchor-only"),
            "should explain that adapt is Anchor-only; got: {err}"
        );
    }

    #[test]
    fn errors_when_program_mod_has_no_pub_fns() {
        let src = r#"
            #[program]
            pub mod empty {
                use super::*;
                // No pub fns yet.
                fn private_helper() {}
            }
        "#;
        let err = parse(src).unwrap_err().to_string();
        assert!(err.contains("no `pub fn` instructions"), "got: {err}");
    }

    #[test]
    fn skips_private_fns_in_program_mod() {
        // Private helpers next to instructions are not listed.
        let src = r#"
            #[program]
            pub mod my_program {
                use super::*;

                pub fn buy(ctx: Context<Buy>) -> Result<()> { Ok(()) }

                fn private_helper() -> u64 { 42 }

                pub fn sell(ctx: Context<Sell>) -> Result<()> { Ok(()) }
            }
        "#;
        let project = parse(src).unwrap();
        assert_eq!(project.instructions.len(), 2);
        let names: Vec<&str> = project
            .instructions
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, vec!["buy", "sell"]);
    }

    #[test]
    fn handles_qualified_program_attribute() {
        let src = r#"
            #[anchor_lang::program]
            pub mod qualified {
                use super::*;
                pub fn handler(ctx: Context<C>) -> Result<()> { Ok(()) }
            }
        "#;
        let project = parse(src).unwrap();
        assert_eq!(project.program_mod_name, "qualified");
        assert_eq!(project.instructions.len(), 1);
    }

    #[test]
    fn preserves_full_fn_item_for_body_inspection() {
        let src = r#"
            #[program]
            pub mod p {
                use super::*;
                pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
                    require!(amount > 0, ErrorCode::InvalidAmount);
                    instructions::buy::handler(ctx, amount)
                }
            }
        "#;
        let project = parse(src).unwrap();
        let buy = &project.instructions[0];
        assert_eq!(buy.name, "buy");
        assert!(!buy.program_fn.block.stmts.is_empty());
    }
}
