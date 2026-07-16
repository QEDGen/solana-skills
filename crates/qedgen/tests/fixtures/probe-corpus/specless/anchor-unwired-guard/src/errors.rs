use anchor_lang::prelude::*;

// Two variants are wired into guards below; one is defined but never
// enforced anywhere in src/ — the #240 dead-guard sweep must flag ONLY the
// last one as `unwired_error_variant`.
#[error_code]
pub enum VaultError {
    #[msg("caller is not the vault authority")]
    Unauthorized,
    #[msg("amount exceeds the configured cap")]
    AmountTooLarge,
    // Defined but wired into no guard — the named check never fires, so the
    // path it was meant to protect (a hook authority that must not be one of
    // the hook accounts) proceeds unchecked. This is the load-bearing dead
    // guard the sweep exists to surface.
    #[msg("hook authority must not be part of the hook accounts")]
    HookAuthorityCannotBePartOfHookAccounts,
}
