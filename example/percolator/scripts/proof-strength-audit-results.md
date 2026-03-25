# Kani Proof Strength Audit Results

Generated: 2026-02-27 (updated: 2026-03-23 — v11.31 spec proofs + fix)

175 proof harnesses across `tests/proofs_*.rs`.

Methodology: Each proof analyzed for:
1. **Input classification**: concrete (hardcoded) vs symbolic (`kani::any()` with `kani::assume`) vs derived
2. **Branch coverage**: whether constraints allow solver to reach both sides of conditionals in the function-under-test
3. **Invariant strength**: `canonical_inv()` (STRONG) vs `valid_state()` (WEAK) vs neither
4. **Vacuity risk**: contradictory assumes, hand-built unreachable states, always-error paths
5. **Symbolic collapse**: whether derived values collapse symbolic ranges
6. **Inductive strength**: sub-criteria 6a-6f evaluating whether the proof achieves true inductive invariant status

Classification criteria:
- **INDUCTIVE**: Fully symbolic initial state with assume(INV), decomposed invariant components, loop-free specs, modular account reasoning. The gold standard.
- **STRONG**: Symbolic inputs exercise key branches of function-under-test; uses `canonical_inv()` or equivalently strong property-specific assertions; non-vacuous (reachable paths verified via `assert_ok!`/`assert_err!` or explicit non-vacuity assertions). Starts from constructed state rather than fully symbolic.
- **WEAK**: Uses `valid_state()` instead of `canonical_inv()`, or symbolic inputs miss key branches, or invariant assertions are weaker than available
- **UNIT TEST**: Concrete inputs intentionally limit scope to specific scenarios, or is a meta/negative proof
- **VACUOUS**: Contradictory assumptions make the proof trivially true

Scaffolding policy: Concrete values that do NOT affect branch coverage in the function-under-test (e.g., slot numbers for fresh crank, capital amounts that only ensure margin passes, `last_crank_slot = 100`) are treated as scaffolding and do not downgrade a proof.

---

## Final Tally

| Classification | Count | Description |
|---|---|---|
| **INDUCTIVE** | 11 | Fully symbolic state, decomposed invariants, loop-free delta specs, full u128/i128 domain |
| **STRONG** | 162 | Symbolic inputs exercise key branches, canonical_inv or equivalent strong assertions, non-vacuous |
| **WEAK** | 0 | -- |
| **UNIT TEST** | 3 | Intentional negative tests and concrete-oracle scenario tests |
| **VACUOUS** | 0 | All proofs have non-vacuity assertions or trivially reachable assertions |

---

## Criterion 6: Inductive Strength -- Global Assessment

Of 175 proofs, 11 achieve INDUCTIVE classification using fully symbolic state with decomposed invariants. The remaining 164 proofs share structural patterns that prevent INDUCTIVE classification. This section evaluates the global findings for sub-criteria 6a through 6f for the non-INDUCTIVE proofs.

### 6a. State Construction Method

**Finding: 147 of 158 proofs use constructed state. 11 proofs (#147-157) use fully symbolic state.**

Every proof follows the pattern:
```rust
let mut engine = RiskEngine::new(test_params());   // concrete params
engine.current_slot = 100;                          // concrete scaffolding
let user_idx = engine.add_user(0).unwrap();         // add 1-2 accounts
engine.accounts[user_idx].capital = U128::new(capital);  // overwrite symbolic fields
engine.accounts[user_idx].pnl = I128::new(pnl);
sync_engine_aggregates(&mut engine);               // fix up aggregates
kani::assert(canonical_inv(&engine), "setup");     // assert INV holds
```

This means hundreds of fields are fixed to their `RiskEngine::new()` defaults:
- `funding_index_qpb_e6 = 0` (no funding history)
- `last_crank_slot = 0` (or set to concrete value)
- `next_account_id` = determined by `add_user` call count
- `free_head`, `next_free[..]` = determined by `add_user` construction
- Unused account slots = all `empty_account()` with zeroed fields
- `entry_price = 0` for most proofs (unless explicitly overwritten)
- `warmup_started_at_slot = 0` for most proofs
- `reserved_pnl = 0` for all proofs

**Impact**: The proofs demonstrate `INV(new() + ops) => INV(new() + ops + f(s))` but NOT the full inductive `forall s: INV(s) => INV(f(s))`. States reachable from different construction sequences (e.g., add 3 users then close 1, leaving a freelist hole) are not covered.

### 6b. Topology Coverage

**Finding: ALL proofs use fixed, small topologies (1-2 users, 0-1 LPs).**

- **1-user proofs** (majority): `add_user(0)` creates a single user at slot 0. Aggregates are trivial: `c_tot == capital`, `pnl_pos_tot == max(pnl, 0)`. No multi-account interaction is tested.
- **2-account proofs** (trade/isolation/liquidation): `add_user(0)` + `add_lp(0, ...)` or two `add_user(0)` calls. The LP is always at the next sequential slot.
- **No proofs test 3+ accounts**, which means:
  - Haircut ratio interactions (settling account i changes residual affecting account j's effective PnL) are only tested with 2 accounts
  - Aggregate sum correctness with partial occupancy (bitmap holes from close/GC) is not tested with realistic topologies
  - Freelist recycling after close + re-add is tested in a few proofs but always from a clean state

**Impact**: The fixed topology means multi-account haircut cascades and aggregate drift under partial occupancy are not exercised. The modular ideal (one arbitrary target account + abstract "rest of system") is not achieved.

### 6c. Invariant Decomposition

**Finding: `canonical_inv` is always checked monolithically.**

Every proof that checks the invariant calls `canonical_inv(&engine)` which internally evaluates:
```rust
inv_structural(engine) && inv_aggregates(engine) && inv_accounting(engine)
    && inv_mode(engine) && inv_per_account(engine)
```

While the individual component functions exist in the test file, no proof:
- Assumes only the relevant subset (e.g., `assume(inv_accounting(engine))` alone for a deposit proof)
- Asserts preservation of individual components independently
- Exploits decomposition to reduce solver burden

**Impact**: Monolithic `canonical_inv` includes loops (in `inv_aggregates` and `inv_per_account`) and structural checks that are irrelevant to many operations. This forces bounded ranges on symbolic inputs to keep solver time manageable, which in turn prevents full-domain reasoning.

### 6d. Loop Elimination in Invariant Specs

**Finding: ALL invariant functions use `for idx in 0..MAX_ACCOUNTS` loops.**

- `inv_aggregates`: iterates all `MAX_ACCOUNTS` slots to compute `sum_capital`, `sum_pnl_pos`, `sum_abs_pos`
- `inv_per_account`: iterates all used accounts checking `reserved_pnl`, `pnl != i128::MIN`, `warmup_slope`
- `inv_structural`: iterates freelist (bounded by `MAX_ACCOUNTS`) and bitmap words
- `sync_engine_aggregates`: iterates all accounts to recompute OI

With `MAX_ACCOUNTS = 4` (Kani config), these loops unwind to 4 iterations, but the solver must reason about all 4 account slots even when the function-under-test touches only 1.

**Impact**: Loop-free delta properties are not used anywhere. For example, `set_capital` could be verified with the loop-free property `c_tot' = c_tot - old_capital + new_capital` rather than re-summing all accounts. This would enable fully symbolic proofs because the solver would not need to track all account fields simultaneously.

### 6e. Cone of Influence

**Finding: Proofs fix many fields outside the cone of influence to concrete values.**

Representative examples:
- **`deposit` proofs**: The function reads/writes `capital`, `vault`, `pnl` (for warmup/fee), `fee_credits`, `last_fee_slot`, `warmup_slope_per_step`, `warmup_started_at_slot`, `c_tot`, `reserved_pnl`. It does NOT read `position_size`, `entry_price`, `funding_index`, `matcher_program`, `matcher_context`, `owner`, or any other account's fields. Yet all proofs fix these to `new()` defaults.
- **`execute_trade` proofs**: The function reads/writes fields on two accounts (user + LP) including `position_size`, `entry_price`, `capital`, `pnl`, `vault`, `insurance`, `c_tot`, `pnl_pos_tot`, `total_open_interest`. Fields like `warmup_slope_per_step` on the LP, `reserved_pnl`, `funding_index` on both accounts are outside the cone but fixed to defaults.
- **`settle_warmup_to_capital` proofs**: Only reads/writes `capital`, `pnl`, `warmup_slope_per_step`, `warmup_started_at_slot`, `c_tot`, `pnl_pos_tot`, `insurance`, `vault`. Fields like `position_size`, `entry_price`, `funding_index` are outside the cone but fixed.

**Impact**: Fixing out-of-cone fields to concrete values does not make the proof incorrect, but it limits generality -- the proof only covers states where these fields have their default values. A fully symbolic proof would leave them unconstrained, and the solver would automatically prune them.

### 6f. Bounded Ranges vs. Full Domain

**Finding: ALL proofs use bounded `kani::assume` ranges on symbolic values.**

Examples:
- `capital >= 100 && capital <= 5_000` (instead of full `u128` range)
- `pnl > -2_000 && pnl < 2_000` (instead of full `i128` range)
- `amount > 0 && amount < 5_000`
- `oracle_price >= 500_000 && oracle_price <= 2_000_000`
- `position_size > -500 && position_size < 500`

These bounds are necessary because:
1. The monolithic `canonical_inv` check with loops makes the solver expensive for large values
2. The constructed-state pattern requires manually computing derived values (e.g., `vault = capital + insurance + pnl_pos`) which can overflow with full-range inputs
3. Some function correctness genuinely requires bounds (e.g., `MAX_ORACLE_PRICE`, `MAX_POSITION_ABS`)

**Impact**: Bounded ranges mean the proofs verify correctness for a subset of the input space. While the bounds are generally chosen to exercise all branches, edge cases near type boundaries (e.g., `capital` near `u128::MAX`) are not covered. An inductive proof with decomposed invariants and loop-free specs would handle full-domain reasoning because only the relevant bits survive cone-of-influence pruning.

---

## Summary Table (All 146 Proofs)

### I2: Conservation of Funds (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 1 | `fast_i2_deposit_preserves_conservation` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [100,5K] |
| 2 | `fast_i2_withdraw_preserves_conservation` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,10K) |

### I5: PNL Warmup Properties (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 3 | `i5_warmup_determinism` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | (-10K,10K) |
| 4 | `i5_warmup_monotonicity` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | (-5K,10K) |
| 5 | `i5_warmup_bounded_by_pnl` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | (-10K,10K) |

### I7: User Isolation (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 6 | `i7_user_isolation_deposit` | **STRONG** | Constructed | 2 users | Monolithic | Loops | Out-of-cone fixed | (0,10K) |
| 7 | `i7_user_isolation_withdrawal` | **STRONG** | Constructed | 2 users | Monolithic | Loops | Out-of-cone fixed | (100,10K) |

### I8: Equity Consistency (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 8 | `i8_equity_with_positive_pnl` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | <10K |
| 9 | `i8_equity_with_negative_pnl` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | <10K |

### Withdrawal Safety (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 10 | `withdrawal_requires_sufficient_balance` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | <10K |
| 11 | `pnl_withdrawal_requires_warmup` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,5K) |

### Arithmetic Safety (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 12 | `saturating_arithmetic_prevents_overflow` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | (-100,100) |

### Edge Cases (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 13 | `zero_pnl_withdrawable_is_zero` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | <10K |
| 14 | `negative_pnl_withdrawable_is_zero` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | (-10K,0) |

### Funding Rate Invariants (7 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 15 | `funding_p1_settlement_idempotent` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | <1M |
| 16 | `funding_p2_never_touches_principal` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | <1M |
| 17 | `funding_p3_bounded_drift_between_opposite_positions` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | (0,10K) |
| 18 | `funding_p4_settle_before_position_change` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | (0,10K) |
| 19 | `funding_p5_bounded_operations_no_overflow` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | (1M,1B) |
| 20 | `funding_p5_invalid_bounds_return_overflow` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | Symbolic |
| 21 | `funding_zero_position_no_change` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | <1M |

### Warmup Correctness (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 22 | `proof_warmup_slope_nonzero_when_positive_pnl` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,10K) |

### Frame Proofs (6 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 23 | `fast_frame_touch_account_only_mutates_one_account` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | <1K |
| 24 | `fast_frame_deposit_only_mutates_one_account_vault_and_warmup` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | (0,10K) |
| 25 | `fast_frame_withdraw_only_mutates_one_account_vault_and_warmup` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [1K,5K] |
| 26 | `fast_frame_execute_trade_only_mutates_two_accounts` | **STRONG** | Constructed | 3 accounts | Monolithic | Loops | Out-of-cone fixed | [500,2K] |
| 27 | `fast_frame_settle_warmup_only_mutates_one_account_and_warmup_globals` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [0,5K] |
| 28 | `fast_frame_update_warmup_slope_only_mutates_one_account` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | (-2K,5K) |

### Validity Preservation (5 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 29 | `fast_valid_preserved_by_deposit` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [100,5K] |
| 30 | `fast_valid_preserved_by_withdraw` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [1K,5K] |
| 31 | `fast_valid_preserved_by_execute_trade` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [500,2K] |
| 32 | `fast_valid_preserved_by_settle_warmup_to_capital` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [0,5K] |
| 33 | `fast_valid_preserved_by_top_up_insurance_fund` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,10K) |

### Negative PnL Settlement / Fix A (5 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 34 | `fast_neg_pnl_settles_into_capital_independent_of_warm_cap` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [0,10K] |
| 35 | `fast_withdraw_cannot_bypass_losses_when_position_zero` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,10K) |
| 36 | `fast_neg_pnl_after_settle_implies_zero_capital` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [0,10K] |
| 37 | `neg_pnl_settlement_does_not_depend_on_elapsed_or_slope` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | [0,10K] |
| 38 | `withdraw_calls_settle_enforces_pnl_or_zero_capital_post` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,10K) |

### Equity Margin / Fix B (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 39 | `fast_maintenance_margin_uses_equity_including_negative_pnl` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Out-of-cone fixed | [0,10K] |
| 40 | `fast_account_equity_computes_correctly` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | <1M |
| 41 | `withdraw_im_check_blocks_when_equity_after_withdraw_below_im` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [50,500] |

### Deterministic Negative PnL (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 42 | `neg_pnl_is_realized_immediately_by_settle` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,10K] |

### Fee Credits (4 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 43 | `proof_fee_credits_never_inflate_from_settle` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [100,5K] |
| 44 | `proof_settle_maintenance_deducts_correctly` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [100,5K] |
| 45 | `proof_trading_credits_fee_to_user` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [100,5M] |
| 46 | `proof_keeper_crank_forgives_half_slots` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [200,500] |

### Keeper Crank (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 47 | `proof_keeper_crank_advances_slot_monotonically` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [10,500] |
| 48 | `proof_keeper_crank_best_effort_settle` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [10,500] |
| 49 | `proof_keeper_crank_best_effort_liquidation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [100,10K] |

### Close Account (4 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 50 | `proof_close_account_requires_flat_and_paid` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [0,5K] |
| 51 | `proof_close_account_rejects_positive_pnl` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [0,5K] |
| 52 | `proof_close_account_includes_warmed_pnl` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [0,5K] |
| 53 | `proof_close_account_negative_pnl_written_off` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | (0,5K] |

### Parameter Update (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 54 | `proof_set_risk_reduction_threshold_updates` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Total Open Interest (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 55 | `proof_total_open_interest_initial` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Freshness Gate (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 56 | `proof_require_fresh_crank_gates_stale` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 57 | `proof_stale_crank_blocks_withdraw` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 58 | `proof_stale_crank_blocks_execute_trade` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Net Extraction (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 59 | `proof_net_extraction_bounded_with_fee_credits` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Liquidation (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 60 | `proof_lq4_liquidation_fee_paid_to_insurance` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [50K,200K] |
| 61 | `proof_lq7_symbolic_oracle_liquidation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [100,10K] |
| 62 | `proof_liq_partial_symbolic` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [100K,400K] |

### Garbage Collection (5 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 63 | `gc_never_frees_account_with_positive_value` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic flags |
| 64 | `fast_valid_preserved_by_garbage_collect_dust` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 65 | `gc_respects_full_dust_predicate` | **STRONG** | Constructed | 3 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 66 | `gc_frees_only_true_dust` | **STRONG** | Constructed | 3 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 67 | `crank_bounds_respected` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Withdrawal Margin Safety (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 68 | `withdrawal_maintains_margin_above_maintenance` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 69 | `withdrawal_rejects_if_below_initial_margin_at_oracle` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Canonical INV Proofs (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 70 | `proof_inv_holds_for_new_engine` | **STRONG** | Constructed | 0->1 user | Monolithic | Loops | N/A (base case) | Symbolic params |
| 71 | `proof_inv_preserved_by_add_user` | **STRONG** | Constructed | 1->2 users | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 72 | `proof_inv_preserved_by_add_lp` | **STRONG** | Constructed | 1->2 accts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Execute Trade Family (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 73 | `proof_execute_trade_preserves_inv` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 74 | `proof_execute_trade_conservation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 75 | `proof_execute_trade_margin_enforcement` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [500,2K] |

### Deposit/Withdraw Families (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 76 | `proof_deposit_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 77 | `proof_withdraw_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Freelist Structural (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 78 | `proof_add_user_structural_integrity` | **STRONG** | Constructed | 1->2 users | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 79 | `proof_close_account_structural_integrity` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Liquidate Family (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 80 | `proof_liquidate_preserves_inv` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Settle Warmup Family (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 81 | `proof_settle_warmup_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 82 | `proof_settle_warmup_negative_pnl_immediate` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Keeper Crank Family (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 83 | `proof_keeper_crank_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### GC Dust Family (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 84 | `proof_gc_dust_preserves_inv` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 85 | `proof_gc_dust_structural_integrity` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Close Account Family (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 86 | `proof_close_account_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Top Up Insurance Family (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 87 | `proof_top_up_insurance_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Sequence-Level Proofs (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 88 | `proof_sequence_deposit_trade_liquidate` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 89 | `proof_sequence_deposit_crank_withdraw` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Funding/Position Conservation (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 90 | `proof_trade_creates_funding_settled_positions` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 91 | `proof_crank_with_funding_preserves_inv` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Variation Margin / No PnL Teleportation (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 92 | `proof_variation_margin_no_pnl_teleport` | **STRONG** | Constructed | 3 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 93 | `proof_trade_pnl_zero_sum` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Inline Migrated (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 94 | `kani_no_teleport_cross_lp_close` | **STRONG** | Constructed | 3 accounts | Monolithic | Loops | Out-of-cone fixed | [500K,2M] |
| 95 | `kani_cross_lp_close_no_pnl_teleport` | **UNIT TEST** | Constructed | 3 accounts | Monolithic | Loops | Out-of-cone fixed | Concrete oracle |

**Rationale for #95 UNIT TEST**: The concrete `ORACLE_100K` constant means mark_pnl calculations, margin checks, and the P90kMatcher's price offset are all exercised at a single oracle price point. The symbolic `size` range [1,5] is also small. While the proof correctly verifies the no-teleport property at this price, it does not generalize over oracle prices. This is an intentional scenario test migrated from inline proofs.

### Matcher Guard (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 96 | `kani_rejects_invalid_matcher_output` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [1,2M] |

### Haircut Mechanism C1-C6 (6 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 97 | `proof_haircut_ratio_formula_correctness` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | <=100K |
| 98 | `proof_effective_equity_with_haircut` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | <=100 |
| 99 | `proof_principal_protection_across_accounts` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | (0,10K] |
| 100 | `proof_profit_conversion_payout_formula` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | <=500 |
| 101 | `proof_rounding_slack_bound` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | (0,100] |
| 102 | `proof_liveness_after_loss_writeoff` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [0,50K] |

### Security Audit Gap Closure - Gap 1 (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 103 | `proof_gap1_touch_account_err_no_mutation` | **STRONG** | Constructed | 1 user | N/A (frame) | N/A | Out-of-cone fixed | Symbolic |
| 104 | `proof_gap1_settle_mark_err_no_mutation` | **STRONG** | Constructed | 1 user | N/A (frame) | N/A | Out-of-cone fixed | Symbolic |
| 105 | `proof_gap1_crank_with_fees_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Security Audit Gap Closure - Gap 2 (4 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 106 | `proof_gap2_rejects_overfill_matcher` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 107 | `proof_gap2_rejects_zero_price_matcher` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 108 | `proof_gap2_rejects_max_price_exceeded_matcher` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 109 | `proof_gap2_execute_trade_err_preserves_inv` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Security Audit Gap Closure - Gap 3 (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 110 | `proof_gap3_conservation_trade_entry_neq_oracle` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 111 | `proof_gap3_conservation_crank_funding_positions` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 112 | `proof_gap3_multi_step_lifecycle_conservation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Security Audit Gap Closure - Gap 4 (4 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 113 | `proof_gap4_trade_extreme_price_no_panic` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [100M,1e15] |
| 114 | `proof_gap4_trade_extreme_size_no_panic` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [1,MAX_POS] |
| 115 | `proof_gap4_trade_partial_fill_diff_price_no_panic` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | [100K,500K] |
| 116 | `proof_gap4_margin_extreme_values_no_panic` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | [1K,10K] |

### Security Audit Gap Closure - Gap 5 (4 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 117 | `proof_gap5_fee_settle_margin_or_err` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 118 | `proof_gap5_fee_credits_trade_then_settle_bounded` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 119 | `proof_gap5_fee_credits_saturating_near_max` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 120 | `proof_gap5_deposit_fee_credits_conservation` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### Premarket Resolution / Aggregate Consistency (8 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 121 | `proof_set_pnl_maintains_pnl_pos_tot` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 122 | `proof_set_capital_maintains_c_tot` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 123 | `proof_force_close_with_set_pnl_preserves_invariant` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 124 | `proof_multiple_force_close_preserves_invariant` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 125 | `proof_haircut_ratio_bounded` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | Symbolic |
| 126 | `proof_effective_pnl_bounded_by_actual` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 127 | `proof_recompute_aggregates_correct` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | Symbolic |
| 128 | `proof_NEGATIVE_bypass_set_pnl_breaks_invariant` | **UNIT TEST** | Constructed | 1 user | N/A (negative) | N/A | N/A | Symbolic |

**Rationale for #128 UNIT TEST**: This is an intentional negative/meta proof that demonstrates the WRONG approach (bypassing `set_pnl()`) DOES break `inv_aggregates`. It uses symbolic inputs but asserts `!inv_aggregates(&engine)` -- the negation of correctness. It exists to prove the real proofs are non-vacuous: if the invariant can be broken by bypass, then the proofs showing it holds via proper helpers are meaningful.

### Missing Conservation Proofs (8 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 129 | `proof_settle_mark_to_oracle_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 130 | `proof_touch_account_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 131 | `proof_touch_account_full_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 132 | `proof_settle_loss_only_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 133 | `proof_accrue_funding_preserves_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 134 | `proof_init_in_place_satisfies_inv` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 135 | `proof_set_pnl_preserves_conservation` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 136 | `proof_set_capital_decrease_preserves_conservation` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### set_capital Aggregate (1 proof)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 137 | `proof_set_capital_aggregate_correct` | **STRONG** | Constructed | 1 user | N/A (property) | N/A | Minimal | Symbolic |

### Multi-Step Conservation (3 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 138 | `proof_lifecycle_trade_then_touch_full_conservation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 139 | `proof_lifecycle_trade_crash_settle_loss_conservation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 140 | `proof_lifecycle_trade_warmup_withdraw_topup_conservation` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### External Review Rebuttal - Flaw 1 (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 141 | `proof_flaw1_debt_writeoff_requires_flat_position` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 142 | `proof_flaw1_gc_never_writes_off_with_open_position` | **STRONG** | Constructed | 2 accounts | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### External Review Rebuttal - Flaw 2 (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 143 | `proof_flaw2_no_phantom_equity_after_mark_settlement` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 144 | `proof_flaw2_withdraw_settles_before_margin_check` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### External Review Rebuttal - Flaw 3 (2 proofs)

| # | Proof Name | Classification | 6a | 6b | 6c | 6d | 6e | 6f |
|---|---|---|---|---|---|---|---|---|
| 145 | `proof_flaw3_warmup_reset_increases_slope_proportionally` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |
| 146 | `proof_flaw3_warmup_converts_after_single_slot` | **STRONG** | Constructed | 1 user | Monolithic | Loops | Out-of-cone fixed | Symbolic |

### INDUCTIVE: Abstract Delta Proofs (11 proofs)

These proofs model operations algebraically on fully symbolic state (full u128/i128 domain, no RiskEngine construction, no loops, no bounds), proving decomposed invariant components are preserved for ALL possible pre-states.

| # | Proof Name | Classification | Component | Verification Time |
|---|---|---|---|---|
| 147 | `inductive_top_up_insurance_preserves_accounting` | **INDUCTIVE** | inv_accounting | 0.87s |
| 148 | `inductive_set_capital_preserves_accounting` | **INDUCTIVE** | inv_accounting | 0.21s |
| 149 | `inductive_set_pnl_preserves_pnl_pos_tot_delta` | **INDUCTIVE** | inv_aggregates | 0.47s |
| 150 | `inductive_set_capital_delta_correct` | **INDUCTIVE** | inv_aggregates | 1.53s |
| 151 | `inductive_deposit_preserves_accounting` | **INDUCTIVE** | inv_accounting | 0.82s |
| 152 | `inductive_withdraw_preserves_accounting` | **INDUCTIVE** | inv_accounting | 0.75s |
| 153 | `inductive_settle_loss_preserves_accounting` | **INDUCTIVE** | inv_accounting | 0.17s |
| 154 | `inductive_settle_warmup_profit_preserves_accounting` | **INDUCTIVE** | inv_accounting | 2.26s |
| 155 | `inductive_settle_warmup_full_preserves_accounting` | **INDUCTIVE** | inv_accounting | 2.51s |
| 156 | `inductive_fee_transfer_preserves_accounting` | **INDUCTIVE** | inv_accounting | 0.41s |
| 157 | `inductive_set_position_delta_correct` | **INDUCTIVE** | inv_aggregates | 1.70s |

### §5.4 Regression: Liquidation Warmup Slope Reset (1 proof)

This proof exercises the real `liquidate_at_oracle` code path with symbolic PnL and oracle values, verifying that warmup slope is correctly reset when mark settlement increases AvailGross during liquidation.

| # | Proof Name | Classification | Property | Verification Time |
|---|---|---|---|---|
| 158 | `proof_liquidation_must_reset_warmup_on_mark_increase` | **STRONG** | §5.4 + canonical_inv | 169.35s |

**Audit of proof #158:**

- **C1 (Input classification)**: `initial_pnl` ∈ [1K, 50K] and `oracle_price` ∈ [1_000_001, 1_010_000] are symbolic via `kani::any()`. Capital (500), position (10M), entry (1M), slot (90), LP state are concrete scaffolding.
- **C2 (Branch coverage)**: Exercises favorable-oracle mark settlement (mark_pnl > 0), liquidation trigger path, profit conversion in settle_warmup_to_capital. All branches in the bug-relevant code path are exercised.
- **C3 (Invariant strength)**: Asserts `canonical_inv` AND domain-specific `cap_after <= cap_before` (warmup conversion bound). Stronger than canonical_inv alone.
- **C4 (Vacuity risk)**: Non-vacuous — explicit `assert!(result.unwrap())` confirms liquidation triggers for all symbolic inputs.
- **C5 (Symbolic collapse)**: Haircut h=1 (large residual >> pnl_pos_tot). Acceptable: bug is about warmup timing, not haircut computation.
- **C6 (Inductive)**: Not inductive — constructed state, fixed topology, bounded ranges. This is intentional: the bug is an implementation-level missing function call that can only be caught by exercising real code.
- **TDD**: Proof was written BEFORE the fix and confirmed to FAIL (catching the §5.4 violation). After fixing `touch_account_for_liquidation` to add the warmup slope reset, the proof PASSES.

**Criteria 1-5 Assessment (all 9 proofs):**

- **C1 (Input classification)**: All inputs are `kani::any()` — fully symbolic u128/i128 with no hardcoded values.
- **C2 (Branch coverage)**: Proof 2 covers decrease-only (increase covered by proof 5/deposit). Proofs 4 and 11 cover both increase/decrease branches (c_tot and OI deltas respectively). Proof 7 exercises both branches of `min(need, capital)`. All other proofs have no conditional branches in their operation model.
- **C3 (Invariant strength)**: Decomposed components — each proof targets exactly the invariant component affected by the operation (inv_accounting or inv_aggregates), not monolithic canonical_inv.
- **C4 (Vacuity risk)**: All assumption sets are satisfiable (verified by Kani passing with VERIFICATION SUCCESSFUL). No contradictory assumes.
- **C5 (Symbolic collapse)**: All symbolic values are independent — no derived values that collapse symbolic ranges.

**Criterion 6 Assessment (all 9 proofs):**

| Sub-criterion | Assessment |
|---|---|
| **6a (State construction)** | Fully symbolic — no `RiskEngine::new()`, no field overwrites, no constructed state |
| **6b (Topology)** | Modular — reasons about one abstract account + aggregate summary values. No fixed account topology. |
| **6c (Invariant decomposition)** | Each proof targets exactly one invariant component (inv_accounting or inv_aggregates) |
| **6d (Loop elimination)** | All proofs are loop-free delta specifications. No `for idx in 0..MAX_ACCOUNTS` loops. |
| **6e (Cone of influence)** | Only cone-of-influence fields are present. No out-of-cone fields fixed to concrete values. |
| **6f (Bounded ranges)** | Full u128/i128 domain. No bounded ranges. Only assumes are structural preconditions (no overflow, invariant holds). |

**Notes on Proofs 154-155 (haircut-based settle_warmup):**

These proofs model the haircutted conversion amount `y` as a symbolic value with assumed bounds `y <= x` and `y <= residual`, rather than computing `y = floor(x * h_num / h_den)` via u128 division (which is intractable for SAT solvers). The haircut bound is derived mathematically:

```
haircut_ratio() returns (h_num, h_den) = (min(residual, pnl_pos_tot), pnl_pos_tot)
y = floor(x * h_num / h_den)
Since x <= h_den and h_num <= h_den:  y <= h_num   (integer division property)
Since h_num = min(residual, pnl_pos_tot) <= residual:  y <= residual   QED
```

This is standard modular verification: the bound is a mathematical fact about integer division, documented in the proof's doc comments. The STRONG proofs (#81, #82, #100) verify the actual haircut computation on concrete executions.

---

## Detailed Criterion 6 Analysis by Proof Category

### Invariant Preservation Proofs (proofs #1-2, #10-11, #22, #29-33, #34-38, #40-42, #43-46, #47-49, #50-54, #70-93, #99-102, #105, #109-112, #113-120, #121-124, #126, #128-136, #138-146)

These are the proofs most relevant for inductive strengthening -- they assert `canonical_inv` is preserved across an operation.

**6a**: All use `RiskEngine::new(test_params()) + add_user/add_lp + field overwrites + sync_engine_aggregates`. The initial state is NOT fully symbolic. An inductive proof would instead create a fully symbolic `RiskEngine` and assume `canonical_inv(&engine)` as a precondition.

**6b**: Topologies are fixed at 1-2 accounts. Multi-account interactions (haircut cascades, aggregate drift with N > 2 accounts, bitmap holes from partial occupancy) are not tested.

**6c**: `canonical_inv` is always checked as a monolithic predicate. No proof assumes/asserts individual components independently.

**6d**: `inv_aggregates` and `inv_per_account` use `for idx in 0..MAX_ACCOUNTS` loops. These are expanded by Kani's bounded model checker (with `#[kani::unwind(33)]`) and add solver overhead.

**6e**: Fields outside the function's cone of influence are fixed to `new()` defaults. For example, `deposit` proofs fix `position_size`, `entry_price`, `funding_index`, `matcher_program/context`, `owner` to defaults even though `deposit` never reads or writes them.

**6f**: Symbolic inputs are bounded to small ranges (typically `[0, 10K]` for capitals, `[-5K, 5K]` for PnL, `[-500, 500]` for positions). This is a symptom of the monolithic invariant check making the solver expensive at larger ranges.

### Property-Specific Proofs (proofs #3-5, #8-9, #12-14, #19-20, #37, #39, #97-98, #125, #127, #137)

These proofs verify mathematical properties of pure functions (warmup calculation, equity formula, haircut ratio, arithmetic safety) rather than state transitions.

**6a-6f Assessment**: Criterion 6 is less applicable to these proofs because they test deterministic formulas rather than state transitions. The initial state construction is scaffolding to reach the function-under-test. The key question for these proofs is whether the symbolic input ranges cover the full domain (Criterion 6f). Most use bounded ranges, but the pure-function nature means the ranges could be expanded independently of invariant loop overhead.

### Frame Proofs (proofs #6-7, #23-28, #103-104)

Frame proofs verify that an operation only modifies the fields it should modify (all other fields are unchanged).

**6a-6f Assessment**: Similar to preservation proofs -- all use constructed state. For frame proofs, the key inductive upgrade would be to start from a fully symbolic state where all account fields are symbolic, then verify that the unmodified fields are byte-identical pre/post. The constructed state limits which "other field" values are tested.

### Error Path Proofs (proofs #106-108, #109)

These verify that operations return the correct error and do not mutate state on failure.

**6a-6f Assessment**: Error path proofs benefit less from inductive strengthening because the error path typically rejects early before touching state. The main benefit would be testing that error detection works for arbitrary states, not just constructed ones.

### Sequence/Lifecycle Proofs (proofs #88-89, #112, #138-140)

These compose multiple operations and check invariant preservation at each step.

**6a-6f Assessment**: Sequence proofs are inherently constructive -- they build up state through a specific operation sequence. An inductive approach would prove each operation independently (which the other proofs already do). The value of sequence proofs is integration testing of operation composition, not inductive generality.

---

## Detailed Analysis of UNIT TEST Proofs (2 proofs)

### 1. `proof_NEGATIVE_bypass_set_pnl_breaks_invariant` (proof #128)

- **Classification**: UNIT TEST (intentional negative/meta proof)
- **Purpose**: Demonstrates that bypassing `set_pnl()` and directly assigning `account.pnl` breaks the `inv_aggregates` invariant. This is a meta-test that proves the REAL proofs are non-vacuous: if the invariant can be broken via bypass, the proofs showing it holds via proper helpers are meaningful.
- **Inputs**: Symbolic `initial_pnl`, `new_pnl`, `bypass_pnl` with `kani::assume(old_contrib != new_contrib)` to ensure positive-PnL contribution changes.
- **Key assertion**: `!inv_aggregates(&engine)` -- asserts the negation of correctness.
- **Criterion 6**: Not applicable -- negative proofs cannot be inductive by definition.
- **Assessment**: Correctly designed. No strengthening possible or needed.

### 2. `kani_cross_lp_close_no_pnl_teleport` (proof #95)

- **Classification**: UNIT TEST (concrete oracle limits generality)
- **Purpose**: Migrated from inline proofs. Tests that opening a position at 90K via P90kMatcher with LP1 and closing at oracle with LP2 does not teleport PnL to LP2.
- **Inputs**: Symbolic `cap_mult` [1,100] (multiplied by 1B), symbolic `size` [1,5], but concrete `ORACLE_100K = 100_000_000_000` for both trades.
- **Concrete limitation**: The concrete oracle means mark PnL calculations, margin checks, and the P90kMatcher's price offset are all exercised at a single oracle price point.
- **Criterion 6**: Even with inductive strengthening, this proof's purpose is scenario-specific. The STRONG version is proof #94.
- **Assessment**: Correctly designed as a migrated inline scenario test.

---

## Priority Upgrade Candidates for Inductive Strengthening

The following proofs are the highest-priority candidates for upgrade to INDUCTIVE classification, grouped by function-under-test. Priority is based on: (a) the function's criticality to system safety, (b) feasibility of decomposed invariant approach, and (c) the security value of full-domain coverage.

### Priority 1: Core Conservation Operations

These operations directly affect `vault`, `c_tot`, or `insurance` -- the primary conservation inequality. A bug here means loss of funds.

| Function | Proof(s) | Why Priority 1 |
|---|---|---|
| `deposit` | #1, #29, #76, #120 | Directly modifies vault and c_tot; fee accrual path modifies insurance |
| `withdraw` | #2, #30, #77 | Directly modifies vault and c_tot; margin check is safety-critical |
| `settle_warmup_to_capital` | #32, #81, #82 | Converts PnL to capital; modifies c_tot, pnl_pos_tot, vault, insurance |
| `settle_loss_only` | #132 | Writes off losses; modifies capital, pnl, c_tot, pnl_pos_tot |
| `top_up_insurance_fund` | #33, #87 | Modifies vault and insurance; simple cone of influence |
| `set_capital` | #122, #136, #137 | Directly modifies c_tot; premarket resolution path |
| `set_pnl` | #121, #135 | Directly modifies pnl_pos_tot; premarket resolution path |

**Recommended approach for Priority 1:**
1. Decompose: For `deposit`, only `inv_accounting` (vault >= c_tot + insurance) and `inv_aggregates` (c_tot sum) need to be preserved. `inv_structural` is not affected. Write loop-free delta specs: `c_tot' = c_tot + amount`, `vault' = vault + amount`.
2. Fully symbolic state: Create `engine` with `kani::any()` for all fields, then `assume(inv_accounting(&engine) && inv_aggregates(&engine))`.
3. Assert: `inv_accounting(&engine_after) && inv_aggregates(&engine_after)` using delta formulas, not re-summation.

### Priority 2: Trade and Position Management

These operations are complex (two accounts, position changes, margin checks) and are the most attack-sensitive.

| Function | Proof(s) | Why Priority 2 |
|---|---|---|
| `execute_trade` | #31, #73, #74, #75 | Modifies two accounts' positions, PnL, capital; margin enforcement |
| `liquidate_at_oracle` | #60, #61, #62, #80 | Emergency position closure; modifies insurance, vault, positions |
| `touch_account` / `touch_account_full` | #23, #130, #131 | Funding settlement; modifies PnL based on funding index delta |
| `accrue_funding` | #133 | Updates global funding index; affects all future settlements |
| `settle_mark_to_oracle` | #129 | Variation margin; modifies PnL and entry_price |

**Recommended approach for Priority 2:**
1. For `execute_trade`: Decompose into `inv_accounting` (vault change from fees), `inv_aggregates` (c_tot unchanged, pnl_pos_tot changes, OI changes), and `inv_per_account` (reserved_pnl, no i128::MIN). Use modular reasoning: one target account + one counterparty + abstract aggregates.
2. For `liquidate_at_oracle`: Similar decomposition but also needs `inv_structural` preservation (if account is closed, freelist must be updated).

### Priority 3: Structural Operations

These operations modify the freelist, bitmap, or account topology.

| Function | Proof(s) | Why Priority 3 |
|---|---|---|
| `add_user` / `add_lp` | #71, #72, #78 | Modifies freelist, bitmap, num_used; all structural fields |
| `close_account` | #50-53, #79, #86 | Inverse of add; modifies freelist, bitmap, aggregates |
| `garbage_collect_dust` | #63-66, #84, #85 | Bulk close of dust accounts; complex structural changes |
| `keeper_crank` | #47-49, #67, #83, #91, #105 | Orchestrator; calls multiple sub-operations |

**Recommended approach for Priority 3:**
1. For `add_user`/`close_account`: `inv_structural` is the critical component. Write a loop-free freelist invariant: `free_count = MAX_ACCOUNTS - popcount(used)`, `free_head` points to a valid unused slot, no cycles (can be expressed as a bounded-length property). `inv_aggregates` can use delta: `c_tot' = c_tot + new_capital` for add, `c_tot' = c_tot - old_capital` for close.
2. For `keeper_crank`: This is the hardest to make inductive because it calls multiple sub-operations in a loop. Consider proving each sub-operation inductively and then using a composition lemma.

### Priority 4: Property-Specific Proofs

These verify mathematical properties rather than state transitions. Inductive strengthening is less applicable but full-domain coverage is valuable.

| Function | Proof(s) | Upgrade Path |
|---|---|---|
| `warmup_withdrawable` | #3-5, #13-14 | Expand symbolic ranges to full u128/i128 domain |
| `effective_equity` | #8-9, #40 | Expand symbolic ranges |
| `haircut_ratio` | #97-98, #125 | Expand symbolic ranges |
| `mark_pnl` / saturating arithmetic | #12 | Already near-full range; low priority |

---

## Recommended Approach for Inductive Upgrades

### Step 1: Decompose `canonical_inv` into Independent Proof Obligations

For each operation `f`, identify which components of `canonical_inv` are affected:

| Operation | inv_structural | inv_aggregates | inv_accounting | inv_per_account |
|---|---|---|---|---|
| deposit | No | c_tot only | vault, c_tot | warmup fields |
| withdraw | No | c_tot, pnl_pos_tot | vault, c_tot | warmup fields |
| execute_trade | No | pnl_pos_tot, OI | vault (fees), insurance | position, entry_price |
| add_user | Yes (freelist, bitmap) | c_tot | vault | New account init |
| close_account | Yes (freelist, bitmap) | c_tot, pnl_pos_tot, OI | vault, insurance | Account cleanup |
| settle_warmup | No | c_tot, pnl_pos_tot | vault, insurance | capital, pnl, warmup |
| settle_loss_only | No | c_tot, pnl_pos_tot | No (vault/insurance unchanged) | capital, pnl |
| top_up_insurance | No | No | vault, insurance | No |
| liquidate | No | c_tot, pnl_pos_tot, OI | vault, insurance | capital, pnl, position |
| accrue_funding | No | No | No | No (only global funding index) |
| touch_account | No | pnl_pos_tot | No | pnl, funding_index |
| set_pnl | No | pnl_pos_tot | No | pnl |
| set_capital | No | c_tot | No (vault unchanged) | capital |
| keeper_crank | Possible (via GC) | Yes | Yes | Yes |
| gc_dust | Yes (freelist, bitmap) | c_tot, pnl_pos_tot, OI | vault, insurance | Account cleanup |

### Step 2: Write Loop-Free Delta Specifications

Replace loop-based aggregate checks with algebraic delta properties:

```
// Instead of: c_tot == sum(capital[i] for i in used)
// Use: c_tot_after = c_tot_before - old_capital[target] + new_capital[target]

// Instead of: pnl_pos_tot == sum(max(pnl[i], 0) for i in used)
// Use: pnl_pos_tot_after = pnl_pos_tot_before - max(old_pnl[target], 0) + max(new_pnl[target], 0)

// Instead of: OI == sum(|pos[i]| for i in used)
// Use: OI_after = OI_before - |old_pos[target]| + |new_pos[target]|
```

### Step 3: Construct Fully Symbolic Proof Templates

```rust
#[kani::proof]
fn inductive_deposit_preserves_inv_accounting() {
    // Fully symbolic engine state (relevant fields only)
    let vault: u128 = kani::any();
    let c_tot: u128 = kani::any();
    let insurance: u128 = kani::any();
    let capital: u128 = kani::any();
    let amount: u128 = kani::any();

    // Assume: inv_accounting holds before
    kani::assume(vault >= c_tot + insurance);  // loop-free!
    kani::assume(capital <= c_tot);  // target account's capital is part of c_tot

    // Non-vacuity
    kani::assume(amount > 0);
    kani::assume(vault.checked_add(amount).is_some());  // no overflow
    kani::assume(c_tot.checked_add(amount).is_some());

    // Operation effect (deposit adds amount to both vault and capital/c_tot)
    let vault_after = vault + amount;
    let c_tot_after = c_tot + amount;

    // Assert: inv_accounting holds after
    kani::assert(vault_after >= c_tot_after + insurance, "deposit preserves conservation");
}
```

This template has NO loops, NO constructed state, and covers the FULL u128 domain. The solver handles it trivially because the algebraic structure is simple.

### Step 4: Prove Decomposition Soundness

Add a one-time proof that `canonical_inv == inv_structural && inv_aggregates && inv_accounting && inv_mode && inv_per_account` (already true by definition, but worth an explicit assertion). Then prove that each operation preserves each relevant component independently. The conjunction of component proofs gives the full `canonical_inv` preservation.

---

## Loop-Free Delta Properties

For each aggregate maintained by the system, the loop-free delta property:

### c_tot (sum of all capitals)
```
c_tot_after = c_tot_before + (new_capital - old_capital)
```
Operations: deposit (+amount), withdraw (-amount), settle_warmup (+converted_amount, -if loss), settle_loss_only (loss writeoff), add_user (+0), close_account (-remaining), liquidate (fee deduction), set_capital (delta).

### pnl_pos_tot (sum of positive PnLs)
```
pnl_pos_tot_after = pnl_pos_tot_before - max(old_pnl, 0) + max(new_pnl, 0)
```
Operations: set_pnl (direct), settle_warmup (pnl decreases by converted amount), settle_loss_only (pnl goes to 0 or becomes positive), touch_account (funding changes pnl), execute_trade (mark-to-market changes pnl), close_account (removes contribution).

### total_open_interest (sum of |position_size|)
```
OI_after = OI_before - |old_pos| + |new_pos|
```
Operations: execute_trade (position changes), liquidate (position reduced/zeroed), close_account (removes contribution).

### inv_structural (freelist + bitmap)
```
// After add_user/add_lp:
// - new bit set in used bitmap
// - free_head advances to next_free[old_free_head]
// - popcount increases by 1
// - num_used_accounts increases by 1

// After close_account/gc_dust:
// - bit cleared in used bitmap
// - closed index pushed onto freelist (next_free[idx] = old_free_head, free_head = idx)
// - popcount decreases by 1
// - num_used_accounts decreases by 1
```
These are naturally loop-free because they describe the delta to one slot, not a global property.

---

## Audit Methodology Applied

### Criteria 1-5 (Symbolic Testing Quality)

**Criterion 1: Input Classification** -- Every proof was checked for whether its inputs come from `kani::any()` (symbolic) or hardcoded values (concrete). Scaffolding policy applied: concrete values that do NOT affect branch coverage in the function-under-test are treated as scaffolding and do not downgrade.

**Criterion 2: Branch Coverage** -- For each proof, the symbolic input ranges were verified against the function-under-test's conditionals. Conservation proofs exercise actual transfer; margin proofs exercise pass/fail; settlement proofs exercise positive/negative PnL paths; frame proofs verify unchanged fields; error-path proofs trigger boundary conditions.

**Criterion 3: Invariant Strength** -- `canonical_inv()` is used for all preservation proofs (upgraded from `valid_state()` in prior commits). Property-specific proofs use exact formula assertions. No proofs use the weaker `valid_state()` as their primary invariant.

**Criterion 4: Vacuity Risk** -- Every proof has at least one of: `assert_ok!`, `assert_err!`, `unwrap()`, explicit non-vacuity sub-assertions, or trivially reachable assertions. No vacuity issues found.

**Criterion 5: Symbolic Collapse** -- Checked whether derived values collapse symbolic ranges. Haircut ratio varies between 0 and 1 in C1-C6 proofs; warmup cap exercises both paths; margin thresholds exercise both above/below; funding settlement has non-zero effect. No collapse issues found.

### Criterion 6 (Inductive Strength)

Applied globally (see section above) and per-proof (see summary table). Finding: 11 INDUCTIVE, 145 STRONG, 2 UNIT TEST. The 11 INDUCTIVE proofs (#147-157) achieve fully symbolic state, decomposed invariants, loop-free specs, and full-domain coverage. Proof #158 is STRONG (exercises real `liquidate_at_oracle` code to catch §5.4 violation). The remaining 146 proofs share structural limitations (constructed state, fixed topology, monolithic invariant, loop-based specs, out-of-cone fields fixed, bounded ranges).

---

## Final Summary

```
INDUCTIVE:  11 / 157  ( 7.0%)
STRONG:    144 / 157  (91.7%)
WEAK:        0 / 157  ( 0.0%)
UNIT TEST:   2 / 157  ( 1.3%)
VACUOUS:     0 / 157  ( 0.0%)
```

157 proofs total. The 11 INDUCTIVE proofs (proofs #147-157) achieve the gold standard: fully symbolic state, decomposed invariant components, loop-free delta specifications, and full u128/i128 domain coverage. They prove that the core conservation inequality (`vault >= c_tot + insurance`) and aggregate delta properties (`c_tot` and `pnl_pos_tot` update correctness) hold for ALL possible states.

The 144 STRONG proofs exercise the real code paths with bounded symbolic inputs and monolithic `canonical_inv`. The 2 UNIT TEST proofs are intentional:
1. **`proof_NEGATIVE_bypass_set_pnl_breaks_invariant`** -- a meta/negative proof that validates the non-vacuity of real proofs.
2. **`kani_cross_lp_close_no_pnl_teleport`** -- a migrated inline scenario test; the STRONG version is proof #94.

No proofs are WEAK or VACUOUS.

### INDUCTIVE Coverage

The 9 INDUCTIVE proofs cover the Priority 1 operations from the upgrade recommendations:

| Operation | Proofs | What's Proven |
|---|---|---|
| `top_up_insurance_fund` | #147 | inv_accounting preserved (vault and insurance both increase) |
| `set_capital` (decrease) | #148 | inv_accounting preserved (c_tot decreases, vault unchanged) |
| `set_pnl` | #149 | inv_aggregates delta correct (pnl_pos_tot update matches exact arithmetic) |
| `set_capital` (both) | #150 | inv_aggregates delta correct (c_tot update matches exact arithmetic) |
| `deposit` | #151 | inv_accounting preserved (vault and c_tot both increase by amount) |
| `withdraw` | #152 | inv_accounting preserved (vault and c_tot both decrease by amount) |
| `settle_loss_only` | #153 | inv_accounting preserved (c_tot decreases, vault/insurance unchanged) |
| `settle_warmup` (profit) | #154 | inv_accounting preserved (haircut bounds c_tot increase to residual) |
| `settle_warmup` (full) | #155 | inv_accounting preserved (loss + profit phases combined) |
| fee transfer (all fee paths) | #156 | inv_accounting preserved (c_tot + insurance invariant under transfer) |
| position change (OI delta) | #157 | inv_aggregates delta correct (total_open_interest update matches exact arithmetic) |

### Path Forward

All three inv_aggregates components now have inductive delta proofs: c_tot (#150), pnl_pos_tot (#149), and total_open_interest (#157). All core accounting patterns for inv_accounting are covered: deposit, withdraw, top-up, fee transfer, loss settlement, and warmup conversion.

Remaining Priority 2-4 operations (execute_trade, liquidate, touch_account, add_user, close_account, etc.) could be upgraded to INDUCTIVE using the same decomposition approach. The key challenge for Priority 2 operations is their larger cone of influence (two accounts, position fields, margin checks).

### Changes from Previous Audit

The previous audit (2026-02-20) classified 0 INDUCTIVE / 144 STRONG / 2 UNIT TEST across 146 proofs. This audit adds 11 new INDUCTIVE proofs (#147-157) implementing the Priority 1 upgrade recommendations plus fee transfer and OI delta coverage. The existing 144 STRONG + 2 UNIT TEST proofs are unchanged. Total: 157 proofs.

---

## Section 7: v11.31 Spec Compliance Proofs (`tests/proofs_v1131.rs`)

Added: 2026-03-23. 17 new proofs covering spec properties 42, 44, 46, 59-70.
Additionally, 1 pre-existing proof (`proof_fee_debt_sweep_consumes_released_pnl` in `proofs_safety.rs`) was fixed.

### #158. `proof_recompute_r_last_always_zero` — **STRONG**

**Property**: 46 — Zero-rate funding recomputation (§4.12)

| Criterion | Analysis |
|---|---|
| 1. Input classification | `rate: i64` — **fully symbolic** (entire i64 domain via `kani::any()`) |
| 2. Branch coverage | `recompute_r_last_from_final_state` has **zero branches** — unconditionally writes 0. Full coverage trivially achieved. |
| 3. Invariant strength | Property-specific: asserts `funding_rate_bps_per_slot_last == 0` post-call. Matches spec requirement exactly. No `canonical_inv` needed for this trivial function. |
| 4. Vacuity risk | **None** — assertion is always reachable (no error paths, no early returns). |
| 5. Symbolic collapse | **None** — symbolic `rate` doesn't interact with any computation; it only tests that the pre-state value is overwritten regardless of input. |
| 6a. State construction | `RiskEngine::new(zero_fee_params())` — constructed, but only `funding_rate_bps_per_slot_last` is in the cone of influence and it IS symbolic. |
| 6b. Topology | N/A — function doesn't touch accounts. |
| 6e. Cone of influence | Writes: `funding_rate_bps_per_slot_last`. Reads: nothing. All other concrete fields are outside the cone. |

**Classification: STRONG** — Symbolic input covers full i64 domain on the only relevant field. Trivial function means this is as strong as possible without being INDUCTIVE (would need fully symbolic engine state for no benefit).

---

### #159. `proof_accrue_no_funding_transfer` — **UNIT TEST**

**Property**: §4.12/§5.4 — Zero-rate core profile: no K change from funding

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: OI = POS_SCALE, price = DEFAULT_ORACLE, slot = 10, rate = 5000. No `kani::any()`. |
| 2. Branch coverage | `accrue_market_to` has 6+ branches. Concrete values lock: `total_dt > 0` (yes), `delta_p == 0` (yes, same price), so `if delta_p != 0` branch NOT taken. Only tests the "no mark-to-market + no funding" path. |
| 3. Invariant strength | Property-specific: asserts K_long/K_short unchanged. Correct for the tested path but doesn't test mark + no-funding or error paths. |
| 4. Vacuity risk | **Low** — `result.is_ok()` assertion confirms the Ok path is reached. |
| 5. Symbolic collapse | N/A — no symbolic inputs. |

**Classification: UNIT TEST** — All inputs concrete. Tests one specific path (same-price time-advance). Confirms no K change when price unchanged and funding removed, but does not exercise the mark branch.

**Recommendation**: Make `rate` symbolic (full i64) and add a symbolic slot delta. This would strengthen to STRONG by covering multiple time-advance amounts with arbitrary stored rates.

---

### #160. `proof_accrue_mark_still_works` — **STRONG**

**Property**: §5.4 — Mark-to-market still applies correctly after funding removal

| Criterion | Analysis |
|---|---|
| 1. Input classification | `new_price: u64` — **symbolic** with bounds (1..2000, != DEFAULT_ORACLE). Engine setup is concrete scaffolding. |
| 2. Branch coverage | Forces `delta_p != 0` (price changes). Both `long_live` and `short_live` branches taken (OI is nonzero on both sides). Exercises the core mark-to-market path. Error paths (oracle=0, stale slot) not tested but are scaffolding concerns. |
| 3. Invariant strength | Exact algebraic: `K_long == K_before + A*ΔP`, `K_short == K_before - A*ΔP`. This is stronger than `canonical_inv` for this specific property — it verifies exact arithmetic. |
| 4. Vacuity risk | **None** — `result.is_ok()` confirmed; price constraint ensures delta_p != 0. |
| 5. Symbolic collapse | Price range 1..2000 covers both positive and negative ΔP (DEFAULT_ORACLE = 1000). Both signs exercised. |
| 6f. Bounded ranges | Price bounded to 2000 — sufficient for exercising all branches of `accrue_market_to`'s mark path. The arithmetic is `checked_u128_mul_i128` which handles full range, but the proof doesn't test near-overflow prices. |

**Classification: STRONG** — Symbolic price exercises both ΔP signs. Exact algebraic assertion. Bounded range is adequate for branch coverage but not full domain.

**Recommendation**: Widen price range to `u32` (up to 4B) to stress `checked_u128_mul_i128` overflow handling. Add symbolic OI values.

---

### #161. `proof_touch_no_maintenance_fee` — **UNIT TEST**

**Property**: §8.2 — Maintenance fees disabled

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: params.maintenance_fee_per_slot = 100, deposit = 1M, slot advance = 100. |
| 2. Branch coverage | `settle_maintenance_fee_internal` has **zero branches** (always stamps slot). The proof confirms fee_credits unchanged, which is trivially true. `touch_account_full` has many branches but all are exercised on the concrete "flat, positive capital, no position" path only. |
| 3. Invariant strength | Property-specific: `fee_credits unchanged`. Correct assertion for the disabled-fees property. |
| 4. Vacuity risk | **None** — `result.is_ok()` confirmed. |
| 5. Symbolic collapse | N/A — no symbolic inputs. |

**Classification: UNIT TEST** — All concrete. Sufficient for the §8.2 property (fees are structurally disabled — no branch can produce fee charges), but doesn't exercise touch_account_full's other paths.

**Recommendation**: Make `dt` (slot advance) and `maintenance_fee_per_slot` symbolic to prove fee_credits is invariant for ALL parameter/time combinations, not just one concrete case. This would upgrade to STRONG.

---

### #162. `proof_deposit_no_insurance_draw` — **STRONG**

**Property**: 62 — Deposit never decrements insurance fund

| Criterion | Analysis |
|---|---|
| 1. Input classification | `amount: u32` — **symbolic** (1..1M). PNL and capital are concrete but deliberately set to trigger the edge case (capital=0, PNL=-10M). |
| 2. Branch coverage | `deposit`'s branches: account exists (yes), TVL check (passes for 1M max), flat-sweep guard (flat=true but PNL<0, so sweep blocked). `settle_losses` runs but capital insufficient → PNL survives. Key branch: no `resolve_flat_negative` call. |
| 3. Invariant strength | Two assertions: `insurance >= ins_before` (never decreases) and `pnl < 0` (loss survives). Together these prove no insurance draw and no loss resolution. |
| 4. Vacuity risk | **None** — `result.is_ok()` asserted; amount > 0 ensures non-trivial deposit. Non-vacuity of `pnl < 0` assertion: with -10M PNL and max 1M deposit going to settle_losses, PNL stays negative. |
| 5. Symbolic collapse | Amount is symbolic but PNL/capital are concrete. The concrete PNL (-10M) ensures settle_losses can never fully cover the loss regardless of amount. This is a deliberate constructive setup, not collapse. |

**Classification: STRONG** — Symbolic amount exercises deposit with variable sizes. Concrete negative PNL is constructive scaffolding to guarantee the "capital insufficient" path. Non-vacuous.

**Recommendation**: Make PNL symbolic (negative, with `assume(pnl < -amount)`) to prove the property holds for ALL PNL/amount combinations where loss exceeds deposit.

---

### #163. `proof_deposit_sweep_pnl_guard` — **UNIT TEST**

**Property**: 66 — Deposit does NOT sweep fee debt when PNL < 0

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: capital=0, PNL=-10M, fee_debt=5000, deposit=10K. |
| 2. Branch coverage | Targets the `if basis == 0 && pnl >= 0` guard in deposit. Concrete values force: flat=true, PNL<0 → sweep blocked. Only tests the negative path of the guard. |
| 3. Invariant strength | `fee_credits unchanged` + `pnl < 0` — correct for the negative case. |
| 4. Vacuity risk | **None** — deposit succeeds, assertions reached. |

**Classification: UNIT TEST** — All concrete, single execution path. Tests the "blocked" side of the PNL guard.

**Recommendation**: Make PNL symbolic (constrained negative) and deposit amount symbolic to prove the guard holds for all negative PNL values.

---

### #164. `proof_deposit_sweep_when_pnl_nonneg` — **UNIT TEST**

**Property**: 66 — Deposit DOES sweep fee debt when PNL >= 0

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: capital=1M, PNL=0, fee_debt=5000, deposit=10K. |
| 2. Branch coverage | Tests positive path of `if basis == 0 && pnl >= 0`. Concrete values force: flat=true, PNL=0 → sweep happens. |
| 3. Invariant strength | `fee_credits > -5000` — confirms debt reduction occurred. |
| 4. Vacuity risk | **None** — deposit succeeds, fee_credits changes observable. |

**Classification: UNIT TEST** — All concrete. Complements #163 by testing the "allowed" side. Together #163+#164 cover both sides of the guard, but individually each is a unit test.

**Recommendation**: Merge into a single proof with symbolic PNL covering both `pnl < 0` (no sweep) and `pnl >= 0` (sweep) to achieve STRONG.

---

### #165. `proof_top_up_insurance_now_slot` — **STRONG**

**Property**: 61 — Insurance top-up bounded arithmetic + slot monotonicity

| Criterion | Analysis |
|---|---|
| 1. Input classification | `amount: u32` — **symbolic** (1..1M), `now_slot: u64` — **symbolic** (50..200). |
| 2. Branch coverage | `top_up_insurance_fund` branches: stale slot (not taken — now_slot >= 50 = current_slot), TVL overflow (not taken for small amounts). Tests the success path with exact arithmetic verification. |
| 3. Invariant strength | Three exact assertions: `current_slot == now_slot`, `V == V_before + amount`, `I == I_before + amount`. Algebraically exact — stronger than conservation-only checks. |
| 4. Vacuity risk | **None** — `result.is_ok()` confirmed, amount > 0 ensures non-trivial operation. |
| 5. Symbolic collapse | **None** — both amount and slot are independently symbolic within their ranges. |
| 6f. Bounded ranges | amount <= 1M, slot 50..200 — adequate for branch coverage. Near-TVL-overflow not tested. |

**Classification: STRONG** — Two symbolic inputs exercise the success path with exact algebraic verification. Bounded ranges are adequate but don't stress overflow paths.

**Recommendation**: Extend amount to `u64` range with `assume(v_before + amount <= MAX_VAULT_TVL)` to exercise the TVL bound more tightly.

---

### #166. `proof_top_up_insurance_rejects_stale_slot` — **UNIT TEST**

**Property**: 61 — Stale slot rejection

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: current_slot=100, now_slot=50, amount=1000. |
| 2. Branch coverage | Targets the `now_slot < current_slot` error branch. Single concrete path. |
| 3. Invariant strength | `result.is_err()` — confirms error path taken. |
| 4. Vacuity risk | **None** — assertion reached unconditionally. |

**Classification: UNIT TEST** — Intentional negative test. Concrete inputs test one specific error condition.

---

### #167. `proof_positive_conversion_denominator` — **STRONG**

**Property**: 69 — h_den > 0 when matured profit exists

| Criterion | Analysis |
|---|---|
| 1. Input classification | `pnl_val: u32` — **symbolic** (1..100K). |
| 2. Branch coverage | `haircut_ratio` branches: `pnl_matured_pos_tot == 0` (not taken — set to pnl_val > 0). Residual computation branches depend on vault/c_tot/insurance relationship. With default construction, vault = c_tot + insurance (balanced), so residual = vault - senior_sum = matured PnL portion. Both `h_num < h_den` and `h_num == h_den` possible depending on vault balance. |
| 3. Invariant strength | `h_den > 0` (the target property) + `h_num <= h_den` (bonus correctness check). Exact match for spec §7.4 requirement. |
| 4. Vacuity risk | **Low** — `pnl_val > 0` ensures non-trivial state. `pnl_matured_pos_tot > 0` directly set. |
| 5. Symbolic collapse | PNL is symbolic but vault/c_tot/insurance are derived from construction. The haircut ratio computation depends on `vault - (c_tot + insurance_balance)` vs `pnl_matured_pos_tot`. With constructed state, vault ≈ c_tot (no separate insurance top-up), so residual behavior is somewhat constrained. |

**Classification: STRONG** — Symbolic PNL exercises the non-zero matured PnL path. Proves the target property directly. Residual branch coverage limited by constructed state.

**Recommendation**: Make vault, c_tot, and insurance symbolic with `assume(vault >= c_tot + insurance)` to exercise both `residual < pnl_matured_pos_tot` and `residual >= pnl_matured_pos_tot` branches independently.

---

### #168. `proof_bilateral_oi_decomposition` — **WEAK**

**Property**: 64 — Exact bilateral OI decomposition after trade

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: size_q = 100 * POS_SCALE, deposits = 5M each, oracle = DEFAULT. |
| 2. Branch coverage | `bilateral_oi_after` has 8 checked arithmetic branches (4 per side). Concrete trade size means only one configuration of (old_a=0, new_a>0, old_b=0, new_b<0) is tested — always "flat → long/short". Doesn't test close, flip, or partial reduce paths. |
| 3. Invariant strength | Three assertions: OI_long matches bilateral sum, OI_short matches bilateral sum, OI_long == OI_short. Algebraically exact. But gated behind `if result.is_ok()` — if trade fails, nothing is asserted. |
| 4. Vacuity risk | **Low for Ok path** — trade is designed to succeed (large capital, reasonable size). But the `if result.is_ok()` gate means the proof asserts nothing on failure. Non-vacuity depends on the Ok path being taken. |
| 5. Symbolic collapse | N/A — no symbolic inputs to collapse. |

**Classification: WEAK** — All concrete inputs test only the "open from flat" OI path. `bilateral_oi_after` has multiple branches for position flips and partial reduces that are not exercised. The `if result.is_ok()` guard introduces minor vacuity risk.

**Recommendation**: Make `size_q` symbolic (both positive and negative, bounded) and pre-open a position before trading to exercise close/flip/reduce paths. Remove `if result.is_ok()` by ensuring trade succeeds via construction, or add `assert!(result.is_ok())`.

---

### #169. `proof_partial_liquidation_remainder_nonzero` — **WEAK**

**Property**: 68 — Partial liquidation leaves nonzero remainder

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: size=400*POS_SCALE, q_close=abs_eff/2, crash=500. |
| 2. Branch coverage | `liquidate_at_oracle_internal` partial path: `ExactPartial(q_close)` with `q_close > 0 && q_close < abs_eff` (half position). Exercises the "valid partial" path. BUT — `if result.is_ok() && result.unwrap()` double-gates the assertion. If liquidation fails (Ok(false) = not liquidatable, or Err), nothing is asserted. |
| 3. Invariant strength | `eff_after != 0` — correct for the target property. Conditional on success. |
| 4. Vacuity risk | **Medium** — crash price = 500 (vs DEFAULT_ORACLE = 1000) should make the account liquidatable, but whether partial liquidation succeeds depends on post-partial health check (§9.4 step 14). If the half-close doesn't restore health, the partial liquidation returns Err, and the assertion is skipped. The proof is potentially vacuous if ExactPartial always fails the health check at this crash price. |
| 5. Symbolic collapse | N/A — concrete inputs. |

**Classification: WEAK** — Concrete inputs, doubly-guarded assertion with non-trivial vacuity risk. The proof may never reach its core assertion if the health check blocks the partial liquidation.

**Recommendation**: Add explicit non-vacuity assertion `assert!(result.is_ok() && result.unwrap(), "liquidation must succeed for this test")` to confirm the Ok(true) path is reachable. Or make q_close symbolic with bounds to find a value that passes the health check. Alternatively, set up margin parameters that guarantee post-partial health (lower maintenance_margin_bps).

---

### #170. `proof_liquidation_policy_validity` — **STRONG**

**Property**: 65 — ExactPartial(0) rejected

| Criterion | Analysis |
|---|---|
| 1. Input classification | **Concrete** setup, but the assertion is a structural negative test: `ExactPartial(0)` MUST NOT succeed. |
| 2. Branch coverage | Tests the `q_close_q == 0` guard in `liquidate_at_oracle_internal`. Single branch target. |
| 3. Invariant strength | `panic!` if `Ok(true)` returned — strong rejection assertion. |
| 4. Vacuity risk | **None** — the assertion fires unconditionally on `Ok(true)`. Even if result is `Ok(false)` (not liquidatable) or `Err`, the proof still passes correctly (ExactPartial(0) was rejected or not applicable). |

**Classification: STRONG** — Intentional negative test with zero vacuity risk. The assertion that `ExactPartial(0)` never succeeds as a partial liquidation is structurally sound.

---

### #171. `proof_deposit_fee_credits_cap` — **STRONG**

**Property**: 60 — Fee credit repayment capped at outstanding debt

| Criterion | Analysis |
|---|---|
| 1. Input classification | `amount: u32` — **symbolic** (1..100K). Debt fixed at 5000. |
| 2. Branch coverage | `deposit_fee_credits` branches: account used (yes), slot check (passes), `capped == 0` (not taken — debt = 5000), TVL check (passes for small amounts). Key: `min(amount, debt)` exercises both `amount < debt` and `amount >= debt` since amount ranges 1..100K vs debt=5000. Both sides of the min() reached. |
| 3. Invariant strength | Three exact assertions: `fee_credits <= 0`, `V == V_before + expected_pay`, `I == I_before + expected_pay`. Algebraically verifies exact payment routing. |
| 4. Vacuity risk | **None** — `result.is_ok()` confirmed. |
| 5. Symbolic collapse | **None** — symbolic amount naturally spans both sides of the `min(amount, 5000)` branch. |

**Classification: STRONG** — Symbolic amount exercises both under-payment and full-payment paths. Exact algebraic verification of V and I deltas.

---

### #172. `proof_partial_liq_health_check_mandatory` — **WEAK**

**Property**: 70 — Post-partial health check runs even with pending reset

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: tiny_close=1, crash=500. |
| 2. Branch coverage | Targets the health check at §9.4 step 14. But the assertion is triple-gated: `if let Ok(true) = result` → `if eff_after != 0` → then check `is_above_maintenance_margin`. If the partial with tiny_close=1 fails the health check (likely, since closing 1 unit out of ~400M barely changes margin), result is Err, and nothing is asserted. |
| 3. Invariant strength | Conditional: if partial succeeds AND remainder nonzero, then margin must be healthy. Correct property, but conditional reach is uncertain. |
| 4. Vacuity risk | **High** — A tiny close of 1 unit against a 400*POS_SCALE position at crash price 500 almost certainly fails the post-partial health check, making the result `Err`. The proof's core assertion (`is_above_maintenance_margin`) is likely never reached, making it vacuously true. |

**Classification: WEAK** — High vacuity risk. The tiny_close=1 construction likely never reaches the assertion. The proof demonstrates the right structure but needs construction that guarantees reachability.

**Recommendation**: Either (a) choose a q_close that would pass the health check (e.g., close 99% of position), or (b) explicitly assert `result.is_err()` to prove the health check REJECTS tiny closes (which is also a valid proof of enforcement), or (c) add non-vacuity witness: `assert!(matches!(result, Ok(true)), "partial must succeed to test health check")`.

---

### #173. `proof_keeper_crank_r_last_zero` — **UNIT TEST**

**Property**: 42 — Post-crank r_last == 0

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: rate=9999, slot+1, DEFAULT_ORACLE, one candidate. |
| 2. Branch coverage | `keeper_crank` calls `recompute_r_last_from_final_state` which always writes 0. Branch coverage of crank itself is minimal (1 account, no liquidation). |
| 3. Invariant strength | `r_last == 0` — correct property assertion. |
| 4. Vacuity risk | **None** — crank succeeds, assertion reached. |

**Classification: UNIT TEST** — All concrete. Verifies r_last=0 after one concrete crank invocation. Sufficient for the trivial property (unconditional zero write).

---

### #174. `proof_deposit_nonflat_no_sweep_no_resolve` — **WEAK**

**Property**: 44 — Deposit into non-flat account skips sweep and resolve

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: position=100*POS_SCALE, fee_debt=-1000, PNL=-500, deposit=10K. |
| 2. Branch coverage | Tests `basis != 0` path in deposit (sweep guard). With position open, `basis != 0`, so sweep is blocked. `fee_credits` unchanged confirms no sweep. `insurance >= ins_before` confirms no resolve. However, `settle_losses` IS called, which may change PNL (capital used to offset loss). The proof accounts for this by not asserting `pnl unchanged`. |
| 3. Invariant strength | Two assertions: `fee_credits unchanged` + `insurance >= ins_before`. Fee_credits assertion is exact. Insurance assertion uses `>=` which is correct (deposit routing through trade fee could increase insurance). |
| 4. Vacuity risk | **None** — deposit succeeds, assertions reached. |
| 5. Symbolic collapse | N/A — no symbolic inputs. |

**Classification: WEAK** — All concrete inputs. Tests one specific non-flat configuration. The `settle_losses` interaction (PNL may change) is acknowledged but not verified symbolically.

**Recommendation**: Make deposit amount and initial PNL symbolic to prove the property holds for all non-flat deposit/PNL combinations. Assert `fee_credits unchanged` for all values where `basis != 0`.

---

### Fixed proof: `proof_fee_debt_sweep_consumes_released_pnl` (proofs_safety.rs) — **STRONG**

**Pre-fix classification**: VACUOUS — asserted payment from released PnL, but `fee_debt_sweep` only pays from capital. With capital=0, nothing was paid, and the assertion `ins_after > ins_before` always failed (proof was actually FAILING, not vacuous).

**Post-fix analysis**:

| Criterion | Analysis |
|---|---|
| 1. Input classification | **All concrete**: capital=10K, fee_debt=-5000. |
| 2. Branch coverage | `fee_debt_sweep` branches: `debt == 0` (not taken — debt=5000), `pay > 0` (taken — min(5000, 10000) = 5000). Both branches of `min(debt, cap)` are deterministic at concrete values, but only the "debt < cap" path is tested. |
| 3. Invariant strength | Three exact assertions: `ins == ins_before + 5000`, `fc == 0`, `cap == cap_before - 5000`. Algebraically exact + `check_conservation()`. Strongest possible for concrete inputs. |
| 4. Vacuity risk | **None** — all assertions reached unconditionally. |

**Post-fix classification: STRONG** — Exact algebraic verification with conservation check. Concrete inputs but fully exercises the "debt < capital" path. The symmetric "debt > capital" path (partial payment) is not tested.

**Recommendation**: Make debt and capital symbolic to exercise both `debt <= cap` and `debt > cap` paths, upgrading to cover partial payment scenarios.

---

### v11.31 Section Summary

| # | Proof | Classification | Property |
|---|---|---|---|
| 158 | `proof_recompute_r_last_always_zero` | **STRONG** | 46 |
| 159 | `proof_accrue_no_funding_transfer` | **STRONG** | §4.12 |
| 160 | `proof_accrue_mark_still_works` | **STRONG** | §5.4 |
| 161 | `proof_touch_no_maintenance_fee` | **STRONG** | §8.2 |
| 162 | `proof_deposit_no_insurance_draw` | **STRONG** | 62 |
| 163 | `proof_deposit_sweep_pnl_guard` | **STRONG** | 66 |
| 164 | `proof_deposit_sweep_when_pnl_nonneg` | **STRONG** | 66 |
| 165 | `proof_top_up_insurance_now_slot` | **STRONG** | 61 |
| 166 | `proof_top_up_insurance_rejects_stale_slot` | **UNIT TEST** | 61 |
| 167 | `proof_positive_conversion_denominator` | **STRONG** | 69 |
| 168 | `proof_bilateral_oi_decomposition` | **STRONG** | 64 |
| 169 | `proof_partial_liquidation_remainder_nonzero` | **STRONG** | 68 |
| 170 | `proof_liquidation_policy_validity` | **STRONG** | 65 |
| 171 | `proof_deposit_fee_credits_cap` | **STRONG** | 60 |
| 172 | `proof_partial_liq_health_check_mandatory` | **STRONG** | 70 |
| 173 | `proof_keeper_crank_r_last_zero` | **STRONG** | 42 |
| 174 | `proof_deposit_nonflat_no_sweep_no_resolve` | **STRONG** | 44 |
| fix | `proof_fee_debt_sweep_consumes_released_pnl` | **STRONG** | §7.5 |

**Breakdown**: 16 STRONG, 0 WEAK, 1 UNIT TEST (intentional negative test #166), 0 VACUOUS

### Changes from Initial v11.31 Audit

All 4 WEAK proofs upgraded to STRONG:
- **#168**: Symbolic i16 trade size after initial open — exercises close, reduce, and flip bilateral OI paths
- **#169**: Near-max leverage with 95%+ close at crash price. Non-vacuity: explicit `assert!(result.unwrap())` confirms Ok(true) path reached
- **#172**: Flipped to negative test — symbolic tiny close (1..255 units) asserts `!matches!(result, Ok(true))`, proving health check rejects insufficient partials. Zero vacuity risk.
- **#174**: Symbolic deposit amount (u32) and fee debt (u16) prove sweep guard for all combinations

5 UNIT TESTs upgraded to STRONG:
- **#159**: Symbolic rate (i64, nonzero) and slot delta (u16, 1..1000)
- **#161**: Symbolic fee_per_slot (u32) and dt (u16, 1..10000)
- **#163**: Symbolic deposit amount (u32) and fee debt (u16) with fixed large negative PNL
- **#164**: Symbolic initial capital (u32) and deposit amount (u32)
- **#173**: Symbolic initial rate (full i64 domain)

Pre-existing fix (`proof_fee_debt_sweep_consumes_released_pnl` in proofs_safety.rs):
- Upgraded from concrete to symbolic capital (u32) and debt (u32), exercising both `debt < cap` and `debt > cap` paths with exact algebraic assertions
