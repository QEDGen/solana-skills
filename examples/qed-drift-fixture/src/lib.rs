//! Drift-loop fixture — exercises `#[qed(verified, ...)]` end-to-end.
//!
//! The two functions below carry sealed body + spec hashes. When you
//! run `cargo build`, the proc-macro recomputes both hashes and
//! compares to what's pinned in the attribute. Mismatch → drift →
//! `compile_error!`.
//!
//! ## How to refresh the hashes after intentional edits
//!
//! 1. Replace the `hash = "..."` and `spec_hash = "..."` strings with
//!    empty values (or remove them).
//! 2. Run `cargo build -p qed-drift-fixture` — the macro will print
//!    the freshly computed hashes in the error message.
//! 3. Paste the new values back into the attribute.
//!
//! ## What this fixture pins
//!
//! - `qedgen::spec_hash::body_hash_for_fn` agrees with
//!   `qedgen-macros::verified::content_hash` (otherwise the body hash
//!   the user pastes wouldn't match what the macro recomputes).
//! - `qedgen::spec_hash::spec_hash_for_handler` agrees with
//!   `qedgen-macros::spec_bind::spec_hash_for_handler` (likewise for
//!   the spec block).
//! - The `#[qed(verified, spec=, handler=, hash=, spec_hash=)]`
//!   attribute compiles cleanly when both hashes match. CI exercises
//!   this on every workspace `cargo test`.

use qedgen_macros::qed;

#[qed(
    verified,
    spec = "example.qedspec",
    handler = "deposit",
    hash = "ac26f349ac12dd3e",
    spec_hash = "ead5f06cee4818d0"
)]
pub fn deposit(amount: u64) -> u64 {
    amount + 1
}

#[qed(
    verified,
    spec = "example.qedspec",
    handler = "withdraw",
    hash = "cc247c5a61f6bbba",
    spec_hash = "cd2763ac8735efc7"
)]
pub fn withdraw(amount: u64) -> Result<u64, &'static str> {
    if amount == 0 {
        Err("InsufficientFunds")
    } else {
        Ok(amount - 1)
    }
}
