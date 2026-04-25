use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedSpec};
use crate::fingerprint::SpecFingerprint;
use crate::spec_hash;

/// Compute a path, as a string, from a program `Cargo.toml` directory to the
/// spec file. This value is embedded verbatim in the `#[qed(spec = "...")]`
/// attribute and resolved at compile time relative to `CARGO_MANIFEST_DIR`.
///
/// Best-effort: if the spec isn't under a path we can express relatively,
/// fall back to the absolute path (works as long as the repo doesn't move).
fn relative_spec_path(spec_path: &Path, manifest_dir: &Path) -> String {
    // Canonicalize both; fall back to the raw paths on failure.
    let spec = spec_path
        .canonicalize()
        .unwrap_or_else(|_| spec_path.to_path_buf());
    let manifest = manifest_dir
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.to_path_buf());
    let spec_components: Vec<_> = spec.components().collect();
    let manifest_components: Vec<_> = manifest.components().collect();

    // Find common prefix length.
    let common = spec_components
        .iter()
        .zip(manifest_components.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut out = std::path::PathBuf::new();
    for _ in 0..(manifest_components.len().saturating_sub(common)) {
        out.push("..");
    }
    for comp in &spec_components[common..] {
        out.push(comp.as_os_str());
    }
    if out.as_os_str().is_empty() {
        spec.display().to_string()
    } else {
        out.to_string_lossy().replace('\\', "/")
    }
}

/// Map a DSL type to its Rust equivalent.
///
/// Handles:
///   - primitives (U8..U128, I8..I128, Bool, Pubkey),
///   - `Map[N] T` fixed-size containers (N = numeric literal or declared
///     constant; inner T recurses through this function) → `[T; N]`,
///   - `Fin[N]` → `usize` (index type with a bound; bound is informational),
///   - type aliases declared via `type Name = RHS` — resolved transitively,
///   - record type names (`type Foo = { ... }`) — returned as-is; the
///     generated Rust emits a corresponding `struct Foo { ... }` declaration
///     (see `emit_record_decls` in rust_codegen_util.rs),
///   - sum type names (`type Error | A | B | C`) — returned as-is; the
///     generated Rust emits a corresponding Rust enum (unit variants only;
///     payload variants are S3 narrow: name resolves but enum is flattened).
///
/// Returns an error for anything else, rather than silently passing it
/// through — the fall-through in v2.6.1 was the root cause of the codegen-
/// bug class where types like `U16` or `Map[N] UserAccount` leaked verbatim
/// into generated Rust (see docs/prds/PRD-v2.6.2.md G1).
pub fn map_type(dsl_type: &str, spec: &ParsedSpec) -> Result<String> {
    let dsl_type = dsl_type.trim();

    // Compound type: Map[BOUND] T → [T; N]
    if let Some(rest) = dsl_type.strip_prefix("Map") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('[') {
            if let Some(close) = rest.find(']') {
                let bound_src = rest[..close].trim();
                let inner_src = rest[close + 1..].trim();
                let n = resolve_map_bound(bound_src, &spec.constants)?;
                let inner_rust = map_type(inner_src, spec)?;
                return Ok(format!("[{inner_rust}; {n}]"));
            }
        }
        anyhow::bail!(
            "malformed Map type `{}` — expected `Map[BOUND] T`",
            dsl_type
        );
    }

    // Fin[N] → usize. N is informational (bound for index-type safety in
    // the DSL); in Rust we just use usize.
    if let Some(rest) = dsl_type.strip_prefix("Fin") {
        let rest = rest.trim_start();
        if rest.starts_with('[') {
            return Ok("usize".to_string());
        }
    }

    // Primitive match — check first so `U8` etc. never hit the alias path.
    if let Some(rust) = primitive_map(dsl_type) {
        return Ok(rust.to_string());
    }

    // Type alias: `type Foo = Bar` — recurse on the RHS. Transitive.
    if let Some((_, rhs)) = spec.type_aliases.iter().find(|(n, _)| n == dsl_type) {
        return map_type(rhs, spec);
    }

    // Record type declared in the spec — return the name as-is. The generator
    // is responsible for emitting a `struct <Name> { ... }` alongside the
    // State struct.
    if spec.records.iter().any(|r| r.name == dsl_type) {
        return Ok(dsl_type.to_string());
    }

    // Sum type declared in the spec — return the name as-is. For S3 narrow,
    // only no-payload sums (Error-like enums) are fully supported; sums with
    // payload variants resolve by name but the generator flattens to a
    // primary variant's fields (see `resolve_state_fields`).
    if spec.sum_types.iter().any(|s| s.name == dsl_type) {
        return Ok(dsl_type.to_string());
    }

    anyhow::bail!(
        "unsupported DSL type `{}` — expected a primitive (U8/U16/U32/U64/U128, I8/I16/I32/I64/I128, Bool, Pubkey), a compound (Map[N] T, Fin[N]), or a user-defined type declared with `type` in the spec",
        dsl_type
    );
}

/// Map a DSL primitive name to its Rust equivalent, if one exists. Factored
/// out of `map_type` so both the primitive fast-path and the alias-recursion
/// base case can share it.
fn primitive_map(dsl_type: &str) -> Option<&'static str> {
    Some(match dsl_type {
        "Pubkey" => "Address",
        "U8" => "u8",
        "U16" => "u16",
        "U32" => "u32",
        "U64" => "u64",
        "U128" => "u128",
        "I8" => "i8",
        "I16" => "i16",
        "I32" => "i32",
        "I64" => "i64",
        "I128" => "i128",
        "Bool" => "bool",
        _ => return None,
    })
}

/// Resolve the bound expression inside `Map[BOUND] T`. Accepts either a
/// numeric literal (e.g. `Map[16] U64`) or a constant declared in the spec
/// (e.g. `Map[MAX_ACCOUNTS] U64`).
fn resolve_map_bound(bound: &str, constants: &[(String, String)]) -> Result<String> {
    let bound = bound.trim();
    if bound.chars().all(|c| c.is_ascii_digit()) && !bound.is_empty() {
        return Ok(bound.to_string());
    }
    match constants.iter().find(|(n, _)| n == bound) {
        Some((_, value)) => Ok(value.clone()),
        None => anyhow::bail!(
            "Map bound `{}` is not a numeric literal and not declared as a `const` in the spec",
            bound
        ),
    }
}

/// Sanitize a field-path string (e.g. `accounts[i].active`) into a legal
/// Rust identifier stem suitable for interpolation into `fn verify_*` names
/// and similar. Non-identifier characters become `_`; consecutive and
/// trailing `_` are collapsed.
///
/// Motivated by the v2.6.1 eval (percolator-prog, qedgen-bug-report §2):
/// subscripted effect targets like `accounts[i].active` landed verbatim
/// inside `format!("fn verify_{}_effect_{}", op.name, field)`, producing
/// Rust-illegal identifiers such as `verify_init_user_effect_accounts[i].active`.
pub fn sanitize_ident(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_underscore = false;
    for c in path.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            prev_underscore = c == '_';
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Convert a snake_case operation name to PascalCase for struct names.
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + &chars.collect::<String>(),
            }
        })
        .collect()
}

/// Format the "GENERATED BY QEDGEN" marker with the per-file spec hash.
/// Thin wrapper around `crate::banner::banner` that resolves the hash from
/// the fingerprint table by file_key.
fn marker(label: &str, fp: &SpecFingerprint, file_key: &str) -> String {
    let hash = fp
        .file_hashes
        .get(file_key)
        .map(String::as_str)
        .unwrap_or("");
    crate::banner::banner(Some(label), hash)
}

// ============================================================================
// File generators
// ============================================================================

/// Generate src/lib.rs. Skip if the file already exists — once the user has
/// stamped custom imports or extra modules onto the crate shell, regenerating
/// it would silently clobber that edit. Paired with the per-handler
/// `instructions/<name>.rs` skip, this keeps `qedgen codegen` idempotent.
fn generate_lib(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let lib_path = src_dir.join("lib.rs");
    if lib_path.exists() {
        eprintln!(
            "programs/{}/src/lib.rs already exists — skipping (user-owned). guards.rs regenerated.",
            output_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<program>")
        );
        return Ok(());
    }

    let program_name = spec.program_name.to_lowercase();
    let program_id = spec
        .program_id
        .as_deref()
        .unwrap_or("11111111111111111111111111111111");

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/lib.rs"));
    out.push_str("#![no_std]\n\n");
    out.push_str("use anchor_lang::prelude::*;\n\n");
    out.push_str("mod instructions;\nuse instructions::*;\n");

    if !spec.events.is_empty() {
        out.push_str("pub mod events;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("pub mod errors;\n");
    }
    out.push_str("pub mod state;\n");
    out.push_str("pub mod guards;\n\n");

    out.push_str(&format!("declare_id!(\"{}\");\n\n", program_id));

    out.push_str("#[program]\n");
    out.push_str(&format!("mod {} {{\n", program_name));
    out.push_str("    use super::*;\n\n");

    for (i, handler) in spec.handlers.iter().enumerate() {
        let pascal = to_pascal_case(&handler.name);

        if let Some(ref doc) = handler.doc {
            out.push_str(&format!("    /// {}\n", doc));
        }
        out.push_str(&format!("    #[instruction(discriminator = {})]\n", i));

        let mut params = String::from("ctx: Ctx<");
        params.push_str(&pascal);
        params.push('>');

        for (pname, ptype) in &handler.takes_params {
            params.push_str(&format!(", {}: {}", pname, map_type(ptype, spec)?));
        }

        out.push_str(&format!(
            "    pub fn {}({}) -> Result<(), ProgramError> {{\n",
            handler.name, params
        ));

        if handler.has_bumps() {
            out.push_str(&format!(
                "        ctx.accounts.handler({}&ctx.bumps)\n",
                handler
                    .takes_params
                    .iter()
                    .map(|(n, _)| format!("{}, ", n))
                    .collect::<String>()
            ));
        } else {
            out.push_str(&format!(
                "        ctx.accounts.handler({})\n",
                handler
                    .takes_params
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push_str("    }\n\n");
    }

    out.push_str("}\n");
    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("lib.rs"), &out)?;
    Ok(())
}

/// Generate src/state.rs
fn generate_state(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let is_multi = spec.account_types.len() > 1;

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/state.rs"));
    out.push_str("use anchor_lang::prelude::*;\n\n");

    if is_multi {
        for (idx, acct) in spec.account_types.iter().enumerate() {
            let struct_name = format!("{}Account", acct.name);

            let pda_seeds = if let Some(ref pda_name) = acct.pda_ref {
                if let Some(pda) = spec.pdas.iter().find(|p| &p.name == pda_name) {
                    gen_pda_seeds_attr(pda, &acct.fields, spec)?
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            out.push_str(&format!(
                "#[account(discriminator = {}, set_inner)]\n{}pub struct {} {{\n",
                idx + 1,
                pda_seeds,
                struct_name
            ));

            for (fname, ftype) in &acct.fields {
                out.push_str(&format!("    pub {}: {},\n", fname, map_type(ftype, spec)?));
            }

            if acct.pda_ref.is_some() && !acct.fields.iter().any(|(n, _)| n == "bump") {
                out.push_str("    pub bump: u8,\n");
            }

            out.push_str("}\n\n");

            if !acct.lifecycle.is_empty() {
                out.push_str(&format!("/// {} lifecycle states.\n", acct.name));
                out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
                out.push_str("#[repr(u8)]\n");
                out.push_str(&format!("pub enum {}Status {{\n", acct.name));
                for (i, state) in acct.lifecycle.iter().enumerate() {
                    out.push_str(&format!("    {} = {},\n", state, i));
                }
                out.push_str("}\n\n");
            }
        }
    } else {
        let state_name = format!("{}Account", to_pascal_case(&spec.program_name));

        let pda_seeds = if !spec.pdas.is_empty() {
            gen_pda_seeds_attr(&spec.pdas[0], &spec.state_fields, spec)?
        } else {
            String::new()
        };

        out.push_str(&format!(
            "#[account(discriminator = 1, set_inner)]\n{}pub struct {} {{\n",
            pda_seeds, state_name
        ));

        for (fname, ftype) in &spec.state_fields {
            out.push_str(&format!("    pub {}: {},\n", fname, map_type(ftype, spec)?));
        }

        if !spec.pdas.is_empty() && !spec.state_fields.iter().any(|(n, _)| n == "bump") {
            out.push_str("    pub bump: u8,\n");
        }

        out.push_str("}\n");

        if !spec.lifecycle_states.is_empty() {
            out.push_str("\n/// Program lifecycle states.\n");
            out.push_str("#[derive(Clone, Copy, PartialEq, Eq)]\n");
            out.push_str("#[repr(u8)]\n");
            out.push_str("pub enum Status {\n");
            for (i, state) in spec.lifecycle_states.iter().enumerate() {
                out.push_str(&format!("    {} = {},\n", state, i));
            }
            out.push_str("}\n");
        }
    }

    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("state.rs"), &out)?;
    Ok(())
}

/// Generate PDA seeds attribute for a PDA declaration.
fn gen_pda_seeds_attr(
    pda: &crate::check::ParsedPda,
    fields: &[(String, String)],
    spec: &ParsedSpec,
) -> Result<String> {
    let mut seed_parts = Vec::new();
    for seed in &pda.seeds {
        let trimmed = seed.trim_matches('"');
        if seed.starts_with('"') || seed.starts_with('\"') {
            seed_parts.push(format!("b\"{}\"", trimmed));
        } else {
            let field_type = match fields.iter().find(|(n, _)| n == trimmed) {
                Some((_, t)) => map_type(t, spec)?,
                None => "Address".to_string(),
            };
            seed_parts.push(format!("{}: {}", trimmed, field_type));
        }
    }
    Ok(format!("#[seeds({})]\n", seed_parts.join(", ")))
}

/// Generate src/events.rs (only if events are declared)
fn generate_events(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    if spec.events.is_empty() {
        return Ok(());
    }

    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/events.rs"));
    out.push_str("use anchor_lang::prelude::*;\n\n");

    for (i, event) in spec.events.iter().enumerate() {
        out.push_str(&format!("#[event(discriminator = {})]\n", i + 1));
        out.push_str(&format!("pub struct {} {{\n", event.name));
        for (fname, ftype) in &event.fields {
            out.push_str(&format!("    pub {}: {},\n", fname, map_type(ftype, spec)?));
        }
        out.push_str("}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("events.rs"), &out)?;
    Ok(())
}

/// Generate src/errors.rs (only if error codes are declared)
fn generate_errors(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    if spec.error_codes.is_empty() {
        return Ok(());
    }

    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let error_name = format!("{}Error", to_pascal_case(&spec.program_name));

    let mut out = String::new();
    out.push_str(&marker("DO NOT EDIT", fp, "src/errors.rs"));
    out.push_str("use anchor_lang::prelude::*;\n\n");

    out.push_str("#[error_code]\n");
    out.push_str(&format!("pub enum {} {{\n", error_name));
    for (i, code) in spec.error_codes.iter().enumerate() {
        out.push_str(&format!("    {} = {},\n", code, i));
    }
    out.push_str("}\n");
    out.push_str("// ---- END GENERATED ----\n");

    std::fs::write(src_dir.join("errors.rs"), &out)?;
    Ok(())
}

/// Generate src/instructions/mod.rs and per-handler files.
///
/// `mod.rs` is always regenerated (pure scaffold: `pub mod` declarations).
/// Per-handler `src/instructions/<name>.rs` files are USER-OWNED: emitted
/// only when missing. Each scaffolded handler body calls
/// `crate::guards::<name>(...)?` then falls through to `todo!()` for the
/// agent to fill in business logic. The `#[qed(verified, spec, handler,
/// hash, spec_hash)]` attribute ties the body and the spec contract
/// together at compile time.
fn generate_instructions(
    spec: &ParsedSpec,
    fp: &SpecFingerprint,
    spec_path: &Path,
    output_dir: &Path,
) -> Result<()> {
    let instr_dir = output_dir.join("src").join("instructions");
    std::fs::create_dir_all(&instr_dir)?;

    let is_multi = spec.account_types.len() > 1;
    let default_state_name = format!("{}Account", to_pascal_case(&spec.program_name));

    // mod.rs — always regenerated, pure scaffold.
    let mut mod_out = String::new();
    mod_out.push_str(&marker("DO NOT EDIT", fp, "src/instructions/mod.rs"));
    for handler in &spec.handlers {
        mod_out.push_str(&format!("pub mod {};\n", handler.name));
    }
    mod_out.push('\n');
    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        mod_out.push_str(&format!("pub use {}::{};\n", handler.name, pascal));
    }
    mod_out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(instr_dir.join("mod.rs"), &mod_out)?;

    // Read spec source once — used for spec_hash attributes.
    // `read_spec_source` handles both single-file and multi-file (directory)
    // specs, concatenating fragments in the same order the loader merges them.
    let spec_src = crate::check::read_spec_source(spec_path).unwrap_or_default();
    let spec_attr = relative_spec_path(spec_path, output_dir);

    // Per-handler instruction files — skip if existing (user-owned).
    for handler in &spec.handlers {
        let handler_path = instr_dir.join(format!("{}.rs", handler.name));
        if handler_path.exists() {
            eprintln!(
                "programs/{}/src/instructions/{}.rs already exists — skipping (user-owned). guards.rs regenerated.",
                output_dir.file_name().and_then(|n| n.to_str()).unwrap_or("<program>"),
                handler.name
            );
            continue;
        }

        let out = render_handler_scaffold(
            handler,
            spec,
            is_multi,
            &default_state_name,
            &spec_src,
            &spec_attr,
        )?;
        std::fs::write(&handler_path, &out)?;
    }

    Ok(())
}

/// Render the initial scaffold for a single user-owned handler file.
/// Identify the writable state-holding account in a handler. A handler's
/// accounts include user signers, token/mint accounts, programs, and
/// PDA-derived state holders; only the last category can receive a `self.X.field = ...`
/// effect expansion. Returns None when the handler has zero or multiple
/// plausible state accounts — in which case the caller must fall back to
/// `todo!()` and let a human (or M4 agent) disambiguate.
fn find_state_account(handler: &ParsedHandler) -> Option<&crate::check::ParsedHandlerAccount> {
    let mut candidates: Vec<&crate::check::ParsedHandlerAccount> = handler
        .accounts
        .iter()
        .filter(|a| a.is_writable && !a.is_signer && !a.is_program)
        .filter(|a| {
            // Drop token/mint accounts — they hold balances, not program state.
            !matches!(a.account_type.as_deref(), Some("token") | Some("mint"))
        })
        .collect();

    // Prefer PDA-derived candidates when available.
    let pda_candidates: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|a| a.pda_seeds.is_some())
        .collect();
    if !pda_candidates.is_empty() {
        candidates = pda_candidates;
    }

    if candidates.len() == 1 {
        Some(candidates[0])
    } else {
        None
    }
}

/// Canonical SPL Token program ID. Calls into an interface whose
/// `program_id "..."` matches this constant get the `anchor_spl::token::*`
/// CPI shape (v2.8 G4). Other program IDs fall through to the
/// agent-fill comment for now — generic `invoke` codegen lands in v2.9.
const SPL_TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Try to emit a real Anchor CPI invocation for one `call Interface.handler(...)`
/// site. Returns `None` when the interface isn't recognized (caller falls
/// back to a comment + `todo!()` so the user / an LLM fills the body).
///
/// v2.8 covers all five SPL Token handlers — `transfer`, `mint_to`,
/// `burn`, `initialize_account`, `close_account` — via `anchor_spl::token::*`.
/// Non-SPL-Token interfaces ship a generic `solana_program::program::invoke`
/// shape in v2.9. The canonical SPL handlers are the bulk of CPI traffic
/// in deployed programs, so this scope is enough to remove `todo!()` from
/// the typical escrow / lending / vault shape.
fn try_emit_anchor_cpi(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let iface = spec
        .interfaces
        .iter()
        .find(|i| i.name == call.target_interface)?;

    // Only the canonical SPL Token program is supported in v2.8.
    if iface.program_id.as_deref() != Some(SPL_TOKEN_PROGRAM_ID) {
        return None;
    }

    let token_program_acct = find_token_program_account(handler)?;
    let prog_name = &token_program_acct.name;

    match call.target_handler.as_str() {
        "transfer" => emit_spl(
            call,
            handler,
            prog_name,
            "Transfer",
            &[("from", "from"), ("to", "to"), ("authority", "authority")],
            Some("amount"),
            "transfer",
        ),
        "mint_to" => emit_spl(
            call,
            handler,
            prog_name,
            "MintTo",
            &[
                ("mint", "mint"),
                ("to", "to"),
                // anchor_spl's MintTo uses `authority`; the canonical
                // qedspec interface names it `mint_authority` to match the
                // SPL Token instruction docs. Map between them at the
                // codegen boundary.
                ("authority", "mint_authority"),
            ],
            Some("amount"),
            "mint_to",
        ),
        "burn" => emit_spl(
            call,
            handler,
            prog_name,
            "Burn",
            &[
                ("mint", "mint"),
                ("from", "from"),
                ("authority", "authority"),
            ],
            Some("amount"),
            "burn",
        ),
        "initialize_account" => emit_spl(
            call,
            handler,
            prog_name,
            "InitializeAccount",
            &[
                ("account", "account"),
                ("mint", "mint"),
                // anchor_spl's InitializeAccount uses `authority` for the
                // owner slot; the canonical qedspec interface names it
                // `owner` to match SPL Token instruction docs.
                ("authority", "owner"),
                ("rent", "rent"),
            ],
            None,
            "initialize_account",
        ),
        "close_account" => emit_spl(
            call,
            handler,
            prog_name,
            "CloseAccount",
            &[
                ("account", "account"),
                ("destination", "destination"),
                ("authority", "authority"),
            ],
            None,
            "close_account",
        ),
        _ => None,
    }
}

/// Find the handler-side `<name> : program` account that points at the
/// token program. Convention: any `is_program` account named
/// `token_program`, or the unique `is_program` account otherwise.
fn find_token_program_account(
    handler: &ParsedHandler,
) -> Option<&crate::check::ParsedHandlerAccount> {
    handler
        .accounts
        .iter()
        .find(|a| a.is_program && a.name == "token_program")
        .or_else(|| {
            let programs: Vec<_> = handler.accounts.iter().filter(|a| a.is_program).collect();
            (programs.len() == 1).then_some(programs[0])
        })
}

/// Emit one `anchor_spl::token::<fn>` CPI body. Generic over which SPL
/// Token handler is being called — the differences are the Anchor accounts
/// struct name, the call-arg → struct-field name map, the optional
/// scalar argument (e.g. `amount` for transfer / mint_to / burn; absent
/// for initialize_account / close_account), and the function name.
///
/// `field_to_arg` is `(anchor_field_name, call_arg_name)` pairs. The arg
/// name is the call-site identifier (matches the qedspec interface's
/// account block); the anchor field name is what `anchor_spl::token`'s
/// accounts struct expects. Most are identity (`("from", "from")`) but
/// some interfaces expose a more semantic name than anchor_spl uses
/// (e.g. `mint_authority` vs `authority`).
fn emit_spl(
    call: &crate::check::ParsedCall,
    handler: &ParsedHandler,
    token_program: &str,
    accounts_struct: &str,
    field_to_arg: &[(&str, &str)],
    scalar_arg: Option<&str>,
    fn_name: &str,
) -> Option<String> {
    // Resolve every account argument via the call site.
    let mut acct_lines: Vec<String> = Vec::with_capacity(field_to_arg.len());
    let max_field = field_to_arg.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
    for (anchor_field, call_arg) in field_to_arg {
        let arg = call.args.iter().find(|a| a.name == *call_arg)?;
        let pad = " ".repeat(max_field - anchor_field.len());
        acct_lines.push(format!(
            "                {}:{} self.{}.to_account_info(),\n",
            anchor_field, pad, arg.rust_expr
        ));
    }

    // Resolve the optional scalar arg (e.g. `amount`).
    let scalar_rhs = match scalar_arg {
        Some(name) => {
            let arg = call.args.iter().find(|a| a.name == name)?;
            Some(resolve_call_arg_for_amount(&arg.rust_expr, handler))
        }
        None => None,
    };

    let mut out = String::new();
    out.push_str("        {\n");
    out.push_str(&format!(
        "            use anchor_spl::token::{{self, {}}};\n",
        accounts_struct
    ));
    out.push_str(&format!(
        "            let cpi_accounts = {} {{\n",
        accounts_struct
    ));
    for line in &acct_lines {
        out.push_str(line);
    }
    out.push_str("            };\n");
    out.push_str(&format!(
        "            let cpi_program = self.{}.to_account_info();\n",
        token_program
    ));
    let invocation = match scalar_rhs {
        Some(rhs) => format!(
            "            token::{}(CpiContext::new(cpi_program, cpi_accounts), {})?;\n",
            fn_name, rhs
        ),
        None => format!(
            "            token::{}(CpiContext::new(cpi_program, cpi_accounts))?;\n",
            fn_name
        ),
    };
    out.push_str(&invocation);
    out.push_str("        }\n");
    Some(out)
}

/// Resolve a numeric / value argument's rust_expr to a form that's in
/// scope inside the handler `impl` block. Bare identifiers that match a
/// state field get the `self.<state_acct>.` prefix; handler params and
/// literals pass through unchanged.
fn resolve_call_arg_for_amount(rust_expr: &str, handler: &ParsedHandler) -> String {
    let is_simple_ident = rust_expr
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !is_simple_ident {
        return rust_expr.to_string();
    }
    if handler.takes_params.iter().any(|(n, _)| n == rust_expr) {
        return rust_expr.to_string();
    }
    if let Some(sa) = find_state_account(handler) {
        return format!("self.{}.{}", sa.name, rust_expr);
    }
    rust_expr.to_string()
}

/// Try to translate a single effect tuple to a real Rust statement. Returns
/// None when the RHS is too complex for mechanical expansion (match/arith/
/// pre-rendered Lean form); the caller falls through to a `todo!()` so an
/// LLM or human fills the body.
fn mechanize_effect(
    effect: &(String, String, String),
    state_acct: &crate::check::ParsedHandlerAccount,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Option<String> {
    let (field, op_kind, value) = effect;

    // Refuse complex RHS. `render_effect` pre-renders match/record/arith into
    // Lean string form; those start looking nothing like Rust identifiers.
    // A simple param / literal / constant is what's always safe.
    let simple_rhs = value
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-');
    if !simple_rhs {
        return None;
    }

    let rhs = crate::rust_codegen_util::resolve_value(value, handler, spec);
    let acct = &state_acct.name;
    // v2.7 G3: `+=` default lowers to `checked_add(...).ok_or(err)?` — the
    // pattern deployed Anchor programs use. Pre-v2.7 this lowered to
    // `wrapping_add` which produced Kani false-positives and didn't match
    // production behavior. Explicit `+=!` / `+=?` opt into saturating /
    // wrapping. `MathOverflow` is the default error code; specs with an
    // `Error` sum can surface it via the existing error_codes pipeline.
    let line = match op_kind.as_str() {
        "set" => format!("        self.{}.{} = {};\n", acct, field, rhs),
        "add" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.checked_add({rhs}).ok_or(ErrorCode::MathOverflow)?;\n"
        ),
        "add_sat" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.saturating_add({rhs});\n"
        ),
        "add_wrap" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.wrapping_add({rhs});\n"
        ),
        "sub" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.checked_sub({rhs}).ok_or(ErrorCode::MathOverflow)?;\n"
        ),
        "sub_sat" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.saturating_sub({rhs});\n"
        ),
        "sub_wrap" => format!(
            "        self.{acct}.{field} = self.{acct}.{field}.wrapping_sub({rhs});\n"
        ),
        _ => return None,
    };
    Some(line)
}

fn render_handler_scaffold(
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    is_multi: bool,
    default_state_name: &str,
    spec_src: &str,
    spec_attr: &str,
) -> Result<String> {
    let pascal = to_pascal_case(&handler.name);
    let bumps_name = format!("{}Bumps", pascal);
    let any_mut = handler.accounts.iter().any(|a| a.is_writable);

    let mut out = String::new();
    out.push_str("// User-owned. Regenerating the spec does NOT overwrite this file.\n");
    out.push_str("// Guard checks live in the sibling `crate::guards` module and ARE\n");
    out.push_str("// regenerated on every `qedgen codegen`. Drift between the spec\n");
    out.push_str("// handler block and the `spec_hash` below fires a compile_error!\n");
    out.push_str("// via the `#[qed(verified, ...)]` macro.\n\n");
    out.push_str("use anchor_lang::prelude::*;\n");
    out.push_str("use crate::state::*;\n");
    out.push_str("use crate::guards;\n");
    out.push_str("use qedgen_macros::qed;\n");
    if !spec.events.is_empty() && !handler.emits.is_empty() {
        out.push_str("use crate::events::*;\n");
    }
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    out.push('\n');

    // #[derive(Accounts)] struct
    out.push_str("#[derive(Accounts)]\n");
    out.push_str(&format!("pub struct {} {{\n", pascal));

    if !handler.accounts.is_empty() {
        for acct in &handler.accounts {
            let state_name = if is_multi {
                infer_state_name(acct, spec, default_state_name)
            } else {
                default_state_name.to_string()
            };
            let attr = acct.quasar_account_attr(handler, &state_name);
            let field_type = acct.quasar_field_type();
            out.push_str(&format!("{}    pub {}: {},\n", attr, acct.name, field_type));
        }
    } else if handler.who.is_some() {
        out.push_str("    pub signer: Signer,\n");
    }

    out.push_str("}\n\n");

    // impl block with handler
    out.push_str(&format!("impl {} {{\n", pascal));
    if let Some(ref doc) = handler.doc {
        out.push_str(&format!("    /// {}\n", doc));
    }

    // Emit the spec-bound #[qed(...)] attribute. Hashes are empty on first
    // scaffold — the macro errors with the computed values for copy-paste.
    let spec_h = spec_hash::spec_hash_for_handler(spec_src, &handler.name).unwrap_or_default();
    out.push_str(&format!(
        "    #[qed(verified, spec = \"{}\", handler = \"{}\", spec_hash = \"{}\")]\n",
        spec_attr, handler.name, spec_h
    ));

    out.push_str("    #[inline(always)]\n");

    let self_ref = if any_mut { "&mut self" } else { "&self" };
    let mut handler_params = vec![self_ref.to_string()];
    let mut param_names: Vec<String> = Vec::new();
    for (pname, ptype) in &handler.takes_params {
        handler_params.push(format!("{}: {}", pname, map_type(ptype, spec)?));
        param_names.push(pname.clone());
    }
    if handler.has_bumps() {
        handler_params.push(format!("bumps: &{}", bumps_name));
    }

    out.push_str(&format!(
        "    pub fn handler({}) -> Result<(), ProgramError> {{\n",
        handler_params.join(", ")
    ));

    // Call the always-regenerated guards module. Signature: takes `&Self`
    // plus every handler-level parameter, returns `Result<(), ProgramError>`.
    let guard_args = if param_names.is_empty() {
        "self".to_string()
    } else {
        format!("self, {}", param_names.join(", "))
    };
    out.push_str(&format!(
        "        guards::{}({})?;\n",
        handler.name, guard_args
    ));

    // Spec-level `let` bindings (e.g. `let total_fee = amount * 125 / 10000`)
    // must be emitted BEFORE the effect block — effect RHSs reference them.
    // Pre-fix: they were dropped on the Rust side, leaving undefined-variable
    // errors on `cargo build`.
    for (binding_name, _lean_expr, rust_expr) in &handler.let_bindings {
        out.push_str(&format!("        let {} = {};\n", binding_name, rust_expr));
    }

    // Mechanical-effect expansion (v2.4-M3). For each spec effect we try to
    // emit a real Rust statement; anything non-mechanical stays as a comment
    // and forces a trailing `todo!()` so the user / an LLM (M4) fills it in.
    let state_acct = find_state_account(handler);
    let mut any_unmechanized = false;
    for effect in &handler.effects {
        let mechanized = state_acct.and_then(|sa| mechanize_effect(effect, sa, handler, spec));
        match mechanized {
            Some(line) => out.push_str(&line),
            None => {
                let (field, op_kind, value) = effect;
                out.push_str(&format!(
                    "        // Spec effect (needs fill): {} {} {}\n",
                    field, op_kind, value
                ));
                any_unmechanized = true;
            }
        }
    }

    // Events are always agent-fill for now (M4): the spec declares the event
    // name but not the payload binding.
    for emit in &handler.emits {
        out.push_str(&format!("        // Spec: emit!({})\n", emit));
    }
    let has_events = !handler.emits.is_empty();

    // Token transfers (CPI calls) are also agent-fill: building the CPI
    // context from the handler accounts is mechanical-ish but involves
    // framework-specific helpers that differ per Quasar/Anchor/raw.
    let has_transfers = !handler.transfers.is_empty();
    for t in &handler.transfers {
        out.push_str(&format!(
            "        // Spec transfer: {} -> {} amount={}\n",
            t.from,
            t.to,
            t.amount.as_deref().unwrap_or("?")
        ));
    }

    // `call Interface.handler(name = expr, ...)` sites — the uniform CPI
    // surface introduced in v2.5 (slice 2). v2.8 G4 lands real Anchor CPI
    // codegen for the canonical SPL Token transfer; other interfaces and
    // handlers still emit a structured comment + todo!() so an LLM /
    // human fills the body. The boolean tracks whether any call site
    // remained unmechanized so the tail `todo!()` only fires for those.
    let mut any_unmechanized_call = false;
    for c in &handler.calls {
        match try_emit_anchor_cpi(c, handler, spec) {
            Some(rendered) => {
                out.push_str(&format!(
                    "        // Spec call: {}.{} (Anchor CPI emitted by v2.8 G4)\n",
                    c.target_interface, c.target_handler
                ));
                out.push_str(&rendered);
            }
            None => {
                let args = c
                    .args
                    .iter()
                    .map(|a| format!("{}={}", a.name, a.rust_expr))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "        // Spec call: {}.{}({}) — v2.9 will emit a generic Anchor CPI\n",
                    c.target_interface, c.target_handler, args
                ));
                any_unmechanized_call = true;
            }
        }
    }

    let needs_fill = any_unmechanized || has_events || has_transfers || any_unmechanized_call;
    if needs_fill {
        out.push_str("        todo!(\"fill non-mechanical effects, events, transfers, calls\")\n");
    } else {
        out.push_str("        Ok(())\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n");
    Ok(out)
}

/// Generate src/guards.rs — one function per handler containing all the
/// spec-declared guard checks. This file is always regenerated; any edit
/// is clobbered on the next `qedgen codegen` (by design).
fn generate_guards(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    let src_dir = output_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    let mut out = String::new();
    out.push_str(&marker(
        "DO NOT EDIT — regenerated from .qedspec",
        fp,
        "src/guards.rs",
    ));
    out.push_str("//! Per-handler guard checks derived from the `.qedspec`.\n");
    out.push_str("//! Called from user-owned `instructions/<name>::handler` before\n");
    out.push_str("//! business logic; keep guard logic here, policy-free logic there.\n\n");
    out.push_str(
        "#![allow(unused_variables, unused_imports, dead_code, clippy::too_many_arguments)]\n\n",
    );
    out.push_str("use anchor_lang::prelude::*;\n");
    if !spec.error_codes.is_empty() {
        out.push_str("use crate::errors::*;\n");
    }
    out.push_str("use crate::instructions::*;\n\n");

    for handler in &spec.handlers {
        let pascal = to_pascal_case(&handler.name);
        let any_mut = handler.accounts.iter().any(|a| a.is_writable);
        let self_ref = if any_mut { "&mut " } else { "&" };
        let mut params = vec![format!("ctx: {}{}", self_ref, pascal)];
        for (pname, ptype) in &handler.takes_params {
            params.push(format!("{}: {}", pname, map_type(ptype, spec)?));
        }
        out.push_str(&format!(
            "/// Guards for `{}`.  \n/// Generated from the `requires` clauses of the spec handler block.\n",
            handler.name
        ));
        out.push_str(&format!(
            "pub fn {}({}) -> Result<(), ProgramError> {{\n",
            handler.name,
            params.join(", ")
        ));

        if handler.requires.is_empty() && handler.aborts_if.is_empty() {
            out.push_str("    // No guards declared in spec — nothing to check.\n");
        }

        for req in &handler.requires {
            // Emit as a comment for human readers + an executable check.
            // The Rust expression comes directly from the spec; callers are
            // expected to bring the identifiers in scope (typically via
            // `ctx.<account>.<field>` style access).
            out.push_str(&format!("    // requires: {}\n", req.lean_expr.trim()));
            let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
            if let Some(err) = &req.error_name {
                out.push_str(&format!(
                    "    if !({}) {{ return Err(ProgramError::from({}::{})); }}\n",
                    req.rust_expr.trim(),
                    err_enum,
                    err
                ));
            } else {
                out.push_str(&format!("    debug_assert!({});\n", req.rust_expr.trim()));
            }
        }

        let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
        for ab in &handler.aborts_if {
            out.push_str(&format!(
                "    if ({}) {{ return Err(ProgramError::from({}::{})); }}\n",
                ab.rust_expr.trim(),
                err_enum,
                ab.error_name
            ));
        }

        out.push_str("    Ok(())\n");
        out.push_str("}\n\n");
    }

    out.push_str("// ---- END GENERATED ----\n");
    std::fs::write(src_dir.join("guards.rs"), &out)?;
    Ok(())
}

/// Infer the state struct name for a handler account in multi-account specs.
fn infer_state_name(
    acct: &crate::check::ParsedHandlerAccount,
    spec: &ParsedSpec,
    default: &str,
) -> String {
    // Check if this account name matches any account type name (lowercase match)
    for at in &spec.account_types {
        if acct.name == at.name.to_lowercase() || acct.name.starts_with(&at.name.to_lowercase()) {
            return format!("{}Account", at.name);
        }
    }
    default.to_string()
}

/// Generate Cargo.toml
fn generate_cargo_toml(spec: &ParsedSpec, fp: &SpecFingerprint, output_dir: &Path) -> Result<()> {
    const TEMPLATE: &str = include_str!("../templates/program-Cargo.toml");
    let program_name = spec.program_name.to_lowercase().replace('_', "-");
    let needs_spl = spec.handlers.iter().any(|h| h.has_token_accounts());
    let hash = fp
        .file_hashes
        .get("Cargo.toml")
        .cloned()
        .unwrap_or_default();

    let mut out = TEMPLATE
        .replace("{SPEC_HASH}", &hash)
        .replace("{PROGRAM_NAME}", &program_name)
        .replace("{QEDGEN_VERSION}", env!("CARGO_PKG_VERSION"));
    if needs_spl {
        out.push_str("\n# TODO: SPL helper crate (spec declares token transfers) — e.g.:\n");
        out.push_str("# anchor-spl = \"0.32.1\"\n");
    }
    std::fs::write(output_dir.join("Cargo.toml"), &out)?;
    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Generate a Quasar program skeleton from a spec file (.lean or .qedspec).
pub fn generate(spec_path: &Path, output_dir: &Path) -> Result<()> {
    let spec = check::parse_spec_file(spec_path)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No handlers found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    crate::rust_codegen_util::check_effect_targets(&spec)?;

    // Check that the project is initialized (.qed/ next to the spec file)
    if crate::init::find_qed_dir(spec_path).is_none() {
        anyhow::bail!(
            "No .qed/ directory found next to {} — run `qedgen init` first.",
            spec_path.display()
        );
    }

    std::fs::create_dir_all(output_dir)?;

    let fp = crate::fingerprint::compute_fingerprint(&spec);

    generate_lib(&spec, &fp, output_dir)?;
    generate_state(&spec, &fp, output_dir)?;
    generate_events(&spec, &fp, output_dir)?;
    generate_errors(&spec, &fp, output_dir)?;
    generate_instructions(&spec, &fp, spec_path, output_dir)?;
    generate_guards(&spec, &fp, output_dir)?;
    generate_cargo_toml(&spec, &fp, output_dir)?;

    let file_count = 4
        + spec.handlers.len()
        + if spec.events.is_empty() { 0 } else { 1 }
        + if spec.error_codes.is_empty() { 0 } else { 1 };

    eprintln!("Generated {} files in {}", file_count, output_dir.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_spec() -> ParsedSpec {
        ParsedSpec::default()
    }

    fn spec_with_constants(pairs: &[(&str, &str)]) -> ParsedSpec {
        ParsedSpec {
            constants: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..ParsedSpec::default()
        }
    }

    #[test]
    fn map_type_covers_all_primitives() {
        let spec = empty_spec();

        // Integer primitives
        assert_eq!(map_type("U8", &spec).unwrap(), "u8");
        assert_eq!(map_type("U16", &spec).unwrap(), "u16");
        assert_eq!(map_type("U32", &spec).unwrap(), "u32");
        assert_eq!(map_type("U64", &spec).unwrap(), "u64");
        assert_eq!(map_type("U128", &spec).unwrap(), "u128");
        assert_eq!(map_type("I8", &spec).unwrap(), "i8");
        assert_eq!(map_type("I16", &spec).unwrap(), "i16");
        assert_eq!(map_type("I32", &spec).unwrap(), "i32");
        assert_eq!(map_type("I64", &spec).unwrap(), "i64");
        assert_eq!(map_type("I128", &spec).unwrap(), "i128");

        // Non-integer primitives
        assert_eq!(map_type("Bool", &spec).unwrap(), "bool");
        assert_eq!(map_type("Pubkey", &spec).unwrap(), "Address");
    }

    #[test]
    fn map_type_errors_on_unknown_type() {
        // v2.6.1 bug: DSL types not in the four-item allowlist (U8/U64/U128/I128)
        // fell through as-is, leaking `U16` verbatim into Rust. v2.6.2: unknown
        // types must surface as errors at codegen time.
        let spec = empty_spec();
        let err = map_type("Blorb", &spec).unwrap_err().to_string();
        assert!(
            err.contains("Blorb"),
            "error should name the bad type: {err}"
        );
        assert!(
            err.contains("unsupported DSL type"),
            "error should call it out as unsupported: {err}"
        );
    }

    #[test]
    fn map_type_renders_map_with_literal_bound() {
        let spec = empty_spec();
        assert_eq!(map_type("Map[4] U64", &spec).unwrap(), "[u64; 4]");
        assert_eq!(map_type("Map[16] U8", &spec).unwrap(), "[u8; 16]");
        assert_eq!(map_type("Map[32] Pubkey", &spec).unwrap(), "[Address; 32]");
    }

    #[test]
    fn map_type_resolves_map_bound_via_constants() {
        // Mirrors the percolator eval case: `Map[MAX_ACCOUNTS] U64` where
        // MAX_ACCOUNTS is declared as a spec constant.
        let spec = spec_with_constants(&[("MAX_ACCOUNTS", "256"), ("UNRELATED", "99")]);
        assert_eq!(
            map_type("Map[MAX_ACCOUNTS] U64", &spec).unwrap(),
            "[u64; 256]"
        );
    }

    #[test]
    fn map_type_errors_when_map_bound_is_unknown_symbol() {
        // Bound is neither a literal nor a declared constant → clear error
        // naming the unresolved symbol.
        let spec = empty_spec();
        let err = map_type("Map[MISSING] U64", &spec).unwrap_err().to_string();
        assert!(
            err.contains("MISSING"),
            "error should name the bound: {err}"
        );
        assert!(
            err.contains("not a numeric literal") || err.contains("not declared"),
            "error should explain why the bound didn't resolve: {err}"
        );
    }

    #[test]
    fn map_type_resolves_fin_to_usize() {
        // `Fin[N]` → `usize`. Used for index types like `Fin[MAX_ACCOUNTS]`.
        let spec = spec_with_constants(&[("MAX_ACCOUNTS", "256")]);
        assert_eq!(map_type("Fin[MAX_ACCOUNTS]", &spec).unwrap(), "usize");
        assert_eq!(map_type("Fin[4]", &spec).unwrap(), "usize");
    }

    #[test]
    fn map_type_resolves_type_aliases_transitively() {
        // The percolator pattern: `type AccountIdx = Fin[MAX_ACCOUNTS]`.
        // `map_type("AccountIdx")` must resolve through the alias to `usize`.
        use crate::check::ParsedRecordType;
        let mut spec = ParsedSpec {
            type_aliases: vec![
                ("AccountIdx".to_string(), "Fin[MAX_ACCOUNTS]".to_string()),
                ("MyAlias".to_string(), "U64".to_string()),
            ],
            ..ParsedSpec::default()
        };
        assert_eq!(map_type("AccountIdx", &spec).unwrap(), "usize");
        assert_eq!(map_type("MyAlias", &spec).unwrap(), "u64");

        // Record name stays as-is for struct emission downstream.
        spec.records.push(ParsedRecordType {
            name: "UserAccount".to_string(),
            fields: vec![
                ("active".to_string(), "U8".to_string()),
                ("capital".to_string(), "U128".to_string()),
            ],
        });
        assert_eq!(map_type("UserAccount", &spec).unwrap(), "UserAccount");
        // `Map[N] UserAccount` → `[UserAccount; N]`.
        spec.constants = vec![("MAX_ACCOUNTS".to_string(), "4".to_string())];
        assert_eq!(
            map_type("Map[MAX_ACCOUNTS] UserAccount", &spec).unwrap(),
            "[UserAccount; 4]"
        );
    }

    #[test]
    fn sanitize_ident_replaces_subscripts_and_dots() {
        // The eval's actual output:
        //   fn verify_init_user_effect_accounts[i].active()
        // must become a legal Rust identifier.
        assert_eq!(sanitize_ident("accounts[i].active"), "accounts_i_active");
        assert_eq!(sanitize_ident("s.foo.bar"), "s_foo_bar");
        assert_eq!(sanitize_ident("plain_field"), "plain_field");
    }

    #[test]
    fn sanitize_ident_collapses_consecutive_and_trailing_underscores() {
        // Repeated non-ident chars should not pile up as `___`.
        assert_eq!(sanitize_ident("foo[ ].bar"), "foo_bar");
        // Leading non-ident chars produce a leading `_` that stays (doesn't
        // collapse to empty) — this keeps the resulting string non-empty.
        assert_eq!(sanitize_ident("[i]"), "_i");
        // Trailing non-ident chars drop cleanly.
        assert_eq!(sanitize_ident("foo."), "foo");
    }

    #[test]
    fn map_type_errors_on_undeclared_user_type() {
        // `Map[N] UserAccount` where UserAccount is neither a primitive nor
        // declared via `type UserAccount = …` / `type UserAccount { … }` /
        // `type UserAccount | …`. Must surface as an error naming the bad
        // inner type rather than silently emitting broken Rust.
        let spec = spec_with_constants(&[("MAX_ACCOUNTS", "8")]);
        let err = map_type("Map[MAX_ACCOUNTS] UserAccount", &spec)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("UserAccount"),
            "error should name the unsupported inner type: {err}"
        );
    }

    // ----- v2.8 G4: Anchor CPI codegen for SPL Token transfer -----

    /// Exercise try_emit_anchor_cpi against an end-to-end-parsed spec.
    /// Hits the resolver pipeline (no need to construct ParsedSpec by
    /// hand) and confirms the SPL Token transfer shape lands.
    #[test]
    fn cpi_emits_anchor_spl_transfer_for_canonical_program_id() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state         : writable
    src           : writable
    dst           : writable
    auth          : signer
    token_program : program
  }
  call Token.transfer(from = src, to = dst, amount = n, authority = auth)
}
"#,
        )
        .unwrap();
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "send")
            .expect("send handler");
        let call = handler.calls.first().expect("call site");
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit Anchor CPI");
        assert!(
            rendered.contains("anchor_spl::token::{self, Transfer}"),
            "must use anchor_spl::token::Transfer; got:\n{rendered}"
        );
        assert!(
            rendered.contains("from:      self.src.to_account_info()"),
            "from arg must resolve to self.src; got:\n{rendered}"
        );
        assert!(
            rendered.contains("token::transfer(CpiContext::new(cpi_program, cpi_accounts), n)"),
            "amount arg `n` is a handler param and should pass through bare; got:\n{rendered}"
        );
    }

    #[test]
    fn cpi_returns_none_when_program_id_is_not_spl_token() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface MyAmm {
  program_id "MyAmm22222222222222222222222222222222222222"
  handler swap (amount : U64) {
    discriminant "0x01"
    accounts {
      src : writable
      dst : writable
    }
    ensures amount > 0
  }
}

type State | Active of { balance : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    src          : writable
    dst          : writable
    amm_program  : program
  }
  call MyAmm.swap(src = src, dst = dst, amount = balance)
}
"#,
        )
        .unwrap();
        let handler = spec
            .handlers
            .iter()
            .find(|h| h.name == "send")
            .expect("send handler");
        let call = handler.calls.first().expect("call site");
        // Non-SPL-Token program ID — v2.8 falls back to the comment-only
        // path, so try_emit_anchor_cpi returns None.
        assert!(
            try_emit_anchor_cpi(call, handler, &spec).is_none(),
            "v2.8 must defer non-SPL-Token CPI codegen (None ⇒ caller emits comment + todo!())"
        );
    }

    #[test]
    fn cpi_emits_anchor_spl_mint_to_with_authority_renaming() {
        // Spec exposes `mint_authority` per SPL Token docs; anchor_spl's
        // MintTo struct calls the same slot `authority`. The codegen
        // boundary maps the names.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler mint_to (amount : U64) {
    discriminant "0x07"
    accounts {
      mint            : writable
      to              : writable, type token
      mint_authority  : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { stash : U64 }
type Error | E

handler do_mint (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    the_mint       : writable
    holder_ta      : writable, type token
    minter         : signer
    token_program  : program
  }
  call Token.mint_to(mint = the_mint, to = holder_ta, mint_authority = minter, amount = n)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_mint").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(
            rendered.contains("anchor_spl::token::{self, MintTo}"),
            "should use MintTo struct; got:\n{rendered}"
        );
        // anchor_spl uses `authority`; spec uses `mint_authority` — the
        // mapping should land the call-site `minter` value at the
        // `authority` field. Padding may insert extra whitespace before
        // `self`, so we check the substring on each side independently.
        assert!(
            rendered.contains("self.minter.to_account_info()"),
            "minter should be wired into the cpi_accounts struct; got:\n{rendered}"
        );
        assert!(
            rendered.contains("authority:"),
            "MintTo struct should use field name `authority`; got:\n{rendered}"
        );
        assert!(
            rendered.contains("token::mint_to(CpiContext::new(cpi_program, cpi_accounts), n)"),
            "should invoke token::mint_to with the amount; got:\n{rendered}"
        );
    }

    #[test]
    fn cpi_emits_anchor_spl_burn() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler burn (amount : U64) {
    discriminant "0x08"
    accounts {
      from      : writable, type token
      mint      : writable
      authority : signer
    }
    requires amount > 0
    ensures  amount > 0
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_burn (n : U64) : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    holder_ta      : writable, type token
    the_mint       : writable
    holder         : signer
    token_program  : program
  }
  call Token.burn(from = holder_ta, mint = the_mint, authority = holder, amount = n)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_burn").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(rendered.contains("anchor_spl::token::{self, Burn}"));
        assert!(rendered.contains("token::burn(CpiContext::new"));
        // Padding aligns colons across fields; use a substring that's
        // independent of whitespace.
        assert!(
            rendered.contains("self.holder_ta.to_account_info()"),
            "burn's `from` should resolve to self.holder_ta; got:\n{rendered}"
        );
        assert!(rendered.contains("authority: self.holder.to_account_info()"));
    }

    #[test]
    fn cpi_emits_anchor_spl_initialize_account_no_amount() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler initialize_account {
    discriminant "0x01"
    accounts {
      account : writable
      mint    : readonly
      owner   : readonly
      rent    : readonly
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_init : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    new_acct       : writable
    the_mint       : writable
    the_owner      : writable
    rent_sysvar    : writable
    token_program  : program
  }
  call Token.initialize_account(account = new_acct, mint = the_mint, owner = the_owner, rent = rent_sysvar)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_init").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(rendered.contains("InitializeAccount"));
        // No scalar arg — the invocation has no second positional parameter.
        assert!(
            rendered.contains(
                "token::initialize_account(CpiContext::new(cpi_program, cpi_accounts))?;"
            ),
            "no-amount handler should not get a trailing argument; got:\n{rendered}"
        );
        // Owner-as-authority renaming.
        assert!(
            rendered.contains("self.the_owner.to_account_info()"),
            "the_owner should be wired in; got:\n{rendered}"
        );
        assert!(
            rendered.contains("authority:"),
            "InitializeAccount should use field name `authority` for the owner slot; got:\n{rendered}"
        );
    }

    #[test]
    fn cpi_emits_anchor_spl_close_account_no_amount() {
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler close_account {
    discriminant "0x09"
    accounts {
      account     : writable, type token
      destination : writable
      authority   : signer
    }
  }
}

type State | Active of { x : U64 }
type Error | E

handler do_close : State.Active -> State.Active {
  permissionless
  accounts {
    state          : writable
    target_acct    : writable, type token
    sweep_target   : writable
    closer         : signer
    token_program  : program
  }
  call Token.close_account(account = target_acct, destination = sweep_target, authority = closer)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "do_close").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(rendered.contains("CloseAccount"));
        assert!(
            rendered.contains("token::close_account(CpiContext::new(cpi_program, cpi_accounts))?;")
        );
    }

    #[test]
    fn cpi_resolves_state_field_amount_to_self_state_field() {
        // The amount arg references a state field — the emitted code should
        // bind it as self.<state_acct>.<field>, not bare.
        let spec = crate::chumsky_adapter::parse_str(
            r#"spec Caller
program_id "11111111111111111111111111111111"

interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts {
      from      : writable
      to        : writable
      authority : signer
    }
    ensures amount > 0
  }
}

type State | Active of { stash : U64 }
type Error | E

handler send : State.Active -> State.Active {
  permissionless
  accounts {
    state         : writable
    src           : writable, type token
    dst           : writable, type token
    auth          : signer
    token_program : program
  }
  call Token.transfer(from = src, to = dst, amount = stash, authority = auth)
}
"#,
        )
        .unwrap();
        let handler = spec.handlers.iter().find(|h| h.name == "send").unwrap();
        let call = handler.calls.first().unwrap();
        let rendered = try_emit_anchor_cpi(call, handler, &spec).expect("should emit");
        assert!(
            rendered.contains("self.state.stash"),
            "state-field amount must resolve to self.<state_acct>.<field>; got:\n{rendered}"
        );
    }
}
