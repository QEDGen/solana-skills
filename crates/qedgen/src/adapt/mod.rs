use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use self::anchor_adapt::{AnchorAdapter, HandlerOverride};
use self::pinocchio_to_spec::{NativeAdapter, PinocchioAdapter};
use self::program_model::{ProgramAdapter, ProgramFramework};

pub(crate) mod anchor_adapt;
pub(crate) mod anchor_check;
pub(crate) mod anchor_extractor;
pub(crate) mod anchor_project;
pub(crate) mod anchor_resolver;
pub(crate) mod native_extractor;
pub(crate) mod pinocchio_extractor;
pub(crate) mod pinocchio_profile;
pub(crate) mod pinocchio_to_spec;
pub(crate) mod program_model;

#[derive(Clone, Copy)]
pub(crate) struct AdapterConfig<'a> {
    pub program_name: &'a str,
    pub anchor_overrides: &'a HashMap<String, HandlerOverride>,
}

impl<'a> AdapterConfig<'a> {
    pub(crate) fn new(
        program_name: &'a str,
        anchor_overrides: &'a HashMap<String, HandlerOverride>,
    ) -> Self {
        Self {
            program_name,
            anchor_overrides,
        }
    }
}

pub(crate) fn default_program_name(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("program")
        .to_string()
}

pub(crate) fn adapter_for_framework<'a>(
    framework: ProgramFramework,
    config: AdapterConfig<'a>,
) -> Box<dyn ProgramAdapter + 'a> {
    match framework {
        ProgramFramework::Anchor => Box::new(AnchorAdapter::new(config.anchor_overrides)),
        ProgramFramework::Pinocchio => Box::new(PinocchioAdapter::new(config.program_name)),
        ProgramFramework::Native => Box::new(NativeAdapter::new(config.program_name)),
    }
}

pub(crate) fn detect_adapter<'a>(
    root: &Path,
    config: AdapterConfig<'a>,
) -> Result<Box<dyn ProgramAdapter + 'a>> {
    let mut checked = Vec::new();
    for framework in [
        ProgramFramework::Anchor,
        ProgramFramework::Pinocchio,
        ProgramFramework::Native,
    ] {
        let adapter = adapter_for_framework(framework, config);
        checked.push(format!("{:?}", adapter.framework()));
        if adapter.detect(root) {
            return Ok(adapter);
        }
    }

    anyhow::bail!(
        "no supported program adapter detected for {} (checked: {})",
        root.display(),
        checked.join(", ")
    )
}

pub(crate) fn render_skeleton(root: &Path, config: AdapterConfig<'_>) -> Result<String> {
    detect_adapter(root, config)?.adapt(root)
}

pub(crate) fn render_skeleton_for_framework(
    framework: ProgramFramework,
    root: &Path,
    config: AdapterConfig<'_>,
) -> Result<String> {
    adapter_for_framework(framework, config).adapt(root)
}

pub(crate) fn render_skeleton_to_file(
    root: &Path,
    output_path: &Path,
    config: AdapterConfig<'_>,
) -> Result<()> {
    let rendered = render_skeleton(root, config)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    std::fs::write(output_path, &rendered)
        .with_context(|| format!("writing {}", output_path.display()))?;
    eprintln!("Wrote {} ({} bytes)", output_path.display(), rendered.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_src(root: &Path, source: &str) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), source).unwrap();
    }

    #[test]
    fn detects_anchor_adapter() {
        let dir = tempfile::tempdir().unwrap();
        write_src(
            dir.path(),
            r#"
            use anchor_lang::prelude::*;

            #[program]
            pub mod demo {
                use super::*;
                pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
            }
            "#,
        );
        let overrides = HashMap::new();
        let adapter = detect_adapter(dir.path(), AdapterConfig::new("demo", &overrides)).unwrap();

        assert_eq!(adapter.framework(), ProgramFramework::Anchor);
    }

    #[test]
    fn detects_pinocchio_before_native() {
        let dir = tempfile::tempdir().unwrap();
        write_src(
            dir.path(),
            "pub fn process_transfer(accounts: &[AccountInfo]) -> ProgramResult { Ok(()) }\n",
        );
        let overrides = HashMap::new();
        let adapter =
            detect_adapter(dir.path(), AdapterConfig::new("p-token", &overrides)).unwrap();

        assert_eq!(adapter.framework(), ProgramFramework::Pinocchio);
    }

    #[test]
    fn detects_native_adapter() {
        let dir = tempfile::tempdir().unwrap();
        write_src(dir.path(), "pub fn initialize() {}\n");
        let overrides = HashMap::new();
        let adapter =
            detect_adapter(dir.path(), AdapterConfig::new("native-program", &overrides)).unwrap();

        assert_eq!(adapter.framework(), ProgramFramework::Native);
    }
}
