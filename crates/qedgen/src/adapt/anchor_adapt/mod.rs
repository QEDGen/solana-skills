//! Brownfield adapter: emit a starter `.qedspec` for an existing Anchor crate.
//!
//! Pipeline: `anchor_project` lists `#[program]` instructions; `anchor_resolver`
//! follows each forwarder to its handler ItemFn (or Unrecognized); this module
//! renders a parseable skeleton with `// TODO:` markers. Output is round-tripped
//! through `chumsky_adapter::parse_str` so renderer bugs surface at adapt-time.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::anchor_project::{parse_anchor_project, Instruction};
use crate::anchor_resolver::{resolve_handler, HandlerLocation};
use crate::program_model::{
    AccountRoleModel, ErrorModel, HandlerArgModel, HandlerModel, HandlerShape, ProgramAdapter,
    ProgramFramework, ProgramModel, StateModel,
};

mod adapter;
mod attributes;
mod extract;
mod handler_override;
mod render;
#[cfg(test)]
mod tests;

// Sibling visibility: each submodule does `use super::*`, so these re-globs let
// the buckets (and the test module) see one another's items. The consumers are
// the child modules, which rustc's unused-import pass doesn't credit here.
#[allow(unused_imports)]
use adapter::*;
#[allow(unused_imports)]
use attributes::*;
#[allow(unused_imports)]
use extract::*;
#[allow(unused_imports)]
use handler_override::*;
#[allow(unused_imports)]
use render::*;

// External surface — preserve the existing `crate::anchor_adapt::<X>` /
// `self::anchor_adapt::<X>` paths at their current visibility. Some re-exports
// have no cross-module consumer today but are kept `pub` deliberately.
pub(crate) use adapter::{looks_like_anchor, AnchorAdapter};
#[allow(unused_imports)]
pub use attributes::{compute_attributes, render_attributes, AttributeEntry};
pub use handler_override::parse_handler_override;
pub(crate) use handler_override::HandlerOverride;

#[allow(unused_imports)]
pub use adapter::{adapt, adapt_to_file, extract_program_model};
