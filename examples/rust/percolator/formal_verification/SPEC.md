# Percolator Risk Engine Verification Spec v1.0

A perpetual DEX risk engine managing protected principal, junior profit claims, and lazy A/K side indices. Preserves conservation, bounded insolvency handling, and deterministic overflow recovery.

## 0. Security Goals

The engine MUST provide the following properties:

1. **Conservation**: The vault MUST NOT create withdrawable claims exceeding vault tokens. `V >= C_tot + I` after every operation.
2. **Arithmetic safety**: All arithmetic MUST use checked operations. No silent overflow, underflow, or wrap. `PNL_i != i128::MIN` in every negation path.
3. **ADL lifecycle correctness**: Side modes MUST follow Normal → DrainOnly → ResetPending → Normal. Resets MUST complete before fresh OI is accepted.
4. **Fee isolation**: Unpaid explicit fees MUST become negative `fee_credits_i`, not inflate PnL or deficit.
5. **Deposit safety**: Pure deposit MUST NOT decrement insurance fund or call loss-absorption routines.

## 1. State Model

```
EngineState {
  V:           u128    -- total vault TVL
  I:           u128    -- insurance fund
  I_floor:     u128    -- insurance floor
  C_tot:       u128    -- sum of all account capitals
  PNL_pos_tot: u128    -- sum of positive PnL claims
  PNL_matured_pos_tot: u128 -- matured positive PnL
}

AccountState {
  C_i:            u128   -- protected principal
  PNL_i:          i128   -- realized PnL claim
  R_i:            u128   -- reserved positive PnL (warmup)
  fee_credits_i:  i128   -- fee balance (always <= 0)
  basis_pos_q_i:  i128   -- signed position basis
}

SideMode = Normal | DrainOnly | ResetPending
```

### Lifecycle (ADL State Machine)

```
Normal  →  DrainOnly  →  ResetPending  →  Normal
```

## 2. Operations

### 2.1 deposit(i, amount)

**Preconditions**: `V + amount <= MAX_VAULT_TVL`
**Effects**:
1. `V = V + amount`
2. `C_i = C_i + amount`
3. `C_tot = C_tot + amount`
4. Settle losses from principal
5. If flat and PNL_i >= 0, sweep fee debt
**Postconditions**: `V >= C_tot + I` preserved

### 2.2 deposit_fee_credits(i, amount)

**Preconditions**: Account materialized, `fee_credits_i <= 0`
**Effects**:
1. `pay = min(amount, -fee_credits_i)`
2. `V = V + pay`
3. `I = I + pay`
4. `fee_credits_i = fee_credits_i + pay`
**Postconditions**: `fee_credits_i <= 0`, conservation preserved

### 2.3 top_up_insurance_fund(amount)

**Preconditions**: `V + amount <= MAX_VAULT_TVL`
**Effects**:
1. `V = V + amount`
2. `I = I + amount`
**Postconditions**: Conservation preserved, `I` increased by exactly `amount`

## 3. Formal Properties

### 3.1 Conservation

**deposit_conservation**: For all engine states `s` and amounts `a`,
if `deposit(s, a) = some s'` then `s'.V >= s'.C_tot + s'.I`.

**top_up_insurance_conservation**: For all engine states `s` and amounts `a`,
if `top_up_insurance(s, a) = some s'` then `s'.V >= s'.C_tot + s'.I`.

**deposit_fee_credits_conservation**: For all engine states `s`, accounts `i`, and amounts `a`,
if `deposit_fee_credits(s, i, a) = some s'` then `s'.V >= s'.C_tot + s'.I`.

### 3.2 Arithmetic Safety

**pnl_negation_safety**: For all `x : i128`,
if `x != i128::MIN` then `negate(x)` does not overflow.

**deposit_bounded**: For all engine states `s` and amounts `a`,
if `deposit(s, a) = some s'` then `s'.V <= MAX_VAULT_TVL`.

### 3.3 State Machine

**adl_lifecycle**: For all side states `m` and transitions `t`,
the ADL lifecycle follows Normal → DrainOnly → ResetPending → Normal
and no other transitions are permitted.

### 3.4 Fee Isolation

**fee_credits_nonpositive**: For all accounts `i` and operations `op`,
if `op` modifies `fee_credits_i`, then `fee_credits_i' <= 0`.

## 4. Trust Boundary

The following are axiomatic (not verified):
- **Solana runtime**: Account ownership, rent, CPI mechanics
- **Checked arithmetic primitives**: `checked_add_u128`, `checked_add_i128` return None on overflow
- **Oracle price**: Provided by trusted oracle, within `MAX_ORACLE_PRICE`

## 5. Verification Results

| Property | Status | Proof |
|---|---|---|
| deposit_conservation | **Verified** | `DepositConservation.deposit_conservation` |
| top_up_insurance_conservation | **Verified** | `TopUpInsuranceConservation.top_up_insurance_conservation` |
| deposit_fee_credits_conservation | **Verified** | `DepositFeeCreditsConservation.deposit_fee_credits_conservation` |
| pnl_negation_safety | **Verified** | `PnlNegationSafety.pnl_negation_safety` |
| deposit_bounded | **Verified** | `DepositBounded.deposit_bounded` |
| adl_lifecycle | **Verified** | `AdlLifecycle.adl_lifecycle_{drain,reset,normal,total}` |
| fee_credits_nonpositive | **Verified** | `FeeCreditsNonpositive.fee_credits_nonpositive` |

7 of 7 properties verified. All proofs compile via `lake build`.
