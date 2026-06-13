//! Shared test builders used by the colocated lint/check tests across
//! the `check` submodules. Compiled only under `#[cfg(test)]`.

use super::*;

pub(crate) fn empty_spec() -> ParsedSpec {
    ParsedSpec::default()
}

pub(crate) fn make_handler(name: &str) -> ParsedHandler {
    ParsedHandler {
        name: name.to_string(),
        doc: None,
        who: Some("authority".to_string()),
        on_account: None,
        pre_status: Some("Active".to_string()),
        post_status: Some("Active".to_string()),
        takes_params: vec![],
        guard_str: None,
        guard_str_rust: None,
        aborts_if: vec![],
        requires: vec![],
        ensures: vec![],
        modifies: None,
        let_bindings: vec![],
        aborts_total: false,
        permissionless: false,
        effects: vec![],
        effect_on_error: vec![],
        accounts: vec![],
        transfers: vec![],
        emits: vec![],
        invariants: vec![],
        establishes: vec![],
        properties: vec![],
        schema_includes: vec![],
        calls: vec![],
        effect_branches: None,
        abstract_binders: vec![],
    }
}
