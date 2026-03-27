# Verification Scope & Trust Boundary

## What We Verify (Program Author's Responsibility)

✅ **Business Logic**
- Authorization checks (who can call what)
- State machine transitions (lifecycle, one-shot safety)
- Parameter validation

✅ **CPI Interface Correctness**
- Correct program IDs (TOKEN_PROGRAM_ID, SYSTEM_PROGRAM_ID, etc.)
- Distinct from/to accounts (no self-transfers)
- Bounded amounts (within U64 range)
- Correct authorities for each CPI
- All required CPIs are constructed

✅ **Compositional Properties**
- Multiple CPIs have valid parameters
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

## Trust Assumptions

### External Program Execution

**What we trust:**
- SPL Token program executes correctly when called with valid parameters
- System program handles account creation and lamport transfers correctly
- Solana runtime routes CPIs to the correct programs
- PDAs provide correct authority delegation via seeds

**What we verify:**
- CPI parameters are constructed correctly before being passed to external programs
- Program IDs match expected values (TOKEN_PROGRAM_ID, etc.)
- Account relationships are valid (from ≠ to, correct authorities, etc.)
- Amounts are within valid bounds (≤ U64_MAX)

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

### 1. Extract CPI Construction

From source like:
```rust
let cpi_accounts = Transfer {
    from: ctx.accounts.taker_deposit.to_account_info(),
    to: ctx.accounts.initializer_receive.to_account_info(),
    authority: ctx.accounts.taker.to_account_info(),
};
let cpi_ctx = CpiContext::new(token_program, cpi_accounts);
token::transfer(cpi_ctx, escrow.taker_amount)?;
```

We extract the CPI as a generic `CpiInstruction` (models `invoke_signed`):
```
CpiInstruction {
  programId: TOKEN_PROGRAM_ID,
  accounts: [taker_deposit (writable), initializer_receive (writable), taker (signer)],
  data: [3]  -- SPL Token Transfer discriminator
}
```

### 2. Model as CPI Constructors

```lean
structure ExchangeContext where
  taker : Pubkey
  escrow : Pubkey
  taker_deposit : Pubkey
  initializer_receive : Pubkey
  taker_amount : U64
  ...

def exchange_build_cpi_1 (ctx : ExchangeContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [
      ⟨ctx.taker_deposit, false, true⟩,        -- source: writable
      ⟨ctx.initializer_receive, false, true⟩,   -- dest: writable
      ⟨ctx.taker, true, false⟩                   -- authority: signer
    ]
  , data := [DISC_TRANSFER]
  }
```

### 3. Prove CPI Correctness (No Axioms!)

```lean
theorem exchange_cpi_correct (ctx : ExchangeContext) :
    let cpi := exchange_build_cpi_1 ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.taker_deposit false true ∧
    accountAt cpi 1 ctx.initializer_receive false true ∧
    accountAt cpi 2 ctx.taker true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold exchange_build_cpi_1 targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩
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

✅ **Authorization bugs** - Wrong signer checks, missing constraints
✅ **State machine errors** - Reentrance, closed account use, invalid transitions
✅ **CPI parameter bugs** - Wrong program ID, same from/to account, overflow amounts
✅ **Missing CPIs** - Forgot to construct required transfer
✅ **Wrong authority** - Using incorrect signer for CPI
✅ **Arithmetic overflow/underflow** - U64 bounds violations

## What This Misses (By Design)

⚠️ **SPL Token implementation bugs** - We trust it executes transfers correctly
⚠️ **Solana runtime vulnerabilities** - We trust CPI routing and PDA derivation
⚠️ **Account data deserialization** - We trust Anchor/Borsh correctly
⚠️ **Anchor framework bugs** - We trust constraint validation
⚠️ **External program behavior** - We only verify we call them with valid parameters

These are **out of scope** by design - we verify parameter construction, not infrastructure execution.
