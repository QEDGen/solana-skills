use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use self::anchor_adapt::{looks_like_anchor, AnchorAdapter, HandlerOverride};
use self::pinocchio_to_spec::{NativeAdapter, PinocchioAdapter};
use self::program_model::{ProgramAdapter, ProgramFramework};

pub(crate) mod anchor_adapt;
pub(crate) mod anchor_check;
pub(crate) mod anchor_extractor;
pub(crate) mod anchor_project;
pub(crate) mod anchor_resolver;
pub(crate) mod arith_heuristics;
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
    // An `anchor-lang` dependency is an unambiguous, parse-independent Anchor
    // signal. Commit to the Anchor adapter up front so a malformed Anchor
    // crate surfaces the real Anchor parse error downstream rather than being
    // swallowed by the permissive native source-walk, which only regex-scans
    // for `pub fn` and would happily emit a wrong-shaped skeleton.
    if looks_like_anchor(root) {
        return Ok(adapter_for_framework(ProgramFramework::Anchor, config));
    }

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

    #[test]
    fn anchor_lang_dep_commits_to_anchor_adapter() {
        // Even with `pub fn` text the native source-walk would match, an
        // `anchor-lang` dependency pins detection to the Anchor adapter.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n\n[dependencies]\nanchor-lang = \"0.30\"\n",
        )
        .unwrap();
        write_src(dir.path(), "pub fn process_transfer() {}\n");
        let overrides = HashMap::new();
        let adapter = detect_adapter(dir.path(), AdapterConfig::new("demo", &overrides)).unwrap();

        assert_eq!(adapter.framework(), ProgramFramework::Anchor);
    }

    #[test]
    fn malformed_anchor_surfaces_parse_error_not_native_skeleton() {
        // An `anchor-lang` crate whose `lib.rs` can't be parsed as Rust must
        // fail loudly with the Anchor parse error — not silently emit a
        // native source-walk skeleton (the pre-fix regression).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n\n[dependencies]\nanchor-lang = \"0.30\"\n",
        )
        .unwrap();
        // Unterminated `#[program]` mod — valid-looking but syn rejects it.
        write_src(
            dir.path(),
            "use anchor_lang::prelude::*;\n\
             #[program]\n\
             pub mod demo {\n\
                 pub fn initialize(ctx: Context<Init>) -> Result<()> {\n",
        );
        let overrides = HashMap::new();
        let err = render_skeleton(dir.path(), AdapterConfig::new("demo", &overrides))
            .expect_err("malformed Anchor source must error, not render a skeleton");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("Anchor project") || msg.contains("Rust source"),
            "expected an Anchor parse error, got: {msg}"
        );
    }
}
