//! Mollusk-backed in-process Solana sandbox for qedgen probe reproducers.
//!
//! ## Why this crate exists
//!
//! PLAN-v2.16 D3 ships per-probe reproducers as Rust integration tests
//! under `<project>/target/qedgen-repros/<finding-id>/`. Each repro
//! invokes the user's deployed handler via [Mollusk] (in-process SVM),
//! observes state corruption / unauthorized state change, and asserts
//! the bug fires. If the assertion fires, the probe finding is surfaced
//! with the test trace inline; if not, the finding is silently dropped
//! (no advisory tier — see `feedback_probes_reproducible_only.md`).
//!
//! This crate is the **library surface those reproducers depend on**.
//! Each generated repro carries a `Cargo.toml` with
//! `qedgen-sandbox = "0.1"`, then writes:
//!
//! ```ignore
//! use qedgen_sandbox::Sandbox;
//!
//! #[test]
//! fn probe_<finding_id>() {
//!     let sandbox = Sandbox::for_program("my_program", PROGRAM_ID);
//!     let result = sandbox.invoke(&attack_ix, &pre_state);
//!     assert!(result.program_result.is_err(),
//!             "expected MathOverflow but program returned ok");
//! }
//! ```
//!
//! ## Why a separate crate (and not a `qedgen` module)
//!
//! Mollusk pulls Agave + the Solana SDK transitively. Folding that into
//! the `qedgen` CLI's dep graph would balloon clean-build time and risk
//! diamond conflicts with the Anchor / Quasar IDL libraries already
//! imported (ratchet-anchor, ratchet-quasar). The CLI invokes generated
//! repros via `cargo test` instead — so the heavy deps live only in the
//! ephemeral repro crate, never in the CLI.
//!
//! ## v2.16 surface
//!
//! Deliberately thin. We re-export Mollusk and provide a `Sandbox`
//! newtype with the constructor pattern repros use most. The API
//! hardens as D3 retrofits categories and we learn what the
//! auto-generated repros actually need — premature design here would
//! ossify before the use cases are clear.
//!
//! [Mollusk]: https://docs.rs/mollusk-svm

pub use mollusk_svm;
pub use mollusk_svm::result::InstructionResult;
pub use mollusk_svm::Mollusk;

use mollusk_svm::program::keyed_account_for_system_program;
use solana_account::Account;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

/// Thin wrapper around [`Mollusk`] sized for qedgen probe repros.
///
/// `Sandbox::for_program("my_program", PROGRAM_ID)` loads the program
/// binary at `target/deploy/my_program.so` (relative to the test crate's
/// `CARGO_MANIFEST_DIR`, the standard Mollusk convention). Repros then
/// build an [`Instruction`] targeting a specific handler and call
/// [`Sandbox::invoke`] with the pre-state accounts.
pub struct Sandbox {
    inner: Mollusk,
}

impl Sandbox {
    /// Build a sandbox for `program_id`, loading the program binary
    /// from the standard Mollusk path
    /// (`<test-crate>/target/deploy/<program_name>.so`).
    pub fn for_program(program_name: &str, program_id: Pubkey) -> Self {
        Self {
            inner: Mollusk::new(&program_id, program_name),
        }
    }

    /// Invoke `ix` against the given account snapshot and return the
    /// raw [`InstructionResult`]. The repro inspects this to decide
    /// whether the bug fired (e.g. unexpected `Ok`, post-state value,
    /// log contents).
    pub fn invoke(&self, ix: &Instruction, accounts: &[(Pubkey, Account)]) -> InstructionResult {
        self.inner.process_instruction(ix, accounts)
    }

    /// Invoke `ix` with the system program account injected — the
    /// common shape for handlers that CPI to System for account
    /// allocation. Caller still supplies any non-System accounts.
    pub fn invoke_with_system(
        &self,
        ix: &Instruction,
        accounts: &[(Pubkey, Account)],
    ) -> InstructionResult {
        let mut all = accounts.to_vec();
        all.push(keyed_account_for_system_program());
        self.inner.process_instruction(ix, all.as_slice())
    }

    /// Borrow the wrapped Mollusk for advanced use (multi-instruction
    /// transactions, sysvar overrides, compute budget tuning).
    pub fn mollusk(&self) -> &Mollusk {
        &self.inner
    }
}
