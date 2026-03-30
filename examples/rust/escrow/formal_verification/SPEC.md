# Escrow Verification Spec v1.0

A token escrow that lets two parties trade safely. The initializer deposits tokens and sets terms, a taker completes the trade, or the initializer cancels and reclaims their deposit.

## 0. Security Goals

The program MUST provide the following properties:

1. **Authorization**: Only the escrow's initializer MAY cancel. Any signer MAY execute the exchange, but the escrow MUST reference a valid initializer. Only the initializer MAY create their escrow.
2. **CPI parameter correctness**: Every token transfer CPI MUST pass the correct program ID, source account, destination account, authority, and amount. No transfer MAY route tokens to an unintended account.
3. **One-shot safety**: After cancel or exchange completes, the escrow account MUST be closed. A closed escrow MUST NOT be reusable.
4. **Arithmetic bounds**: The `u64` amount parameters MUST NOT overflow during processing.

## 1. State Model

```
EscrowState {
  initializer:              Pubkey    -- creator of the escrow
  initializer_token_account: Pubkey   -- where initializer's deposit came from
  initializer_amount:       u64       -- amount deposited by initializer
  taker_amount:             u64       -- amount required from taker
  escrow_token_account:     Pubkey    -- PDA vault holding deposited tokens
  bump:                     u8        -- PDA bump seed
}
```

The escrow account is a PDA derived from `["escrow", initializer.key()]`. The escrow token account is a PDA derived from `["escrow_token", initializer.key()]`.

### Lifecycle

An escrow has two terminal outcomes. There is no intermediate state — it is either open or closed (account deleted).

```
initialize  →  [open]  →  cancel    →  [closed]
                       →  exchange  →  [closed]
```

## 2. Operations

### 2.1 Initialize

**Signers**: `initializer` (MUST sign)

**Preconditions**:
- `amount > 0` (enforced by `InvalidAmount` error)
- `taker_amount > 0` (enforced by `InvalidAmount` error)

**Effects**:
1. Create `EscrowState` account with initializer's pubkey and terms
2. Transfer `amount` tokens: `initializer_deposit_token_account → escrow_token_account`
3. Authority for transfer MUST be `initializer`

**Postconditions**:
- `escrow.initializer = initializer.key()`
- `escrow.initializer_amount = amount`
- `escrow.taker_amount = taker_amount`
- Escrow token account holds `amount` tokens

### 2.2 Exchange

**Signers**: `taker` (MUST sign)

**Preconditions**:
- Escrow account exists and is open
- `escrow.initializer` references a valid account

**Effects** (two transfers, atomic):
1. Transfer `escrow.taker_amount` tokens: `taker_deposit_token_account → initializer_receive_token_account`
   - Authority MUST be `taker`
2. Transfer `escrow.initializer_amount` tokens: `escrow_token_account → taker_receive_token_account`
   - Authority MUST be the escrow PDA
3. Close escrow account, return rent to `initializer`

**Postconditions**:
- Escrow account MUST NOT exist (closed)
- Initializer received `taker_amount` tokens
- Taker received `initializer_amount` tokens

### 2.3 Cancel

**Signers**: `initializer` (MUST sign, enforced by `has_one = initializer`)

**Preconditions**:
- Escrow account exists and is open
- Signer MUST equal `escrow.initializer`

**Effects**:
1. Transfer all tokens: `escrow_token_account → initializer_deposit_token_account`
   - Authority MUST be the escrow PDA
2. Close escrow account, return rent to `initializer`

**Postconditions**:
- Escrow account MUST NOT exist (closed)
- Initializer received all deposited tokens back

## 3. Formal Properties

### 3.1 Access Control

**cancel_access_control**: For all states `s` and signers `p`,
if `cancelTransition(s, p) ≠ none` then `p = s.initializer`.

**exchange_access_control**: For all states `s` and signers `p`,
if `exchangeTransition(s, p) ≠ none` then `p = s.initializer`.

**initialize_access_control**: For all states `s` and signers `p`,
if `initializeTransition(s, p) ≠ none` then `p = s.initializer`.

### 3.2 CPI Parameter Correctness

**cancel_cpi_correct**: For all contexts `ctx`,
`cancel_build_transfer_cpi(ctx).program = TOKEN_PROGRAM_ID` and
`cancel_build_transfer_cpi(ctx).from = ctx.escrow_token_account` and
`cancel_build_transfer_cpi(ctx).to = ctx.initializer_deposit_token_account` and
`cancel_build_transfer_cpi(ctx).authority = ctx.authority`.

**exchange_cpi_correct**: For all contexts `ctx`, both transfer CPIs MUST have correct `program`, `from`, `to`, `authority`, and `amount` fields matching the context.

**initialize_cpi_correct**: For all contexts `ctx`,
`initialize_build_transfer_cpi(ctx).program = TOKEN_PROGRAM_ID` and
`initialize_build_transfer_cpi(ctx).from = ctx.initializer_deposit_token_account` and
`initialize_build_transfer_cpi(ctx).to = ctx.escrow_token_account` and
`initialize_build_transfer_cpi(ctx).authority = ctx.authority`.

### 3.3 State Machine Safety

**cancel_closes_escrow**: For all states `s, s'`,
if `cancelTransition(s) = some s'` then `s'.lifecycle = closed`.

**exchange_closes_escrow**: For all states `s, s'`,
if `exchangeTransition(s) = some s'` then `s'.lifecycle = closed`.

### 3.4 Arithmetic Safety

**arithmetic_bounds**: All `u64` parameters MUST satisfy `value ≤ U64_MAX` after any computation. Priority: lower — defensive property, not a primary concern.

## 4. Trust Boundary

The following are axiomatic (not verified):

- **SPL Token program**: Transfer semantics are correct. We verify parameters passed, not the transfer itself.
- **Solana runtime**: PDA derivation, account ownership enforcement, rent collection.
- **Anchor framework**: Constraint enforcement (`has_one`, `signer`, account deserialization, close semantics).

## 5. Verification Results

| Property | Status | Proof |
|---|---|---|
| cancel_access_control | **Verified** | `CancelAccessControl.cancel_access_control` |
| exchange_access_control | **Verified** | `ExchangeAccessControl.exchange_access_control` |
| initialize_access_control | **Verified** | `InitializeAccessControl.initialize_access_control` |
| cancel_cpi_correct | **Verified** | `CancelCpiCorrectness.cancel_cpi_correct` |
| exchange_cpi_correct | **Verified** | `ExchangeCpiCorrectness.exchange_cpi_correct` |
| initialize_cpi_correct | **Verified** | `InitializeCpiCorrectness.initialize_cpi_correct` |
| cancel_closes_escrow | **Verified** | `CancelStateMachine.cancel_closes_escrow` |
| exchange_closes_escrow | **Verified** | `ExchangeStateMachine.exchange_closes_escrow` |
| initialize_arithmetic_safety | **Verified** | `InitializeArithmeticSafety.initialize_arithmetic_safety` |

9 of 9 properties verified. All proofs compile via `lake build`.
