use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedHandlerAccount, ParsedSpec};
use crate::codegen_shared::{map_type, to_pascal_case, write_generated_file};
use crate::Target;

/// Generate Parallax integration test scaffolds: tests run the compiled
/// binary in an in-process LiteSVM world, exercising the full instruction flow
/// (account validation, deserialization, handler execution, state
/// persistence) — unlike unit tests, which run effects on a plain struct.
///
/// The instruction-builder edge is Quasar-shaped (`program::client` imports),
/// while world setup, execution, outcomes, and checks use Parallax. It does
/// not yet compile inside an Anchor or Pinocchio crate —
/// `generate` refuses non-Quasar targets so `--target anchor` can't
/// silently drop a broken artifact into the crate.
pub fn generate(spec_path: &Path, output_path: &Path, target: Target) -> Result<()> {
    if !matches!(target, Target::Quasar) {
        anyhow::bail!(
            "Integration-test codegen is Quasar-client-only today — the scaffold \
             imports the generated program::client module, which \
             doesn't exist for a {:?} program.",
            target
        );
    }

    let spec = check::parse_spec_file(spec_path)?;

    if spec.is_assembly_target() {
        anyhow::bail!("Integration tests are only supported for Quasar targets, not assembly/sBPF");
    }

    crate::rust_codegen_util::check_effect_targets(&spec)?;

    if spec.handlers.is_empty() {
        anyhow::bail!(
            "No handlers found in {}. Is this a valid qedspec file?",
            spec_path.display()
        );
    }

    let fp = crate::fingerprint::compute_fingerprint(&spec);
    let hash = crate::codegen_shared::fingerprint_hash(&fp, "tests/unit.rs");

    let out = render(&spec, &hash)?;
    write_generated_file(output_path, &out)?;
    ensure_parallax_dev_dependencies(output_path)?;
    eprintln!("  wrote {}", output_path.display());

    Ok(())
}

const PARALLAX_DEV_DEPENDENCIES: &str = "\
parallax-svm = { git = \"https://github.com/blueshift-gg/parallax\", rev = \"804c5662832c65330e7299901cc5195a78d87256\" }\n\
solana-sdk-ids = \"3.1\"\n\
solana-address = \"=2.6.1\"\n\
solana-hash = \"=4.5.0\"\n\
solana-nonce = \"=3.2.0\"\n\
solana-short-vec = \"=3.2.2\"\n\
solana-last-restart-slot = \"=3.1.0\"\n\
solana-slot-history = \"=3.1.0\"\n\
solana-epoch-rewards = \"=3.1.0\"\n\
solana-slot-hashes = \"=3.1.0\"\n\
spl-token = { version = \"=9.0.0\", default-features = false, features = [\"no-entrypoint\"] }\n\
wincode = { version = \"0.5\", features = [\"derive\"] }\n";

const PARALLAX_DEV_DEP_NAMES: &[&str] = &[
    "parallax-svm",
    "solana-sdk-ids",
    "solana-address",
    "solana-hash",
    "solana-nonce",
    "solana-short-vec",
    "solana-last-restart-slot",
    "solana-slot-history",
    "solana-epoch-rewards",
    "solana-slot-hashes",
    "spl-token",
    "wincode",
];

/// Upsert the dependencies owned by the Parallax integration artifact into
/// the generated program manifest. Integration tests conventionally live at
/// `<crate>/tests/*.rs`, so only that crate root is considered; an arbitrary
/// ancestor manifest must never be mutated for a custom output path.
fn ensure_parallax_dev_dependencies(output_path: &Path) -> Result<()> {
    let Some(crate_root) = output_path.parent().and_then(Path::parent) else {
        return Ok(());
    };
    let manifest = crate_root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(());
    }

    let existing = std::fs::read_to_string(&manifest)?;
    let merged = upsert_dev_dependencies(&existing);
    std::fs::write(manifest, merged)?;
    Ok(())
}

/// Replace just the `[dev-dependencies]` body while preserving every other
/// byte-level TOML header, including array tables such as `[[bin]]`.
fn upsert_dev_dependencies(existing: &str) -> String {
    const HEADER: &str = "[dev-dependencies]";
    let mut offset = 0;
    let mut body_start = None;
    let mut body_end = existing.len();

    for line in existing.split_inclusive('\n') {
        let trimmed = line.trim();
        if body_start.is_none() {
            if trimmed == HEADER {
                body_start = Some(offset + line.len());
            }
        } else if trimmed.starts_with('[') && trimmed.ends_with(']') {
            body_end = offset;
            break;
        }
        offset += line.len();
    }

    let Some(body_start) = body_start else {
        let mut out = existing.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(HEADER);
        out.push('\n');
        out.push_str(PARALLAX_DEV_DEPENDENCIES);
        return out;
    };

    let body = crate::codegen_shared::merge_dependencies_section(
        &existing[body_start..body_end],
        PARALLAX_DEV_DEPENDENCIES,
        PARALLAX_DEV_DEP_NAMES,
    );
    let mut out = String::with_capacity(existing.len() + body.len());
    out.push_str(&existing[..body_start]);
    out.push_str(&body);
    out.push_str(&existing[body_end..]);
    out
}

/// Render the complete integration test file.
pub fn render(spec: &ParsedSpec, hash: &str) -> Result<String> {
    let mut out = String::new();
    let program_name = spec.program_name.to_lowercase();
    let state_name = format!("{}Account", to_pascal_case(&program_name));
    let needs_token = spec.handlers.iter().any(|h| h.has_token_accounts());

    // Header
    out.push_str(&crate::banner::banner(Some("DO NOT EDIT"), hash));
    out.push_str("// Parallax integration test scaffold.\n");
    out.push_str("// Tests run against the compiled .so binary via parallax-svm/LiteSVM.\n");
    out.push_str("//\n");
    out.push_str("// AGENT: fill instruction builder data and assertions marked with todo!().\n");
    out.push_str("//\n");
    out.push_str("// Prerequisites:\n");
    out.push_str("//   1. Build your program: cargo build-sbf (or cargo build --target bpfel-unknown-none)\n");
    out.push_str("//   2. Run tests: cargo test --features client\n");
    out.push_str("//\n");
    out.push_str("// Dev-dependencies (add to Cargo.toml):\n");
    out.push_str("//   [dev-dependencies]\n");
    out.push_str("//   parallax-svm = { git = \"https://github.com/blueshift-gg/parallax\", rev = \"804c5662832c65330e7299901cc5195a78d87256\" }\n");
    out.push_str("//   solana-sdk-ids = \"3.1\"\n");
    out.push_str(
        "//   # Parallax 0.1 compatibility pins; dependency Cargo.lock files are not inherited.\n",
    );
    out.push_str("//   solana-address = \"=2.6.1\"\n");
    out.push_str("//   solana-hash = \"=4.5.0\"\n");
    out.push_str("//   solana-nonce = \"=3.2.0\"\n");
    out.push_str("//   solana-short-vec = \"=3.2.2\"\n");
    out.push_str("//   solana-last-restart-slot = \"=3.1.0\"\n");
    out.push_str("//   solana-slot-history = \"=3.1.0\"\n");
    out.push_str("//   solana-epoch-rewards = \"=3.1.0\"\n");
    out.push_str("//   solana-slot-hashes = \"=3.1.0\"\n");
    out.push_str("//   spl-token = { version = \"=9.0.0\", default-features = false, features = [\"no-entrypoint\"] }\n");
    out.push_str("//   wincode = { version = \"0.5\", features = [\"derive\"] }\n\n");

    // Imports
    out.push_str("extern crate std;\n");
    out.push_str("use {\n");
    out.push_str(&format!(
        "    {} as program,\n",
        program_name.replace('-', "_")
    ));
    out.push_str("    program::client::*,\n");
    if !spec.state_fields.is_empty() {
        out.push_str("    program::state::*,\n");
    }
    out.push_str("    parallax_svm::prelude::*,\n");
    if needs_token {
        out.push_str("    spl_token::{\n");
        out.push_str("        solana_program::{program_option::COption, program_pack::Pack},\n");
        out.push_str(
            "        state::{Account as SplTokenAccount, AccountState, Mint as SplMint},\n",
        );
        out.push_str("    },\n");
    }
    out.push_str("    std::vec,\n");
    out.push_str("};\n\n");

    // ── Setup ────────────────────────────────────────────────────────────────
    emit_setup(&mut out, &program_name, needs_token);

    // ── Account helpers ──────────────────────────────────────────────────────
    emit_account_helpers(&mut out, &state_name, spec, needs_token)?;

    // ── Per-handler happy-path tests ────────────────────────────────────────
    for (i, handler) in spec.handlers.iter().enumerate() {
        emit_happy_path_test(&mut out, handler, spec, i)?;
    }

    // ── Unauthorized access tests ────────────────────────────────────────────
    for handler in &spec.handlers {
        if handler.who.is_some() {
            emit_unauthorized_test(&mut out, handler, spec)?;
        }
    }

    // ── Lifecycle sequence test ──────────────────────────────────────────────
    if spec.lifecycle_states.len() > 1 {
        emit_lifecycle_sequence_test(&mut out, spec);
    }

    Ok(out)
}

// ============================================================================
// Code generation helpers
// ============================================================================

fn emit_setup(out: &mut String, program_name: &str, needs_token: bool) {
    out.push_str("// ── Setup ────────────────────────────────────────────────────────\n\n");
    let _ = needs_token;
    out.push_str("fn setup() -> Ctx {\n");
    out.push_str("    Ctx::builder(program::ID)\n");
    out.push_str(&format!(
        "        .program_path(\"../../target/deploy/{}.so\")\n",
        program_name.replace('-', "_")
    ));
    out.push_str("        .build()\n");
    out.push_str("        .expect(\"load compiled program into Parallax\")\n");
    out.push_str("}\n\n");
}

fn emit_account_helpers(
    out: &mut String,
    state_name: &str,
    spec: &ParsedSpec,
    needs_token: bool,
) -> Result<()> {
    const BASE_HELPERS: &str = include_str!("../../templates/integration-helpers-base.rs");
    const TOKEN_HELPERS: &str = include_str!("../../templates/integration-helpers-token.rs");

    out.push_str("// ── Account helpers ──────────────────────────────────────────────\n\n");
    out.push_str(BASE_HELPERS);

    // state_account() — pre-populated program-owned account
    let fields = &spec.state_fields;
    if !fields.is_empty() {
        out.push_str(&format!(
            "/// Create a pre-populated {} account (program-owned).\n",
            state_name
        ));
        out.push_str("fn state_account(\n");
        out.push_str("    address: Pubkey,\n");
        for (name, ty) in fields {
            let rust_ty = map_type(ty, spec)?;
            if rust_ty == "Address" {
                out.push_str(&format!("    {}: Pubkey,\n", name));
            } else {
                out.push_str(&format!("    {}: {},\n", name, rust_ty));
            }
        }
        out.push_str("    bump: u8,\n");
        out.push_str(") -> Account {\n");
        out.push_str(&format!("    let state = {} {{\n", state_name));
        for (name, _) in fields {
            out.push_str(&format!("        {},\n", name));
        }
        out.push_str("        bump,\n");
        out.push_str("    };\n");
        out.push_str("    Account {\n");
        out.push_str("        address,\n");
        out.push_str("        lamports: 2_000_000,\n");
        out.push_str("        data: wincode::serialize(&state).unwrap(),\n");
        out.push_str("        owner: program::ID,\n");
        out.push_str("        executable: false,\n");
        out.push_str("    }\n");
        out.push_str("}\n\n");
    }

    if needs_token {
        out.push_str(TOKEN_HELPERS);
    }
    Ok(())
}

fn emit_happy_path_test(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
    _discriminator: usize,
) -> Result<()> {
    let test_name = format!("test_{}", handler.name);
    let pascal = to_pascal_case(&handler.name);
    let instr_struct = format!("{}Instruction", pascal);

    out.push_str(&format!("// ── {} ──\n\n", handler.name));

    if let Some(ref doc) = handler.doc {
        out.push_str(&format!("/// Happy path: {}\n", doc.trim()));
    }
    out.push_str(&format!("#[test]\nfn {}() {{\n", test_name));
    out.push_str("    let mut ctx = setup();\n\n");

    // Emit system_program + rent if handler initializes accounts
    let accounts = &handler.accounts;
    let is_init_handler = handler.pre_status.as_deref() == Some("Uninitialized")
        || handler.pre_status.as_deref() == Some("Empty");
    let has_system = accounts
        .iter()
        .any(|a| a.is_program && a.name.contains("system"));

    out.push_str("    // Account addresses\n");
    if has_system {
        out.push_str("    let system_program = system_program::ID;\n");
    }
    let has_token_program = accounts
        .iter()
        .any(|a| a.is_program && a.name.contains("token"));
    if has_token_program {
        out.push_str("    let token_program = SPL_TOKEN_PROGRAM_ID;\n");
    }
    if accounts.iter().any(|a| a.name == "rent") {
        out.push_str("    let rent = solana_sdk_ids::sysvar::rent::ID;\n");
    }
    for seed in missing_pda_seed_bindings(handler, spec) {
        out.push_str(&format!(
            "    let {seed} = Pubkey::new_unique(); // seed-only fixture\n"
        ));
    }

    // Emit unique keys for non-program, non-sysvar accounts
    for acct in accounts {
        if acct.is_program || acct.name == "rent" {
            continue;
        }
        if let Some(ref seeds) = acct.pda_seeds {
            // PDA — derive it
            let pda = spec
                .pdas
                .iter()
                .find(|p| !seeds.is_empty() && p.name == acct.name);
            if let Some(pda) = pda {
                let seed_exprs: Vec<String> = pda
                    .seeds
                    .iter()
                    .map(|s| {
                        if s.starts_with('"') || s.starts_with('\'') {
                            // Literal string seed
                            format!("b{}", s)
                        } else {
                            // Field reference — use .as_ref()
                            format!("{}.as_ref()", s)
                        }
                    })
                    .collect();
                out.push_str(&format!(
                    "    let ({}, _{}_bump) = Pubkey::find_program_address(\n        &[{}],\n        &program::ID,\n    );\n",
                    acct.name,
                    acct.name,
                    seed_exprs.join(", ")
                ));
            } else {
                out.push_str(&format!("    let {} = Pubkey::new_unique();\n", acct.name));
            }
        } else {
            out.push_str(&format!("    let {} = Pubkey::new_unique();\n", acct.name));
        }
    }
    out.push('\n');

    if !handler.takes_params.is_empty() {
        out.push_str("    // Instruction parameters\n");
        for (name, ty) in &handler.takes_params {
            let rust_ty = map_type(ty, spec)?;
            let default = default_value(&rust_ty);
            out.push_str(&format!(
                "    let {}: {} = {}; // AGENT: set appropriate value\n",
                name, rust_ty, default
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "    let instruction: Instruction = {} {{\n",
        instr_struct
    ));
    for acct in accounts {
        if acct.is_program {
            if acct.name.contains("system") {
                out.push_str("        system_program,\n");
            } else if acct.account_type.as_deref() == Some("token") {
                out.push_str("        token_program,\n");
            } else {
                out.push_str(&format!("        {},\n", acct.name));
            }
        } else {
            out.push_str(&format!("        {},\n", acct.name));
        }
    }
    for (name, _) in &handler.takes_params {
        out.push_str(&format!("        {},\n", name));
    }
    out.push_str("    }\n    .into();\n\n");

    out.push_str("    let outcome = ctx.execute_with(\n");
    out.push_str("        instruction,\n");
    out.push_str("        vec![\n");
    for acct in accounts {
        if acct.is_program {
            continue; // programs are not passed as accounts
        }
        let helper = account_helper_call(acct, handler, spec);
        out.push_str(&format!("            {},\n", helper));
    }
    out.push_str("        ],\n");
    out.push_str("    );\n\n");

    out.push_str(&format!(
        "    outcome.check(Outcome::success()); // {}\n",
        handler.name
    ));

    // State verification hints
    if handler.has_effect() {
        out.push('\n');
        out.push_str("    // AGENT: verify account state after instruction\n");
        for acct in accounts {
            let acct_is_init = is_init_handler && acct.pda_seeds.is_some() && !acct.is_signer;
            if acct_is_init || acct.is_writable {
                if acct.is_signer && !acct.is_program {
                    continue;
                }
                out.push_str(&format!(
                    "    // let {}_data = &outcome.account({}).unwrap().data;\n",
                    acct.name, acct.name
                ));
            }
        }
        for eff in &handler.effects {
            out.push_str(&format!(
                "    // Spec effect: {} {} {}\n",
                eff.field, eff.op, eff.value
            ));
        }
    }

    out.push_str(&format!(
        "\n    outcome.check(Cu::spent(|cu| cu > 0)); // {} implementation-bound witness\n",
        handler.name
    ));
    out.push_str("}\n\n");
    Ok(())
}

fn emit_unauthorized_test(
    out: &mut String,
    handler: &ParsedHandler,
    spec: &ParsedSpec,
) -> Result<()> {
    let who = match &handler.who {
        Some(w) => w,
        None => return Ok(()),
    };
    let test_name = format!("test_{}_unauthorized", handler.name);
    let pascal = to_pascal_case(&handler.name);
    let instr_struct = format!("{}Instruction", pascal);

    out.push_str(&format!(
        "/// {} must reject unauthorized callers (wrong {}).\n",
        handler.name, who
    ));
    out.push_str(&format!("#[test]\nfn {}() {{\n", test_name));
    out.push_str("    let mut ctx = setup();\n\n");

    let accounts = &handler.accounts;
    let has_system = accounts
        .iter()
        .any(|a| a.is_program && a.name.contains("system"));
    let has_token_program = accounts
        .iter()
        .any(|a| a.is_program && a.name.contains("token"));
    if has_system {
        out.push_str("    let system_program = system_program::ID;\n");
    }
    if has_token_program {
        out.push_str("    let token_program = SPL_TOKEN_PROGRAM_ID;\n");
    }
    if accounts.iter().any(|a| a.name == "rent") {
        out.push_str("    let rent = solana_sdk_ids::sysvar::rent::ID;\n");
    }

    // Create a wrong_signer that differs from the `who` account
    out.push_str(&format!("    let wrong_{} = Pubkey::new_unique();\n", who));

    for acct in accounts {
        if acct.is_program || acct.name == "rent" {
            continue;
        }
        if acct.name == *who {
            // Use wrong signer
            continue;
        }
        out.push_str(&format!("    let {} = Pubkey::new_unique();\n", acct.name));
    }
    out.push('\n');

    out.push_str(&format!(
        "    let instruction: Instruction = {} {{\n",
        instr_struct
    ));
    for acct in accounts {
        if acct.is_program {
            if acct.name.contains("system") {
                out.push_str("        system_program,\n");
            } else if acct.account_type.as_deref() == Some("token") {
                out.push_str("        token_program,\n");
            } else {
                out.push_str(&format!("        {},\n", acct.name));
            }
        } else if acct.name == *who {
            out.push_str(&format!("        {}: wrong_{},\n", who, who));
        } else {
            out.push_str(&format!("        {},\n", acct.name));
        }
    }
    for (name, ty) in &handler.takes_params {
        let rt = map_type(ty, spec)?;
        let default = default_value(&rt);
        out.push_str(&format!("        {}: {},\n", name, default));
    }
    out.push_str("    }\n    .into();\n\n");

    out.push_str("    let outcome = ctx.execute_with(\n");
    out.push_str("        instruction,\n");
    out.push_str("        vec![\n");
    for acct in accounts {
        if acct.is_program {
            continue;
        }
        if acct.name == *who {
            out.push_str(&format!("            signer_account(wrong_{}),\n", who));
        } else {
            let helper = account_helper_call(acct, handler, spec);
            out.push_str(&format!("            {},\n", helper));
        }
    }
    out.push_str("        ],\n");
    out.push_str("    );\n\n");

    out.push_str(&format!(
        "    assert!(outcome.is_err(), \"{} should reject wrong {}\");\n",
        handler.name, who
    ));
    out.push_str("}\n\n");
    Ok(())
}

fn emit_lifecycle_sequence_test(out: &mut String, spec: &ParsedSpec) {
    out.push_str("// ── Lifecycle sequence ────────────────────────────────────────────\n\n");
    out.push_str("/// End-to-end lifecycle: execute operations in spec order.\n");
    out.push_str("/// AGENT: fill in instruction parameters and account setup for each step.\n");
    out.push_str("#[test]\nfn test_lifecycle_sequence() {\n");
    out.push_str("    let mut ctx = setup();\n\n");

    let lifecycle_handlers: Vec<&ParsedHandler> = spec
        .handlers
        .iter()
        .filter(|h| h.pre_status.is_some() || h.post_status.is_some())
        .collect();

    if lifecycle_handlers.is_empty() {
        out.push_str("    // No lifecycle transitions found — nothing to sequence.\n");
        out.push_str("}\n\n");
        return;
    }

    out.push_str("    // Lifecycle transitions:\n");
    for h in &lifecycle_handlers {
        let pre = h.pre_status.as_deref().unwrap_or("*");
        let post = h.post_status.as_deref().unwrap_or(pre);
        out.push_str(&format!("    //   {} : {} → {}\n", h.name, pre, post));
    }
    out.push('\n');

    // Find an init handler (Uninitialized → X)
    let init_op = lifecycle_handlers
        .iter()
        .find(|h| h.pre_status.as_deref() == Some("Uninitialized"));

    if let Some(op) = init_op {
        out.push_str(&format!(
            "    // Step 1: {} ({} → {})\n",
            op.name,
            op.pre_status.as_deref().unwrap_or("*"),
            op.post_status.as_deref().unwrap_or("*")
        ));
        out.push_str(&format!(
            "    // AGENT: build and execute {} instruction\n",
            op.name
        ));
        out.push_str("    todo!(\"build instruction sequence\");\n");
    } else {
        out.push_str("    // AGENT: build instruction sequence to exercise lifecycle\n");
        out.push_str("    todo!(\"build instruction sequence\");\n");
    }

    out.push_str("}\n\n");
}

// ============================================================================
// Utility functions
// ============================================================================

/// Return an appropriate helper function call for an account entry.
fn account_helper_call(
    acct: &ParsedHandlerAccount,
    handler: &ParsedHandler,
    _spec: &ParsedSpec,
) -> String {
    if acct.is_signer && !acct.is_program {
        return format!("signer_account({})", acct.name);
    }

    // Token mints and accounts
    if acct.account_type.as_deref() == Some("mint") || acct.name == "mint" {
        let authority = handler
            .accounts
            .iter()
            .find(|account| account.is_signer)
            .map(|account| account.name.as_str())
            .unwrap_or("Pubkey::new_unique()");
        return format!("mint_account({}, {})", acct.name, authority);
    }
    if let Some(ref account_type) = acct.account_type {
        if account_type == "token" {
            // Infer init from handler lifecycle + pda_seeds
            let is_init = {
                let init_lifecycle = handler.pre_status.as_deref() == Some("Uninitialized")
                    || handler.pre_status.as_deref() == Some("Empty");
                init_lifecycle && acct.pda_seeds.is_some()
            };
            if is_init {
                return format!("empty_account({})", acct.name);
            }
            let mint = handler
                .accounts
                .iter()
                .find(|account| account.name == "mint")
                .map(|account| account.name.as_str())
                .unwrap_or("Pubkey::new_unique()");
            let owner = acct
                .authority
                .as_deref()
                .or_else(|| {
                    handler
                        .accounts
                        .iter()
                        .find(|account| account.is_signer)
                        .map(|account| account.name.as_str())
                })
                .unwrap_or("Pubkey::new_unique()");
            return format!(
                "token_account({}, {}, {}, 1_000_000) /* AGENT: tune amount */",
                acct.name, mint, owner
            );
        }
    }

    // Init accounts start empty (infer from handler lifecycle + pda_seeds)
    let is_init = {
        let init_lifecycle = handler.pre_status.as_deref() == Some("Uninitialized")
            || handler.pre_status.as_deref() == Some("Empty");
        init_lifecycle && !acct.is_signer && acct.pda_seeds.is_some()
    };
    if is_init {
        return format!("empty_account({})", acct.name);
    }

    // Mutable non-signer, non-program accounts need pre-populated state
    if acct.is_writable && !acct.is_signer && !acct.is_program {
        return format!(
            "empty_account({}) /* AGENT: use state_account() with appropriate fields */",
            acct.name
        );
    }

    format!("empty_account({})", acct.name)
}

/// Seed values used to derive a handler PDA can come from persisted state
/// without also appearing as instruction accounts (for example
/// `pda escrow ["escrow", initializer]` on a later `exchange` handler).
/// Declare deterministic stand-ins so the generated scaffold is syntactically
/// complete; the agent replaces them with values decoded from the pre-state.
fn missing_pda_seed_bindings(handler: &ParsedHandler, spec: &ParsedSpec) -> Vec<String> {
    let mut missing = Vec::new();
    for account in &handler.accounts {
        let Some(seeds) = &account.pda_seeds else {
            continue;
        };
        let Some(pda) = spec
            .pdas
            .iter()
            .find(|pda| !seeds.is_empty() && pda.name == account.name)
        else {
            continue;
        };
        for seed in &pda.seeds {
            let seed = seed.trim();
            let is_literal = seed.starts_with('"') || seed.starts_with('\'');
            let is_identifier = !seed.is_empty()
                && seed.chars().enumerate().all(|(index, ch)| {
                    ch == '_' || ch.is_ascii_alphanumeric() && (index > 0 || !ch.is_ascii_digit())
                });
            let already_bound = handler.accounts.iter().any(|account| account.name == seed)
                || handler.takes_params.iter().any(|(name, _)| name == seed);
            if !is_literal
                && is_identifier
                && !already_bound
                && !missing.iter().any(|existing| existing == seed)
            {
                missing.push(seed.to_string());
            }
        }
    }
    missing
}

/// Default value for a Rust type (for parameter placeholders).
fn default_value(rust_type: &str) -> &str {
    match rust_type {
        "u8" => "1",
        "u64" => "1_000_000",
        "u128" => "1_000_000",
        "i128" => "1_000_000",
        "bool" => "true",
        "Address" => "[0u8; 32]",
        _ => "todo!()",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chumsky_adapter;

    const MULTISIG_SPEC: &str = include_str!("../../../../examples/rust/multisig/multisig.qedspec");

    const ESCROW_SPEC: &str = include_str!("../../../../examples/rust/escrow/escrow.qedspec");

    #[test]
    fn integration_test_multisig_generates() {
        let spec = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");
        // Has setup
        assert!(out.contains("fn setup() -> Ctx"));
        assert!(out.contains("parallax_svm::prelude::*"));
        assert!(out.contains("ctx.execute_with("));
        assert!(out.contains("outcome.check(Outcome::success())"));
        // Has signer/empty helpers
        assert!(out.contains("fn signer_account(address: Pubkey)"));
        assert!(out.contains("fn empty_account(address: Pubkey)"));
        // Has per-operation tests
        assert!(out.contains("fn test_create_vault()"));
        assert!(out.contains("fn test_propose()"));
        assert!(out.contains("fn test_approve()"));
        assert!(out.contains("fn test_execute()"));
        // Has unauthorized test for who: creator
        assert!(out.contains("fn test_create_vault_unauthorized()"));
        // Has lifecycle sequence
        assert!(out.contains("fn test_lifecycle_sequence()"));
        // Uses the Quasar program's generated host-side client module.
        assert!(out.contains("multisig as program"));
        assert!(out.contains("program::client::*"));
        // Uses instruction structs
        assert!(out.contains("CreateVaultInstruction"));
    }

    #[test]
    fn integration_test_escrow_has_token_helpers() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");
        // Escrow uses SPL tokens — should have token helpers
        assert!(out.contains("fn mint_account("));
        assert!(out.contains("fn token_account("));
        assert!(out.contains("SplMint::pack"));
        assert!(out.contains("SplTokenAccount::pack"));
        assert!(out.contains("SPL_TOKEN_PROGRAM_ID"));
        // Should have test for each operation
        assert!(out.contains("fn test_initialize()"));
        assert!(out.contains("fn test_exchange()"));
        assert!(out.contains("fn test_cancel()"));
    }

    #[test]
    fn parallax_dev_dependencies_are_upserted_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("programs");
        let output = program.join("tests/integration_tests.rs");
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(
            program.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n\
             [dev-dependencies]\nproptest = \"1\"\ncustom-test-helper = \"2\"\n",
        )
        .unwrap();

        ensure_parallax_dev_dependencies(&output).unwrap();
        ensure_parallax_dev_dependencies(&output).unwrap();

        let manifest = std::fs::read_to_string(program.join("Cargo.toml")).unwrap();
        for dependency in PARALLAX_DEV_DEP_NAMES {
            assert_eq!(
                manifest.matches(&format!("{dependency} =")).count(),
                1,
                "{dependency} must be emitted exactly once"
            );
        }
        assert!(manifest.contains("proptest = \"1\""));
        assert!(manifest.contains("custom-test-helper = \"2\""));
    }

    #[test]
    fn parallax_dependency_upsert_preserves_array_tables() {
        let manifest = "[package]\nname = \"demo\"\n\n\
                        [[bin]]\nname = \"demo\"\npath = \"src/main.rs\"\n\n\
                        [dev-dependencies]\nproptest = \"1\"\n\n\
                        [[example]]\nname = \"client\"\n";

        let merged = upsert_dev_dependencies(manifest);

        assert!(merged.contains("[[bin]]\nname = \"demo\""));
        assert!(merged.contains("[[example]]\nname = \"client\""));
        assert!(!merged.contains("[bin]\n"));
        assert_eq!(merged.matches("parallax-svm =").count(), 1);
    }

    #[test]
    fn integration_test_rejects_assembly_target() {
        let dir = std::env::temp_dir().join("qedgen_integration_test_asm");
        let spec_path = dir.join("test.qedspec");
        let out_path = dir.join("out.rs");
        std::fs::create_dir_all(&dir).unwrap();
        // generate() must refuse assembly-targeted specs before looking at
        // the handler body.
        std::fs::write(
            &spec_path,
            "spec Test\n\npragma sbpf {}\n\ntype State | Idle\n\nhandler noop : State.Idle -> State.Idle { }\n",
        )
        .unwrap();
        let result = generate(&spec_path, &out_path, Target::Quasar);
        assert!(
            result.is_err(),
            "expected error for assembly target, got Ok"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("only supported for Quasar"),
            "unexpected error: {}",
            err_msg
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn integration_test_rejects_non_quasar_targets() {
        // The scaffold uses Parallax for execution but still imports the
        // Quasar <name>-client — refusing Anchor/Pinocchio up front is what
        // keeps `codegen --all --target anchor` from dropping a
        // non-compiling test file into an Anchor crate.
        let dir = std::env::temp_dir().join("qedgen_integration_test_target_gate");
        std::fs::create_dir_all(&dir).unwrap();
        let spec_path = dir.join("test.qedspec");
        let out_path = dir.join("out.rs");
        std::fs::write(&spec_path, "spec Test\n").unwrap();
        for target in [Target::Anchor, Target::Pinocchio] {
            let result = generate(&spec_path, &out_path, target);
            let err_msg = result
                .expect_err("non-Quasar target must be refused")
                .to_string();
            assert!(
                err_msg.contains("Quasar-client-only"),
                "unexpected error for {:?}: {}",
                target,
                err_msg
            );
            assert!(
                !out_path.exists(),
                "no artifact may be written for {:?}",
                target
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
