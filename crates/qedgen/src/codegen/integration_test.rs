use anyhow::Result;
use std::path::Path;

use crate::check::{self, ParsedHandler, ParsedHandlerAccount, ParsedSpec};
use crate::codegen_shared::{map_type, map_type_quasar, to_pascal_case, write_generated_file};
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

/// Upstream Parallax repository. Not published to crates.io, so consumers
/// pin a git revision.
pub(crate) const PARALLAX_GIT_URL: &str = "https://github.com/blueshift-gg/parallax";

/// The pinned Parallax revision — the SINGLE source for it in this repo.
///
/// It previously appeared as three independent literals (this dependency
/// set, the header comment the scaffold emits, and the compile-gate
/// fixture's manifest). Two of those drifting apart is a silent failure in
/// both directions: a scaffold whose comment advertises a revision its own
/// `[dev-dependencies]` does not use, and — worse — a gate that compiles the
/// OLD revision, stays green, and stops gating what actually ships.
/// `parallax_gate_fixture_pins_the_same_revision` holds the fixture to this.
///
/// Bumping: change this, run
/// `cargo test -p qedgen-solana-skills --test parallax_integration_gate -- --ignored`,
/// and regenerate the bundled examples.
pub(crate) const PARALLAX_GIT_REV: &str = "804c5662832c65330e7299901cc5195a78d87256";

/// The `parallax-svm` dependency line, built from the pin above.
fn parallax_dependency_line() -> String {
    format!("parallax-svm = {{ git = \"{PARALLAX_GIT_URL}\", rev = \"{PARALLAX_GIT_REV}\" }}")
}

/// Solana crates held at the versions Parallax 0.1 resolves against. A
/// dependency's `Cargo.lock` is not inherited by consumers, so without these
/// a fresh resolve picks the wincode-0.6 line and fails to compile.
pub(crate) fn parallax_dev_dependencies() -> String {
    format!(
        "{}\n\
         solana-sdk-ids = \"3.1\"\n\
         solana-address = \"=2.6.1\"\n\
         solana-hash = \"=4.5.0\"\n\
         solana-nonce = \"=3.2.0\"\n\
         solana-short-vec = \"=3.2.2\"\n\
         solana-last-restart-slot = \"=3.1.0\"\n\
         solana-slot-history = \"=3.1.0\"\n\
         solana-epoch-rewards = \"=3.1.0\"\n\
         solana-slot-hashes = \"=3.1.0\"\n\
         spl-token = {{ version = \"=9.0.0\", default-features = false, features = [\"no-entrypoint\"] }}\n\
         wincode = {{ version = \"0.5\", features = [\"derive\"] }}\n",
        parallax_dependency_line()
    )
}

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
    if merged == existing {
        return Ok(());
    }
    std::fs::write(&manifest, merged)?;
    // Cargo.toml is user-owned (see the artifact table in SKILL.md), unlike
    // the generated test beside it. Editing it silently leaves the user to
    // discover 12 new dependencies from a diff.
    eprintln!(
        "  updated {} ([dev-dependencies] for Parallax)",
        manifest.display()
    );
    Ok(())
}

/// Strip a trailing `# comment` from a TOML line, respecting quoted strings
/// so a `#` inside a value (`rev = "a#b"`) is not treated as a comment.
fn strip_toml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (index, &byte) in bytes.iter().enumerate() {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'#' => return &line[..index],
            None => {}
        }
    }
    line
}

/// Is this line the `[dev-dependencies]` table header? Matched after
/// stripping a trailing comment, so `[dev-dependencies]  # test only` counts.
/// Getting this wrong is not a cosmetic miss: an unmatched header sends the
/// upsert down the append path, which writes a SECOND `[dev-dependencies]`
/// table and makes the user's manifest fail to parse.
fn is_dev_dependencies_header(line: &str) -> bool {
    strip_toml_comment(line).trim() == "[dev-dependencies]"
}

/// Does this line open a new TOML table (ending the `[dev-dependencies]`
/// body)? Also matched after stripping comments, and true for array tables
/// (`[[bin]]`) and dotted tables (`[target.'cfg(unix)'.dev-dependencies]`).
fn is_table_header(line: &str) -> bool {
    let trimmed = strip_toml_comment(line).trim();
    trimmed.starts_with('[') && trimmed.ends_with(']')
}

/// Replace just the `[dev-dependencies]` body while preserving every other
/// byte-level TOML header, including array tables such as `[[bin]]`.
///
/// Byte-level splicing rather than a parse/serialize round-trip: the `toml`
/// crate would reformat the whole manifest and drop the user's comments.
fn upsert_dev_dependencies(existing: &str) -> String {
    const HEADER: &str = "[dev-dependencies]";

    // A dependency may also be declared as its own subtable
    // (`[dev-dependencies.parallax-svm]`). Emitting the inline key as well
    // would leave the manifest with a duplicate key, so leave those alone
    // and let the existing declaration stand.
    let subtabled: Vec<&str> = PARALLAX_DEV_DEP_NAMES
        .iter()
        .copied()
        .filter(|name| {
            existing
                .lines()
                .any(|line| strip_toml_comment(line).trim() == format!("[dev-dependencies.{name}]"))
        })
        .collect();
    let owned_names: Vec<&str> = PARALLAX_DEV_DEP_NAMES
        .iter()
        .copied()
        .filter(|name| !subtabled.contains(name))
        .collect();
    let dev_dependencies = parallax_dev_dependencies();
    let additions: String = dev_dependencies
        .lines()
        .filter(|line| {
            let key = line.split('=').next().unwrap_or("").trim();
            !subtabled.contains(&key)
        })
        .map(|line| format!("{line}\n"))
        .collect();

    let mut offset = 0;
    let mut body_start = None;
    let mut body_end = existing.len();

    for line in existing.split_inclusive('\n') {
        if body_start.is_none() {
            if is_dev_dependencies_header(line) {
                body_start = Some(offset + line.len());
            }
        } else if is_table_header(line) {
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
        out.push_str(&additions);
        return out;
    };

    let body = crate::codegen_shared::merge_dependencies_section(
        &existing[body_start..body_end],
        &additions,
        &owned_names,
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
    // Rendered from `parallax_dev_dependencies()` rather than repeated as
    // literals: a comment advertising a different revision than the one
    // `codegen` actually writes into Cargo.toml is a lie the user would act
    // on. `qedgen codegen --integration` upserts these automatically; the
    // block is here so the file is self-describing when read on its own.
    out.push_str("// Dev-dependencies (upserted into Cargo.toml by qedgen):\n");
    out.push_str("//   [dev-dependencies]\n");
    for (index, line) in parallax_dev_dependencies().lines().enumerate() {
        // The compatibility pins start after parallax-svm + solana-sdk-ids.
        if index == 2 {
            out.push_str(
                "//   # Parallax 0.1 compatibility pins; dependency Cargo.lock files are not inherited.\n",
            );
        }
        out.push_str(&format!("//   {line}\n"));
    }
    out.push('\n');

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
    emit_setup(&mut out);

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

fn emit_setup(out: &mut String) {
    out.push_str("// ── Setup ────────────────────────────────────────────────────────\n\n");
    // `crate_name` rather than a hardcoded relative path: Parallax resolves
    // the artifact by honouring `PARALLAX_PROGRAM_PATH` first, then walking
    // ancestors for `target/deploy/<crate>.so`. A literal
    // "../../target/deploy/x.so" is CWD-relative and only holds when the
    // package sits exactly two levels under the workspace root. This is the
    // same resolution `#[parallax_test]` itself uses.
    out.push_str("fn setup() -> Ctx {\n");
    out.push_str("    Ctx::builder(program::ID)\n");
    out.push_str("        .crate_name(env!(\"CARGO_PKG_NAME\"))\n");
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
        // Every call site is an AGENT hole (`empty_account(x) /* AGENT: use
        // state_account() */`), so this is dead until the agent wires it up.
        // Without the allow, a DO-NOT-EDIT file warns on generation.
        out.push_str("#[allow(dead_code)]\n");
        out.push_str("fn state_account(\n");
        out.push_str("    address: Pubkey,\n");
        // Quasar's type mapping, not the standalone one. This helper builds
        // the program's own state struct, and `src/state.rs` is emitted with
        // `map_type_quasar` — a `Pubkey` field there against a `[u8; 32]`
        // parameter here is a type error the moment an agent wires the
        // helper up.
        for (name, ty) in fields {
            let rust_ty = map_type_quasar(ty, spec)?;
            out.push_str(&format!("    {}: {},\n", name, rust_ty));
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
    // A stand-in seed derives a PDA that matches nothing in the pre-state,
    // so it must carry an AGENT marker like every other hole in this file.
    for seed in missing_pda_seed_bindings(handler, spec) {
        out.push_str(&format!(
            "    let {seed} = Pubkey::new_unique(); // AGENT: replace with the {seed} from pre-state\n"
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
        let helper = account_helper_call(acct, handler, spec, None);
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

    // No CU assertion is emitted. A committed transaction always spends
    // compute units, so `Cu::spent(|cu| cu > 0)` cannot fail — it reads as
    // coverage while proving nothing. The spec carries no CU budget, so the
    // real bound is a measurement the agent takes once and pins here.
    out.push_str("\n    // AGENT: pin a compute budget once measured, e.g.\n");
    out.push_str("    //   outcome.check(Cu::spent(|cu| cu <= 20_000));\n");
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
            let helper = account_helper_call(acct, handler, spec, Some(who));
            out.push_str(&format!("            {},\n", helper));
        }
    }
    out.push_str("        ],\n");
    out.push_str("    );\n\n");

    // Assert the SPECIFIC rejection, not merely "some error". `is_err()`
    // also passes when the instruction failed to deserialize or an account
    // was missing, so an authorization test can go green while the
    // authorization check under test never ran. Pick the same error the
    // program's own guards return for this case (`codegen_shared::guards`
    // makes the identical choice), so the assertion tracks the emitted
    // program rather than a guess. A too-tight assertion fails loudly,
    // which is the right direction for a negative test.
    match authorization_error(spec) {
        Some(error) => {
            let err_enum = format!("{}Error", to_pascal_case(&spec.program_name));
            out.push_str(&format!(
                "    // {} must reject a forged {} with the spec's authorization error.\n",
                handler.name, who
            ));
            out.push_str(&format!(
                "    outcome.check(Outcome::error(program::errors::{}::{}));\n",
                err_enum, error
            ));
        }
        None => {
            // The spec declares no authorization error, so there is nothing
            // to name. Keep the weak form but say why it is weak instead of
            // letting it read as a real authorization assertion.
            out.push_str(&format!(
                "    // AGENT: the spec declares no authorization error, so this only\n\
                 \x20   // asserts that SOME error fired — it also passes on a deserialization\n\
                 \x20   // or missing-account failure. Declare the error in the spec, then\n\
                 \x20   // assert it with `outcome.check(Outcome::error(..))`.\n\
                 \x20   assert!(outcome.is_err(), \"{} should reject wrong {}\");\n",
                handler.name, who
            ));
        }
    }
    out.push_str("}\n\n");
    Ok(())
}

/// The error a forged signer is expected to produce: `Unauthorized` when the
/// spec declares it, else `InvalidLifecycle`, matching the preference order
/// `codegen_shared::guards` uses for the program's own authorization checks.
///
/// Only SPEC-DECLARED codes are named. `guards` can fall back to
/// `InvalidLifecycle` unconditionally because it emits that check only where
/// the variant exists; this scaffold emits one negative test per `who`
/// handler, and `codegen_mir` synthesizes `InvalidLifecycle` / `InvalidPda`
/// into the enum only when `needs_lifecycle` / `needs_invalid_pda` hold. So
/// `None` here means "degrade to the marked weak form" rather than name a
/// variant the generated enum may not carry.
fn authorization_error(spec: &ParsedSpec) -> Option<&'static str> {
    ["Unauthorized", "InvalidLifecycle"]
        .into_iter()
        .find(|candidate| spec.error_codes.iter().any(|code| code == candidate))
}

fn emit_lifecycle_sequence_test(out: &mut String, spec: &ParsedSpec) {
    out.push_str("// ── Lifecycle sequence ────────────────────────────────────────────\n\n");
    out.push_str("/// End-to-end lifecycle: execute operations in spec order.\n");
    out.push_str("/// AGENT: fill in instruction parameters and account setup for each step.\n");
    out.push_str("#[test]\n");
    // The body ends in `todo!()`, so `ctx` is unused until an agent fills
    // the sequence in. Keeping the binding named `ctx` (rather than `_ctx`)
    // means the agent does not have to rename it; the allow keeps a
    // DO-NOT-EDIT file warning-clean until then.
    out.push_str("#[allow(unused_mut, unused_variables)] // until the sequence is filled in\n");
    out.push_str("fn test_lifecycle_sequence() {\n");
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
///
/// `forged` names the `who` account that the unauthorized test replaces with
/// a `wrong_<who>` binding. Every inferred reference has to follow that
/// rename: the authorization test never binds `<who>` itself, so a helper
/// that still names it does not compile. This is invisible to a text
/// assertion — only `parallax_integration_gate` catches it.
fn account_helper_call(
    acct: &ParsedHandlerAccount,
    handler: &ParsedHandler,
    _spec: &ParsedSpec,
    forged: Option<&str>,
) -> String {
    // Resolve an account name to the identifier actually in scope.
    let bind = |name: &str| -> String {
        match forged {
            Some(who) if who == name => format!("wrong_{who}"),
            _ => name.to_string(),
        }
    };

    if acct.is_signer && !acct.is_program {
        return format!("signer_account({})", bind(&acct.name));
    }

    // Token mints and accounts. Every argument the spec does not pin is
    // inferred, and every inferred argument is named in the AGENT marker:
    // a narrower marker than the guess behind it reads as "this is filled
    // in" and hides a placeholder the agent must replace.
    if acct.account_type.as_deref() == Some("mint") || acct.name == "mint" {
        // No spec field carries a mint authority, so this is always a guess.
        let authority = handler
            .accounts
            .iter()
            .find(|account| account.is_signer)
            .map(|account| bind(&account.name))
            .unwrap_or_else(|| "Pubkey::new_unique()".to_string());
        return format!(
            "mint_account({}, {}) /* AGENT: confirm mint authority */",
            bind(&acct.name),
            authority
        );
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
            // `unresolved` collects every argument that did NOT come from
            // the spec. A handler with no `mint` account (e.g. escrow's
            // `exchange`) gets a fresh `Pubkey::new_unique()` per token
            // account, so the accounts end up on mutually incompatible
            // mints and no transfer between them can succeed. That must be
            // stated, not left under a marker that only mentions the amount.
            let mut unresolved: Vec<&str> = Vec::new();
            let mint = match handler.accounts.iter().find(|a| a.name == "mint") {
                Some(account) => bind(&account.name),
                None => {
                    unresolved.push("mint");
                    "Pubkey::new_unique()".to_string()
                }
            };
            let owner = match acct.authority.as_deref() {
                // `authority` is spec-declared, so it is not a guess.
                Some(authority) => bind(authority),
                None => {
                    unresolved.push("owner");
                    handler
                        .accounts
                        .iter()
                        .find(|a| a.is_signer)
                        .map(|a| bind(&a.name))
                        .unwrap_or_else(|| "Pubkey::new_unique()".to_string())
                }
            };
            let marker = if unresolved.is_empty() {
                " /* AGENT: tune amount */".to_string()
            } else {
                format!(" /* AGENT: set {}; tune amount */", unresolved.join(", "))
            };
            return format!(
                "token_account({}, {}, {}, 1_000_000){}",
                bind(&acct.name),
                mint,
                owner,
                marker
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
        return format!("empty_account({})", bind(&acct.name));
    }

    // Mutable non-signer, non-program accounts need pre-populated state
    if acct.is_writable && !acct.is_signer && !acct.is_program {
        return format!(
            "empty_account({}) /* AGENT: use state_account() with appropriate fields */",
            bind(&acct.name)
        );
    }

    format!("empty_account({})", bind(&acct.name))
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

    /// An AGENT marker must name every argument the codegen guessed. Escrow's
    /// `exchange` has no `mint` account, so each token account gets its own
    /// `Pubkey::new_unique()` mint and they end up mutually incompatible. A
    /// marker that mentions only the amount would hide that.
    #[test]
    fn inferred_token_arguments_are_named_in_the_agent_marker() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");

        assert!(
            out.contains("/* AGENT: set mint, owner; tune amount */"),
            "a token account with neither mint nor authority resolved must name both:\n{out}"
        );
        assert!(
            out.contains("/* AGENT: confirm mint authority */"),
            "mint authority is always inferred and must say so:\n{out}"
        );
        // A spec-declared `authority` is not a guess, so `owner` drops out of
        // the marker while the unresolved `mint` stays.
        assert!(
            out.contains("/* AGENT: set mint; tune amount */"),
            "a spec-declared authority must not be reported as inferred:\n{out}"
        );
        // The pre-fix marker claimed only the amount needed attention.
        assert!(
            !out.contains("Pubkey::new_unique(), taker, 1_000_000) /* AGENT: tune amount */"),
            "a placeholder mint must never sit behind an amount-only marker:\n{out}"
        );
    }

    /// `Cu::spent(|cu| cu > 0)` holds for every committed transaction, so it
    /// reads as coverage while proving nothing.
    #[test]
    fn no_tautological_compute_unit_assertion_is_emitted() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");

        assert!(
            !out.contains("Cu::spent(|cu| cu > 0)"),
            "an assertion that cannot fail must not be emitted:\n{out}"
        );
        assert!(
            out.contains("// AGENT: pin a compute budget once measured"),
            "the scaffold must point at a real budget instead:\n{out}"
        );
    }

    /// A negative test that accepts any error also goes green when the
    /// instruction fails to deserialize, so the authorization check under
    /// test never runs.
    #[test]
    fn unauthorized_tests_assert_the_spec_declared_error() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");

        assert!(
            out.contains(
                "outcome.check(Outcome::error(program::errors::EscrowError::Unauthorized))"
            ),
            "the forged-signer test must assert the spec's authorization error:\n{out}"
        );
        assert!(
            !out.contains("assert!(outcome.is_err()"),
            "`is_err()` passes for the wrong reason and must not survive:\n{out}"
        );
    }

    /// Only a spec-declared error may be named. `InvalidLifecycle` and
    /// `InvalidPda` are synthesized into the generated enum conditionally
    /// (`codegen_mir`'s `needs_lifecycle` / `needs_invalid_pda`), so naming
    /// one on a spec that does not declare it can reference a variant that
    /// was never emitted.
    #[test]
    fn authorization_error_names_only_spec_declared_codes() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        assert_eq!(authorization_error(&spec), Some("Unauthorized"));

        // Multisig's `type Error` block declares neither candidate, so the
        // scaffold degrades to the marked weak form instead of guessing.
        let multisig = chumsky_adapter::parse_str(MULTISIG_SPEC).unwrap();
        assert_eq!(authorization_error(&multisig), None);

        let out = render(&multisig, "test").expect("render");
        if out.contains("_unauthorized()") {
            assert!(
                out.contains("the spec declares no authorization error"),
                "a weak negative assertion must say why it is weak:\n{out}"
            );
        }
    }

    /// A hardcoded "../../target/deploy/x.so" only resolves when the package
    /// sits exactly two levels under the workspace root.
    #[test]
    fn setup_resolves_the_artifact_through_parallax() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");

        assert!(out.contains(".crate_name(env!(\"CARGO_PKG_NAME\"))"));
        assert!(
            !out.contains("../../target/deploy/"),
            "artifact discovery must not be CWD-relative:\n{out}"
        );
    }

    /// An unmatched header sends the upsert down the append path, which writes
    /// a second `[dev-dependencies]` table and breaks the user's manifest.
    #[test]
    fn dev_dependencies_header_matches_through_a_trailing_comment() {
        let manifest = "[package]\nname = \"demo\"\n\n\
                        [dev-dependencies]  # test only\nproptest = \"1\"\n";

        let merged = upsert_dev_dependencies(manifest);

        assert_eq!(
            merged.matches("[dev-dependencies]").count(),
            1,
            "a commented header must not produce a duplicate table:\n{merged}"
        );
        assert!(merged.contains("[dev-dependencies]  # test only"));
        assert_eq!(merged.matches("parallax-svm =").count(), 1);
        assert!(merged.contains("proptest = \"1\""));
    }

    /// A dependency already declared as its own subtable must not also be
    /// emitted inline — that is a duplicate key and a manifest parse error.
    #[test]
    fn dev_dependency_subtable_is_not_duplicated_inline() {
        let manifest = "[package]\nname = \"demo\"\n\n\
                        [dev-dependencies]\nproptest = \"1\"\n\n\
                        [dev-dependencies.parallax-svm]\n\
                        git = \"https://github.com/blueshift-gg/parallax\"\n\
                        rev = \"deadbeef\"\n";

        let merged = upsert_dev_dependencies(manifest);

        assert_eq!(
            merged.matches("parallax-svm =").count(),
            0,
            "an existing subtable declaration must be left alone:\n{merged}"
        );
        assert!(merged.contains("[dev-dependencies.parallax-svm]"));
        // Everything not subtabled still lands.
        assert_eq!(merged.matches("solana-sdk-ids =").count(), 1);
    }

    /// The compile gate is only meaningful if it builds the revision the
    /// scaffold actually ships. If codegen bumps `PARALLAX_GIT_REV` and the
    /// fixture manifest keeps the old one, the gate compiles the OLD
    /// Parallax, passes green, and silently stops gating — the exact failure
    /// a gate exists to prevent.
    #[test]
    fn parallax_gate_fixture_pins_the_same_revision() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parallax-api-gate/stub/Cargo.toml");
        let manifest = std::fs::read_to_string(&fixture)
            .unwrap_or_else(|e| panic!("read {}: {e}", fixture.display()));

        assert!(
            manifest.contains(PARALLAX_GIT_REV),
            "gate fixture pins a different Parallax revision than codegen emits.\n\
             codegen: {PARALLAX_GIT_REV}\n\
             fixture: {}\n\
             Update {} so the gate compiles what ships.",
            manifest
                .lines()
                .find(|line| line.contains("parallax-svm"))
                .unwrap_or("<no parallax-svm line>")
                .trim(),
            fixture.display()
        );
        assert!(manifest.contains(PARALLAX_GIT_URL));
    }

    /// The emitted header must advertise the revision codegen actually
    /// writes into `[dev-dependencies]`, not a stale copy of it.
    #[test]
    fn scaffold_header_and_manifest_agree_on_the_pin() {
        let spec = chumsky_adapter::parse_str(ESCROW_SPEC).unwrap();
        let out = render(&spec, "test").expect("render");

        assert_eq!(
            out.matches(PARALLAX_GIT_REV).count(),
            1,
            "the scaffold should name the pin exactly once (in the header block):\n{out}"
        );
        assert!(parallax_dev_dependencies().contains(PARALLAX_GIT_REV));
    }

    /// A `#` inside a quoted value is not a comment.
    #[test]
    fn toml_comment_stripping_respects_quotes() {
        assert_eq!(strip_toml_comment("rev = \"a#b\""), "rev = \"a#b\"");
        assert_eq!(strip_toml_comment("key = 1 # note").trim(), "key = 1");
        assert!(is_dev_dependencies_header("[dev-dependencies] # x"));
        assert!(!is_dev_dependencies_header("[dev-dependencies.foo]"));
        assert!(is_table_header("[[bin]] # the binary"));
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
