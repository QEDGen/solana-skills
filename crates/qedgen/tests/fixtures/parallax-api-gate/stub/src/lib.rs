//! Compile-only stand-in for the Quasar program crate that the generated
//! Parallax integration test imports as `program`.
//!
//! The generated scaffold reaches into three program modules — `client`
//! (instruction builders), `state` (the account struct), `errors` (the error
//! enum) — plus `program::ID`. This crate provides exactly that surface for
//! `crates/qedgen/tests/fixtures/parallax-api-gate/vault.qedspec`, so the
//! gate can type-check the scaffold against the REAL pinned `parallax-svm`
//! without a Solana toolchain.
//!
//! Why a stub and not the real thing: `codegen --target quasar` emits
//! `quasar-lang = { version = "0.0.0" }`, a placeholder that does not
//! resolve from any registry, so a genuine Quasar crate cannot be compiled
//! in CI today. That boundary therefore stays ungated; what this gate does
//! cover is every Parallax symbol the scaffold emits, which is the half that
//! moves — `parallax-svm` is pinned to a git revision on a fast-moving repo.
//!
//! Shapes here mirror what Quasar codegen produces for this spec: state
//! fields use the Quasar type mapping (`Pubkey`, not `[u8; 32]`), and the
//! error enum converts to `u32` the way `#[error_code]` does, so
//! `Outcome::error(VaultError::Unauthorized)` resolves through Parallax's
//! blanket `impl<E: Into<u32>> IntoTransactionError for E`.

use parallax_svm::Pubkey;

/// Program address. Any fixed key works; the gate never executes.
pub const ID: Pubkey = Pubkey::new_from_array([7u8; 32]);

pub mod state {
    //! Mirrors a `codegen --target quasar` `src/state.rs`.

    use parallax_svm::Pubkey;

    #[derive(wincode::SchemaRead, wincode::SchemaWrite, Clone, PartialEq, Debug)]
    pub struct VaultAccount {
        pub owner: Pubkey,
        pub owner_ta: Pubkey,
        pub amount: u64,
        pub bump: u8,
    }
}

pub mod errors {
    //! Mirrors a `codegen --target quasar` `src/errors.rs`. Quasar's
    //! `#[error_code]` offsets custom codes the way Anchor does; the exact
    //! offset does not matter to the gate, only that `Into<u32>` holds.

    pub const ERROR_CODE_OFFSET: u32 = 6000;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    #[repr(u32)]
    pub enum VaultError {
        InvalidAmount = 0,
        Unauthorized = 1,
    }

    impl From<VaultError> for u32 {
        fn from(error: VaultError) -> u32 {
            error as u32 + ERROR_CODE_OFFSET
        }
    }
}

pub mod client {
    //! Mirrors the generated host-side instruction builders: one struct per
    //! handler, each convertible into a Solana `Instruction`.

    use parallax_svm::{AccountMeta, Instruction, Pubkey};

    pub struct OpenInstruction {
        pub owner: Pubkey,
        pub vault: Pubkey,
        pub mint: Pubkey,
        pub owner_ta: Pubkey,
        pub vault_ta: Pubkey,
        pub token_program: Pubkey,
        pub system_program: Pubkey,
        pub deposit: u64,
    }

    impl From<OpenInstruction> for Instruction {
        fn from(value: OpenInstruction) -> Instruction {
            let mut data = vec![0u8];
            data.extend_from_slice(&value.deposit.to_le_bytes());
            Instruction {
                program_id: super::ID,
                accounts: vec![
                    AccountMeta::new(value.owner, true),
                    AccountMeta::new(value.vault, false),
                    AccountMeta::new_readonly(value.mint, false),
                    AccountMeta::new(value.owner_ta, false),
                    AccountMeta::new(value.vault_ta, false),
                    AccountMeta::new_readonly(value.token_program, false),
                    AccountMeta::new_readonly(value.system_program, false),
                ],
                data,
            }
        }
    }

    pub struct CloseInstruction {
        pub owner: Pubkey,
        pub vault: Pubkey,
        pub owner_ta: Pubkey,
        pub vault_ta: Pubkey,
    }

    impl From<CloseInstruction> for Instruction {
        fn from(value: CloseInstruction) -> Instruction {
            Instruction {
                program_id: super::ID,
                accounts: vec![
                    AccountMeta::new(value.owner, true),
                    AccountMeta::new(value.vault, false),
                    AccountMeta::new(value.owner_ta, false),
                    AccountMeta::new(value.vault_ta, false),
                ],
                data: vec![1u8],
            }
        }
    }
}
