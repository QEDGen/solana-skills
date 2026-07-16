//! IDL-enrichment overlay for spec-less probe envelopes (#235).
//!
//! Source discovery stays ground truth; when an on-disk IDL exists
//! (Anchor legacy / Anchor ≥0.30 / Codama IR — the same dialects the
//! Crucible brownfield dispatcher accepts), it opportunistically enriches
//! the bootstrap envelope:
//!
//! - per-handler account metas (signer / writable flags) and arg types
//!   ride on `handlers[]` for the auditor;
//! - on Anchor / Quasar — where the framework *enforces* declared signer
//!   flags — an IDL-derived intent tag narrows `applicable_categories`
//!   for handlers the body classifier left untagged;
//! - handler-set disagreement between source and IDL surfaces as
//!   `idl_source_drift` candidates, never silently reconciled;
//! - Pinocchio bootstrap (source discovery yields no handlers) fills
//!   `handlers[]` from the Codama IDL instruction list.
//!
//! No IDL on disk → the overlay is skipped silently (fresh clone
//! pre-build); a `shank` / `codama` marker without an on-disk IDL is
//! reported as `derivable_idl` so the agent knows one command
//! (`shank idl` / `codama run`) away exists. Codama / Shank flags on
//! non-framework runtimes are declarative, not enforced, so they enrich
//! but never narrow there.

use serde::Serialize;
use std::path::{Path, PathBuf};

use super::crucible_brownfield::{accounts_per_handler_from_idl, handlers_with_args_from_idl};
use super::handler_intent::{filter_categories, IntentTag};
use super::{BootstrapHandler, Candidate, Category, Runtime};

/// Per-account meta lifted from the IDL onto a `handlers[]` entry.
/// `signer` is `false` for Codama's `"either"` (conservative: only a
/// definite `true` counts as a gate).
#[derive(Debug, Clone, Serialize)]
pub struct IdlAccountMeta {
    pub name: String,
    pub signer: bool,
    pub writable: bool,
}

/// Per-arg meta lifted from the IDL (`defaultValueStrategy: "omitted"`
/// args — discriminators — are already elided by the shared parser).
/// Types use the qedspec DSL vocabulary (`U64`, `Pubkey`, `Bool`, …),
/// matching the proto-clause/skeleton surface, not raw Rust names.
#[derive(Debug, Clone, Serialize)]
pub struct IdlArgMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

/// What `apply` contributes to the envelope beyond the in-place
/// `handlers[]` enrichment.
#[derive(Debug, Default)]
pub struct OverlayOutcome {
    /// On-disk IDL consumed, relative to the project root when possible.
    pub idl_path: Option<String>,
    /// `"shank"` / `"codama"` when no IDL is on disk but the marker says
    /// one is mechanically derivable.
    pub derivable_idl: Option<String>,
    /// `idl_source_drift` investigation leads (both directions).
    pub drift_candidates: Vec<Candidate>,
}

/// Find an on-disk IDL under the canonical paths (first match wins):
/// `idl.json`, `program/idl.json`, `target/idl/*.json`, `idl/*.json` —
/// the same order the Crucible brownfield dispatcher documents. Returns
/// the path alongside the text so the envelope can name its input.
/// Unreadable files fail loudly (an existing-but-broken IDL should not
/// silently degrade to "no IDL").
pub(crate) fn discover_idl(project_root: &Path) -> anyhow::Result<Option<(PathBuf, String)>> {
    use anyhow::Context;
    let candidates = [
        project_root.join("idl.json"),
        project_root.join("program").join("idl.json"),
    ];
    for c in &candidates {
        if c.is_file() {
            let text =
                std::fs::read_to_string(c).with_context(|| format!("reading {}", c.display()))?;
            return Ok(Some((c.clone(), text)));
        }
    }
    for sub in ["target/idl", "idl"] {
        let dir = project_root.join(sub);
        if dir.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
                .with_context(|| format!("reading {}", dir.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                .collect();
            entries.sort();
            if let Some(first) = entries.first() {
                let text = std::fs::read_to_string(first)
                    .with_context(|| format!("reading {}", first.display()))?;
                return Ok(Some((first.clone(), text)));
            }
        }
    }
    Ok(None)
}

/// Detect an IDL-derivable marker when nothing is on disk: a `shank` dep
/// (the derive macros imply `shank idl` extracts an IDL from source, no
/// build needed) or a `codama` dep / config file. Root `Cargo.toml` only,
/// mirroring `detect_runtime`'s dep heuristics.
pub(crate) fn detect_derivable_idl(project_root: &Path) -> Option<&'static str> {
    let cargo = std::fs::read_to_string(project_root.join("Cargo.toml")).unwrap_or_default();
    let has_dep = |name: &str| {
        cargo.lines().any(|line| {
            let t = line.trim();
            !t.starts_with('#')
                && t.strip_prefix(name)
                    .is_some_and(|after| after.starts_with(['=', '.', '-', ' ']))
        })
    };
    if has_dep("shank") {
        return Some("shank");
    }
    let codama_config = ["codama.json", "codama.js", "codama.mjs"]
        .iter()
        .any(|f| project_root.join(f).is_file());
    if codama_config || has_dep("codama") {
        return Some("codama");
    }
    None
}

/// A parsed IDL instruction in overlay shape, keyed by snake_case name.
struct IdlInstruction {
    snake_name: String,
    accounts: Vec<IdlAccountMeta>,
    args: Vec<IdlArgMeta>,
}

fn parse_instructions(idl_text: &str) -> Vec<IdlInstruction> {
    let accounts_map = accounts_per_handler_from_idl(idl_text);
    handlers_with_args_from_idl(idl_text)
        .into_iter()
        .map(|(snake, args)| {
            let accounts = accounts_map
                .get(&snake)
                .map(|list| {
                    list.iter()
                        .map(|a| IdlAccountMeta {
                            name: a.name.clone(),
                            signer: a.is_signer,
                            writable: a.is_writable,
                        })
                        .collect()
                })
                .unwrap_or_default();
            IdlInstruction {
                snake_name: snake,
                accounts,
                args: args
                    .into_iter()
                    .map(|(name, ty)| IdlArgMeta { name, ty })
                    .collect(),
            }
        })
        .collect()
}

/// Names a source-discovered handler can match an IDL instruction under:
/// the handler name itself plus the Shank `entry_fn` with its `process_`
/// prefix stripped, both snake_cased.
fn match_keys(h: &BootstrapHandler) -> Vec<String> {
    let mut keys = vec![to_snake(&h.name)];
    if let Some(entry) = &h.entry_fn {
        keys.push(to_snake(entry.strip_prefix("process_").unwrap_or(entry)));
    }
    keys.dedup();
    keys
}

/// camelCase / PascalCase → snake_case; already-snake input is identity.
fn to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Derive an intent tag from framework-enforced IDL account flags. Only
/// meaningful where declaring `signer` makes the runtime enforce it
/// (Anchor / Quasar); callers gate on runtime. Mirrors the body
/// classifier's vocabulary so `filter_categories` composes unchanged:
/// no declared signer → `Permissionless`; a signer named like an
/// authority → `AuthorityGated`; any other signer → `TraderGated`.
fn intent_from_idl_accounts(accounts: &[IdlAccountMeta]) -> Option<IntentTag> {
    if accounts.is_empty() {
        // No account list in the IDL — absence of signers is meaningless.
        return None;
    }
    let signers: Vec<&IdlAccountMeta> = accounts.iter().filter(|a| a.signer).collect();
    if signers.is_empty() {
        return Some(IntentTag::Permissionless);
    }
    let authority_named = signers.iter().any(|a| {
        let n = a.name.to_ascii_lowercase();
        ["authority", "admin", "manager", "owner", "delegate"]
            .iter()
            .any(|kw| n.contains(kw))
    });
    Some(if authority_named {
        IntentTag::AuthorityGated
    } else {
        IntentTag::TraderGated
    })
}

fn drift_candidate(handler: &str, direction: &str, detail: String) -> Candidate {
    Candidate {
        category: Category::IdlSourceDrift,
        category_tag: Category::IdlSourceDrift.tag().to_string(),
        handler: handler.to_string(),
        spec_silent_on: detail,
        suppression_hint: "rebuild the IDL from current source (`anchor build` / `shank idl` / \
                           `codama run`) or delete the dead instruction, then re-run the probe"
            .to_string(),
        investigation_hint: match direction {
            "idl_only" => "the shipped IDL advertises an instruction the source dispatcher no \
                           longer exposes — check whether clients can still reach it and \
                           whether its account constraints went stale"
                .to_string(),
            _ => "the source exposes a handler the IDL does not declare — undeclared surface \
                  is invisible to IDL-driven clients and audits; confirm it is intentional"
                .to_string(),
        },
        reason: "deterministic IDL/source cross-check — no runnable reproducer applies".to_string(),
        repro_harness: None,
    }
}

/// Apply the overlay to a spec-less envelope's handler list, in place.
///
/// - Always (any runtime, IDL present): attach `idl_accounts` /
///   `idl_args` to name-matched handlers; emit drift candidates when both
///   sides discovered a non-empty set.
/// - Anchor / Quasar only: derive an intent tag from enforced signer
///   flags for handlers the body classifier left untagged, narrowing
///   `applicable_categories` (narrow-only, same table as `handler_intent`).
/// - Pinocchio / empty discovery: fill `handlers[]` from the IDL
///   instruction list (`source_file` points at the IDL; `discovered_via`
///   = `"idl"`), so the auditor gets a work list instead of an empty vec.
pub(crate) fn apply(
    project_root: &Path,
    runtime: &Runtime,
    handlers: &mut Vec<BootstrapHandler>,
    global_categories: &[String],
) -> OverlayOutcome {
    // Anchor workspaces aggregate handlers from `programs/*`, while their
    // build emits one IDL per program under the workspace-level target dir.
    // Overlay each source crate only with its own IDL; comparing the aggregate
    // handler set to the alphabetically first IDL creates false drift and is
    // ambiguous when two programs expose the same instruction name.
    if matches!(runtime, Runtime::Anchor | Runtime::Quasar) {
        if let Some(outcome) =
            apply_anchor_workspace(project_root, runtime, handlers, global_categories)
        {
            return outcome;
        }
    }

    let mut outcome = OverlayOutcome::default();

    let Ok(discovered) = discover_idl(project_root) else {
        // Unreadable IDL: leave the envelope source-only; the source_scan
        // engine-run already reports unreadable-file coverage separately.
        outcome.derivable_idl = detect_derivable_idl(project_root).map(str::to_string);
        return outcome;
    };
    let Some((idl_path, idl_text)) = discovered else {
        outcome.derivable_idl = detect_derivable_idl(project_root).map(str::to_string);
        return outcome;
    };

    apply_discovered_idl(
        project_root,
        runtime,
        handlers,
        global_categories,
        idl_path,
        idl_text,
    )
}

fn apply_anchor_workspace(
    project_root: &Path,
    runtime: &Runtime,
    handlers: &mut [BootstrapHandler],
    global_categories: &[String],
) -> Option<OverlayOutcome> {
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, handler) in handlers.iter().enumerate() {
        let mut components = Path::new(&handler.source_file).components();
        let programs = components.next()?.as_os_str().to_str()?;
        if programs != "programs" {
            return None;
        }
        let program_dir = components.next()?.as_os_str().to_str()?.to_string();
        groups.entry(program_dir).or_default().push(index);
    }
    if groups.is_empty() {
        return None;
    }

    let mut aggregate = OverlayOutcome::default();
    for (program_dir, indices) in groups {
        let crate_root = project_root.join("programs").join(&program_dir);
        let Ok(discovered) =
            discover_workspace_program_idl(project_root, &crate_root, &program_dir)
        else {
            continue;
        };
        let Some((idl_path, idl_text)) = discovered else {
            // Missing one program's IDL is the same graceful pre-build absence
            // as a missing single-crate IDL; do not compare it to a sibling's.
            continue;
        };

        let mut program_handlers: Vec<BootstrapHandler> = indices
            .iter()
            .map(|index| handlers[*index].clone())
            .collect();
        let mut outcome = apply_discovered_idl(
            project_root,
            runtime,
            &mut program_handlers,
            global_categories,
            idl_path,
            idl_text,
        );
        for (index, handler) in indices.into_iter().zip(program_handlers) {
            handlers[index] = handler;
        }
        if aggregate.idl_path.is_none() {
            aggregate.idl_path = outcome.idl_path.take();
        }
        aggregate
            .drift_candidates
            .append(&mut outcome.drift_candidates);
    }
    Some(aggregate)
}

fn discover_workspace_program_idl(
    workspace_root: &Path,
    crate_root: &Path,
    fallback_name: &str,
) -> anyhow::Result<Option<(PathBuf, String)>> {
    use anyhow::Context;

    let manifest = std::fs::read_to_string(crate_root.join("Cargo.toml")).unwrap_or_default();
    let manifest: Option<toml::Value> = toml::from_str(&manifest).ok();
    let idl_name = manifest
        .as_ref()
        .and_then(|value| value.get("lib"))
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            manifest
                .as_ref()
                .and_then(|value| value.get("package"))
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
        })
        .unwrap_or(fallback_name)
        .replace('-', "_");

    for subdir in ["target/idl", "idl"] {
        let path = workspace_root.join(subdir).join(format!("{idl_name}.json"));
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            return Ok(Some((path, text)));
        }
    }
    Ok(None)
}

fn apply_discovered_idl(
    project_root: &Path,
    runtime: &Runtime,
    handlers: &mut Vec<BootstrapHandler>,
    global_categories: &[String],
    idl_path: PathBuf,
    idl_text: String,
) -> OverlayOutcome {
    let mut outcome = OverlayOutcome::default();

    let instructions = parse_instructions(&idl_text);
    if instructions.is_empty() {
        // A JSON file at a canonical path that carries no instructions is
        // not an IDL we can use — treat as absent.
        outcome.derivable_idl = detect_derivable_idl(project_root).map(str::to_string);
        return outcome;
    }

    let rel_idl_path = idl_path
        .strip_prefix(project_root)
        .unwrap_or(&idl_path)
        .display()
        .to_string();
    outcome.idl_path = Some(rel_idl_path.clone());

    // Framework-enforced signer flags → intent narrowing is sound.
    let flags_enforced = matches!(runtime, Runtime::Anchor | Runtime::Quasar);

    // Pinocchio (and any runtime whose source discovery came up empty):
    // the IDL *is* the handler list. No drift possible — one side is empty.
    if handlers.is_empty() {
        *handlers = instructions
            .iter()
            .map(|ix| BootstrapHandler {
                name: ix.snake_name.clone(),
                source_file: rel_idl_path.clone(),
                enum_variant: None,
                entry_fn: None,
                line: None,
                applicable_categories: None,
                intent_tag: None,
                idl_accounts: Some(ix.accounts.clone()),
                idl_args: Some(ix.args.clone()),
                discovered_via: Some("idl".to_string()),
            })
            .collect();
        return outcome;
    }

    let mut matched_idl: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for h in handlers.iter_mut() {
        let keys = match_keys(h);
        let Some(ix) = instructions.iter().find(|ix| keys.contains(&ix.snake_name)) else {
            outcome.drift_candidates.push(drift_candidate(
                &h.name,
                "source_only",
                format!(
                    "handler `{}` is exposed by the source dispatcher but absent from `{}`",
                    h.name, rel_idl_path
                ),
            ));
            continue;
        };
        matched_idl.insert(ix.snake_name.as_str());
        h.idl_accounts = Some(ix.accounts.clone());
        h.idl_args = Some(ix.args.clone());

        // Body classification wins; the IDL only speaks when the body
        // classifier stayed silent, and only on enforced-flag runtimes.
        if flags_enforced && h.intent_tag.is_none() {
            if let Some(tag) = intent_from_idl_accounts(&ix.accounts) {
                let narrowed = filter_categories(global_categories, Some(tag));
                if narrowed.len() < global_categories.len() {
                    h.applicable_categories = Some(narrowed);
                }
                h.intent_tag = Some(tag.as_str().to_string());
            }
        }
    }

    for ix in &instructions {
        if !matched_idl.contains(ix.snake_name.as_str()) {
            outcome.drift_candidates.push(drift_candidate(
                &ix.snake_name,
                "idl_only",
                format!(
                    "`{}` declares instruction `{}` but source discovery found no matching \
                     handler",
                    rel_idl_path, ix.snake_name
                ),
            ));
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_handler(name: &str) -> BootstrapHandler {
        BootstrapHandler {
            name: name.to_string(),
            source_file: "src/lib.rs".to_string(),
            enum_variant: None,
            entry_fn: None,
            line: None,
            applicable_categories: None,
            intent_tag: None,
            idl_accounts: None,
            idl_args: None,
            discovered_via: None,
        }
    }

    const ANCHOR_030_IDL: &str = r#"{
        "address": "Prog1111111111111111111111111111111111111",
        "metadata": { "name": "vault", "version": "0.1.0", "spec": "0.1.0" },
        "instructions": [
            {
                "name": "initialize",
                "accounts": [
                    { "name": "admin", "signer": true, "writable": true },
                    { "name": "vault", "writable": true,
                      "pda": { "seeds": [ { "kind": "const", "value": [118] } ] } }
                ],
                "args": [ { "name": "cap", "type": "u64" } ]
            },
            {
                "name": "crank",
                "accounts": [ { "name": "vault", "writable": true } ],
                "args": []
            }
        ]
    }"#;

    const CODAMA_IDL: &str = r#"{
        "program": {
            "name": "counter",
            "instructions": [
                {
                    "name": "increment",
                    "accounts": [
                        { "name": "payer", "isSigner": true, "isWritable": true },
                        { "name": "counter", "isSigner": false, "isWritable": true }
                    ],
                    "arguments": [ { "name": "amount", "type": { "kind": "numberTypeNode", "format": "u64" } } ]
                }
            ]
        }
    }"#;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qedgen-idl-overlay-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_idl_no_markers_is_a_silent_skip() {
        let root = tmp_root("none");
        let mut handlers = vec![mk_handler("initialize")];
        let out = apply(&root, &Runtime::Anchor, &mut handlers, &[]);
        assert!(out.idl_path.is_none());
        assert!(out.derivable_idl.is_none());
        assert!(out.drift_candidates.is_empty());
        assert!(handlers[0].idl_accounts.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shank_dep_without_idl_reports_derivable() {
        let root = tmp_root("shank");
        std::fs::write(
            root.join("Cargo.toml"),
            "[dependencies]\nshank = \"0.4\"\nsolana-program = \"2\"\n",
        )
        .unwrap();
        let out = apply(&root, &Runtime::Native, &mut Vec::new(), &[]);
        assert_eq!(out.derivable_idl.as_deref(), Some("shank"));
        assert!(out.idl_path.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codama_config_without_idl_reports_derivable() {
        let root = tmp_root("codama-cfg");
        std::fs::write(root.join("codama.mjs"), "export default {}\n").unwrap();
        let out = apply(&root, &Runtime::Pinocchio, &mut Vec::new(), &[]);
        assert_eq!(out.derivable_idl.as_deref(), Some("codama"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shankles_cargo_dep_does_not_false_positive() {
        // `shankles = ...` must not match the `shank` marker.
        let root = tmp_root("shankles");
        std::fs::write(
            root.join("Cargo.toml"),
            "[dependencies]\nshankles = \"1\"\n",
        )
        .unwrap();
        assert_eq!(detect_derivable_idl(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn anchor_idl_enriches_and_narrows_untagged_handlers() {
        let root = tmp_root("anchor-enrich");
        std::fs::create_dir_all(root.join("target/idl")).unwrap();
        std::fs::write(root.join("target/idl/vault.json"), ANCHOR_030_IDL).unwrap();

        let global: Vec<String> = [
            "missing_signer",
            "arithmetic_overflow_wrapping",
            "permissionless_state_writer",
            "permissionless_create_account_dos",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let mut handlers = vec![mk_handler("initialize"), mk_handler("crank")];
        let out = apply(&root, &Runtime::Anchor, &mut handlers, &global);

        assert_eq!(out.idl_path.as_deref(), Some("target/idl/vault.json"));
        assert!(out.drift_candidates.is_empty());

        // `initialize`: admin is a signer named like an authority →
        // authority_gated, permissionless shapes dropped.
        let init = &handlers[0];
        assert_eq!(init.intent_tag.as_deref(), Some("authority_gated"));
        let narrowed = init.applicable_categories.as_ref().unwrap();
        assert!(!narrowed.iter().any(|c| c == "permissionless_state_writer"));
        assert!(narrowed.iter().any(|c| c == "missing_signer"));
        let accounts = init.idl_accounts.as_ref().unwrap();
        assert!(accounts.iter().any(|a| a.name == "admin" && a.signer));
        assert_eq!(init.idl_args.as_ref().unwrap()[0].name, "cap");

        // `crank`: no signer declared (and Anchor enforces flags) →
        // permissionless, missing_signer dropped.
        let crank = &handlers[1];
        assert_eq!(crank.intent_tag.as_deref(), Some("permissionless"));
        assert!(!crank
            .applicable_categories
            .as_ref()
            .unwrap()
            .iter()
            .any(|c| c == "missing_signer"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn anchor_composite_accounts_preserve_nested_signers() {
        let root = tmp_root("anchor-composite-signer");
        std::fs::write(
            root.join("idl.json"),
            r#"{
                "instructions": [{
                    "name": "execute",
                    "accounts": [{
                        "name": "nested",
                        "accounts": [{
                            "name": "authority",
                            "signer": true,
                            "writable": false
                        }]
                    }],
                    "args": []
                }]
            }"#,
        )
        .unwrap();

        let global = vec![
            "missing_signer".to_string(),
            "permissionless_state_writer".to_string(),
        ];
        let mut handlers = vec![mk_handler("execute")];
        apply(&root, &Runtime::Anchor, &mut handlers, &global);

        let handler = &handlers[0];
        assert_eq!(handler.intent_tag.as_deref(), Some("authority_gated"));
        assert!(handler
            .applicable_categories
            .as_ref()
            .unwrap()
            .iter()
            .any(|category| category == "missing_signer"));
        assert_eq!(handler.idl_accounts.as_ref().unwrap().len(), 1);
        assert!(handler.idl_accounts.as_ref().unwrap()[0].signer);
        assert_eq!(handler.idl_accounts.as_ref().unwrap()[0].name, "authority");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn body_classification_wins_over_idl_intent() {
        let root = tmp_root("body-wins");
        std::fs::create_dir_all(root.join("target/idl")).unwrap();
        std::fs::write(root.join("target/idl/vault.json"), ANCHOR_030_IDL).unwrap();

        let mut h = mk_handler("crank");
        h.intent_tag = Some("authority_gated".to_string());
        let mut handlers = vec![h];
        apply(
            &root,
            &Runtime::Anchor,
            &mut handlers,
            &["missing_signer".to_string()],
        );
        // IDL would say permissionless; the body tag must survive.
        assert_eq!(handlers[0].intent_tag.as_deref(), Some("authority_gated"));
        // Enrichment still applies.
        assert!(handlers[0].idl_accounts.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codama_flags_enrich_but_never_narrow_on_pinocchio() {
        let root = tmp_root("codama-no-narrow");
        std::fs::write(root.join("idl.json"), CODAMA_IDL).unwrap();

        let mut handlers = vec![mk_handler("increment")];
        let global = vec!["missing_signer".to_string()];
        apply(&root, &Runtime::Pinocchio, &mut handlers, &global);

        let h = &handlers[0];
        // Declarative flags ride along for the auditor...
        assert!(h
            .idl_accounts
            .as_ref()
            .unwrap()
            .iter()
            .any(|a| a.name == "payer" && a.signer));
        // ...but narrow nothing: Codama flags are not runtime-enforced.
        assert!(h.intent_tag.is_none());
        assert!(h.applicable_categories.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_discovery_fills_handlers_from_idl() {
        let root = tmp_root("idl-fill");
        std::fs::write(root.join("idl.json"), CODAMA_IDL).unwrap();

        let mut handlers = Vec::new();
        let out = apply(&root, &Runtime::Pinocchio, &mut handlers, &[]);

        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name, "increment");
        assert_eq!(handlers[0].discovered_via.as_deref(), Some("idl"));
        assert_eq!(handlers[0].source_file, "idl.json");
        assert!(out.drift_candidates.is_empty(), "one empty side = no drift");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drift_fires_in_both_directions() {
        let root = tmp_root("drift");
        std::fs::create_dir_all(root.join("target/idl")).unwrap();
        std::fs::write(root.join("target/idl/vault.json"), ANCHOR_030_IDL).unwrap();

        // Source has `initialize` + an undeclared `emergency_withdraw`;
        // IDL additionally declares `crank` which source lacks.
        let mut handlers = vec![mk_handler("initialize"), mk_handler("emergency_withdraw")];
        let out = apply(&root, &Runtime::Anchor, &mut handlers, &[]);

        let tags: Vec<(&str, &str)> = out
            .drift_candidates
            .iter()
            .map(|c| (c.handler.as_str(), c.category_tag.as_str()))
            .collect();
        assert!(tags.contains(&("emergency_withdraw", "idl_source_drift")));
        assert!(tags.contains(&("crank", "idl_source_drift")));
        assert_eq!(out.drift_candidates.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn anchor_workspace_pairs_each_program_with_its_own_idl() {
        let root = tmp_root("anchor-workspace-idls");
        for program in ["alpha", "beta"] {
            let crate_root = root.join("programs").join(program);
            std::fs::create_dir_all(&crate_root).unwrap();
            std::fs::write(
                crate_root.join("Cargo.toml"),
                format!("[package]\nname = \"{program}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
        }
        std::fs::create_dir_all(root.join("target/idl")).unwrap();
        std::fs::write(
            root.join("target/idl/alpha.json"),
            r#"{
                "instructions": [{
                    "name": "shared",
                    "accounts": [{"name": "alpha_admin", "signer": true}],
                    "args": []
                }]
            }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("target/idl/beta.json"),
            r#"{
                "instructions": [{
                    "name": "shared",
                    "accounts": [{"name": "beta_payer", "signer": true}],
                    "args": []
                }]
            }"#,
        )
        .unwrap();

        let mut alpha = mk_handler("shared");
        alpha.source_file = "programs/alpha/src/lib.rs".to_string();
        let mut beta = mk_handler("shared");
        beta.source_file = "programs/beta/src/lib.rs".to_string();
        let mut handlers = vec![alpha, beta];

        let out = apply(&root, &Runtime::Anchor, &mut handlers, &[]);

        assert!(out.drift_candidates.is_empty());
        assert_eq!(
            handlers[0].idl_accounts.as_ref().unwrap()[0].name,
            "alpha_admin"
        );
        assert_eq!(
            handlers[1].idl_accounts.as_ref().unwrap()[0].name,
            "beta_payer"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn camel_case_idl_names_match_snake_source_handlers() {
        let root = tmp_root("camel");
        // Legacy Anchor IDLs carry camelCase instruction names.
        std::fs::write(
            root.join("idl.json"),
            r#"{ "instructions": [ { "name": "initializeMarket",
                 "accounts": [ { "name": "payer", "isSigner": true, "isMut": true } ],
                 "args": [] } ] }"#,
        )
        .unwrap();
        let mut handlers = vec![mk_handler("initialize_market")];
        let out = apply(&root, &Runtime::Anchor, &mut handlers, &[]);
        assert!(out.drift_candidates.is_empty());
        assert!(handlers[0].idl_accounts.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shank_entry_fn_matches_with_process_prefix_stripped() {
        let root = tmp_root("shank-match");
        std::fs::write(
            root.join("idl.json"),
            r#"{ "instructions": [ { "name": "initializeMarket", "accounts": [], "args": [] } ] }"#,
        )
        .unwrap();
        let mut h = mk_handler("InitializeMarket");
        h.entry_fn = Some("process_initialize_market".to_string());
        let mut handlers = vec![h];
        let out = apply(&root, &Runtime::Native, &mut handlers, &[]);
        assert!(
            out.drift_candidates.is_empty(),
            "{:?}",
            out.drift_candidates
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
