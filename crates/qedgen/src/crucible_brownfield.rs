//! Brownfield Crucible harness emission — v2.21 Slice 1, extended in
//! v2.22 Slice 3 for Pinocchio.
//!
//! Lifts the `qedgen probe --fuzz requires --spec` gate by synthesising
//! a minimal [`ParsedSpec`] from a brownfield project root (no
//! `.qedspec` required). The synthesised spec carries enough handler
//! metadata for [`crucible_gen::generate`] to emit a working harness
//! whose `invariant_test()` body is empty — Crucible's intrinsic
//! crash detector (panic / unwrap-on-None / BorrowMutError / arithmetic
//! overflow) does the lifting. See PRD-v2.21 §"Slice 1".
//!
//! ## Runtime coverage
//!
//! - **Anchor / Quasar / qedgen-codegen** (v2.21) — handler enumeration
//!   via the regex used by `anchor_extractor::scan_handler_context_map`.
//!   Anchor IDL discovery hooks the same `target/idl/<prog>.json` lookup
//!   as spec-mode.
//! - **Pinocchio** (v2.22) — requires an on-disk Codama / Anchor 0.30
//!   IDL (canonical paths: `idl.json`, `program/idl.json`,
//!   `idl/*.json`, `target/idl/*.json`). The IDL is passed through to
//!   `<harness>/idls/<prog>.json` verbatim. Scanner-based metadata
//!   inference from handler bodies is intentionally out of scope —
//!   account flags and arg types extracted via regex are too noisy to
//!   ship; the maintainer-authored Codama IDL is the trusted source.
//! - **Native / sBPF** — deferred (errors with a clear message). Native
//!   programs follow the same gate: Shank IDL discovery is the next
//!   target after v2.23 (v2.23 shipped the pre/post property lowering
//!   trust fix + brownfield first-contact onboarding flow instead).
//!   sBPF brownfield fuzz is parked indefinitely (no AccountInfo
//!   abstraction at source level).

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};

use crate::check::{ParsedHandler, ParsedHandlerAccount, ParsedSpec};
use crate::probe::Runtime;

/// Output of a brownfield synthesis: the [`ParsedSpec`] that drives
/// `crucible_gen::generate` plus, when the runtime needs it, a
/// pre-rendered IDL JSON to drop at `<harness>/idls/<prog>.json` (the
/// macro input). Anchor-family programs return `idl_json: None` and let
/// the existing `crucible_probe::discover_idl` symlink the
/// `anchor build`-produced IDL.
#[derive(Debug)]
pub struct BrownfieldSynthesis {
    pub spec: ParsedSpec,
    /// Pinocchio: anchor-shaped IDL JSON synthesised from the source
    /// scan. Anchor-family: `None` (the existing IDL pickup path
    /// applies).
    pub idl_json: Option<String>,
}

/// Synthesise a [`ParsedSpec`] from a brownfield project root. The
/// resulting spec has:
///
/// - `program_name` derived from `Cargo.toml`'s `[package] name`
///   (falling back to the root's leaf directory name).
/// - `handlers[]` populated from `pub fn <name>(ctx: Context<X>, ...)`
///   signatures scanned across the crate's `src/` tree. Handler params
///   are intentionally left empty in v2.21 — Crucible's IDL-derived
///   typed builders generate the param payload at fuzz time, and the
///   per-action stub gets `agent-fill` `todo!()` for the typed accounts
///   literal (same shape as spec-mode).
/// - No invariants, properties, account types, or PDAs — protocol mode
///   doesn't assert spec invariants. See
///   [`crate::crucible_gen::InvariantMode::Protocol`].
///
/// Errors on Native / sBPF (deferred); routes Pinocchio through the
/// v2.22 metadata-extraction + IDL-synthesis path; otherwise falls
/// through to the v2.21 Anchor-family handler enumeration.
pub fn synthesize_spec(project_root: &Path, runtime: Runtime) -> Result<BrownfieldSynthesis> {
    match runtime {
        Runtime::Anchor | Runtime::Quasar | Runtime::QedgenCodegen => {
            synthesize_anchor_family(project_root)
        }
        Runtime::Pinocchio => synthesize_pinocchio(project_root),
        Runtime::Native | Runtime::Sbpf | Runtime::Unknown => bail!(
            "Crucible brownfield mode (`--fuzz --root`) on `{runtime:?}` is tracked for v2.24+. \
             v2.22 covers Anchor / Quasar / qedgen-codegen / Pinocchio (v2.23 shipped pre/post \
             property lowering + brownfield first-contact instead of touching this gate). \
             Until then, fall back to `qedgen probe --program <path>` for the site-catalogue \
             audit envelope. Pass `--runtime <name>` to override detection if needed."
        ),
    }
}

fn empty_handler(name: String) -> ParsedHandler {
    ParsedHandler {
        name,
        doc: None,
        who: None,
        on_account: None,
        pre_status: None,
        post_status: None,
        takes_params: vec![],
        guard_str: None,
        guard_str_rust: None,
        aborts_if: vec![],
        requires: vec![],
        ensures: vec![],
        modifies: None,
        let_bindings: vec![],
        aborts_total: false,
        permissionless: true,
        effects: vec![],
        effect_on_error: vec![],
        accounts: vec![],
        transfers: vec![],
        emits: vec![],
        invariants: vec![],
        establishes: vec![],
        schema_includes: vec![],
        properties: vec![],
        calls: vec![],
        effect_branches: None,
        abstract_binders: vec![],
    }
}

fn synthesize_anchor_family(project_root: &Path) -> Result<BrownfieldSynthesis> {
    let program_name = program_name_from_root(project_root)?;
    let handlers = scan_anchor_handlers(project_root)?;
    if handlers.is_empty() {
        bail!(
            "No `pub fn <name>(ctx: Context<X>, ...)` handlers found under {}. \
             Brownfield mode needs at least one Anchor handler to fuzz; \
             confirm `--root` points at the program crate (e.g. `programs/my_prog/`).",
            project_root.display()
        );
    }
    let spec = ParsedSpec {
        program_name,
        handlers: handlers.into_iter().map(empty_handler).collect(),
        ..Default::default()
    };
    Ok(BrownfieldSynthesis {
        spec,
        idl_json: None,
    })
}

fn synthesize_pinocchio(project_root: &Path) -> Result<BrownfieldSynthesis> {
    // v2.22 Slice 3 gates Pinocchio brownfield on a maintainer-authored
    // Codama / Anchor 0.30 IDL. Scanner-based account & arg inference
    // from `pub fn process_*` bodies is fragile — `.borrow_mut_*`
    // patterns miss CPI-mutated accounts, `from_le_bytes` patterns miss
    // zero-copy unpacking, and account-name suffix conventions vary by
    // codebase. A hand-validated Codama IDL is the trusted source.
    //
    // Future runtimes follow the same gate: Shank for legacy native
    // Rust programs (v2.24+); custom dispatchers carry a Codama IDL via
    // codama-cli or are out of scope.
    let idl_text = discover_pinocchio_idl(project_root)?.ok_or_else(|| {
        anyhow!(
            "Brownfield Pinocchio fuzz requires a Codama / Anchor 0.30 IDL on disk. \
             Checked: {root}/idl.json, {root}/program/idl.json, {root}/idl/*.json, \
             {root}/target/idl/*.json — none found. \
             Generate one with `codama --output idl.json` (https://codama.org), then re-run. \
             For Anchor programs, point `--root` at the crate that runs `anchor build`.",
            root = project_root.display()
        )
    })?;

    let handlers_with_args = handlers_with_args_from_idl(&idl_text);
    let accounts_per_handler = accounts_per_handler_from_idl(&idl_text);
    if handlers_with_args.is_empty() {
        bail!(
            "Codama IDL at {} parsed but has no `instructions[]` entries. \
             Brownfield fuzz needs at least one instruction to dispatch.",
            project_root.display()
        );
    }
    // Pull the program name from the IDL itself when present. The
    // declare_fuzz_program! macro derives the generated module name
    // from the IDL's `program.name` (Codama IR) or `metadata.name`
    // (Anchor 0.30), so the harness's `use {prog}::instruction;` must
    // line up with that — not with the surrounding Cargo workspace's
    // leaf directory. Fall back to the Cargo / leaf-dir name when the
    // IDL doesn't carry one (rare; an Anchor IDL always does, and
    // Codama IR carries `program.name` even when the file doesn't
    // declare `metadata`).
    let program_name = program_name_from_idl(&idl_text)
        .or_else(|| program_name_from_root(project_root).ok())
        .unwrap_or_else(|| "program".to_string());
    let handlers = handlers_with_args
        .into_iter()
        .map(|(name, args)| {
            let mut h = empty_handler(name.clone());
            h.takes_params = args;
            h.accounts = accounts_per_handler.get(&name).cloned().unwrap_or_default();
            h
        })
        .collect();
    let spec = ParsedSpec {
        program_name,
        handlers,
        ..Default::default()
    };
    Ok(BrownfieldSynthesis {
        spec,
        idl_json: Some(idl_text),
    })
}

/// Extract the program name from either an Anchor 0.30 IDL
/// (`metadata.name`) or a Codama IR JSON (`program.name`). Returns
/// `None` when neither is present.
fn program_name_from_idl(idl_text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(idl_text).ok()?;
    v.get("metadata")
        .and_then(|m| m.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            v.get("program")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
}

/// Search the brownfield project root for a Codama / Anchor 0.30 IDL
/// JSON. Returns the file contents verbatim when found so the macro
/// consumes the maintainer-authored schema rather than a regex-derived
/// reconstruction.
///
/// Lookup order (first match wins):
/// 1. `<root>/idl.json` — Codama convention (also used by
///    `solana-program/` Pinocchio crates).
/// 2. `<root>/program/idl.json` — workspace-rooted variant.
/// 3. `<root>/target/idl/<*>.json` — Anchor `anchor build` output (may
///    appear in a Pinocchio workspace that also builds via Anchor).
/// 4. `<root>/idl/*.json` — Codama default output dir.
///
/// Multiple matches at the same precedence level are sorted
/// alphabetically and the first picked, so behavior is deterministic
/// across runs.
pub(crate) fn discover_pinocchio_idl(project_root: &Path) -> Result<Option<String>> {
    let candidates = [
        project_root.join("idl.json"),
        project_root.join("program").join("idl.json"),
    ];
    for c in &candidates {
        if c.is_file() {
            return Ok(Some(
                std::fs::read_to_string(c).with_context(|| format!("reading {}", c.display()))?,
            ));
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
                return Ok(Some(
                    std::fs::read_to_string(first)
                        .with_context(|| format!("reading {}", first.display()))?,
                ));
            }
        }
    }
    Ok(None)
}

/// Extract per-handler argument lists alongside instruction names. The
/// harness emitter uses `takes_params` to build `instruction::Foo { arg1,
/// arg2, ... }` literals that match the macro-generated struct shape;
/// without typed args the literals leave fields uninitialised and the
/// build fails with E0063.
///
/// Returns one entry per IDL instruction. Each entry: `(snake_name,
/// vec![(arg_name, rust_type), ...])`. Discriminator arguments and any
/// argument flagged `defaultValueStrategy: "omitted"` are skipped — the
/// macro doesn't surface those as struct fields.
///
/// Type mapping (current scope):
/// - Codama `numberTypeNode { format: "u8" | "u16" | ... | "i128" }` →
///   `"u8"` / `"u16"` / ... / `"i128"`.
/// - Codama `publicKeyTypeNode`, Anchor `"pubkey"` → `"Pubkey"`.
/// - Codama `booleanTypeNode`, Anchor `"bool"` → `"bool"`.
/// - Codama `stringTypeNode`, Anchor `"string"` → `"String"`.
/// - Anything else → `"u64"` placeholder (the action param will compile
///   but the fuzzer-generated value may not type-coerce into the
///   macro's field; the caller should refine the IDL or accept the
///   compile error as a signal that the type isn't supported yet).
fn handlers_with_args_from_idl(idl_text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(idl_text) else {
        return Vec::new();
    };
    let ixs = v
        .get("instructions")
        .and_then(|v| v.as_array())
        .or_else(|| {
            v.get("program")
                .and_then(|p| p.get("instructions"))
                .and_then(|v| v.as_array())
        });
    let Some(ixs) = ixs else {
        return Vec::new();
    };
    ixs.iter()
        .filter_map(|ix| {
            let raw_name = ix.get("name").and_then(|n| n.as_str())?;
            let snake = camel_to_snake(raw_name);
            // Codama IR uses `arguments[]`; Anchor 0.30 uses `args[]`.
            let args_array = ix
                .get("arguments")
                .or_else(|| ix.get("args"))
                .and_then(|v| v.as_array());
            let args = args_array
                .map(|arr| {
                    arr.iter()
                        .filter(|a| {
                            // Codama: skip args with `defaultValueStrategy: "omitted"`
                            // (e.g. the discriminator field). The macro elides
                            // those from the generated struct.
                            a.get("defaultValueStrategy")
                                .and_then(|s| s.as_str())
                                .map(|s| s != "omitted")
                                .unwrap_or(true)
                        })
                        .filter_map(|a| {
                            let name = a.get("name").and_then(|n| n.as_str())?;
                            let ty = idl_arg_type(a.get("type")?);
                            Some((name.to_string(), ty))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some((snake, args))
        })
        .collect()
}

/// Extract per-handler account lists. Keyed by snake_case handler name
/// (matching `handlers_with_args_from_idl`'s output). Each entry is the
/// flat list of accounts in declaration order — the `.accounts(...)`
/// literal in the emitted harness uses this list to produce a real
/// `accounts::Foo { ... }` initializer instead of a `todo!()` placeholder.
///
/// Codama account fields used:
/// - `name` → ParsedHandlerAccount.name (kept camelCase to match the
///   macro-generated struct field name).
/// - `isSigner` / `isWritable` → flags.
/// - `defaultValue.kind: "publicKeyValueNode"` + `publicKey` → store
///   the literal base58 in `default_pubkey` so the emitter renders
///   `solana_pubkey::pubkey!("...")`.
/// - `defaultValue.kind: "pdaValueNode"` → leave `default_pubkey: None`
///   and mark `pda_seeds: Some(vec![])` so the emitter knows to derive
///   via `Pubkey::find_program_address` (v2.22 emits a placeholder; full
///   seed-aware derivation is v2.22.x).
fn accounts_per_handler_from_idl(
    idl_text: &str,
) -> std::collections::HashMap<String, Vec<ParsedHandlerAccount>> {
    let mut out = std::collections::HashMap::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(idl_text) else {
        return out;
    };
    let ixs = v
        .get("instructions")
        .and_then(|v| v.as_array())
        .or_else(|| {
            v.get("program")
                .and_then(|p| p.get("instructions"))
                .and_then(|v| v.as_array())
        });
    let Some(ixs) = ixs else {
        return out;
    };
    for ix in ixs {
        let Some(raw_name) = ix.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let snake = camel_to_snake(raw_name);
        let accounts_array = ix.get("accounts").and_then(|v| v.as_array());
        let Some(arr) = accounts_array else {
            out.insert(snake, Vec::new());
            continue;
        };
        let accounts: Vec<ParsedHandlerAccount> = arr
            .iter()
            .filter_map(|a| {
                let name = a.get("name").and_then(|n| n.as_str())?.to_string();
                let is_signer = a.get("isSigner").and_then(|b| b.as_bool()).unwrap_or(false);
                let is_writable = a
                    .get("isWritable")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                let default = a.get("defaultValue");
                let default_kind = default.and_then(|d| d.get("kind")).and_then(|k| k.as_str());
                let (default_pubkey, pda_seeds, is_program) = match default_kind {
                    Some("publicKeyValueNode") => {
                        let pk = default
                            .and_then(|d| d.get("publicKey"))
                            .and_then(|k| k.as_str())
                            .map(|s| s.to_string());
                        // Codama marks program defaults via the
                        // accountValueNode/programLink chain in other
                        // shapes; for our scope, a publicKeyValueNode
                        // pointing at a fixed pubkey is effectively a
                        // program/sysvar account.
                        (pk, None, true)
                    }
                    Some("pdaValueNode") => (None, Some(vec![]), false),
                    _ => (None, None, false),
                };
                Some(ParsedHandlerAccount {
                    name,
                    is_signer,
                    is_writable,
                    is_program,
                    pda_seeds,
                    account_type: None,
                    authority: None,
                    default_pubkey,
                    imported_namespace: None,
                })
            })
            .collect();
        out.insert(snake, accounts);
    }
    out
}

fn camel_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Map an IDL argument's `type` field to a qedgen DSL type identifier
/// (`U8`, `U64`, `Pubkey`, `Bool`, ...). The harness emitter runs the
/// result back through `crucible_gen::map_simple_type` to get the
/// Rust-side type, so this returns the DSL convention used by
/// `ParsedHandler::takes_params`.
///
/// Tolerates both the Codama IR tree shape (`{kind, format, ...}`) and
/// the Anchor 0.30 string-shorthand shape (`"u64"`).
fn idl_arg_type(ty: &serde_json::Value) -> String {
    if let Some(s) = ty.as_str() {
        return anchor_str_to_dsl(s);
    }
    let kind = ty.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    match kind {
        "numberTypeNode" => ty
            .get("format")
            .and_then(|f| f.as_str())
            .map(anchor_str_to_dsl)
            .unwrap_or_else(|| "U64".to_string()),
        "publicKeyTypeNode" => "Pubkey".to_string(),
        "booleanTypeNode" => "Bool".to_string(),
        _ => "U64".to_string(),
    }
}

fn anchor_str_to_dsl(s: &str) -> String {
    match s {
        "u8" => "U8".to_string(),
        "u16" => "U16".to_string(),
        "u32" => "U32".to_string(),
        "u64" => "U64".to_string(),
        "u128" => "U128".to_string(),
        "i8" => "I8".to_string(),
        "i16" => "I16".to_string(),
        "i32" => "I32".to_string(),
        "i64" => "I64".to_string(),
        "i128" => "I128".to_string(),
        "pubkey" | "publicKey" => "Pubkey".to_string(),
        "bool" => "Bool".to_string(),
        _ => "U64".to_string(),
    }
}

/// Write the synthesised IDL JSON into `<harness>/idls/<prog>.json`.
/// Idempotent — if the destination already exists it is overwritten so
/// re-runs pick up scanner improvements without manual cleanup.
pub fn write_synthesized_idl(
    harness_dir: &Path,
    program_name: &str,
    idl_json: &str,
) -> Result<PathBuf> {
    let idls_dir = harness_dir.join("idls");
    std::fs::create_dir_all(&idls_dir)
        .with_context(|| format!("creating {}", idls_dir.display()))?;
    let dest = idls_dir.join(format!("{program_name}.json"));
    std::fs::write(&dest, idl_json).with_context(|| format!("writing {}", dest.display()))?;
    Ok(dest)
}

/// Read `Cargo.toml`'s `[package] name`. Falls back to the root's
/// leaf-directory name (lowercased, hyphens kept) when `Cargo.toml` is
/// missing or unparseable — both happen on multi-program workspaces
/// where the user pointed `--root` at a workspace-level path. The
/// downstream caller surfaces a cleaner error if the program crate
/// can't be resolved at IDL-discovery time.
fn program_name_from_root(root: &Path) -> Result<String> {
    let manifest = root.join("Cargo.toml");
    if manifest.exists() {
        let raw = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        if let Some(name) = parse_package_name(&raw) {
            return Ok(name);
        }
    }
    root.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            anyhow!(
                "Could not determine program name from {} (no Cargo.toml, no leaf-directory name).",
                root.display()
            )
        })
}

/// Extract `name = "..."` from a `[package]` section. Hand-rolled
/// rather than pulling `toml` as a dep — qedgen already vends a regex
/// + manual parser for similar single-key reads (`anchor_resolver.rs`).
fn parse_package_name(toml_str: &str) -> Option<String> {
    let mut in_package = false;
    for line in toml_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed.starts_with("[package");
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name") {
            let rest = rest.trim_start_matches([' ', '\t']);
            let rest = rest.strip_prefix('=')?;
            let rest = rest.trim().trim_matches(['"', '\''].as_ref());
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Walk `<root>/src/**/*.rs` and collect handler names from
/// `pub fn <name>(ctx: Context<X>, ...)` signatures. De-dupes by name
/// (Anchor sometimes splits handlers across module re-exports).
fn scan_anchor_handlers(root: &Path) -> Result<Vec<String>> {
    let src_dir = root.join("src");
    if !src_dir.exists() {
        bail!(
            "Brownfield root {} has no `src/` — confirm `--root` points at a Rust crate.",
            root.display()
        );
    }
    let pat =
        Regex::new(r"(?m)^\s*pub\s+fn\s+(\w+)\s*\(\s*(?:mut\s+)?ctx\s*:\s*Context\s*<\s*\w+\s*>")
            .expect("static regex");
    let mut handlers: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for file in collect_rust_files(&src_dir)? {
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        for caps in pat.captures_iter(&src) {
            let name = caps.get(1).unwrap().as_str().to_string();
            if seen.insert(name.clone()) {
                handlers.push(name);
            }
        }
    }
    Ok(handlers)
}

fn collect_rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|s| s.to_str()) == Some("target") {
                    continue;
                }
                walk(&path, out)?;
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// Default harness location for brownfield mode: `<root>/.qed/fuzz/`.
/// `crucible_gen::generate` appends the program-name leaf, so this
/// returns the parent directory (matching the spec-mode convention).
pub fn brownfield_harness_parent(root: &Path) -> PathBuf {
    root.join(".qed").join("fuzz")
}

/// Best-effort project-root discovery from `--root`: if the user
/// pointed at a Cargo workspace (with `programs/<prog>/`), walk down
/// to the first `pub mod ... declare_id!` crate. v2.21 returns the
/// input unchanged — workspace traversal is a v2.22 polish. We keep
/// the function defined so the caller can swap in a smarter walker
/// without a CLI shape change.
pub fn resolve_program_root(input: &Path) -> Result<PathBuf> {
    Ok(input.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, body: &str) {
        std::fs::write(dir.join("Cargo.toml"), body).unwrap();
    }

    #[test]
    fn parses_simple_package_name() {
        let toml = r#"
[package]
name = "my_program"
version = "0.1.0"
"#;
        assert_eq!(parse_package_name(toml).as_deref(), Some("my_program"));
    }

    #[test]
    fn parses_with_dependencies_section_after_package() {
        let toml = r#"
[package]
name = "buggy_anchor"
version = "0.1.0"

[dependencies]
anchor-lang = "0.30"
"#;
        assert_eq!(parse_package_name(toml).as_deref(), Some("buggy_anchor"));
    }

    #[test]
    fn ignores_name_outside_package_section() {
        let toml = r#"
[lib]
name = "shouldnt_match"

[package]
name = "real_name"
"#;
        assert_eq!(parse_package_name(toml).as_deref(), Some("real_name"));
    }

    #[test]
    fn returns_none_for_missing_package_block() {
        let toml = r#"
[workspace]
members = ["programs/*"]
"#;
        assert_eq!(parse_package_name(toml), None);
    }

    #[test]
    fn program_name_falls_back_to_leaf_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let crate_dir = tmp.path().join("standalone_prog");
        std::fs::create_dir_all(&crate_dir).unwrap();
        // No Cargo.toml — fallback path.
        let name = program_name_from_root(&crate_dir).unwrap();
        assert_eq!(name, "standalone_prog");
    }

    #[test]
    fn scan_anchor_handlers_collects_unique_names() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            r#"
#[program]
pub mod my_prog {
    use super::*;

    pub fn initialize(ctx: Context<Init>) -> Result<()> { Ok(()) }
    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> { Ok(()) }
    pub fn withdraw(mut ctx: Context<Withdraw>, amount: u64) -> Result<()> { Ok(()) }
}
"#,
        )
        .unwrap();
        let handlers = scan_anchor_handlers(tmp.path()).unwrap();
        assert_eq!(handlers, vec!["initialize", "deposit", "withdraw"]);
    }

    #[test]
    fn scan_anchor_handlers_dedupes_re_exports() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("handlers")).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn deposit(ctx: Context<Deposit>, amt: u64) -> Result<()> { Ok(()) }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("handlers").join("deposit.rs"),
            "pub fn deposit(ctx: Context<Deposit>, amt: u64) -> Result<()> { Ok(()) }\n",
        )
        .unwrap();
        let handlers = scan_anchor_handlers(tmp.path()).unwrap();
        assert_eq!(handlers, vec!["deposit"]);
    }

    #[test]
    fn scan_errors_when_src_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let err = scan_anchor_handlers(tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no `src/`"), "got: {msg}");
    }

    #[test]
    fn synthesize_spec_rejects_native_and_sbpf() {
        let tmp = tempfile::tempdir().unwrap();
        for rt in [Runtime::Native, Runtime::Sbpf] {
            let label = format!("{rt:?}");
            let err = synthesize_spec(tmp.path(), rt).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("v2.24"),
                "{label} bail should cite v2.24+ deferral, got: {msg}"
            );
        }
    }

    #[test]
    fn synthesize_spec_builds_handler_list_for_anchor() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            r#"
[package]
name = "buggy_anchor"
version = "0.1.0"
"#,
        );
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn run(ctx: Context<Run>) -> Result<()> { Ok(()) }\n",
        )
        .unwrap();
        let synth = synthesize_spec(tmp.path(), Runtime::Anchor).unwrap();
        assert_eq!(synth.spec.program_name, "buggy_anchor");
        assert_eq!(synth.spec.handlers.len(), 1);
        assert_eq!(synth.spec.handlers[0].name, "run");
        // Brownfield handlers are `permissionless` — no `auth` to lift.
        assert!(synth.spec.handlers[0].permissionless);
        assert!(synth.spec.invariants.is_empty());
        assert!(synth.spec.properties.is_empty());
        // Anchor path doesn't synthesise an IDL — the v2.21 discover_idl
        // symlink picks up `target/idl/<prog>.json`.
        assert!(synth.idl_json.is_none());
    }

    #[test]
    fn synthesize_errors_when_no_handlers_found() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            r#"
[package]
name = "empty"
version = "0.1.0"
"#,
        );
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "// no anchor handlers\n").unwrap();
        let err = synthesize_spec(tmp.path(), Runtime::Anchor).unwrap_err();
        assert!(format!("{err:#}").contains("No `pub fn"));
    }

    #[test]
    fn brownfield_harness_parent_is_qed_fuzz() {
        let root = Path::new("/workspace/my_prog");
        assert_eq!(
            brownfield_harness_parent(root),
            PathBuf::from("/workspace/my_prog/.qed/fuzz")
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // v2.22 Slice 3 — Pinocchio brownfield
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn pinocchio_brownfield_requires_codama_idl_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            r#"
[package]
name = "p"
version = "0.1.0"
"#,
        );
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "// no IDL\n").unwrap();
        let err = synthesize_spec(tmp.path(), Runtime::Pinocchio).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Codama"), "should cite Codama, got: {msg}");
        assert!(
            msg.contains("codama"),
            "should reference the codama CLI; got: {msg}"
        );
    }

    #[test]
    fn pinocchio_brownfield_consumes_codama_idl() {
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            r#"
[package]
name = "subscriptions"
version = "0.1.0"
"#,
        );
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // Source has zero `process_*` handlers — would normally bail.
        std::fs::write(src.join("lib.rs"), "// dispatcher elsewhere\n").unwrap();
        // Codama IDL is on disk → discovery takes precedence.
        let codama = r#"{
  "address": "Suprm111111111111111111111111111111111111",
  "metadata": { "name": "subscriptions", "version": "1.0.0", "spec": "0.1.0" },
  "instructions": [
    { "name": "createPlan", "discriminator": [0], "accounts": [], "args": [] },
    { "name": "updatePlan", "discriminator": [1], "accounts": [], "args": [] }
  ],
  "accounts": [], "errors": [], "events": [], "types": []
}"#;
        std::fs::write(tmp.path().join("idl.json"), codama).unwrap();
        let synth = synthesize_spec(tmp.path(), Runtime::Pinocchio).unwrap();
        let idl = synth.idl_json.expect("on-disk IDL passed through");
        assert!(idl.contains("Suprm"));
        // Handler list synthesized from instructions[].name
        let handler_names: Vec<&str> = synth
            .spec
            .handlers
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(handler_names, vec!["create_plan", "update_plan"]);
    }

    #[test]
    fn pinocchio_brownfield_takes_program_name_from_idl_not_cargo() {
        // Cargo.toml at workspace root has no `[package] name` (the
        // `solana-program-escrow-2026-05` audit corpus shape). The
        // discovered IDL declares `program.name = escrowProgram`. The
        // synthesized spec must use the IDL's name so the harness's
        // `use {prog}::instruction;` matches the
        // `declare_fuzz_program!` macro's module name.
        let tmp = tempfile::tempdir().unwrap();
        // Workspace Cargo.toml — no [package].
        std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "// dispatch elsewhere\n").unwrap();
        let codama_ir = r#"{
  "kind": "rootNode",
  "additionalPrograms": [],
  "program": {
    "kind": "programNode",
    "name": "escrowProgram",
    "publicKey": "Esc1111111111111111111111111111111111111111",
    "version": "1.0.0",
    "instructions": [
      { "kind": "instructionNode", "name": "deposit", "arguments": [], "accounts": [] }
    ],
    "accounts": [], "errors": [], "definedTypes": [], "pdas": []
  },
  "standard": "codama",
  "version": "1.0.0"
}"#;
        std::fs::write(tmp.path().join("idl.json"), codama_ir).unwrap();
        let synth = synthesize_spec(tmp.path(), Runtime::Pinocchio).unwrap();
        // IDL's program.name takes precedence over the tmpdir leaf name.
        assert_eq!(synth.spec.program_name, "escrowProgram");
    }

    #[test]
    fn pinocchio_brownfield_consumes_codama_ir_tree() {
        // Codama IR (the format `solana-program/` Pinocchio crates ship):
        // instructions are nested under `program.instructions[]` with a
        // top-level `kind: "rootNode"` envelope. We must enumerate handlers
        // from this shape too — the multi_delegator subscriptions bench
        // entry uses exactly this layout.
        let tmp = tempfile::tempdir().unwrap();
        write_manifest(
            tmp.path(),
            r#"
[package]
name = "multi_delegator"
version = "0.1.0"
"#,
        );
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "// dispatcher elsewhere\n").unwrap();
        let codama_ir = r#"{
  "additionalPrograms": [],
  "kind": "rootNode",
  "program": {
    "name": "multi_delegator",
    "publicKey": "11111111111111111111111111111111",
    "version": "1.0.0",
    "instructions": [
      { "kind": "instructionNode", "name": "createPlan", "arguments": [], "accounts": [] },
      { "kind": "instructionNode", "name": "transferFixed", "arguments": [], "accounts": [] }
    ],
    "accounts": [], "errors": [], "definedTypes": [], "pdas": []
  },
  "standard": "codama",
  "version": "1.0.0"
}"#;
        std::fs::write(tmp.path().join("idl.json"), codama_ir).unwrap();
        let synth = synthesize_spec(tmp.path(), Runtime::Pinocchio).unwrap();
        let handler_names: Vec<&str> = synth
            .spec
            .handlers
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(handler_names, vec!["create_plan", "transfer_fixed"]);
    }

    #[test]
    fn discover_pinocchio_idl_walks_canonical_paths() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty root → None
        assert!(discover_pinocchio_idl(tmp.path()).unwrap().is_none());
        // target/idl/<x>.json present
        std::fs::create_dir_all(tmp.path().join("target/idl")).unwrap();
        std::fs::write(
            tmp.path().join("target/idl/foo.json"),
            "{\"address\":\"A\"}",
        )
        .unwrap();
        let found = discover_pinocchio_idl(tmp.path()).unwrap().unwrap();
        assert!(found.contains("\"A\""));
        // <root>/idl.json beats target/idl
        std::fs::write(tmp.path().join("idl.json"), "{\"address\":\"B\"}").unwrap();
        let found2 = discover_pinocchio_idl(tmp.path()).unwrap().unwrap();
        assert!(
            found2.contains("\"B\""),
            "root idl.json should take precedence"
        );
    }

    #[test]
    fn write_synthesized_idl_creates_idls_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = write_synthesized_idl(tmp.path(), "myprog", "{\"address\": \"x\"}").unwrap();
        assert!(dest.ends_with("idls/myprog.json"));
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "{\"address\": \"x\"}"
        );
    }
}
