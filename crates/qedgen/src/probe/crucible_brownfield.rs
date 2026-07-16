//! Brownfield Crucible harness emission.
//!
//! Lifts the `qedgen probe --fuzz requires --spec` gate by synthesising a
//! minimal [`ParsedSpec`] from a brownfield project root (no `.qedspec`
//! required). The synthesised spec carries enough handler metadata for
//! [`crucible_gen::generate`] to emit a working harness with mechanical
//! post-state guards for lamports, account ownership/type, close/realloc
//! behavior, rent exemption, and token conservation. Program-internal faults
//! remain transaction errors; they require a spec assertion or a separate
//! reproducer.
//!
//! ## Runtime coverage
//!
//! - **Anchor / Quasar / qedgen-codegen** — handler enumeration via the
//!   regex used by `anchor_extractor::scan_handler_context_map`; IDL
//!   discovery reuses spec-mode's `target/idl/<prog>.json` lookup.
//! - **Pinocchio** — deliberately gated on an on-disk Codama / Anchor 0.30
//!   IDL (canonical paths: `idl.json`, `program/idl.json`, `idl/*.json`,
//!   `target/idl/*.json`), passed through to `<harness>/idls/<prog>.json`
//!   verbatim. Scanner-based metadata inference from handler bodies is
//!   intentionally out of scope — account flags and arg types extracted
//!   via regex are too noisy to ship; the maintainer-authored Codama IDL
//!   is the trusted source.
//! - **Native / sBPF** — unsupported (errors with a clear message). Native
//!   retains its static probe path. sBPF assembly uses the dedicated
//!   `.qedspec` and Lean/qedsvm proof path.

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};

use crate::check::{ParsedHandler, ParsedHandlerAccount, ParsedSpec};
use crate::probe::Runtime;

/// Output of a brownfield synthesis: the [`ParsedSpec`] that drives
/// `crucible_gen::generate` plus, when the runtime needs it, a
/// pre-rendered IDL JSON to drop at `<harness>/idls/<prog>.json` (the
/// macro input).
#[derive(Debug)]
pub struct BrownfieldSynthesis {
    pub spec: ParsedSpec,
    /// Pinocchio: IDL JSON for the harness. Anchor-family: `None` —
    /// `crucible_probe::discover_idl` symlinks the `anchor build` IDL.
    pub idl_json: Option<String>,
}

/// Synthesise a [`ParsedSpec`] from a brownfield project root:
///
/// - `program_name` from `Cargo.toml`'s `[package] name` (falling back to
///   the root's leaf directory name).
/// - `handlers[]` from `pub fn <name>(ctx: Context<X>, ...)` signatures
///   under `src/`. Handler params stay empty — Crucible's IDL-derived
///   typed builders generate the payload at fuzz time, and the per-action
///   stub gets an agent-fill `todo!()` (same shape as spec-mode).
/// - No invariants / properties / account types / PDAs — protocol mode
///   doesn't assert spec invariants
///   ([`crate::crucible_gen::InvariantMode::Protocol`]).
pub fn synthesize_spec(project_root: &Path, runtime: Runtime) -> Result<BrownfieldSynthesis> {
    match runtime {
        Runtime::Anchor | Runtime::Quasar | Runtime::QedgenCodegen => {
            synthesize_anchor_family(project_root)
        }
        Runtime::Pinocchio => synthesize_pinocchio(project_root),
        Runtime::Native | Runtime::Sbpf | Runtime::Unknown => bail!(
            "Crucible brownfield mode (`--fuzz --root`) is not supported for `{runtime:?}`. \
             Brownfield Crucible currently supports Anchor, Quasar, qedgen-codegen, and \
             Pinocchio (with an on-disk IDL). Use `qedgen probe --program <path>` for the \
             native static audit envelope. For sBPF assembly, use the dedicated `.qedspec` \
             and Lean/qedsvm proof path. Pass `--runtime <name>` to override detection if needed."
        ),
    }
}

fn empty_handler(name: String) -> ParsedHandler {
    ParsedHandler {
        name,
        permissionless: true,
        ..Default::default()
    }
}

fn synthesize_anchor_family(project_root: &Path) -> Result<BrownfieldSynthesis> {
    // Prefer a committed / `anchor build` IDL: it carries per-account
    // `signer`/`writable` flags, so the emitter fills real `accounts::X
    // { ... }` literals (no `todo!()`) and the §S1.2 lamport-inflation
    // guard gets a tracked signer set to check. Without an IDL we fall back
    // to a source scan that yields handler names only (empty accounts →
    // agent-fill `todo!()`), preserving the v2.21 behaviour.
    if let Some(idl_text) = discover_pinocchio_idl(project_root)? {
        if !handlers_with_args_from_idl(&idl_text).is_empty() {
            return synthesize_from_idl(project_root, idl_text);
        }
    }
    let program_name = program_name_from_root(project_root)?;
    let handlers = scan_anchor_handlers(project_root)?;
    if handlers.is_empty() {
        bail!(
            "No `pub fn <name>(ctx: Context<X>, ...)` handlers found under {}, \
             and no IDL on disk. Brownfield mode needs at least one Anchor \
             handler to fuzz; confirm `--root` points at the program crate \
             (e.g. `programs/my_prog/`), or drop an `idl.json` at the root.",
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

/// Build a brownfield [`BrownfieldSynthesis`] from a parsed IDL — handler
/// args + per-account signer/writable/address flags. Shared by the Anchor
/// (IDL-present) and Pinocchio paths.
fn synthesize_from_idl(project_root: &Path, idl_text: String) -> Result<BrownfieldSynthesis> {
    let handlers_with_args = handlers_with_args_from_idl(&idl_text);
    let accounts_per_handler = accounts_per_handler_from_idl(&idl_text);
    if handlers_with_args.is_empty() {
        bail!(
            "IDL at {} parsed but has no `instructions[]` entries. \
             Brownfield fuzz needs at least one instruction to dispatch.",
            project_root.display()
        );
    }
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

fn synthesize_pinocchio(project_root: &Path) -> Result<BrownfieldSynthesis> {
    // Pinocchio brownfield is gated on a maintainer-authored Codama /
    // Anchor 0.30 IDL. Scanner-based account & arg inference from
    // `pub fn process_*` bodies is fragile — `.borrow_mut_*` patterns miss
    // CPI-mutated accounts, `from_le_bytes` patterns miss zero-copy
    // unpacking, and account-name suffix conventions vary by codebase.
    //
    // Future runtimes follow the same gate: Shank for legacy native Rust
    // programs; custom dispatchers carry a Codama IDL via codama-cli or
    // are out of scope.
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

    // Program name comes from the IDL inside `synthesize_from_idl`:
    // declare_fuzz_program! derives the generated module name from the
    // IDL's `program.name` (Codama IR) or `metadata.name` (Anchor 0.30),
    // and the harness's `use {prog}::instruction;` must line up with that.
    synthesize_from_idl(project_root, idl_text)
}

/// Program name from an Anchor 0.30 IDL (`metadata.name`) or Codama IR
/// (`program.name`); `None` when neither is present.
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

/// Find a Codama / Anchor 0.30 IDL under the project root; contents are
/// returned verbatim so the macro consumes the maintainer-authored schema.
///
/// Lookup order (first match wins):
/// 1. `<root>/idl.json` — Codama convention.
/// 2. `<root>/program/idl.json` — workspace-rooted variant.
/// 3. `<root>/target/idl/*.json` — `anchor build` output.
/// 4. `<root>/idl/*.json` — Codama default output dir.
///
/// Same-level matches are sorted alphabetically and the first picked —
/// deterministic across runs. The path walk itself lives in
/// `idl_overlay::discover_idl` (#235) so the fuzz gate and the bootstrap
/// enrichment overlay can never disagree on where an IDL may live.
pub(crate) fn discover_pinocchio_idl(project_root: &Path) -> Result<Option<String>> {
    Ok(crate::probe::idl_overlay::discover_idl(project_root)?.map(|(_, text)| text))
}

/// Per-instruction `(snake_name, vec![(arg_name, type), ...])`. The
/// emitter needs `takes_params` to build `instruction::Foo { ... }`
/// literals matching the macro-generated struct — untyped args leave
/// fields uninitialised (E0063). Args flagged
/// `defaultValueStrategy: "omitted"` (e.g. discriminators) are skipped;
/// the macro doesn't surface them as struct fields.
///
/// Unrecognised types map to a `"u64"` placeholder: it compiles, but a
/// type-coercion failure in the macro's field is the signal that the type
/// isn't supported yet (refine the IDL or accept the compile error).
pub(crate) fn handlers_with_args_from_idl(idl_text: &str) -> Vec<(String, Vec<(String, String)>)> {
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
                            // Skip `defaultValueStrategy: "omitted"` args —
                            // the macro elides them from the struct.
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

/// Per-handler account lists, keyed by snake_case handler name (matching
/// `handlers_with_args_from_idl`), in declaration order — lets the emitted
/// harness produce a real `accounts::Foo { ... }` initializer instead of
/// `todo!()`.
///
/// Codama fields used:
/// - `name` → ParsedHandlerAccount.name, kept camelCase to match the
///   macro-generated struct field.
/// - `isSigner` / `isWritable` → flags.
/// - `defaultValue.kind: "publicKeyValueNode"` → base58 in `default_pubkey`
///   so the emitter renders `solana_pubkey::pubkey!("...")`.
/// - Anchor `pda.seeds` → literal/account/argument seed expressions retained
///   for seed-aware `Pubkey::find_program_address` codegen.
/// - `defaultValue.kind: "pdaValueNode"` → `pda_seeds: Some(vec![])` when
///   Codama does not carry the derivation inline (an empty seed tuple is also
///   a valid Anchor PDA and remains distinguishable only from the IDL shape).
pub(crate) fn accounts_per_handler_from_idl(
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
        let mut accounts = Vec::new();
        collect_idl_accounts(arr, &mut accounts);
        out.insert(snake, accounts);
    }
    out
}

/// Flatten Anchor composite account groups into the actual instruction-account
/// order. Anchor serializes a composite as `{ name, accounts: [...] }`; treating
/// that wrapper as a non-signer hides signer leaves and can make the bootstrap
/// overlay unsafely classify a gated instruction as permissionless.
fn collect_idl_accounts(items: &[serde_json::Value], out: &mut Vec<ParsedHandlerAccount>) {
    for a in items {
        if let Some(nested) = a.get("accounts").and_then(|v| v.as_array()) {
            collect_idl_accounts(nested, out);
            continue;
        }
        let Some(name) = a.get("name").and_then(|n| n.as_str()).map(str::to_string) else {
            continue;
        };
        // Anchor ≥0.30 IDLs use `signer`/`writable`; Codama IRs use
        // `isSigner`/`isWritable`; legacy Anchor uses `isSigner`/
        // `isMut`. Accept all three.
        let is_signer = a
            .get("signer")
            .or_else(|| a.get("isSigner"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let is_writable = a
            .get("writable")
            .or_else(|| a.get("isWritable"))
            .or_else(|| a.get("isMut"))
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let default = a.get("defaultValue");
        let default_kind = default.and_then(|d| d.get("kind")).and_then(|k| k.as_str());
        let (default_pubkey, pda_seeds, is_program) = if let Some(pda) = a.get("pda") {
            // Anchor ≥0.30 marks a PDA account with a top-level
            // `"pda"` object. Preserve its seed tuple so Crucible
            // derives the same address as the deployed program.
            (None, Some(render_idl_pda_seeds(pda)), false)
        } else {
            match default_kind {
                Some("publicKeyValueNode") => {
                    let pk = default
                        .and_then(|d| d.get("publicKey"))
                        .and_then(|k| k.as_str())
                        .map(|s| s.to_string());
                    // For our scope, a publicKeyValueNode pointing at
                    // a fixed pubkey is effectively a program/sysvar
                    // account.
                    (pk, None, true)
                }
                // Codama IR PDA node.
                Some("pdaValueNode") => (None, Some(vec![]), false),
                // Anchor ≥0.30 emits a fixed-address account (e.g.
                // `Program<System>`) as a top-level `"address"` field
                // rather than a `defaultValue` node — treat it the
                // same as a publicKeyValueNode so the emitter
                // auto-fills it.
                _ => match a.get("address").and_then(|k| k.as_str()) {
                    Some(addr) => (Some(addr.to_string()), None, true),
                    None => (None, None, false),
                },
            }
        };
        out.push(ParsedHandlerAccount {
            name,
            is_signer,
            is_writable,
            is_program,
            pda_seeds,
            account_type: None,
            authority: None,
            default_pubkey,
            imported_namespace: None,
        });
    }
}

/// Render Anchor 0.30 PDA seed nodes into the same compact representation
/// used by `ParsedHandlerAccount::pda_seeds`: quoted UTF-8 literals and bare
/// account/argument identifiers. Unknown constants remain explicit instead
/// of being silently converted into an empty seed tuple.
fn render_idl_pda_seeds(pda: &serde_json::Value) -> Vec<String> {
    pda.get("seeds")
        .and_then(|seeds| seeds.as_array())
        .into_iter()
        .flatten()
        .map(|seed| {
            if let Some(path) = seed.get("path").and_then(|path| path.as_str()) {
                return path.split('.').next_back().unwrap_or(path).to_string();
            }
            if let Some(bytes) = seed.get("value").and_then(|value| value.as_array()) {
                let bytes: Vec<u8> = bytes
                    .iter()
                    .filter_map(|byte| byte.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect();
                if let Ok(text) = String::from_utf8(bytes.clone()) {
                    return format!("\"{}\"", text.escape_default());
                }
                return format!(
                    "bytes:{}",
                    bytes
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                );
            }
            "__qedgen_unsupported_const_seed".to_string()
        })
        .collect()
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

/// Map an IDL argument `type` to a qedgen DSL type identifier (`U8`,
/// `Pubkey`, ...) — the `ParsedHandler::takes_params` convention, which
/// the emitter runs back through `crucible_gen::map_simple_type`.
/// Tolerates both Codama IR trees (`{kind, format, ...}`) and Anchor 0.30
/// string shorthand (`"u64"`).
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

/// Write the synthesised IDL JSON into `<harness>/idls/<prog>.json`,
/// overwriting any existing file so re-runs pick up scanner improvements.
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

/// `Cargo.toml`'s `[package] name`, falling back to the root's
/// leaf-directory name when missing or unparseable (both happen when
/// `--root` points at a workspace-level path). Downstream surfaces a
/// cleaner error if the program crate can't be resolved at IDL discovery.
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

/// Extract `name = "..."` from a `[package]` section. Hand-rolled rather
/// than pulling `toml` as a dep (cf. `anchor_resolver.rs`).
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

/// Collect handler names from `pub fn <name>(ctx: Context<X>, ...)`
/// signatures under `src/`. De-dupes by name (Anchor sometimes splits
/// handlers across module re-exports).
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
    for file in crate::fs_walk::collect_rs_files(&src_dir, crate::fs_walk::DEFAULT_SKIP_DIRS) {
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

/// Default brownfield harness location: `<root>/.qed/fuzz/`. Returns the
/// parent dir — `crucible_gen::generate` appends the program-name leaf
/// (spec-mode convention).
pub fn brownfield_harness_parent(root: &Path) -> PathBuf {
    root.join(".qed").join("fuzz")
}

/// Project-root resolution from `--root`. Currently returns the input
/// unchanged; kept as a seam so a workspace walker (descend to the first
/// `declare_id!` crate) can swap in without a CLI shape change.
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
                msg.contains("not supported")
                    && msg.contains("Anchor, Quasar, qedgen-codegen, and Pinocchio"),
                "{label} bail should name the current supported runtimes, got: {msg}"
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
        // Anchor path doesn't synthesise an IDL — discover_idl symlinks
        // `target/idl/<prog>.json`.
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

    // Pinocchio brownfield

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
    fn anchor_idl_pda_seeds_survive_brownfield_synthesis() {
        let idl = r#"{
  "metadata": { "name": "seeded_vault" },
  "instructions": [{
    "name": "withdraw",
    "accounts": [
      { "name": "authority", "signer": true },
      { "name": "vault", "writable": true, "pda": { "seeds": [
        { "kind": "const", "value": [118, 97, 117, 108, 116] },
        { "kind": "account", "path": "authority" },
        { "kind": "arg", "path": "laneId" }
      ] } }
    ],
    "args": [{ "name": "laneId", "type": "u64" }]
  }]
}"#;
        let accounts = accounts_per_handler_from_idl(idl);
        let vault = accounts["withdraw"]
            .iter()
            .find(|account| account.name == "vault")
            .expect("vault account");
        assert_eq!(
            vault
                .pda_seeds
                .as_ref()
                .map(|seeds| seeds.iter().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["\"vault\"", "authority", "laneId"])
        );
    }

    #[test]
    fn pinocchio_brownfield_takes_program_name_from_idl_not_cargo() {
        // Workspace-root Cargo.toml with no `[package] name`; the IDL
        // declares `program.name = escrowProgram`. The spec must use the
        // IDL's name so the harness's `use {prog}::instruction;` matches
        // the `declare_fuzz_program!` module name.
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
        // Codama IR nests instructions under `program.instructions[]`
        // inside a `kind: "rootNode"` envelope — handlers must be
        // enumerable from this shape too.
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
