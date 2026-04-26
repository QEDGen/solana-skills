//! Drift-loop fixture — exercises `#[qed(verified, ...)]` end-to-end
//! across the v2.9 surface:
//!
//! 1. Free-fn body+spec hashes (`deposit`, `withdraw`).
//! 2. Body+spec+accounts hashes (`deposit_with_accounts`).
//! 3. Impl-method body hash (`Account::process`).
//!
//! When you run `cargo build`, the proc-macro recomputes every leg
//! and compares it to what's pinned in the attribute. Mismatch in any
//! leg → drift → `compile_error!`.
//!
//! ## What this fixture pins
//!
//! - `qedgen::spec_hash::body_hash_for_fn` ↔
//!   `qedgen-macros::verified::content_hash` (free-fn body hash agrees).
//! - `qedgen::spec_hash::body_hash_for_impl_fn` ↔
//!   `qedgen-macros::verified::FnLike::content_hash` (impl-method
//!   body hash agrees — v2.9 method-shape support).
//! - `qedgen::spec_hash::spec_hash_for_handler` ↔
//!   `qedgen-macros::spec_bind::spec_hash_for_handler` (spec block
//!   agrees).
//! - `qedgen::spec_hash::accounts_struct_hash` ↔
//!   `qedgen-macros::spec_bind::accounts_struct_hash_in` (accounts
//!   struct hash agrees — v2.9 second-pass G2d).
//!
//! ## How to refresh the hashes after intentional edits
//!
//! 1. Clear the `hash = "..."` strings (or remove them).
//! 2. Run `cargo build -p qed-drift-fixture` — the macro prints the
//!    freshly computed hashes in the error message.
//! 3. Paste the new values back in.

use qedgen_macros::qed;

// ──────────────────────────────────────────────────────────────────
// Leg 1: free-fn, body + spec only
// ──────────────────────────────────────────────────────────────────

#[qed(
    verified,
    spec = "example.qedspec",
    handler = "deposit",
    hash = "ac26f349ac12dd3e",
    spec_hash = "fe6610e4cf4a98e3"
)]
pub fn deposit(amount: u64) -> u64 {
    amount + 1
}

#[qed(
    verified,
    spec = "example.qedspec",
    handler = "withdraw",
    hash = "cc247c5a61f6bbba",
    spec_hash = "24f34f81bd83188c"
)]
pub fn withdraw(amount: u64) -> Result<u64, &'static str> {
    if amount == 0 {
        Err("InsufficientFunds")
    } else {
        Ok(amount - 1)
    }
}

// ──────────────────────────────────────────────────────────────────
// Leg 2: free-fn, with accounts struct sealed in
// ──────────────────────────────────────────────────────────────────

/// Stand-in for an Anchor `#[derive(Accounts)] pub struct Vault`.
/// The macro hashes its tokens (after stripping outer attrs) and
/// compares to `accounts_hash` below. Adding a field, changing a
/// type, or adding/removing an inner `#[field_attr]` fires drift.
pub struct Vault {
    pub balance: u64,
    pub authority: u64,
}

#[qed(
    verified,
    spec = "example.qedspec",
    handler = "deposit_with_accounts",
    hash = "d3ca667dc9867fd2",
    spec_hash = "fe6610e4cf4a98e3",
    accounts = "Vault",
    accounts_file = "src/lib.rs",
    accounts_hash = "133e997ce3d3b469"
)]
pub fn deposit_with_accounts(vault: &mut Vault, amount: u64) -> u64 {
    vault.balance += amount;
    vault.balance
}

// ──────────────────────────────────────────────────────────────────
// Leg 3: impl-method body hash (Marinade/Squads-shape handler)
// ──────────────────────────────────────────────────────────────────

pub struct Account {
    pub balance: u64,
}

impl Account {
    #[qed(
        verified,
        spec = "example.qedspec",
        handler = "process",
        hash = "8dc48270e93d2a13",
        spec_hash = "98282f05baf0b306"
    )]
    pub fn process(&mut self, delta: u64) -> Result<(), &'static str> {
        self.balance = self.balance.checked_add(delta).ok_or("Overflow")?;
        Ok(())
    }
}
