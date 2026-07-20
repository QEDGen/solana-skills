use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::check::ParsedSpec;

/// Per-file section hashes derived from the spec.
/// Each generated file depends on a specific subset of ParsedSpec fields.
/// Changing an event should not mark instruction files as stale.
pub struct SpecFingerprint {
    /// filename → sha256 hex string (first 16 chars for brevity)
    pub file_hashes: BTreeMap<String, String>,
}

/// SHA-256 hash of a canonical string, truncated to 16 hex chars.
fn section_hash(canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let full = format!("{:x}", hasher.finalize());
    full[..16].to_string()
}

/// Compute per-file fingerprints from a parsed spec.
pub fn compute_fingerprint(spec: &ParsedSpec) -> SpecFingerprint {
    let mut hashes = BTreeMap::new();

    // src/lib.rs — program shell, dispatch
    {
        let mut c = String::new();
        c.push_str(&format!("name={}\n", spec.program_name));
        c.push_str(&format!(
            "id={}\n",
            spec.program_id.as_deref().unwrap_or("")
        ));
        for handler in &spec.handlers {
            c.push_str(&format!("op={}|", handler.name));
            for (pn, pt) in &handler.takes_params {
                c.push_str(&format!("{}:{},", pn, pt));
            }
            c.push('\n');
        }
        c.push_str(&format!("has_events={}\n", !spec.events.is_empty()));
        c.push_str(&format!("has_errors={}\n", !spec.error_codes.is_empty()));
        hashes.insert("src/lib.rs".to_string(), section_hash(&c));
    }

    // src/state.rs — account struct, lifecycle enum, PDA seeds
    {
        let mut c = String::new();
        c.push_str(&format!("name={}\n", spec.program_name));
        for (fname, ftype) in &spec.state_fields {
            c.push_str(&format!("field={}:{}\n", fname, ftype));
        }
        for state in &spec.lifecycle_states {
            c.push_str(&format!("status={}\n", state));
        }
        for pda in &spec.pdas {
            c.push_str(&format!("pda={}|{}\n", pda.name, pda.seeds.join(",")));
        }
        // Hash the per-variant payload structure of multi-variant ADT
        // account types — the flat `state_fields` union alone misses
        // variant restructuring, so state.rs would never look stale.
        for acct in &spec.account_types {
            if acct.variants.len() <= 1 {
                continue;
            }
            c.push_str(&format!("variant_state_acct={}\n", acct.name));
            for variant in &acct.variants {
                c.push_str(&format!("  variant={}\n", variant.name));
                for (fname, ftype) in &variant.fields {
                    c.push_str(&format!("    {}:{}\n", fname, ftype));
                }
            }
        }
        hashes.insert("src/state.rs".to_string(), section_hash(&c));
    }

    // src/events.rs
    if !spec.events.is_empty() {
        let mut c = String::new();
        for ev in &spec.events {
            c.push_str(&format!("event={}\n", ev.name));
            for (fname, ftype) in &ev.fields {
                c.push_str(&format!("  {}:{}\n", fname, ftype));
            }
        }
        hashes.insert("src/events.rs".to_string(), section_hash(&c));
    }

    // src/errors.rs
    if !spec.error_codes.is_empty() {
        let mut c = String::new();
        c.push_str(&format!("name={}\n", spec.program_name));
        for code in &spec.error_codes {
            c.push_str(&format!("error={}\n", code));
        }
        hashes.insert("src/errors.rs".to_string(), section_hash(&c));
    }

    // src/instructions/{handler}.rs — one hash per handler
    for handler in &spec.handlers {
        let mut c = String::new();
        c.push_str(&format!("name={}\n", spec.program_name));
        c.push_str(&canonical_handler(handler));
        c.push_str(&canonical_accounts(handler));
        hashes.insert(
            format!("src/instructions/{}.rs", handler.name),
            section_hash(&c),
        );
    }

    // src/instructions/mod.rs
    {
        let mut c = String::new();
        for handler in &spec.handlers {
            c.push_str(&format!("mod={}\n", handler.name));
        }
        hashes.insert("src/instructions/mod.rs".to_string(), section_hash(&c));
    }

    // Cargo.toml
    {
        let needs_spl = spec.handlers.iter().any(|h| h.has_token_accounts());
        let c = format!("name={}\nneeds_spl={}\n", spec.program_name, needs_spl);
        hashes.insert("Cargo.toml".to_string(), section_hash(&c));
    }

    // tests/unit.rs — unit tests depend on state, operations, properties
    {
        let mut c = String::new();
        for (fname, ftype) in &spec.state_fields {
            c.push_str(&format!("state={}:{}\n", fname, ftype));
        }
        for handler in &spec.handlers {
            c.push_str(&canonical_handler(handler));
        }
        for prop in &spec.properties {
            c.push_str(&format!(
                "prop={}|expr={}|{}\n",
                prop.name,
                prop.expression.as_deref().unwrap_or(""),
                prop.preserved_by.join(",")
            ));
        }
        for state in &spec.lifecycle_states {
            c.push_str(&format!("status={}\n", state));
        }
        hashes.insert("tests/unit.rs".to_string(), section_hash(&c));
    }

    // tests/kani.rs — depends on everything that generates harnesses
    {
        let mut c = String::new();
        for (fname, ftype) in &spec.state_fields {
            c.push_str(&format!("state={}:{}\n", fname, ftype));
        }
        for handler in &spec.handlers {
            c.push_str(&canonical_handler(handler));
        }
        for prop in &spec.properties {
            c.push_str(&format!(
                "prop={}|{}\n",
                prop.name,
                prop.preserved_by.join(",")
            ));
        }
        hashes.insert("tests/kani.rs".to_string(), section_hash(&c));
    }

    // tests/proptest.rs — proptest harnesses: state model, transition fns,
    // property/invariant predicates. F8: this key (and fuzz/src/main.rs
    // below) was looked up by the emitters but never inserted, so those
    // banners shipped hash-less and drift detection couldn't see spec
    // changes in them.
    {
        let mut c = String::from("file=tests/proptest.rs\n");
        c.push_str(&canonical_spec_core(spec));
        hashes.insert("tests/proptest.rs".to_string(), section_hash(&c));
    }

    // fuzz/src/main.rs — Crucible fuzz harness: the spec core plus the
    // per-handler accounts surface (action account metas derive from it).
    {
        let mut c = String::from("file=fuzz/src/main.rs\n");
        c.push_str(&format!("name={}\n", spec.program_name));
        c.push_str(&canonical_spec_core(spec));
        for handler in &spec.handlers {
            c.push_str(&canonical_accounts(handler));
        }
        hashes.insert("fuzz/src/main.rs".to_string(), section_hash(&c));
    }

    SpecFingerprint {
        file_hashes: hashes,
    }
}

/// Canonical string for the spec surface the harness generators consume:
/// state fields (incl. multi-variant payload structure), handlers,
/// properties (with bodies), invariants, and lifecycle states.
fn canonical_spec_core(spec: &ParsedSpec) -> String {
    let mut c = String::new();
    for (fname, ftype) in &spec.state_fields {
        c.push_str(&format!("state={}:{}\n", fname, ftype));
    }
    for acct in &spec.account_types {
        if acct.variants.len() <= 1 {
            continue;
        }
        c.push_str(&format!("variant_state_acct={}\n", acct.name));
        for variant in &acct.variants {
            c.push_str(&format!("  variant={}\n", variant.name));
            for (fname, ftype) in &variant.fields {
                c.push_str(&format!("    {}:{}\n", fname, ftype));
            }
        }
    }
    for handler in &spec.handlers {
        c.push_str(&canonical_handler(handler));
    }
    for prop in &spec.properties {
        c.push_str(&format!(
            "prop={}|expr={}|{}\n",
            prop.name,
            prop.expression.as_deref().unwrap_or(""),
            prop.preserved_by.join(",")
        ));
    }
    for inv in &spec.invariants {
        c.push_str(&format!(
            "invariant={}|{}\n",
            inv.name,
            inv.lean_expr.as_deref().unwrap_or("")
        ));
    }
    for state in &spec.lifecycle_states {
        c.push_str(&format!("status={}\n", state));
    }
    c
}

/// Canonical string for a handler (deterministic, sorted).
fn canonical_handler(handler: &crate::check::ParsedHandler) -> String {
    let mut c = String::new();
    c.push_str(&format!("op={}\n", handler.name));
    if let Some(ref doc) = handler.doc {
        c.push_str(&format!("doc={}\n", doc));
    }
    if let Some(ref who) = handler.who {
        c.push_str(&format!("who={}\n", who));
    }
    if let Some(ref pre) = handler.pre_status {
        c.push_str(&format!("when={}\n", pre));
    }
    if let Some(ref post) = handler.post_status {
        c.push_str(&format!("then={}\n", post));
    }
    for (pn, pt) in &handler.takes_params {
        c.push_str(&format!("takes={}:{}\n", pn, pt));
    }
    for eff in &handler.effects {
        c.push_str(&format!("effect={} {} {}\n", eff.field, eff.op, eff.value));
    }
    // Conditional-effect structure: the flat `effects` union above is
    // identical under arm-reordering, so hash the arms explicitly
    // (`qedgen check --frozen` relies on this).
    if let Some(branches) = &handler.effect_branches {
        c.push_str(&format!("effect_match_on={}\n", branches.scrutinee_lean));
        for (idx, arm) in branches.arms.iter().enumerate() {
            c.push_str(&format!("effect_arm[{}]={}\n", idx, arm.pattern_lean));
            for eff in &arm.effects {
                c.push_str(&format!(
                    "effect_arm[{}]_eff={} {} {}\n",
                    idx, eff.field, eff.op, eff.value
                ));
            }
        }
    }
    for emit in &handler.emits {
        c.push_str(&format!("emits={}\n", emit));
    }
    c
}

/// Canonical string for a handler's accounts block.
fn canonical_accounts(handler: &crate::check::ParsedHandler) -> String {
    let mut c = String::new();
    for acct in &handler.accounts {
        c.push_str(&format!(
            "acct={}|signer={}|writable={}|program={}|pda_seeds={}|account_type={}|authority={}\n",
            acct.name,
            acct.is_signer,
            acct.is_writable,
            acct.is_program,
            acct.pda_seeds
                .as_ref()
                .map(|s| s.join(","))
                .unwrap_or_default(),
            acct.account_type.as_deref().unwrap_or(""),
            acct.authority.as_deref().unwrap_or(""),
        ));
    }
    c
}

/// Extract the spec-hash embedded in a generated file's header comment.
/// Looks for `spec-hash:` in the first 5 lines.
pub fn extract_spec_hash(content: &str) -> Option<String> {
    for line in content.lines().take(5) {
        if let Some(idx) = line.find("spec-hash:") {
            let rest = &line[idx + "spec-hash:".len()..];
            // Strip trailing markers like " ----"
            let hash = rest.trim().trim_end_matches('-').trim();
            if !hash.is_empty() {
                return Some(hash.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_section_hash_deterministic() {
        let h1 = section_hash("hello world");
        let h2 = section_hash("hello world");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_section_hash_differs() {
        let h1 = section_hash("hello");
        let h2 = section_hash("world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_extract_spec_hash() {
        let content =
            "// ---- GENERATED BY QEDGEN — DO NOT EDIT ---- spec-hash:abc123def456789a\nuse foo;\n";
        assert_eq!(
            extract_spec_hash(content),
            Some("abc123def456789a".to_string())
        );
    }

    #[test]
    fn test_extract_spec_hash_with_trailing_dashes() {
        let content = "// ---- GENERATED BY QEDGEN — DO NOT EDIT ABOVE THIS LINE ---- spec-hash:abc123def456789a\n";
        assert_eq!(
            extract_spec_hash(content),
            Some("abc123def456789a".to_string())
        );
    }

    #[test]
    fn test_extract_spec_hash_missing() {
        let content = "// ---- GENERATED BY QEDGEN — DO NOT EDIT ----\nuse foo;\n";
        assert_eq!(extract_spec_hash(content), None);
    }

    #[test]
    fn test_fingerprint_deterministic() {
        let spec_content = r#"
spec Counter

type State
  | Active of {
      authority : Pubkey,
      count     : U64,
    }

handler increment (delta : U64) : State.Active -> State.Active {
  auth authority
  requires state.count + delta <= 1000000
  effect {
    count += delta
  }
}

property bounded :
  state.count <= 1000000
  preserved_by [increment]
"#;
        let spec1 = crate::chumsky_adapter::parse_str(spec_content).unwrap();
        let spec2 = crate::chumsky_adapter::parse_str(spec_content).unwrap();
        let fp1 = compute_fingerprint(&spec1);
        let fp2 = compute_fingerprint(&spec2);
        assert_eq!(fp1.file_hashes, fp2.file_hashes);
    }

    /// Adding (or restructuring) a variant on a multi-variant ADT must bump
    /// the `src/state.rs` fingerprint — specs agreeing on the flat union but
    /// not the per-variant layout diverge at codegen time.
    #[test]
    fn test_fingerprint_state_changes_on_variant_restructure() {
        let spec_one_variant = r#"
spec Vault
type State
  | Active of { owner : Pubkey, balance : U64, }
"#;
        let spec_two_variants = r#"
spec Vault
type State
  | Uninitialized
  | Active of { owner : Pubkey, balance : U64, }
"#;
        let fp_one =
            compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_one_variant).unwrap());
        let fp_two =
            compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_two_variants).unwrap());
        assert_ne!(
            fp_one.file_hashes.get("src/state.rs"),
            fp_two.file_hashes.get("src/state.rs"),
            "adding a variant must invalidate the state.rs fingerprint"
        );
    }

    /// Moving a field between variants (flat union unchanged) must still
    /// bump the state.rs fingerprint — the wrapper+enum emission would
    /// otherwise silently flip variant payloads.
    #[test]
    fn test_fingerprint_state_changes_on_variant_field_move() {
        let spec_a = r#"
spec Vault
type State
  | Uninitialized
  | Active of { owner : Pubkey, balance : U64, }
"#;
        let spec_b = r#"
spec Vault
type State
  | Uninitialized of { balance : U64, }
  | Active of { owner : Pubkey, }
"#;
        let fp_a = compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_a).unwrap());
        let fp_b = compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_b).unwrap());
        assert_ne!(
            fp_a.file_hashes.get("src/state.rs"),
            fp_b.file_hashes.get("src/state.rs"),
            "moving a field between variants must invalidate the state.rs fingerprint"
        );
    }

    #[test]
    fn test_fingerprint_changes_on_effect_change() {
        let spec_a = r#"
spec Counter

type State | Active of { count : U64, }

handler increment (delta : U64) : State.Active -> State.Active {
  effect {
    count += delta
  }
}
"#;
        let spec_b = r#"
spec Counter

type State | Active of { count : U64, }

handler increment (delta : U64) : State.Active -> State.Active {
  effect {
    count -= delta
  }
}
"#;
        let fp_a = compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_a).unwrap());
        let fp_b = compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_b).unwrap());
        assert_ne!(
            fp_a.file_hashes.get("src/instructions/increment.rs"),
            fp_b.file_hashes.get("src/instructions/increment.rs")
        );
    }

    #[test]
    fn test_event_change_doesnt_affect_instruction_hash() {
        let spec_a = r#"
spec Counter

type State | Active of { count : U64, }

event Foo {
  x : U64,
}

handler increment (delta : U64) : State.Active -> State.Active {
  effect {
    count += delta
  }
}
"#;
        let spec_b = r#"
spec Counter

type State | Active of { count : U64, }

event Bar {
  y : U64,
}

handler increment (delta : U64) : State.Active -> State.Active {
  effect {
    count += delta
  }
}
"#;
        let fp_a = compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_a).unwrap());
        let fp_b = compute_fingerprint(&crate::chumsky_adapter::parse_str(spec_b).unwrap());
        // Instruction file hash should be THE SAME
        assert_eq!(
            fp_a.file_hashes.get("src/instructions/increment.rs"),
            fp_b.file_hashes.get("src/instructions/increment.rs")
        );
        // But events hash should differ
        assert_ne!(
            fp_a.file_hashes.get("src/events.rs"),
            fp_b.file_hashes.get("src/events.rs")
        );
    }
}
