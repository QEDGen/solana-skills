//! `qedgen probe --bootstrap` — per-handler intent classification
//! (v2.20 §S2.2).
//!
//! Once the Shank dispatcher walker (`shank_probe.rs`) has enumerated
//! the `process_*` handler entries, this module reads each handler's
//! source body and labels it with one or more **intent tags**
//! (`authority_gated`, `permissionless`, `trader_gated`). The tags
//! drive a per-handler filter on the global `applicable_categories`
//! list — an `authority_gated` handler isn't worth walking for
//! `permissionless_state_writer`, while a `permissionless` handler
//! genuinely is.
//!
//! **Approach.** Pure pattern recognition on the handler's first
//! ~30 lines. Per `feedback_agent_lsp_substrate`, semantic
//! interpretation is the agent's job; this module only emits a
//! candidate label set the auditor can refine downstream.
//!
//! The heuristics are deliberately *narrow* (false-negative biased):
//! when we can't see an explicit shape match, we emit no tag rather
//! than guess, and the handler keeps the full global category list.
//! That keeps the spec-less audit complete by default; tags can only
//! *narrow* coverage, never widen it.
//!
//! Heuristics — see [`classify_handler_body`] for the precise rules:
//!
//! - **authority_gated** — body contains a pubkey comparison against a
//!   stored authority field (e.g. `if *signer.key != state.authority`)
//!   or a call to `assert_*authority*`/`assert_valid_*authority*`.
//!   Also: handler name carries `authority` / `admin` / `manager`.
//! - **trader_gated** — body has an `is_signer` check (`signer.is_signer`,
//!   `assert!(... is_signer ...)`) but no named-authority comparison.
//!   The signer's identity is open.
//! - **permissionless** — body has zero signer checks AND zero
//!   authority comparisons.
//!
//! Untagged is a real outcome: nothing matched, so leave the handler
//! with the global category list.

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use syn::Item;

/// Per-handler intent label. Multiple tags can apply (e.g. an
/// authority-gated handler is also "has signer check"), but we emit
/// only the strongest single label per [`classify_handler_body`].
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

/// Read at most `MAX_BODY_LINES` lines of a handler's body — past
/// that, dispatcher arms / loop bodies start to dilute the signal.
/// Phoenix-shape handlers do their authority check in the first 5-15
/// lines; 30 is a generous ceiling.
const MAX_BODY_LINES: usize = 30;

/// Try to locate the source body of `entry_fn` somewhere under
/// `project_root`. Returns `(file_path, body_excerpt)` where
/// `body_excerpt` is the first `MAX_BODY_LINES` lines of the fn's
/// `{ ... }` block (caller-side trimming friendly).
///
/// Walks every `.rs` file under `<project_root>/src`. Parses each
/// with `syn`, looks for a top-level `fn <entry_fn>` OR an
/// `impl ... { fn <entry_fn> }`. Returns the first match.
///
/// Best-effort: an unparseable file just gets skipped. Returns
/// `None` when no file under `src/` contains a matching fn.
pub fn resolve_handler_body(entry_fn: &str, project_root: &Path) -> Option<(PathBuf, String)> {
    let src = project_root.join("src");
    if !src.is_dir() {
        return None;
    }
    let mut candidates = Vec::new();
    collect_rs_files(&src, &mut candidates);

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

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, "target" | ".qed" | "formal_verification") {
            continue;
        }
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Recursively scan items for a fn named `entry_fn`, descending into
/// `impl` blocks and `mod` bodies. Returns the body text rendered via
/// `quote::ToTokens` — close enough to the original for line-by-line
/// pattern matching, which is what the classifier needs.
fn find_fn_body_in_items(items: &[Item], entry_fn: &str) -> Option<String> {
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
    // Name-based prior — handlers literally called `process_set_admin`
    // or `process_collect_authority_fees` rarely turn out to be
    // permissionless. Confirms the strongest signal we can get
    // without semantic analysis.
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
    // No signer machinery visible. We claim Permissionless only when the
    // body is non-trivial enough that absence is meaningful — a handler
    // that's literally `Ok(())` or one-line `msg!()` isn't really
    // "permissionless", it's just unfinished. Detect a trivial body
    // (≤ 2 statements after token-stream normalisation) and back off
    // to "untagged" in that case.
    if body_is_trivial(body) {
        return None;
    }
    Some(IntentTag::Permissionless)
}

/// Heuristic: body contains a comparison of a pubkey against a stored
/// authority-like field. Token-stream rendered bodies look like:
///   `if * signer . key != state . authority { ... }`
/// or
///   `assert_eq ! (signer . key , market . authority)`
///
/// We're matching on the *concatenation* `.authority` /
/// `.admin` / `.owner_pubkey` etc. paired with a `.key` reference. The
/// quote-rendered form spaces tokens, so we look at the unspaced version.
fn body_has_authority_comparison(body: &str) -> bool {
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    // `.key` references in proximity to a `.authority` field access.
    // We use substring co-occurrence — both must appear and both
    // must be field accesses (preceded by `.`). A bare local var
    // named `authority` doesn't count.
    let key_ref = unspaced.contains(".key");
    let authority_field = ["authority", "admin", "manager", "delegate", "owner"]
        .iter()
        .any(|f| unspaced.contains(&format!(".{}", f)));
    key_ref && authority_field
}

/// Heuristic: explicit `assert_valid_authority` / `assert_*_authority`
/// / `check_authority`-style helper invocation. These are the
/// pre-Anchor canonical authority-check shape.
fn body_has_authority_assert(body: &str) -> bool {
    // Token stream form: `assert_valid_authority ( ... )`. We strip
    // whitespace and look for the function-call shape `name(` with a
    // matching keyword in its identifier.
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

/// Heuristic: body references `.is_signer` (field access on an
/// `AccountInfo`) or invokes `Signer::try_from(...)` (the dispatcher
/// adapters land here). Matches Phoenix-style trader-side handlers
/// that gate on signedness but don't compare against an authority pubkey.
fn body_has_signer_check(body: &str) -> bool {
    let unspaced: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    unspaced.contains(".is_signer")
        || unspaced.contains("Signer::try_from(")
        || unspaced.contains("require!(")
            && (unspaced.contains("is_signer") || unspaced.contains("signer"))
}

/// True when the body has so little going on that we can't conclude
/// permissionless intent from it (vs an unfinished stub).
fn body_is_trivial(body: &str) -> bool {
    // Token-stream form starts/ends with `{` / `}`. We strip the
    // outer braces and consider the body trivial if it has zero
    // semicolons (single trailing expression) AND no `if` / `let` /
    // `match` keyword (no branching, no bindings — just a literal
    // or a single call). This lets a body like `msg!("close"); Ok(())`
    // count as permissionless (one stmt + trailing return = real
    // handler body) while keeping bare `Ok(())` or single `msg!(...)`
    // out of the classifier's confidence range.
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

/// Filter the global category list for a handler with the given
/// intent tag. `None` means "no tag inferred — return the full list".
///
/// The mapping is intentionally narrow per v2.20 PRD §S2.2: we filter
/// only the categories where the tag clearly invalidates investigation.
/// Anything else stays in the candidate list.
pub fn filter_categories(global: &[String], tag: Option<IntentTag>) -> Vec<String> {
    let Some(tag) = tag else {
        return global.to_vec();
    };

    let excluded: BTreeSet<&str> = match tag {
        IntentTag::AuthorityGated => {
            // An authority-gated handler can't be "permissionless" in
            // any of the permissionless-shape categories. Missing-signer
            // is also off the table — by construction the body checks
            // authority signedness.
            [
                "permissionless_state_writer",
                "permissionless_create_account_dos",
            ]
            .into_iter()
            .collect()
        }
        IntentTag::TraderGated => {
            // Trader-gated: a signer exists but isn't an admin authority.
            // Permissionless-DoS shapes still apply (any signer counts
            // as a griefer); admin-authority bypass categories don't.
            // We don't have any "admin only" categories to exclude
            // explicitly today — leave the list as-is for now.
            BTreeSet::new()
        }
        IntentTag::Permissionless => {
            // Permissionless handler can't have a missing-signer bug
            // (there's nothing to sign); authority comparisons aren't
            // relevant either.
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
        // Body itself doesn't show authority shape, but the handler
        // name strongly suggests it. We still tag authority_gated —
        // false negatives here cost more than false positives.
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
        // A `msg!()` followed by `Ok(())` is the minimum shape we
        // consider permissionless — there's real handler logic
        // (logging the call) even if it's just a print.
        let body = r#"{ msg ! ( "close" ) ; Ok ( ( ) ) }"#;
        let tag = classify_handler_body("process_close", body);
        assert_eq!(tag, Some(IntentTag::Permissionless));
    }

    #[test]
    fn bare_ok_body_left_untagged() {
        // Bare `Ok(())` body is a stub — classifier should refuse to
        // claim a permissionless tag for it, leaving the auditor to
        // walk every category.
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
