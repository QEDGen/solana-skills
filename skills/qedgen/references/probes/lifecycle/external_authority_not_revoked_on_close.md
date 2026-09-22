# Lifecycle probe: external_authority_not_revoked_on_close

## Pattern

```rust
// initialize_subscription_authority.rs (Stage A — grant)
Approve2022 {
    token_program: accounts.token_program.address(),
    source: accounts.user_ata,
    delegate: accounts.subscription_authority,
    authority: accounts.user,
    amount: u64::MAX,
}
.invoke()?;

// close_subscription_authority.rs (Stage B — close, no Revoke)
ProgramAccount::close(accounts.subscription_authority, accounts.user)
```

A handler closes a PDA that holds external authority (SPL Approve
delegate, mint authority, ATA delegate) without issuing the
corresponding reverse CPI (`Revoke`, `SetAuthority::None`,
`Assign`). The closed PDA's address is still recorded on the
external account as an active delegate / authority.

## Why it matters

Closing a program-owned PDA reclaims its lamports but doesn't touch
the external accounts that point at it. Wallet UIs query SPL Token's
`delegate` field directly: a dangling delegate displays as "this
address can still spend up to N of my tokens" even though the
program-owned PDA is gone. Downstream programs (multi-sig vaults,
on-chain consumer guards) that read the delegate field see live
permission too.

Canonical example (a real-world subscription program; a QEDGen-only finding):

- `close_subscription_authority.rs:70` / `:78` close the
  `subscription_authority` PDA in both the self-funded and
  sponsor-funded branches.
- Neither branch issues a `Revoke` CPI on the user's ATA, but
  `initialize_subscription_authority.rs:121` / `:130` granted SPL
  Approve with `amount = u64::MAX` to the PDA.

Same shape applies on `close_multidelegate.rs` (a manual professional
audit missed this — a QEDGen-only novel finding that fires structurally
now that the rule lands).

## What the agent should check

1. **Confirm the grant is live**: trace every handler that *could*
   have run before the close. Is the SPL Approve still in effect?
   - Approve is overwritten when a new Approve is issued; check
     whether any handler revokes / re-approves between init and close.
2. **Wallet visibility check**: query the user's ATA in a litesvm
   harness; assert that `account_data.delegate` field points at the
   PDA address even after the close completes.
3. **Re-init pairing**: is this close part of an
   init → use → close → re-init pattern? Some programs intentionally
   leave the delegate because the re-init reuses the same seeds (and
   the PDA address re-materialises with the same delegate
   relationship). The PRD calls this "design-intent-ambiguous"; the
   agent confirms by inspecting whether a sibling `initialize_*`
   handler reuses the seeds.

## What counts as a finding

- **Medium** by default. Wallet-visible dangling permission is a
  usability bug; an attacker who can transfer-into the closed
  address can still drain via the stale delegate, but that's a
  multi-step exploit.
- **High** when the dangling authority is mint authority or ATA
  ownership (rather than just delegate). Those primitives don't
  require the holder to still own lamports to exert authority.
- **Suppress** when an `initialize_*` handler at the same seed
  prefix re-creates the PDA and the dangling authority is the
  *desired* persistent state.

## Recommended fix

Add the reverse CPI BEFORE the close:

```rust
pub fn process(accounts: &[AccountView]) -> ProgramResult {
    let accounts = CloseSubscriptionAuthorityAccounts::try_from(accounts)?;
    // ... PDA verification ...

    // Revoke BEFORE close. The PDA's signer seeds must still be
    // available, so we sign with the close-derivation seeds.
    Revoke2022 {
        token_program: accounts.token_program.address(),
        source: accounts.user_ata,
        authority: accounts.user,
    }
    .invoke()?;

    ProgramAccount::close(accounts.subscription_authority, accounts.user)
}
```

The reverse CPI must succeed BEFORE the close primitive so the
external account no longer points at the now-defunct PDA. For
`SetAuthority` grants, use `SetAuthority { new_authority: None, ... }`;
for `Assign` grants, use `Assign { new_owner: SYSTEM_PROGRAM_ID, ... }`.

## Mollusk reproducer

Substitutions: `${CLOSED_ACCOUNT}`, `${CLOSE_FILE}`, `${CLOSE_LINE}`,
`${CLOSE_FN}`, `${GRANT_SITES}`.

```rust
// .qed/probes/lifecycle/${ID}/repro.rs
//
// Reproducer for external_authority_not_revoked_on_close on
// `${CLOSED_ACCOUNT}`.
//
//   Close site:  ${CLOSE_FILE}:${CLOSE_LINE} (fn `${CLOSE_FN}`)
//   Grant sites: ${GRANT_SITES}
//
// Attack: complete the init → close lifecycle. After close, read the
// external account that received the authority and assert it STILL
// points at the now-defunct PDA. Concretely for SPL Approve grants,
// the user's ATA `delegate` field should be Some(<closed_pda>).

use litesvm::LiteSVM;
use solana_pubkey::Pubkey;

#[test]
fn external_authority_not_revoked_on_close_${CLOSED_ACCOUNT}() {
    let mut svm = LiteSVM::new();
    // TODO(agent): load the program .so and create the user / token /
    //              ATA accounts the init path expects.
    todo!("agent-fill: program + ATA setup");

    // TODO(agent): invoke the initialize handler that grants authority
    //              on `${CLOSED_ACCOUNT}`. See ${GRANT_SITES} for the
    //              expected CPI shape.
    let init_outcome = todo!("agent-fill: init call");
    assert!(init_outcome.is_ok());

    // TODO(agent): snapshot the external account's delegate field.
    let delegate_before: Pubkey = todo!("agent-fill: read user_ata.delegate");

    // TODO(agent): invoke the close handler at ${CLOSE_FILE}:${CLOSE_LINE}.
    let close_outcome = todo!("agent-fill: close call");
    assert!(close_outcome.is_ok());

    // TODO(agent): read the external account again. The dangling
    //              authority is the bug: the delegate field still
    //              points at the closed PDA.
    let delegate_after: Pubkey = todo!("agent-fill: read user_ata.delegate again");

    assert_eq!(
        delegate_before, delegate_after,
        "external authority dangling: delegate not revoked across close"
    );
}
```

Time-to-fired-repro target: ≤ 20 min per finding.
