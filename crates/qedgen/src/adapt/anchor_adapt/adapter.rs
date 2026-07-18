use super::*;

pub struct AnchorAdapter<'a> {
    overrides: &'a HashMap<String, HandlerOverride>,
}

impl<'a> AnchorAdapter<'a> {
    pub fn new(overrides: &'a HashMap<String, HandlerOverride>) -> Self {
        Self { overrides }
    }
}

impl ProgramAdapter for AnchorAdapter<'_> {
    fn framework(&self) -> ProgramFramework {
        ProgramFramework::Anchor
    }

    fn detect(&self, root: &Path) -> bool {
        parse_anchor_project(root).is_ok()
    }

    fn extract(&self, root: &Path) -> Result<ProgramModel> {
        extract_program_model(root, self.overrides)
    }

    fn render_spec(&self, model: &ProgramModel) -> Result<String> {
        Ok(render_spec(model))
    }

    fn adapt(&self, root: &Path) -> Result<String> {
        let model = self.extract(root)?;
        let rendered = self.render_spec(&model)?;

        // Round-trip: a parse failure here is a renderer bug, not user input.
        crate::chumsky_adapter::parse_str(&rendered).context(
            "Generated .qedspec failed to parse — this is a bug in `qedgen adapt`. \
             Please report at https://github.com/qedgen/solana-skills/issues",
        )?;

        Ok(rendered)
    }
}

/// Parse-independent "is this an Anchor crate?" check: an `anchor-lang`
/// dependency in the crate's `Cargo.toml`. Adapter detection consults this so
/// a malformed Anchor program surfaces the real Anchor parse error instead of
/// being swallowed by the permissive native source-walk (which regex-scans
/// for `pub fn` and would emit a wrong-shaped skeleton).
pub(crate) fn looks_like_anchor(program_root: &Path) -> bool {
    std::fs::read_to_string(program_root.join("Cargo.toml"))
        .map(|s| s.contains("anchor-lang"))
        .unwrap_or(false)
}

/// Generate a starter `.qedspec` for the Anchor program at `program_root`
/// (the crate dir holding `src/`). `overrides` points unrecognized handlers
/// at their actual implementation.
#[cfg_attr(not(test), allow(dead_code))] // production goes through the FrameworkAdapter trait; kept as the test entry
pub fn adapt(program_root: &Path, overrides: &HashMap<String, HandlerOverride>) -> Result<String> {
    let adapter = AnchorAdapter::new(overrides);
    adapter.adapt(program_root)
}

/// Extract an Anchor program into the neutral brownfield adapter model.
pub fn extract_program_model(
    program_root: &Path,
    overrides: &HashMap<String, HandlerOverride>,
) -> Result<ProgramModel> {
    let project = parse_anchor_project(program_root).with_context(|| {
        format!(
            "failed to parse Anchor project at {}",
            program_root.display()
        )
    })?;

    let mut model = ProgramModel::new(ProgramFramework::Anchor, project.program_mod_name.clone());
    model.primary_source = Some(rel_to(program_root, &project.lib_rs_path));
    model.entry_module = Some(project.program_mod_name.clone());
    model.handlers = Vec::with_capacity(project.instructions.len());

    for instruction in &project.instructions {
        let location = resolve_with_override(
            instruction,
            &project.lib_rs_path,
            program_root,
            overrides.get(&instruction.name),
        )?;
        model.handlers.push(handler_model_from_anchor(
            instruction,
            &location,
            program_root,
        ));
    }

    model.errors = discover_error_enum(program_root);
    model.state = discover_state_enum(program_root);
    Ok(model)
}

/// Convenience wrapper: write the adapted `.qedspec` to disk.
#[cfg_attr(not(test), allow(dead_code))] // production goes through the FrameworkAdapter trait; kept as the test entry
pub fn adapt_to_file(
    program_root: &Path,
    output_path: &Path,
    overrides: &HashMap<String, HandlerOverride>,
) -> Result<()> {
    let rendered = adapt(program_root, overrides)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    std::fs::write(output_path, &rendered)
        .with_context(|| format!("writing {}", output_path.display()))?;
    eprintln!("Wrote {} ({} bytes)", output_path.display(), rendered.len());
    Ok(())
}

/// Resolve a handler; a supplied CLI override always wins. Overrides cover
/// what the classifier can't reach: `Unrecognized` forwarders (custom
/// dispatchers), multi-stmt forwarders conservatively classified `Inline`,
/// and walks that landed on the wrong file. The override is treated as a
/// free-fn forwarder: walk `src/` for `pub fn <name>` at its module path.
pub(super) fn resolve_with_override(
    instruction: &Instruction,
    lib_rs_path: &Path,
    program_root: &Path,
    override_: Option<&HandlerOverride>,
) -> Result<HandlerLocation> {
    if let Some(o) = override_ {
        return crate::anchor_resolver::resolve_free_fn(
            &o.module_path,
            &o.fn_name,
            program_root,
            lib_rs_path,
        );
    }
    resolve_handler(instruction, lib_rs_path, program_root)
}
