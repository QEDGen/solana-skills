//! Parallax reproducer generation for spec-predicate findings.
//!
//! `repro_gen` covers the closed-form arithmetic witness; this module covers
//! the categories whose evidence is a *transaction* — an attack the deployed
//! program either accepts (bug present) or rejects (no bug). Generation is
//! mechanical translation from the spec, same class of work as
//! `codegen --proptest`, so it lives in codegen and never encodes business
//! logic. Like `repro_gen`, generation is pure: it returns source text, and
//! writing/executing is the probe layer's job.
//!
//! ## Polarity — read this before adding a category
//!
//! These reproducers assert the attack **succeeds**. A spec predicate here
//! claims an *absent guard* (`missing_signer`: no `auth` clause;
//! `lifecycle_one_shot_violation`: no `pre_status`), so the evidence for the
//! claim is the program accepting a transaction it should have refused. The
//! test therefore passes iff the bug is real:
//!
//! - guard absent  → attack committed  → test passes → finding confirmed
//! - guard present → attack rejected   → test fails  → candidate dropped
//!
//! That is the opposite polarity from the integration scaffold, where a
//! forged signer is expected to be rejected. Getting it backwards turns the
//! whole lane into a false-positive generator, so every category constructor
//! states which direction it asserts.
//!
//! ## Why Parallax rather than the Mollusk sandbox
//!
//! Measured on the runtime-journey fixture, the token pre-state costs 42
//! lines under Mollusk (`TokenCtx::new`) plus a 10-line `advance()` to thread
//! post-state between instructions, versus 4 lines of fixtures under Parallax
//! with no `advance()` at all (`execute` commits). Mechanical generation is
//! tractable at the second size and not the first. The two lanes cannot share
//! a crate — Mollusk pulls `solana-account 3`, Parallax `4.x` — so this emits
//! a standalone crate beside the existing repro crate rather than into it.
//!
//! ## Pre-state for a PDA the handler READS
//!
//! An `init` handler creates its PDA, so that account must enter the world
//! EMPTY. Every other handler DECODES an existing account, and an empty one
//! aborts with Anchor's `AccountNotInitialized` (3012) before the guard under
//! test ever runs — reporting "no bug" for entirely the wrong reason. So a
//! non-init handler gets its state account installed: the 8-byte
//! `sha256("account:<Name>")` discriminator, the state fields in declaration
//! order, then `bump` and `status`, mirroring `codegen_mir`'s `#[account]`
//! struct. Field CONTENTS are irrelevant to an absent-guard claim, but the
//! byte WIDTHS are not — a short buffer fails deserialization just as an
//! empty account does.
//!
//! ## Verified two-sided
//!
//! Against `tests/fixtures/parallax-repro-gate/vulnerable.qedspec`, built to
//! SBF and executed:
//!
//! - `set_fee` (no `auth`, no signer account, constant-seed singleton PDA):
//!   both the unsigned-invocation and replay reproducers FIRE;
//! - `open` (`auth owner`, a `signer` account, an actor-seeded PDA): the same
//!   generator does NOT fire, rejected with `AccountNotSigner` (3010) — the
//!   guard under test, not an incidental failure.
//!
//! Keep both directions. A lane that only ever fires is a false-positive
//! generator; one that never fires is dead weight.
//!
//! A `signer` marker in the accounts block enforces a signature
//! INDEPENDENTLY of `auth`, so "no `auth` clause" does not imply "no
//! signature required". An earlier fixture whose vulnerable handler had a
//! `signer` account and a caller-seeded PDA was not exploitable at all, and
//! the reproducer correctly refused to confirm it.

use crate::check::{ParsedHandler, ParsedSpec};

/// A generated Parallax reproducer: source plus the human-readable
/// description of the attack it drives.
#[derive(Debug, Clone)]
pub struct GeneratedParallaxRepro {
    /// Complete `tests/*.rs` source for the repro crate.
    pub source: String,
    /// The attack the harness performs, for the finding envelope.
    pub attack: String,
}

/// Which absent-guard claim the reproducer demonstrates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallaxAttack {
    /// `missing_signer`: invoke with NO account marked signer. Succeeding
    /// proves no authority gate exists.
    UnsignedInvocation,
    /// `lifecycle_one_shot_violation`: invoke the same handler twice.
    /// Both succeeding proves no one-shot / lifecycle guard exists.
    Replay,
}

impl ParallaxAttack {
    fn test_fn_suffix(self) -> &'static str {
        match self {
            ParallaxAttack::UnsignedInvocation => "unsigned_invocation_is_accepted",
            ParallaxAttack::Replay => "replay_is_accepted",
        }
    }

    fn describes(self, handler: &str) -> String {
        match self {
            ParallaxAttack::UnsignedInvocation => format!(
                "invoke `{handler}` with every account meta unsigned; \
                 the program committing the transaction proves no authority gate"
            ),
            ParallaxAttack::Replay => format!(
                "invoke `{handler}` twice against the same state; \
                 both committing proves no lifecycle or one-shot guard"
            ),
        }
    }
}

/// Generate the reproducer for `handler` under `attack`.
///
/// `program_id` is the deployed address the repro sends to.
/// `program_path_expr` is a complete Rust expression evaluating to the
/// artifact path — a quoted absolute path from the probe layer, or a
/// `concat!(env!("CARGO_MANIFEST_DIR"), …)` from a fixture. Taking an
/// expression rather than a relative fragment keeps path arithmetic out of
/// this module; getting that arithmetic wrong produced a repro that failed
/// to load the program at all.
/// Anchor-shaped instruction data (8-byte `sha256("global:<name>")`
/// discriminator) — the caller gates on target, since a Pinocchio program
/// tags its instructions differently.
pub fn generate(
    spec: &ParsedSpec,
    handler: &ParsedHandler,
    attack: ParallaxAttack,
    program_id: &str,
    program_path_expr: &str,
) -> GeneratedParallaxRepro {
    let mut out = String::new();
    let test_fn = format!("probe_{}_{}", handler.name, attack.test_fn_suffix());

    out.push_str(&format!(
        "//! Generated Parallax reproducer for `{}` in spec `{}`.\n\
         //!\n\
         //! Attack: {}\n\
         //!\n\
         //! POLARITY: this test passes iff the BUG IS PRESENT. A pass\n\
         //! confirms the finding; a failure means the program refused the\n\
         //! attack and the candidate must be dropped.\n\n",
        handler.name,
        spec.program_name,
        attack.describes(&handler.name)
    ));

    out.push_str("use parallax_svm::prelude::*;\n\n");
    out.push_str(&format!("const PROGRAM_ID: &str = \"{program_id}\";\n\n"));

    out.push_str("fn program_id() -> Pubkey {\n");
    out.push_str("    PROGRAM_ID.parse().expect(\"program id\")\n");
    out.push_str("}\n\n");

    // Discriminators are constants, so they are computed HERE and emitted as
    // byte literals. A runtime `sha256` helper would drag a hashing crate
    // into every generated repro for a value that can never change — and the
    // repro crate is standalone, so every dependency it names has to be
    // declared in a manifest this module also writes.
    out.push_str(&format!(
        "/// Anchor instruction discriminator: `sha256(\"global:{}\")[..8]`.\n",
        handler.name
    ));
    out.push_str(&format!(
        "const IX_DISCRIMINATOR: [u8; 8] = {};\n\n",
        discriminator_literal(&format!("global:{}", handler.name))
    ));

    out.push_str("fn ctx() -> Ctx {\n");
    out.push_str("    Ctx::builder(program_id())\n");
    out.push_str(&format!("        .program_path({program_path_expr})\n"));
    out.push_str("        .build()\n");
    out.push_str("        .expect(\"load the program under test\")\n");
    out.push_str("}\n\n");

    if reads_existing_state(handler) {
        emit_state_bytes_helper(&mut out, spec, handler);
    }
    emit_test(&mut out, spec, handler, attack, &test_fn);

    GeneratedParallaxRepro {
        source: out,
        attack: attack.describes(&handler.name),
    }
}

/// Does this handler READ an account the program expects to already exist,
/// rather than create it?
///
/// An `init` handler (lifecycle pre-state `Uninitialized` / `Empty`) creates
/// its PDA, so the account must enter the world EMPTY — installing one would
/// make the init fail with "already in use". Every other handler decodes an
/// existing account, and an empty one aborts with Anchor's
/// `AccountNotInitialized` (3012) before the guard under test ever runs.
fn reads_existing_state(handler: &ParsedHandler) -> bool {
    !matches!(
        handler.pre_status.as_deref(),
        Some("Uninitialized") | Some("Empty")
    )
}

/// Emit a helper producing the exact bytes the program will decode:
/// Anchor's 8-byte `sha256("account:<Name>")` discriminator, then the state
/// fields in declaration order, then `bump` and `status`. This mirrors
/// `codegen_mir`'s emitted `#[account]` struct; the layout is fully
/// determined by the spec, so it is mechanical.
fn emit_state_bytes_helper(out: &mut String, spec: &ParsedSpec, handler: &ParsedHandler) {
    let state_name = format!(
        "{}Account",
        crate::codegen_shared::to_pascal_case(&spec.program_name)
    );

    out.push_str(&format!(
        "/// Anchor account discriminator: `sha256(\"account:{state_name}\")[..8]`.\n"
    ));
    out.push_str(&format!(
        "const ACCOUNT_DISCRIMINATOR: [u8; 8] = {};\n\n",
        discriminator_literal(&format!("account:{state_name}"))
    ));

    out.push_str(
        "/// Pre-state the handler decodes. Without it the program aborts with\n\
         /// `AccountNotInitialized` before reaching the guard under test, and the\n\
         /// reproducer would report \"no bug\" for entirely the wrong reason.\n",
    );
    out.push_str("fn state_bytes(bump: u8) -> Vec<u8> {\n");
    out.push_str("    let mut data = ACCOUNT_DISCRIMINATOR.to_vec();\n");
    for (name, ty) in &spec.state_fields {
        out.push_str(&format!("    {} // {name}: {ty}\n", zero_field_expr(ty)));
    }
    out.push_str("    data.push(bump);\n");
    out.push_str(&format!(
        "    data.push({}); // status: {}\n",
        status_value(spec, handler),
        status_label(spec, handler)
    ));
    out.push_str("    data\n");
    out.push_str("}\n\n");
}

/// A zero value of the right WIDTH for a state field. The reproducer proves
/// an absent guard, so field contents are irrelevant — but the byte length
/// is not: a short buffer fails Anchor's deserialization before the guard.
fn zero_field_expr(dsl_ty: &str) -> String {
    match crate::codegen::repro_gen::rust_int_type(dsl_ty) {
        Some("u8") => "data.push(0);".to_string(),
        Some(rust_ty) => format!("data.extend_from_slice(&0{rust_ty}.to_le_bytes());"),
        // Pubkey and anything else addressable is 32 bytes.
        None if dsl_ty.trim() == "Bool" => "data.push(0);".to_string(),
        None => "data.extend_from_slice(&[0u8; 32]);".to_string(),
    }
}

/// The lifecycle discriminant the handler expects to find. Uses the declared
/// `pre_status` when there is one; otherwise the first initialized state,
/// since a handler with no lifecycle clause is reachable from any of them
/// (which is exactly what `lifecycle_one_shot_violation` reports).
fn status_value(spec: &ParsedSpec, handler: &ParsedHandler) -> usize {
    let index_of = |name: &str| spec.lifecycle_states.iter().position(|s| s == name);
    handler
        .pre_status
        .as_deref()
        .and_then(index_of)
        .unwrap_or(1)
}

fn status_label(spec: &ParsedSpec, handler: &ParsedHandler) -> String {
    handler.pre_status.clone().unwrap_or_else(|| {
        spec.lifecycle_states
            .get(1)
            .cloned()
            .unwrap_or_else(|| "first initialized state".to_string())
    })
}

fn emit_test(
    out: &mut String,
    spec: &ParsedSpec,
    handler: &ParsedHandler,
    attack: ParallaxAttack,
    test_fn: &str,
) {
    out.push_str("#[test]\n");
    out.push_str(&format!("fn {test_fn}() {{\n"));
    out.push_str("    let mut test = ctx();\n\n");

    // ── Fixtures. One `ctx.add` per account the spec declares; programs and
    // sysvars are runtime-provided, and PDAs are derived rather than added
    // (a PDA the handler creates must enter the world empty).
    out.push_str("    // Pre-state\n");
    let mut derived: Vec<&str> = Vec::new();
    for account in &handler.accounts {
        if account.is_program || account.name == "rent" {
            continue;
        }
        if account.pda_seeds.is_some() {
            derived.push(&account.name);
            continue;
        }
        out.push_str(&format!(
            "    let {} = test.add(Wallet::account());\n",
            account.name
        ));
    }
    let populate = reads_existing_state(handler);
    for name in &derived {
        let seeds = pda_seed_exprs(name, spec);
        // The bump binding is only used when the account gets populated;
        // discard it otherwise so the repro compiles warning-clean.
        let bump_binding = if populate {
            format!("{name}_bump")
        } else {
            "_".to_string()
        };
        out.push_str(&format!(
            "    let ({name}, {bump_binding}) = Pubkey::find_program_address(&[{seeds}], &program_id());\n"
        ));
        if populate {
            out.push_str(&format!(
                "    test.add(Account::new({name}, program_id(), 2_000_000, state_bytes({name}_bump)));\n"
            ));
        }
    }
    out.push('\n');

    // ── Instruction. `is_signer` is forced false for the unsigned attack;
    // the replay attack keeps the declared metas so the ONLY variable under
    // test is invoking twice.
    let signer_for = |declared: bool| match attack {
        ParallaxAttack::UnsignedInvocation => false,
        ParallaxAttack::Replay => declared,
    };

    out.push_str("    let instruction = Instruction {\n");
    out.push_str("        program_id: program_id(),\n");
    out.push_str("        accounts: vec![\n");
    for account in &handler.accounts {
        let meta = account_meta_expr(account, signer_for(account.is_signer));
        out.push_str(&format!("            {meta},\n"));
    }
    out.push_str("        ],\n");
    if handler.takes_params.is_empty() {
        out.push_str("        data: IX_DISCRIMINATOR.to_vec(),\n");
    } else {
        out.push_str("        data: {\n            let mut data = IX_DISCRIMINATOR.to_vec();\n");
        for (name, ty) in &handler.takes_params {
            out.push_str(&format!(
                "            data.extend_from_slice(&{}); // {name}: {ty}\n",
                param_witness(ty)
            ));
        }
        out.push_str("            data\n        },\n");
    }
    out.push_str("    };\n\n");

    out.push_str(
        "    let attack_verdict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n",
    );
    match attack {
        ParallaxAttack::UnsignedInvocation => {
            out.push_str("    // The program accepting an entirely unsigned invocation is the\n");
            out.push_str("    // evidence: no authority gate stands between a caller and this\n");
            out.push_str("    // handler's effects.\n");
            out.push_str("    test.execute(instruction).check(Outcome::success());\n");
        }
        ParallaxAttack::Replay => {
            out.push_str("    // The first call establishes state; the second is the replay.\n");
            out.push_str(
                "    // Both committing is the evidence: nothing pins this handler to a\n",
            );
            out.push_str("    // single lifecycle state.\n");
            out.push_str("    test.execute(instruction.clone()).check(Outcome::success());\n");
            out.push_str("    test.execute(instruction).check(Outcome::success());\n");
        }
    }
    out.push_str("    }));\n");
    out.push_str("    match attack_verdict {\n");
    out.push_str("        Ok(()) => println!(\"QEDGEN_PARALLAX_ATTACK_COMMITTED\"),\n");
    out.push_str("        Err(_) => {\n");
    out.push_str("            println!(\"QEDGEN_PARALLAX_ATTACK_REJECTED\");\n");
    out.push_str("            panic!(\"the program rejected the generated attack\");\n");
    out.push_str("        }\n");
    out.push_str("    }\n");

    out.push_str("}\n");
}

/// Seed expressions for a declared PDA, matching the scaffold's rendering.
fn pda_seed_exprs(name: &str, spec: &ParsedSpec) -> String {
    let Some(pda) = spec.pdas.iter().find(|pda| pda.name == name) else {
        return String::new();
    };
    pda.seeds
        .iter()
        .map(|seed| {
            let seed = seed.trim();
            if seed.starts_with('"') || seed.starts_with('\'') {
                format!("b{seed}")
            } else {
                format!("{seed}.as_ref()")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn account_meta_expr(account: &crate::check::ParsedHandlerAccount, is_signer: bool) -> String {
    // Programs resolve to their runtime ids rather than a fixture.
    let address = if account.is_program {
        if account.name.contains("system") {
            "system_program::ID".to_string()
        } else if account.name.contains("token") {
            "SPL_TOKEN_PROGRAM_ID".to_string()
        } else {
            format!("{}, /* AGENT: program id */", account.name)
        }
    } else {
        account.name.clone()
    };

    if account.is_writable {
        format!("AccountMeta::new({address}, {is_signer})")
    } else {
        format!("AccountMeta::new_readonly({address}, {is_signer})")
    }
}

/// Render `sha256(<preimage>)[..8]` as a Rust byte-array literal.
fn discriminator_literal(preimage: &str) -> String {
    let hex = qedgen_hash_core::sha256_hex16(preimage);
    let bytes: Vec<String> = (0..8)
        .map(|index| format!("0x{}", &hex[index * 2..index * 2 + 2]))
        .collect();
    format!("[{}]", bytes.join(", "))
}

/// A concrete witness value for an instruction parameter. Any in-domain
/// value demonstrates an absent guard, so `1` is used for integers rather
/// than a boundary value — this lane proves the guard is missing, not that
/// arithmetic overflows (that is `repro_gen`'s job).
fn param_witness(dsl_ty: &str) -> String {
    match crate::codegen::repro_gen::rust_int_type(dsl_ty) {
        Some(rust_ty) => format!("1{rust_ty}.to_le_bytes()"),
        None => "[0u8; 32]".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::chumsky_adapter;

    const VULNERABLE_SPEC: &str = r#"
spec Vulnerable

type State
  | Uninitialized
  | Active of { owner : Pubkey, total : U64 }

type Error
  | InvalidAmount

pda vault ["vault", owner]

handler open : State.Uninitialized -> State.Active {
  auth owner
  accounts {
    owner          : signer, writable
    vault          : writable, pda ["vault", owner]
    system_program : program
  }
  effect { owner := owner.pubkey, total := 0 }
}

handler bump_total (amount : U64) {
  accounts {
    owner : signer, writable
    vault : writable, pda ["vault", owner]
  }
  effect { total += amount }
}
"#;

    fn spec() -> ParsedSpec {
        chumsky_adapter::parse_str(VULNERABLE_SPEC).expect("fixture spec parses")
    }

    fn handler(spec: &ParsedSpec, name: &str) -> ParsedHandler {
        spec.handlers
            .iter()
            .find(|h| h.name == name)
            .expect("handler present")
            .clone()
    }

    #[test]
    fn unsigned_invocation_drops_every_signer_flag() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "bump_total"),
            ParallaxAttack::UnsignedInvocation,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
        );

        assert!(
            !generated.source.contains(", true)"),
            "the unsigned attack must not leave any meta marked signer:\n{}",
            generated.source
        );
        assert!(generated.source.contains("AccountMeta::new(owner, false)"));
    }

    /// The polarity is the whole lane: asserting the attack is REJECTED
    /// would confirm every candidate against a correctly guarded program.
    #[test]
    fn attacks_assert_success_not_rejection() {
        let spec = spec();
        for attack in [ParallaxAttack::UnsignedInvocation, ParallaxAttack::Replay] {
            let generated = generate(
                &spec,
                &handler(&spec, "bump_total"),
                attack,
                "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
                "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
            );
            assert!(
                generated.source.contains("check(Outcome::success())"),
                "{attack:?} must assert the attack commits:\n{}",
                generated.source
            );
            assert!(
                !generated.source.contains("Outcome::error"),
                "{attack:?} must not assert rejection:\n{}",
                generated.source
            );
        }
    }

    #[test]
    fn generated_attack_reports_an_explicit_machine_readable_verdict() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "bump_total"),
            ParallaxAttack::UnsignedInvocation,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "\"/tmp/vulnerable.so\"",
        );

        assert!(generated
            .source
            .contains("QEDGEN_PARALLAX_ATTACK_COMMITTED"));
        assert!(generated.source.contains("QEDGEN_PARALLAX_ATTACK_REJECTED"));
    }

    #[test]
    fn replay_executes_the_handler_twice() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "bump_total"),
            ParallaxAttack::Replay,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
        );
        assert_eq!(
            generated.source.matches("test.execute(").count(),
            2,
            "replay must invoke twice:\n{}",
            generated.source
        );
    }

    /// A non-init handler DECODES its state account, so the repro must
    /// install one. Without it the program aborts with
    /// `AccountNotInitialized` (3012) before the guard under test runs, and
    /// the reproducer reports "no bug" for the wrong reason.
    #[test]
    fn read_handlers_get_their_state_account_installed() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "bump_total"),
            ParallaxAttack::UnsignedInvocation,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
        );

        assert!(
            generated
                .source
                .contains("fn state_bytes(bump: u8) -> Vec<u8>"),
            "a read handler must emit the state-bytes helper:\n{}",
            generated.source
        );
        assert!(
            generated
                .source
                .contains("const ACCOUNT_DISCRIMINATOR: [u8; 8] = [0x"),
            "state bytes must lead with Anchor's account discriminator:\n{}",
            generated.source
        );
        assert!(
            generated
                .source
                .contains("let mut data = ACCOUNT_DISCRIMINATOR.to_vec();"),
            "state bytes must start from that discriminator:\n{}",
            generated.source
        );
        // A standalone repro crate can only name deps its own manifest
        // declares, so discriminators must be literals, not a runtime hash.
        assert!(
            !generated.source.contains("qedgen_hash_core"),
            "the repro must not depend on a hashing crate:\n{}",
            generated.source
        );
        assert!(
            generated.source.contains(
                "test.add(Account::new(vault, program_id(), 2_000_000, state_bytes(vault_bump)))"
            ),
            "the derived PDA must be installed with those bytes:\n{}",
            generated.source
        );
        // Widths must match the emitted `#[account]` struct exactly: a short
        // buffer fails deserialization just like an empty account.
        assert!(generated
            .source
            .contains("data.extend_from_slice(&[0u8; 32]); // owner: Pubkey"));
        assert!(generated
            .source
            .contains("data.extend_from_slice(&0u64.to_le_bytes()); // total: U64"));
    }

    /// An `init` handler CREATES its PDA, so installing an account makes the
    /// init fail with "already in use" — the opposite mistake.
    #[test]
    fn init_handlers_leave_their_pda_empty() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "open"),
            ParallaxAttack::UnsignedInvocation,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
        );

        assert!(
            !generated.source.contains("state_bytes"),
            "an init target must not be pre-populated:\n{}",
            generated.source
        );
        assert!(
            generated
                .source
                .contains("let (vault, _) = Pubkey::find_program_address"),
            "an unused bump must be discarded so the repro compiles clean:\n{}",
            generated.source
        );
    }

    /// A PDA the handler creates must be derived, never installed as a
    /// wallet — installing it makes the init target non-empty and the
    /// attack fails for a reason unrelated to the missing guard.
    #[test]
    fn pda_accounts_are_derived_not_installed() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "bump_total"),
            ParallaxAttack::UnsignedInvocation,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
        );
        assert!(generated
            .source
            .contains("Pubkey::find_program_address(&[b\"vault\", owner.as_ref()]"));
        assert!(
            !generated.source.contains("let vault = test.add("),
            "a PDA must not be installed as a fixture:\n{}",
            generated.source
        );
    }

    #[test]
    fn instruction_params_get_a_concrete_witness() {
        let spec = spec();
        let generated = generate(
            &spec,
            &handler(&spec, "bump_total"),
            ParallaxAttack::UnsignedInvocation,
            "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS",
            "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../program/target/deploy/vulnerable.so\")",
        );
        assert!(generated.source.contains("1u64.to_le_bytes()"));
    }
}
