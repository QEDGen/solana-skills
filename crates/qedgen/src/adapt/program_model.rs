//! Runtime-neutral program model for brownfield adapters.
//!
//! Framework-specific extractors should lower source code into this shape
//! before rendering `.qedspec` skeletons or computing adapter metadata. The
//! model intentionally stays close to source facts: handler names, argument
//! types where known, source breadcrumbs, account bindings, and discovered
//! error enums.

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramFramework {
    Anchor,
    Pinocchio,
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramModel {
    pub framework: ProgramFramework,
    /// Source-facing program name. Anchor uses the `#[program] mod` name;
    /// Pinocchio/native adapters use their project/program name.
    pub name: String,
    /// Primary source file, relative to the project root when possible.
    pub primary_source: Option<PathBuf>,
    /// Framework entry module/name when one exists (`#[program] mod foo`).
    pub entry_module: Option<String>,
    pub handlers: Vec<HandlerModel>,
    pub errors: Option<ErrorModel>,
    /// Lifecycle state machine derived from an `#[account]` struct's status-enum
    /// field (e.g. `Proposal.status: ProposalStatus`). `None` when no account
    /// carries a program-defined enum field — the renderer then emits the flat
    /// `Init | Active` placeholder.
    pub state: Option<StateModel>,
}

impl ProgramModel {
    pub fn new(framework: ProgramFramework, name: impl Into<String>) -> Self {
        Self {
            framework,
            name: name.into(),
            primary_source: None,
            entry_module: None,
            handlers: Vec::new(),
            errors: None,
            state: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerModel {
    pub name: String,
    pub args: Vec<HandlerArgModel>,
    pub accounts_type: Option<String>,
    /// Per-account roles resolved from the handler's `#[derive(Accounts)]`
    /// struct (signer / writable / program / typed). Empty when the struct
    /// couldn't be resolved — the renderer then falls back to a `TODO`.
    pub accounts: Vec<AccountRoleModel>,
    pub source_path: Option<PathBuf>,
    pub shape: HandlerShape,
}

/// One account field of a handler's `#[derive(Accounts)]` struct, reduced to
/// the qedspec `accounts { }` attributes mechanically derivable from its Anchor
/// type + `#[account(...)]` constraints. `attrs` are already rendered as
/// DSL tokens (`signer`, `writable`, `program`, `type <T>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRoleModel {
    pub name: String,
    pub attrs: Vec<String>,
    /// True when this field is an Anchor `Signer<'info>` — used to seed the
    /// handler's `auth` clause when there is exactly one signer.
    pub is_signer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerArgModel {
    pub name: String,
    /// qedspec type name when the extractor can map the source type. `None`
    /// means the renderer should emit a parseable placeholder and a TODO.
    pub qedspec_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerShape {
    Inline,
    FreeFn,
    Method { impl_type: String },
    SourceWalk,
    Unrecognized { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorModel {
    pub source_path: Option<PathBuf>,
    pub enum_name: String,
    pub variants: Vec<String>,
}

/// A program-defined status enum carried by an `#[account]` struct field — the
/// mechanically-derivable seed for the skeleton's `type State`. The transition
/// *edges* still need the impl, so only the variant *set* is derived here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateModel {
    pub source_path: Option<PathBuf>,
    /// The enum type (e.g. `ProposalStatus`).
    pub enum_name: String,
    pub variants: Vec<String>,
    /// The `#[account]` struct and field the enum was found on
    /// (e.g. `Proposal` / `status`) — for the provenance comment.
    pub account_struct: String,
    pub field_name: String,
}

pub trait ProgramAdapter {
    fn framework(&self) -> ProgramFramework;
    fn detect(&self, root: &Path) -> bool;
    fn extract(&self, root: &Path) -> Result<ProgramModel>;
    fn render_spec(&self, model: &ProgramModel) -> Result<String>;

    fn adapt(&self, root: &Path) -> Result<String> {
        let model = self.extract(root)?;
        self.render_spec(&model)
    }
}
