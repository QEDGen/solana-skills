//! Walk a user-authored Anchor project (v2.9 M4.1).
//!
//! The brownfield adapter (G2) and `--anchor-project` mode (G4) need a
//! shared parser-side layer that:
//!
//!   1. Locates the program crate's `lib.rs`.
//!   2. Finds its `#[program] pub mod <name>` block — this is the
//!      Anchor convention's universal source of *which* instruction
//!      names exist on the program.
//!   3. Lists each `pub fn` inside the program mod as a discovered
//!      `Instruction { name, fn_item }`.
//!
//! The forwarder-resolution step that maps each instruction to its
//! *actual* handler body (which may live in a sibling module, an
//! `impl` block, or inline) is M4.2 and lives in `anchor_resolver.rs`.
//!
//! Survey-driven design (`reference_anchor_patterns.md`):
//!   - The `#[program]` mod's pub fns are the single universal source
//!     of instruction names. Real Anchor programs vary wildly in
//!     where the actual handler bodies live, but the `#[program]`
//!     mod is consistent across all 5 surveyed programs (Marinade,
//!     Drift, Squads, Raydium, Jito) plus the Anchor scaffold.
//!   - `lib.rs` always contains the `#[program]` mod when one exists
//!     (the convention every Anchor program follows).

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// One discovered instruction in the user's program.
///
/// Manual `Debug` impl prints just the name (the `syn::ItemFn` field
/// doesn't implement `Debug` without the heavy `extra-traits` feature).
#[derive(Clone)]
#[allow(dead_code)]
pub struct Instruction {
    /// Instruction name as exposed on the program ABI — the pub fn's
    /// identifier inside the `#[program]` mod. This is what
    /// off-chain callers use, and what a `.qedspec` handler block
    /// should match.
    pub name: String,
    /// The full `pub fn` item from the `#[program]` mod, parsed via
    /// `syn`. Carries the body that M4.2's forwarder resolver
    /// inspects to find the actual handler.
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
#[allow(dead_code)]
pub struct AnchorProject {
    /// Absolute path to the program's `lib.rs`.
    pub lib_rs_path: PathBuf,
    /// Name of the `#[program] pub mod <name>` block.
    pub program_mod_name: String,
    /// Every `pub fn` inside the `#[program]` mod, in source order.
    pub instructions: Vec<Instruction>,
}

/// Parse a user's Anchor project rooted at `program_root` (the
/// directory containing the program's own `Cargo.toml`). Looks for
/// `src/lib.rs`, parses it with `syn`, and extracts the `#[program]`
/// mod plus its instructions.
///
/// Errors with a clear diagnostic when:
///   - `src/lib.rs` is missing
///   - the file fails to parse as Rust
///   - no `#[program]` mod is present (likely not an Anchor program)
///   - the `#[program]` mod has no pub fns (instruction-less program)
#[allow(dead_code)]
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

/// Parse a lib.rs source string. Exposed for tests so we don't need a
/// real filesystem layout per fixture; production callers go through
/// `parse_anchor_project`.
#[allow(dead_code)]
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

/// Find a `#[program] pub mod <name>` item in a parsed file. Anchor's
/// `#[program]` attribute is a procedural macro from `anchor-lang`; we
/// match by attribute path (the last segment is `program`) so it works
/// whether the user wrote `#[program]`, `#[anchor_lang::program]`, or
/// `#[anchor::program]` (uncommon but legal).
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

/// Collect every `pub fn` directly inside the `#[program]` mod. Skips
/// non-pub fns, nested mods, structs, etc. — Anchor's convention is
/// strict: instruction handlers are pub fns at the mod's top level.
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

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<AnchorProject> {
        parse_lib_rs(Path::new("/test/lib.rs"), src)
    }

    #[test]
    fn finds_program_mod_with_pub_fns() {
        // Anchor scaffold-style: the program mod forwards each
        // instruction to a handler in `instructions/<name>::handler`.
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
        // Real-world: programs sometimes have a private helper next
        // to instructions. We list only pub fns.
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
        // Some programs use the fully-qualified attribute path.
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
        // M4.2 will inspect each instruction's body; M4.1 just needs
        // to keep the full ItemFn around.
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
        // The body has at least one statement (the require! macro)
        // and a tail expression (the handler call).
        assert!(!buy.program_fn.block.stmts.is_empty());
    }
}
