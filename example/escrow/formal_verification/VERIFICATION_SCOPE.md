# Verification Scope & Trust Boundary

## What We Verify (Program Author's Responsibility)

✅ **Business Logic**
- Authorization checks (who can call what)
- State machine transitions (lifecycle, one-shot safety)
- Token amount calculations
- Parameter validation

✅ **Correct API Usage**
- Passing correct accounts to CPIs
- Using appropriate authorities
- Checking return values

✅ **Compositional Properties**
- Multiple transfers preserve total when combined correctly
- State transitions maintain invariants

## What We DON'T Verify (External Dependencies as Axioms)

❌ **Solana Runtime**
- Account ownership validation
- PDA derivation correctness at runtime
- Rent exemption enforcement
- Sysvar access

❌ **SPL Token Program**
- `token::transfer` implementation
- Token account validation
- Authority checks within SPL Token
- Mint/burn operations

❌ **CPI Mechanics**
- Cross-program invocation routing
- Signer privilege escalation with PDAs
- Account passing and borrowing

❌ **System Program**
- Account creation
- Lamport transfers
- Space allocation

## Trust Assumptions (Axioms)

### Token Conservation (Token.lean:54-65)

```lean
axiom transfer_preserves_total :
    -- IF: SPL Token transfer succeeds
    -- THEN: Total balance across accounts is preserved
```

**What this means:**
- We trust that `anchor_spl::token::transfer` correctly moves tokens
- We verify our program passes the right parameters
- We prove conservation by composing these trusted operations

### Account Model (Account.lean)

```lean
structure Account where
  key : Pubkey
  authority : Pubkey
  balance : Nat
  writable : Bool
```

**What this abstracts:**
- Actual Solana account data structure
- Owner and rent fields (not relevant to business logic)
- Data deserialization (assumed correct via Anchor)

### Authority Model (Authority.lean)

```lean
axiom Authorized : Pubkey -> Pubkey -> Prop
```

**What this abstracts:**
- Anchor's constraint checking (`#[account(constraint = ...)]`)
- Signer validation
- PDA authority delegation via `new_with_signer`

## Verification Strategy

### 1. Extract Program Logic Only

From source like:
```rust
let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
token::transfer(cpi_ctx, amount)?;
```

We extract:
```
transfer: from=A, to=B, amount=X, authority=A
```

### 2. Model as State Transition

```lean
def exchangeTransition (accounts : List Account) ... :=
  some (accounts.map (fun acc =>
    if acc.authority = taker then { acc with balance := acc.balance - amount }
    else if acc.authority = initializer then { acc with balance := acc.balance + amount }
    else acc))
```

### 3. Prove Properties Using Axioms

```lean
theorem exchange_conservation ... :=
  four_way_transfer_preserves_total ... -- Apply trusted axiom
```

## Benefits of This Approach

✅ **Focused Verification**
- Verify what the program author controls
- Avoid verifying the entire Solana stack

✅ **Practical Scope**
- Proofs complete in reasonable time
- Surface area remains manageable

✅ **Clear Responsibility**
- Program bugs: verified
- Runtime bugs: trusted (out of scope)
- Library bugs: trusted (assumed correct)

✅ **Compositional**
- Can verify programs independently
- Trust boundaries are explicit

## What This Catches

✅ Authorization bugs (wrong signer checks)
✅ State machine errors (reentrance, closed account use)
✅ Token accounting errors (wrong amounts, missing transfers)
✅ Arithmetic overflow/underflow

## What This Misses

⚠️ SPL Token implementation bugs
⚠️ Solana runtime vulnerabilities
⚠️ Account data deserialization issues
⚠️ Anchor framework bugs

These are **out of scope** by design - we verify the program assuming correct infrastructure.
