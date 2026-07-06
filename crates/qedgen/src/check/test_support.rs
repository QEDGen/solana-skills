//! Shared test builders used by the colocated lint/check tests across
//! the `check` submodules. Compiled only under `#[cfg(test)]`.

use super::*;

pub(crate) fn empty_spec() -> ParsedSpec {
    ParsedSpec::default()
}

pub(crate) fn make_handler(name: &str) -> ParsedHandler {
    ParsedHandler {
        name: name.to_string(),
        who: Some("authority".to_string()),
        pre_status: Some("Active".to_string()),
        post_status: Some("Active".to_string()),
        ..Default::default()
    }
}
