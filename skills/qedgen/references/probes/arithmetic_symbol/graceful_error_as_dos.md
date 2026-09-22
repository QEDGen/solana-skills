# Arithmetic-symbol probe: graceful_error_as_dos

## Pattern

```rust
fn init<'a, T: Sized>(
    payer: &AccountView,
    account: &AccountView,
    seeds: &[Seed<'a>],
    space: usize,
) -> ProgramResult {
    let lamports = Rent::get()?.try_minimum_balance(space)?;
    if account.lamports() == 0 {
        // happy path: create account
    } else {
        let required_lamports = lamports
            .checked_sub(account.lamports())
            .ok_or(MultiDelegatorError::ArithmeticUnderflow)?;
        // ... transfer required_lamports, allocate, assign
    }
    Ok(())
}
```

`checked_sub` / `checked_add` / `checked_mul` in an init / create /
initialize path where the touched account reaches a PDA (via
`find_program_address`, signed CPI, or `seeds:` in the fn signature)
and the operator's `Err` arm propagates via `?` (or `return Err`).

## Why it matters

The arithmetic operator is correct in isolation — it refuses to wrap.
The bug is the failure-mode interaction with the destination's
**permanence**: the PDA's seeds are deterministic, nobody holds its
private key, and every subsequent caller hits the same operator. If
the first call hits the `Err` arm, the address is **permanently
locked** — no escape, no recovery, no way to drain the dust.

Canonical example (a real-world subscription program):

```rust
// helpers/program.rs:48
let required_lamports = lamports
    .checked_sub(account.lamports())
    .ok_or(MultiDelegatorError::ArithmeticUnderflow)?;
```

**Attack**: an attacker observes the subscription PDA address before the
legitimate caller initialises it, transfers `lamports + 1` (rent-exempt
+ one) to the PDA, and *the legitimate caller can never initialise it*.
`account.lamports() > lamports` → `checked_sub` returns `None` → `?`
propagates `ArithmeticUnderflow`. The dust nobody can withdraw plus the
unrecoverable namespace collision is a permanent griefing attack.

## What the agent should check

1. **PDA confirmation**: trace the touched account upstream. Does it
   reach a `find_program_address` / `create_program_address` /
   `invoke_signed(...)` call? If the address is user-provided
   (signer-derived, not PDA-derived), the caller can retry with a
   fresh keypair — finding is suppressed.
2. **Caller pattern**: are there multiple callers? A single internal
   caller that always feeds matching operands isn't this bug. A
   library entrypoint with attacker-controlled inputs is.
3. **Recovery path**: is there an alternate code path that drains the
   pre-funded lamports and unlocks the address? If yes, suppress.
   The canonical example's `program.rs::init` has no recovery
   path — strict `?` propagation, no alternate branch.
4. **Idempotency**: the suppression hint pattern is to treat
   pre-funded PDAs as "lamports already there" and skip the transfer.
   `if account.lamports() >= lamports { /* skip transfer */ }`
   handles the attacker pre-fund gracefully.

## What counts as a finding

- **High severity** when (PDA confirmed) AND (no alternate caller
  path) AND (`Err` propagates via `?` or `return Err`).
- **Medium** when the address is PDA-derived but the function is
  reachable only from internal callers with bounded inputs (still
  a finding because a future caller change could open it).
- **Suppress** when the address is user-provided (signer-derived) or
  the function has a documented retry path.

## Recommended fix

Make the init path idempotent: accept pre-funded lamports as a valid
starting state.

```rust
if account.lamports() == 0 {
    // create-account path
} else if account.lamports() >= lamports {
    // pre-funded — just allocate + assign, no transfer needed
    Allocate { account, space: space as u64 }.invoke_signed(&signer)?;
    Assign { account, owner: &crate::ID }.invoke_signed(&signer)?;
} else {
    // legitimate partial-fund case: top up
    let required_lamports = lamports - account.lamports();
    Transfer { from: payer, to: account, lamports: required_lamports }
        .invoke()?;
    Allocate { account, space: space as u64 }.invoke_signed(&signer)?;
    Assign { account, owner: &crate::ID }.invoke_signed(&signer)?;
}
```

The bare `-` is safe in the `else` branch because the preceding
condition proved the LHS strictly greater than the RHS.

## Mollusk reproducer

Substitutions: `${FILE}`, `${LINE}`, `${OPERATOR}`, `${FN}`.

```rust
// .qed/probes/arithmetic_symbol/${ID}/repro.rs
//
// Reproducer for graceful_error_as_dos at ${FILE}:${LINE}.
// `${OPERATOR}` inside `${FN}`.
//
// Attack: pre-fund the PDA destination address with more lamports
// than the operand. The operator returns None; the `?` propagates;
// the init fails permanently.
//
// Agent fills:
//   1. The litesvm setup that derives the PDA address via
//      Pubkey::find_program_address(...) for the cited handler.
//   2. The attack transfer: System.Transfer to the derived address
//      with an amount strictly greater than the minimum balance the
//      cited operator subtracts from.
//   3. The legitimate init call.
//   4. The assertion that init fails with the cited error.
//   5. A second legitimate init call. Confirm THAT also fails —
//      this is the "permanence" property that distinguishes a
//      retry-able underflow from a permanent DoS.

use litesvm::LiteSVM;
use solana_pubkey::Pubkey;

#[test]
fn graceful_error_as_dos_at_${FILE}_line_${LINE}() {
    let mut svm = LiteSVM::new();
    // TODO(agent): load the program .so and derive the PDA address
    //              that the cited `${FN}` handler initialises.
    let pda: Pubkey = todo!("agent-fill: PDA derivation");

    // TODO(agent): transfer (minimum_balance + 1) lamports to the
    //              PDA from a funded payer keypair. This is the
    //              pre-fund attack.
    todo!("agent-fill: pre-fund attack");

    // TODO(agent): legitimate caller attempts init.
    let first = todo!("agent-fill: init call");
    assert!(first.is_err(), "pre-fund underflow should propagate");

    // TODO(agent): second legitimate caller attempts init.
    let second = todo!("agent-fill: second init call");
    assert!(
        second.is_err(),
        "permanent DoS: the address is locked across retries"
    );
}
```

Time-to-fired-repro target: ≤ 20 min per finding.
