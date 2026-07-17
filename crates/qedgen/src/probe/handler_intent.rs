//! `qedgen probe --bootstrap` — per-handler intent classification.
//!
//! Labels each handler enumerated by `shank_probe.rs` with an intent tag
//! (`authority_gated` / `trader_gated` / `permissionless`) that filters
//! the global `applicable_categories` list per handler — e.g. an
//! `authority_gated` handler isn't worth walking for
//! `permissionless_state_writer`.
//!
//! Pure pattern recognition on the first ~30 body lines; semantic
//! interpretation stays with the agent. Heuristics are deliberately
//! narrow (false-negative biased): no explicit shape match → no tag → the
//! handler keeps the full category list. Tags can only *narrow* coverage,
//! never widen it, so the spec-less audit stays complete by default.
//! Precise rules: [`classify_handler_body`].

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use syn::Item;

/// Per-handler intent label. Only the strongest single label is emitted,
/// per [`classify_handler_body`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentTag {
    /// Body checks a signer pubkey equals a stored authority field, or
    /// calls an `assert_*authority*` helper.
    AuthorityGated,
    /// Body has a signer check but no named-authority comparison.
    TraderGated,
    /// Body has no signer / authority shape we can see.
    Permissionless,
}

impl IntentTag {
    pub fn as_str(self) -> &'static str {
        match self {
            IntentTag::AuthorityGated => "authority_gated",
            IntentTag::TraderGated => "trader_gated",
            IntentTag::Permissionless => "permissionless",
        }
    }
}

/// Body-line cap — past ~30 lines, dispatcher arms / loop bodies dilute
/// the signal; authority checks land in the first 5-15 lines.
const MAX_BODY_LINES: usize = 30;

/// Locate `entry_fn`'s source body under `<project_root>/src` (top-level
/// fns and impl methods, first match wins). Returns `(file_path, first
/// MAX_BODY_LINES lines of the block)`. Best-effort: unparseable files
/// are skipped; `None` when no match.
pub fn resolve_handler_body(entry_fn: &str, project_root: &Path) -> Option<(PathBuf, String)> {
    let src = project_root.join("src");
    if !src.is_dir() {
        return None;
    }
    let candidates = crate::fs_walk::collect_rs_files(&src, crate::fs_walk::DEFAULT_SKIP_DIRS);

    for file in candidates {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(syntax) = syn::parse_file(&source) else {
            continue;
        };
        if let Some(body) = find_fn_body_in_items(&syntax.items, entry_fn) {
            let excerpt = excerpt_lines(&body, MAX_BODY_LINES);
            return Some((file, excerpt));
        }
    }
    None
}

/// Recursively scan items (descending into `impl` and `mod`) for
/// `entry_fn`. Body is rendered via `quote::ToTokens` — close enough to
/// source for the classifier's line-pattern matching. `pub(crate)` for
/// the hypothesizer, which resolves bodies from a known source file.
pub(crate) fn find_fn_body_in_items(items: &[Item], entry_fn: &str) -> Option<String> {
    use quote::ToTokens;
    for item in items {
        match item {
            Item::Fn(f) if f.sig.ident == entry_fn => {
                return Some(f.block.to_token_stream().to_string());
            }
            Item::Impl(impl_block) => {
                for impl_item in &impl_block.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        if method.sig.ident == entry_fn {
                            return Some(method.block.to_token_stream().to_string());
                        }
                    }
                }
            }
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    if let Some(found) = find_fn_body_in_items(inner, entry_fn) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn excerpt_lines(text: &str, max: usize) -> String {
    text.lines().take(max).collect::<Vec<_>>().join("\n")
}

/// Classify a handler given its name and body excerpt. Returns
/// `None` when nothing matches (full global category list applies).
///
/// Rule order matters — checks against a stored authority field are
/// stronger evidence than just having an `is_signer` call.
pub fn classify_handler_body(handler_name: &str, body: &str) -> Option<IntentTag> {
    // Name prior — handlers named `process_set_admin` etc. rarely turn
    // out to be permissionless.
    let name_lower = handler_name.to_ascii_lowercase();
    let name_signals_authority = ["authority", "admin", "manager", "owner"]
        .iter()
        .any(|kw| name_lower.contains(kw));

    let has_authority_comparison = body_has_authority_comparison(body);
    let has_authority_assert = body_has_authority_assert(body);
    let has_signer_check = body_has_signer_check(body);

    if has_authority_comparison || has_authority_assert || name_signals_authority {
        return Some(IntentTag::AuthorityGated);
    }
    if has_signer_check {
        return Some(IntentTag::TraderGated);
    }
    // No signer machinery visible. Claim Permissionless only when the
    // body is non-trivial enough that absence is meaningful — a bare
    // `Ok(())` or one-line `msg!()` is an unfinished stub, not
    // permissionless; back off to untagged.
    if body_is_trivial(body) {
        return None;
    }
    Some(IntentTag::Permissionless)
}

/// Which authority-binding rule (if any) a body matches — the citable
/// anchor the hypothesizer needs, distinct from `classify_handler_body`
/// which also accepts a name prior (a name alone is not evidence).
pub(crate) fn authority_evidence(body: &str) -> Option<&'static str> {
    if body_has_authority_comparison(body) {
        Some("authority_comparison")
    } else if body_has_authority_assert(body) {
        Some("authority_assert_helper")
    } else {
        None
    }
}

/// Heuristic: pubkey compared against a stored authority-like field,
/// e.g. `if * signer . key != state . authority`. quote-rendered bodies
/// space tokens, so we match the unspaced form: co-occurrence of a `.key`
/// reference and a `.<authority-word>` field access. Both must be field
/// accesses — a bare local named `authority` doesn't count.
fn body_has_authority_comparison(body: &str) -> bool {
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let key_ref = unspaced.contains(".key");
    let authority_field = ["authority", "admin", "manager", "delegate", "owner"]
        .iter()
        .any(|f| unspaced.contains(&format!(".{}", f)));
    key_ref && authority_field
}

/// Heuristic: `assert_*_authority` / `check_authority`-style helper call —
/// the pre-Anchor canonical authority-check shape. Matched on the
/// whitespace-stripped call shape `name(`.
fn body_has_authority_assert(body: &str) -> bool {
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let needles = [
        "assert_valid_authority(",
        "assert_with_msg(", // pre-Anchor canonical: assert_with_msg(cond, ProgramError::...)
        "check_authority(",
        "verify_authority(",
        "assert_authority(",
        "require_authority(",
    ];
    needles.iter().any(|n| unspaced.contains(n))
}

/// Heuristic: `.is_signer` access or `Signer::try_from(...)` — trader-side
/// handlers that gate on signedness without comparing an authority pubkey.
fn body_has_signer_check(body: &str) -> bool {
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    unspaced.contains(".is_signer")
        || unspaced.contains("Signer::try_from(")
        || unspaced.contains("require!(")
            && (unspaced.contains("is_signer") || unspaced.contains("signer"))
}

/// True when the body is too small to distinguish permissionless intent
/// from an unfinished stub.
fn body_is_trivial(body: &str) -> bool {
    // Trivial = zero semicolons (single trailing expression) AND no
    // branching/binding keywords. `msg!("close"); Ok(())` counts as a
    // real body; bare `Ok(())` or a single `msg!(...)` does not.
    let inner = body.trim().trim_start_matches('{').trim_end_matches('}');
    let semis = inner.matches(';').count();
    if semis >= 1 {
        return false;
    }
    !inner.contains(" if ")
        && !inner.contains(" let ")
        && !inner.contains(" match ")
        && !inner.contains(" for ")
        && !inner.contains(" while ")
}

/// Filter the global category list by intent tag; `None` = no tag →
/// full list. Intentionally narrow: only categories the tag clearly
/// invalidates are excluded.
pub fn filter_categories(global: &[String], tag: Option<IntentTag>) -> Vec<String> {
    let Some(tag) = tag else {
        return global.to_vec();
    };

    let excluded: BTreeSet<&str> = match tag {
        IntentTag::AuthorityGated => {
            // Can't be permissionless-shape; the body checks authority
            // signedness by construction.
            [
                "permissionless_state_writer",
                "permissionless_create_account_dos",
            ]
            .into_iter()
            .collect()
        }
        IntentTag::TraderGated => {
            // Signer exists but isn't an admin authority: permissionless-DoS
            // shapes still apply (any signer can grief), and we have no
            // admin-only categories to exclude today.
            BTreeSet::new()
        }
        IntentTag::Permissionless => {
            // Nothing to sign — missing-signer can't apply.
            ["missing_signer"].into_iter().collect()
        }
    };

    global
        .iter()
        .filter(|c| !excluded.contains(c.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_comparison_classifies_authority_gated() {
        let body = r#"{
            if * signer . key != state . authority {
                return Err ( ProgramError :: MissingRequiredSignature ) ;
            }
            do_thing ( ) ;
        }"#;
        let tag = classify_handler_body("process_set_fee", body);
        assert_eq!(tag, Some(IntentTag::AuthorityGated));
    }

    #[test]
    fn assert_valid_authority_classifies_authority_gated() {
        let body = r#"{
            assert_valid_authority ( & accounts , & market . authority ) ? ;
            place_order ( ... ) ;
        }"#;
        let tag = classify_handler_body("process_collect_fees", body);
        assert_eq!(tag, Some(IntentTag::AuthorityGated));
    }

    #[test]
    fn name_signal_classifies_authority_gated() {
        // No authority shape in the body, but the name suggests it —
        // false negatives cost more than false positives here.
        let body = r#"{
            update_admin ( accounts , new_value ) ? ;
            Ok ( ( ) )
        }"#;
        let tag = classify_handler_body("process_change_admin", body);
        assert_eq!(tag, Some(IntentTag::AuthorityGated));
    }

    #[test]
    fn is_signer_only_classifies_trader_gated() {
        let body = r#"{
            let trader = next_account_info ( accounts_iter ) ? ;
            if ! trader . is_signer {
                return Err ( ProgramError :: MissingRequiredSignature ) ;
            }
            place_order ( trader , amount ) ;
        }"#;
        let tag = classify_handler_body("process_place_order", body);
        assert_eq!(tag, Some(IntentTag::TraderGated));
    }

    #[test]
    fn no_signer_no_authority_classifies_permissionless() {
        let body = r#"{
            let clock = Clock :: get ( ) ? ;
            state . last_tick = clock . unix_timestamp ;
            state . tick_count += 1 ;
            Ok ( ( ) )
        }"#;
        let tag = classify_handler_body("process_tick", body);
        assert_eq!(tag, Some(IntentTag::Permissionless));
    }

    #[test]
    fn two_stmt_body_classifies_permissionless() {
        // `msg!()` + `Ok(())` is the minimum shape considered
        // permissionless — real handler logic, even if just a print.
        let body = r#"{ msg ! ( "close" ) ; Ok ( ( ) ) }"#;
        let tag = classify_handler_body("process_close", body);
        assert_eq!(tag, Some(IntentTag::Permissionless));
    }

    #[test]
    fn bare_ok_body_left_untagged() {
        // Bare `Ok(())` is a stub — refuse the permissionless tag so the
        // auditor walks every category.
        let body = r#"{ Ok ( ( ) ) }"#;
        let tag = classify_handler_body("process_noop", body);
        assert_eq!(tag, None);
    }

    #[test]
    fn filter_categories_authority_gated_drops_permissionless_shapes() {
        let global: Vec<String> = [
            "missing_signer",
            "arithmetic_overflow_wrapping",
            "permissionless_state_writer",
            "permissionless_create_account_dos",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let filtered = filter_categories(&global, Some(IntentTag::AuthorityGated));
        assert!(filtered.iter().any(|c| c == "missing_signer"));
        assert!(filtered.iter().any(|c| c == "arithmetic_overflow_wrapping"));
        assert!(!filtered.iter().any(|c| c == "permissionless_state_writer"));
        assert!(!filtered
            .iter()
            .any(|c| c == "permissionless_create_account_dos"));
    }

    #[test]
    fn filter_categories_permissionless_drops_missing_signer() {
        let global: Vec<String> = ["missing_signer", "arithmetic_overflow_wrapping"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let filtered = filter_categories(&global, Some(IntentTag::Permissionless));
        assert!(!filtered.iter().any(|c| c == "missing_signer"));
        assert!(filtered.iter().any(|c| c == "arithmetic_overflow_wrapping"));
    }

    #[test]
    fn filter_categories_untagged_returns_full_list() {
        let global: Vec<String> = ["missing_signer", "arithmetic_overflow_wrapping"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let filtered = filter_categories(&global, None);
        assert_eq!(filtered, global);
    }

    #[test]
    fn resolve_handler_body_finds_top_level_fn() {
        let tmp = std::env::temp_dir().join("qedgen-intent-test-resolve");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src/processor")).unwrap();
        std::fs::write(
            tmp.join("src/processor/foo.rs"),
            r#"pub fn process_foo(_a: u64) -> Result<(), ()> {
    Ok(())
}
"#,
        )
        .unwrap();
        // Also put a noise file to make sure we skip cleanly.
        std::fs::write(tmp.join("src/processor/noise.rs"), "// no fn here\n").unwrap();

        let resolved = resolve_handler_body("process_foo", &tmp);
        assert!(resolved.is_some(), "expected to find process_foo");
        let (path, body) = resolved.unwrap();
        assert!(path.ends_with("foo.rs"));
        assert!(body.contains("Ok"), "body excerpt missing body: {body}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_handler_body_returns_none_when_absent() {
        let tmp = std::env::temp_dir().join("qedgen-intent-test-absent");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src/lib.rs"), "pub fn other() {}\n").unwrap();
        let resolved = resolve_handler_body("process_missing", &tmp);
        assert!(resolved.is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
