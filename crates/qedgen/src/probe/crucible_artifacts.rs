//! Generation-input freshness without hashing user-maintained harness source.
use anyhow::{bail, Context, Result};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const MANIFEST: &str = ".qedgen/generation-inputs.json";

/// Supply deterministic serialized effective inputs (including resolved imports,
/// overlay/plans and deployment context when applicable), never source mtimes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationInputs {
    version: u32,
    mode: String,
    spec_sha256: Option<String>,
    idl_sha256: Option<String>,
    bindings_sha256: Option<String>,
    action_layout_sha256: Option<String>,
}

impl GenerationInputs {
    pub fn new(
        mode: &str,
        spec: Option<&[u8]>,
        idl: Option<&[u8]>,
        bindings: Option<&[u8]>,
    ) -> Self {
        fn digest(bytes: &[u8]) -> String {
            format!("{:x}", Sha256::digest(bytes))
        }
        Self {
            version: 1,
            mode: mode.to_owned(),
            spec_sha256: spec.map(digest),
            idl_sha256: idl.map(digest),
            bindings_sha256: bindings.map(digest),
            action_layout_sha256: None,
        }
    }
}

/// Domain seeds use the generated action ordinal and parameter order. Body
/// edits are allowed, but manual signature or order edits invalidate seeds.
fn action_layout_sha256(dir: &Path) -> Result<Option<String>> {
    let path = dir.join("src/main.rs");
    if !path.exists() {
        return Ok(None);
    }
    let source = std::fs::read_to_string(&path)?;
    let file = syn::parse_file(&source)
        .with_context(|| format!("parse Crucible harness {}", path.display()))?;
    let signatures: Vec<String> = file
        .items
        .into_iter()
        .filter_map(|item| match item {
            syn::Item::Impl(item) => Some(item.items),
            _ => None,
        })
        .flatten()
        .filter_map(|item| match item {
            syn::ImplItem::Fn(function)
                if function.sig.ident.to_string().starts_with("action_") =>
            {
                Some(function.sig.to_token_stream().to_string())
            }
            _ => None,
        })
        .collect();
    Ok(Some(format!(
        "{:x}",
        Sha256::digest(signatures.join("\n").as_bytes())
    )))
}

/// Call only after fresh successful generation, never to bless an existing harness.
pub fn record_generation(dir: &Path, inputs: &GenerationInputs) -> Result<()> {
    let path = dir.join(MANIFEST);
    std::fs::create_dir_all(path.parent().expect("manifest parent"))?;
    let mut recorded = inputs.clone();
    recorded.action_layout_sha256 = action_layout_sha256(dir)?;
    std::fs::write(&path, serde_json::to_vec_pretty(&recorded)?)
        .with_context(|| format!("write Crucible generation inputs {}", path.display()))
}

/// Must run before smoke, fuzz or replay on every reused harness.
pub fn validate_generation(dir: &Path, inputs: &GenerationInputs) -> Result<()> {
    let path = dir.join(MANIFEST);
    let bytes = std::fs::read(&path).with_context(|| format!("Crucible harness has no readable freshness manifest at {}; preserve manual edits, then explicitly regenerate the harness in a new directory", path.display()))?;
    let recorded: GenerationInputs = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "Invalid Crucible freshness manifest {}; preserve manual edits and regenerate",
            path.display()
        )
    })?;
    let mut expected = inputs.clone();
    expected.action_layout_sha256 = action_layout_sha256(dir)?;
    if recorded != expected {
        bail!("Stale Crucible harness: generation inputs (spec, mode/context, IDL or bindings) changed; preserve manual edits and explicitly regenerate in a new directory before smoke, fuzz or replay");
    }
    Ok(())
}
