use super::*;

/// Per-handler override naming the real implementation when the classifier
/// can't follow a forwarder (custom dispatchers). Path parses like a free-fn
/// forwarder: `module::sub::function`, fn name last.
#[derive(Debug, Clone)]
pub struct HandlerOverride {
    pub module_path: Vec<String>,
    pub fn_name: String,
}

impl HandlerOverride {
    /// `module::sub::function` → override; bare `function` → empty module
    /// path. None on empty input or empty segment.
    pub fn parse(rust_path: &str) -> Option<Self> {
        let trimmed = rust_path.trim();
        if trimmed.is_empty() {
            return None;
        }
        let mut segments: Vec<String> = trimmed.split("::").map(|s| s.trim().to_string()).collect();
        if segments.iter().any(|s| s.is_empty()) {
            return None;
        }
        let fn_name = segments.pop()?;
        Some(HandlerOverride {
            module_path: segments,
            fn_name,
        })
    }
}

/// Parse one `--handler <name>=<rust_path>` CLI value into
/// `(handler_name, override)`; errors on malformed input.
pub fn parse_handler_override(value: &str) -> Result<(String, HandlerOverride)> {
    let (name, path) = value.split_once('=').ok_or_else(|| {
        anyhow::anyhow!(
            "expected `<handler>=<rust_path>` for `--handler`, got `{}`",
            value
        )
    })?;
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("`--handler` value `{}` has empty handler name", value);
    }
    let rust_override = HandlerOverride::parse(path).ok_or_else(|| {
        anyhow::anyhow!(
            "`--handler {}=<path>` rust path is empty or has empty segments",
            name
        )
    })?;
    Ok((name.to_string(), rust_override))
}
