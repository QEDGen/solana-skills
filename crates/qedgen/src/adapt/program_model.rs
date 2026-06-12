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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerModel {
    pub name: String,
    pub args: Vec<HandlerArgModel>,
    pub accounts_type: Option<String>,
    pub source_path: Option<PathBuf>,
    pub shape: HandlerShape,
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
