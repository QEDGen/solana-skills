# Risk Engine Spec (Source of Truth) — v11.31

**Combined Single-Document Native 128-bit Revision  
(Off-Chain Shortlist Keeper / Flat-Only Auto-Conversion / Full-Local-PnL Maintenance / Explicit Zero-Rate Funding Core Profile / Immutable Configuration / Unencumbered-Flat Deposit Sweep / Mandatory Post-Partial Local Health Check Edition)**

**Design:** Protected Principal + Junior Profit Claims + Lazy A/K Side Indices (Native 128-bit Base-10 Scaling)  
**Status:** implementation source-of-truth (normative language: MUST / MUST NOT / SHOULD / MAY)  
**Scope:** perpetual DEX risk engine for a single quote-token vault.  
**Goal:** preserve conservation, bounded insolvency handling, oracle-manipulation resistance, and liveness while supporting lazy ADL across the opposing open-interest side without global scans, canonical-order dependencies, or sequential prefix requirements for user settlement.

This is a single combined spec. It supersedes prior delta-style revisions by restating the full current design in one document and replacing the earlier integrated on-chain barrier-scan keeper mode with a minimal on-chain exact-revalidation crank that assumes candidate discovery is performed off chain by permissionless keepers.

## Change summary from v11.30

This revision makes two substantive clarifications and one test-suite correction.

1. **Partial-liquidation local maintenance restoration is now mandatory even when `enqueue_adl` schedules a pending reset.** §9.4 now requires the post-step local maintenance-health check to run unconditionally on the current post-partial state. Once a reset is scheduled, the instruction still skips any *further live-OI-dependent* work, but a successful partial liquidation may no longer bypass its own local health-restoration check.
2. **Exact-drain reset semantics are clarified without changing the reachable ADL state machine.** §5.6 now states explicitly that, under the maintained `OI_eff_long == OI_eff_short` invariant, the nested liq-side reset guards inside the opposing-zero branches are currently tautological and are retained only as defensive structure.
3. **Test Property 54 is rephrased to match reachable states.** The impossible unilateral-drain scenario is replaced with a symmetric exact-drain reset test that verifies the required pending resets are scheduled whenever `enqueue_adl` reaches an opposing-zero branch and that subsequent operations cannot underflow against zero authoritative OI.

## 0. Security goals (normative)

The engine MUST provide the following properties.

1. **Protected principal for flat accounts:** An account with effective position `0` MUST NOT have its protected principal directly reduced by another account's insolvency.

2. **Explicit open-position ADL eligibility:** Accounts with open positions MAY be subject to deterministic protocol ADL if they are on the eligible opposing side of a bankrupt liquidation. ADL MUST operate through explicit protocol state, not hidden execution.

3. **Oracle manipulation safety:** Profits created by short-lived oracle distortion MUST NOT immediately dilute the live haircut denominator, immediately become withdrawable principal, or immediately satisfy initial-margin / withdrawal checks. Fresh positive PnL MUST first enter reserved warmup state and only become matured according to §6. On the touched generating account, positive local PnL MAY support only that account's own maintenance equity. If `T == 0`, this time-gate is intentionally disabled.

4. **Profit-first haircuts:** When the system is undercollateralized, haircuts MUST apply to junior matured profit claims before any protected principal of flat accounts is impacted.

5. **Conservation:** The engine MUST NOT create withdrawable claims exceeding vault tokens, except for explicitly bounded rounding slack.

6. **Liveness:** The engine MUST NOT require `OI == 0`, manual admin recovery, a global scan, or reconciliation of an unrelated prefix of accounts before a user can safely settle, deposit, withdraw, trade, or liquidate.

7. **No zombie poisoning:** Non-interacting accounts MUST NOT indefinitely pin the matured-profit haircut denominator with fresh, unwarmed PnL. Touched accounts MUST make warmup progress.

8. **Funding / mark / ADL exactness under laziness:** Any economic quantity whose correct value depends on the position held over an interval MUST be represented through the A/K side-index mechanism or a formally equivalent event-segmented method. Integer rounding MUST NOT mint positive aggregate claims.

9. **No hidden protocol MM:** The protocol MUST NOT secretly internalize user flow against an undisclosed residual inventory.

10. **Defined recovery from precision stress:** The engine MUST define deterministic recovery when side precision is exhausted. It MUST NOT rely on assertion failure, silent overflow, or permanent `DrainOnly` states.

11. **No sequential quantity dependency:** Same-epoch account settlement MUST be fully local. It MAY depend on the account's own stored basis and current global side state, but MUST NOT require a canonical-order prefix or global carry cursor.

12. **Protocol-fee neutrality:** Explicit protocol fees MUST either be collected into `I` immediately or tracked as account-local fee debt. They MUST NOT be socialized through `h`, and unpaid explicit fees MUST NOT inflate bankruptcy deficit `D`. A voluntary organic exit to flat MUST NOT be able to leave a reclaimable account with negative exact `Eq_maint_raw_i` solely because protocol fee debt was left behind.

13. **Synthetic liquidation price integrity:** A synthetic liquidation close MUST execute at the current oracle mark with zero execution-price slippage. Any liquidation penalty MUST be represented only by explicit fee state.

14. **Loss seniority over protocol fees:** When a trade, deposit, or non-bankruptcy liquidation realizes trading losses for an account, those losses are senior to protocol fee collection from that same local capital state.

15. **Instruction-final funding anti-retroactivity:** If an instruction mutates any funding-rate input, the stored next-interval `r_last` MUST correspond to the instruction's final post-reset state, not any intermediate state.

16. **Deterministic overflow handling:** Any arithmetic condition that is not proven unreachable by the spec's numeric bounds MUST have a deterministic fail-safe or bounded fallback path. Silent wrap, unchecked panic, or undefined truncation are forbidden.

17. **Finite-capacity liveness:** Because account capacity is finite, the engine MUST provide permissionless dead-account reclamation or equivalent slot reuse so abandoned empty accounts and flat dust accounts below the live-balance floor cannot permanently exhaust capacity.

18. **Permissionless off-chain keeper compatibility:** Candidate discovery MAY be performed entirely off chain. The engine MUST expose exact current-state shortlist processing and targeted per-account settle / liquidate / reclaim paths so any permissionless keeper can make liquidation and reset progress without any required on-chain phase-1 scan or trusted off-chain classification.

19. **No pure-capital insurance draw without accrual:** A pure capital-only instruction that does not call `accrue_market_to` MUST NOT decrement `I` or record uninsured protocol loss. Such an instruction MAY increase `I` through explicit fee collection, direct fee-credit repayment, or an insurance top-up, and it MAY settle negative PnL from local principal, but any remaining flat negative PnL MUST wait for a later full accrued touch.

20. **Configuration immutability within a market instance:** The warmup, fee, margin, liquidation, insurance-floor, and live-balance-floor parameters that define a market instance MUST remain fixed for the lifetime of that instance unless a future revision defines an explicit safe update procedure.

**Atomic execution model (normative):** Every top-level external instruction defined in §10 MUST be atomic. If any required precondition, checked-arithmetic guard, or conservative-failure condition fails, the instruction MUST roll back all state mutations performed since that instruction began.

---

## 1. Types, units, scaling, and arithmetic requirements

### 1.1 Amounts

- `u128` unsigned amounts are denominated in quote-token atomic units, positive-PnL aggregates, OI, fixed-point position magnitudes, and bounded fee amounts.
- `i128` signed amounts represent realized PnL, K-space liabilities, and fee-credit balances.
- `wide_signed` in formula definitions means any transient exact signed intermediate domain wider than `i128` (for example `i256`) or an equivalent exact comparison-preserving construction.
- All persistent state MUST fit natively into 128-bit boundaries. Emulated wide multi-limb integers (for example `u256` / `i256`) are permitted only within transient intermediate math steps.

### 1.2 Prices and internal positions

- `POS_SCALE = 1_000_000` (6 decimal places of position precision).
- `price: u64` is quote-token atomic units per `1` base. There is no separate `PRICE_SCALE`.
- All external price inputs, including `oracle_price`, `exec_price`, and any stored funding price sample, MUST satisfy `0 < price <= MAX_ORACLE_PRICE`.
- Internally the engine stores position bases as signed fixed-point base quantities:
  - `basis_pos_q_i: i128`, with units `(base * POS_SCALE)`.
- Effective notional at oracle is:
  - `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`.
- Trade fees MUST use executed trade size, not account notional:
  - `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)`.

### 1.3 A/K scale

- `ADL_ONE = 1_000_000` (6 decimal places of fractional decay accuracy).
- `A_side` is dimensionless and scaled by `ADL_ONE`.
- `K_side` has units `(ADL scale) * (quote atomic units per 1 base)`.

### 1.4 Concrete normative bounds

The following bounds are normative and MUST be enforced.

- `MAX_VAULT_TVL = 10_000_000_000_000_000`
- `MAX_ORACLE_PRICE = 1_000_000_000_000`
- `MAX_POSITION_ABS_Q = 100_000_000_000_000`
- `MAX_TRADE_SIZE_Q = MAX_POSITION_ABS_Q`
- `MAX_OI_SIDE_Q = 100_000_000_000_000`
- `MAX_ACCOUNT_NOTIONAL = 100_000_000_000_000_000_000`
- `MAX_PROTOCOL_FEE_ABS = 100_000_000_000_000_000_000`
- configured `MIN_INITIAL_DEPOSIT` MUST satisfy `0 < MIN_INITIAL_DEPOSIT <= MAX_VAULT_TVL`
- configured `MIN_NONZERO_MM_REQ` and `MIN_NONZERO_IM_REQ` MUST satisfy `0 < MIN_NONZERO_MM_REQ < MIN_NONZERO_IM_REQ <= MIN_INITIAL_DEPOSIT`
- deployment configuration of `MIN_INITIAL_DEPOSIT`, `MIN_NONZERO_MM_REQ`, and `MIN_NONZERO_IM_REQ` MUST be economically non-trivial for the quote token and MUST NOT be set below the deployment's tolerated slot-pinning dust threshold
- `MAX_ABS_FUNDING_BPS_PER_SLOT = 10_000`
- `|r_last| <= MAX_ABS_FUNDING_BPS_PER_SLOT`
- `MAX_TRADING_FEE_BPS = 10_000`
- `MAX_INITIAL_BPS = 10_000`
- `MAX_MAINTENANCE_BPS = 10_000`
- `MAX_LIQUIDATION_FEE_BPS = 10_000`
- configured margin parameters MUST satisfy `0 <= maintenance_bps <= initial_bps <= MAX_INITIAL_BPS`
- `MAX_FUNDING_DT = 65_535`
- `MAX_MATERIALIZED_ACCOUNTS = 1_000_000`
- `MAX_ACTIVE_POSITIONS_PER_SIDE` MUST be finite and MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`
- `MAX_ACCOUNT_POSITIVE_PNL = 100_000_000_000_000_000_000_000_000_000_000`
- `MAX_PNL_POS_TOT = MAX_MATERIALIZED_ACCOUNTS * MAX_ACCOUNT_POSITIVE_PNL = 100_000_000_000_000_000_000_000_000_000_000_000_000`
- `MIN_A_SIDE = 1_000`
- `0 <= I_floor <= MAX_VAULT_TVL`
- `0 <= min_liquidation_abs <= liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS`
- `A_side > 0` whenever `OI_eff_side > 0` and the side is still representable.

The following interpretation is normative for dust accounting:

- `stored_pos_count_side` MAY be used as a q-unit conservative term in phantom-dust accounting because each live stored position can contribute at most one additional q-unit from threshold crossing when a global `A_side` truncation occurs.

### 1.5 Trusted time / oracle requirements

- `now_slot` in all top-level instructions MUST come from trusted runtime slot metadata or a formally equivalent trusted source. Production entrypoints MUST NOT accept an arbitrary user-specified substitute.
- `oracle_price` MUST come from a validated configured oracle feed. Stale, invalid, or out-of-range oracle reads MUST fail conservatively before state mutation.
- Any helper or instruction that accepts `now_slot` MUST require `now_slot >= current_slot`.
- Any call to `accrue_market_to(now_slot, oracle_price)` MUST require `now_slot >= slot_last`.
- `current_slot` and `slot_last` MUST be monotonically nondecreasing.

### 1.6 Arithmetic requirements

The engine MUST satisfy all of the following.

1. All products involving `A_side`, `K_side`, `k_snap_i`, `basis_pos_q_i`, `effective_pos_q(i)`, `price`, funding deltas, or ADL deltas MUST use checked arithmetic.
2. This revision has no live funding-transfer loop because `r_last` is permanently zero under §4.12. Implementations MAY therefore omit interval sub-stepping inside `accrue_market_to`. Any future revision that re-enables nonzero funding MUST split `dt` into internal sub-steps with `dt <= MAX_FUNDING_DT`.
3. The conservation check `V >= C_tot + I` and any Residual computation MUST use checked `u128` addition for `C_tot + I`. Overflow is an invariant violation.
4. Signed division with positive denominator MUST use the exact helper in §4.8.
5. Positive ceiling division MUST use the exact helper in §4.8.
6. Warmup-cap computation `w_slope_i * elapsed` MUST use `saturating_mul_u128_u64` or a formally equivalent min-preserving construction.
7. Every decrement of `stored_pos_count_*`, `stale_account_count_*`, or `phantom_dust_bound_*_q` MUST use checked subtraction. Underflow indicates corruption and MUST fail conservatively.
8. Every increment of `stored_pos_count_*`, `phantom_dust_bound_*_q`, `C_tot`, `PNL_pos_tot`, or `PNL_matured_pos_tot` MUST use checked addition and MUST enforce the relevant configured bound.
9. This revision has no live funding-transfer branch. Any future revision that re-enables nonzero funding MUST derive funding from the payer side first so that rounding cannot mint positive aggregate claims.
10. `K_side` is cumulative across epochs. Under the 128-bit limits here, K-side overflow is practically impossible within realistic lifetimes, but implementations MUST still use checked arithmetic and revert on `i128` overflow.
11. Same-epoch or epoch-mismatch `pnl_delta` MUST evaluate the signed numerator `(abs(basis_pos) * K_diff)` in an exact wide intermediate before division by `(a_basis * POS_SCALE)` and MUST use `wide_signed_mul_div_floor_from_k_pair` from §4.8.
12. Any exact helper of the form `floor(a * b / d)` or `ceil(a * b / d)` required by this spec MUST return the exact quotient even when the exact product `a * b` exceeds native `u128`, provided the exact final quotient fits in the destination type.
13. Haircut paths `floor(released_pos_i * h_num / h_den)` and `floor(x * h_num / h_den)` MUST use the exact multiply-divide helpers of §4.8. The final quotient MUST fit in `u128`; the intermediate product need not.
14. The ADL quote-deficit path MUST compute `delta_K_abs = ceil(D_rem * A_old * POS_SCALE / OI)` using exact wide arithmetic. If the exact quotient is not representable as an `i128` magnitude, the engine MUST route `D_rem` through `record_uninsured_protocol_loss` while still continuing quantity socialization.
15. If a K-space K-index delta is representable as a magnitude but the signed addition `K_opp + delta_K_exact` overflows `i128`, the engine MUST route `D_rem` through `record_uninsured_protocol_loss` while still continuing quantity socialization.
16. `PNL_i` MUST be maintained in the closed interval `[i128::MIN + 1, i128::MAX]`, and `fee_credits_i` MUST be maintained in `[i128::MIN + 1, 0]`. Any operation that would set either value to exactly `i128::MIN` is non-compliant and MUST fail conservatively.
17. Global A-truncation dust added in `enqueue_adl` MUST be accounted using checked arithmetic and the exact conservative bound from §5.6.
18. `trade_notional <= MAX_ACCOUNT_NOTIONAL` MUST be enforced before charging trade fees.
19. Any out-of-bound external price input, any invalid oracle read, or any non-monotonic slot input MUST fail conservatively before state mutation.
20. Any direct fee-credit repayment path MUST cap its applied amount at the exact current `FeeDebt_i`; it MUST never set `fee_credits_i > 0`.
21. Any direct insurance top-up or direct fee-credit repayment path that increases `V` or `I` MUST use checked addition and MUST enforce `MAX_VAULT_TVL`.

### 1.7 Reference 128-bit boundary proof

By clamping constants to base-10 metrics, on-chain persistent state fits natively in 128-bit registers without truncation.

Under the zero-rate core profile, the funding-specific bounds below are retained only to justify the future-compatible state shape; no funding transfer occurs in this revision.

- Effective-position numerator: `MAX_POSITION_ABS_Q * ADL_ONE = 10^14 * 10^6 = 10^20`
- Notional / trade-notional numerator: `MAX_POSITION_ABS_Q * MAX_ORACLE_PRICE = 10^14 * 10^12 = 10^26`
- Trade slippage numerator: `MAX_TRADE_SIZE_Q * MAX_ORACLE_PRICE = 10^26`, which fits inside signed 128-bit
- Mark term max step: `ADL_ONE * MAX_ORACLE_PRICE = 10^18`
- Funding payer max step: `ADL_ONE * (MAX_ORACLE_PRICE * MAX_ABS_FUNDING_BPS_PER_SLOT * MAX_FUNDING_DT / 10_000) ≈ 6.55 × 10^22`
- Funding receiver numerator: `6.55 × 10^22 * ADL_ONE ≈ 6.55 × 10^28`
- `A_old * OI_post`: `10^6 * 10^14 = 10^20`
- `PNL_pos_tot` hard cap: `10^38 < u128::MAX ≈ 3.4 × 10^38`
- Absolute nonzero-position margin floors: `MIN_NONZERO_MM_REQ` and `MIN_NONZERO_IM_REQ` are bounded by `MIN_INITIAL_DEPOSIT <= 10^16`, so they fit natively in `u128`
- `K_side` overflow under max-step accumulation requires on the order of `10^12` years
- The three always-wide paths remain:
  1. exact `pnl_delta`
  2. exact haircut multiply-divides
  3. exact ADL `delta_K_abs`

---

## 2. State model

### 2.1 Account state

For each materialized account `i`, the engine stores at least:

- `C_i: u128` — protected principal.
- `PNL_i: i128` — realized PnL claim.
- `R_i: u128` — reserved positive PnL that has not yet matured through warmup, with `0 <= R_i <= max(PNL_i, 0)`.
- `basis_pos_q_i: i128` — signed fixed-point base basis at the last explicit position mutation or forced zeroing.
- `a_basis_i: u128` — side multiplier in effect when `basis_pos_q_i` was last explicitly attached.
- `k_snap_i: i128` — last realized `K_side` snapshot.
- `epoch_snap_i: u64` — side epoch in which the basis is defined.
- `fee_credits_i: i128`.
- `last_fee_slot_i: u64` — deterministic metadata anchor reserved for the disabled recurring maintenance-fee model of this revision.
- `w_start_i: u64`.
- `w_slope_i: u128`.

Derived local quantities on a touched state:

- `ReleasedPos_i = max(PNL_i, 0) - R_i`
- `FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`

Fee-credit bounds:

- `fee_credits_i` MUST be initialized to `0`.
- The engine MUST maintain `-(i128::MAX) <= fee_credits_i <= 0` at all times. `fee_credits_i == i128::MIN` is forbidden.

### 2.2 Global engine state

The engine stores at least:

- `V: u128`
- `I: u128`
- `I_floor: u128`
- `current_slot: u64`
- `P_last: u64`
- `slot_last: u64`
- `r_last: i64`
- `fund_px_last: u64`
- `A_long: u128`
- `A_short: u128`
- `K_long: i128`
- `K_short: i128`
- `epoch_long: u64`
- `epoch_short: u64`
- `K_epoch_start_long: i128`
- `K_epoch_start_short: i128`
- `OI_eff_long: u128`
- `OI_eff_short: u128`
- `mode_long ∈ {Normal, DrainOnly, ResetPending}`
- `mode_short ∈ {Normal, DrainOnly, ResetPending}`
- `stored_pos_count_long: u64`
- `stored_pos_count_short: u64`
- `stale_account_count_long: u64`
- `stale_account_count_short: u64`
- `phantom_dust_bound_long_q: u128`
- `phantom_dust_bound_short_q: u128`
- `C_tot: u128 = Σ C_i`
- `PNL_pos_tot: u128 = Σ max(PNL_i, 0)`
- `PNL_matured_pos_tot: u128 = Σ(max(PNL_i, 0) - R_i)`

The engine MUST also store, or deterministically derive from immutable configuration, at least:

- `T = warmup_period_slots`
- `trading_fee_bps`
- `maintenance_bps`
- `initial_bps`
- `liquidation_fee_bps`
- `liquidation_fee_cap`
- `min_liquidation_abs`
- `MIN_INITIAL_DEPOSIT`
- `MIN_NONZERO_MM_REQ`
- `MIN_NONZERO_IM_REQ`

This revision has **no separate `fee_revenue` state** and **no live recurring maintenance-fee accumulator**. All explicit fee proceeds and direct fee-credit repayments accrue into `I`, and §4.12 fully determines `r_last` without any additional funding-rate parameters.

Global invariants:

- `PNL_matured_pos_tot <= PNL_pos_tot <= MAX_PNL_POS_TOT`
- `C_tot <= V <= MAX_VAULT_TVL`
- `I <= V`
- `|r_last| <= MAX_ABS_FUNDING_BPS_PER_SLOT`

### 2.2.1 Configuration immutability

All configuration values that affect economics or liveness are immutable for the lifetime of a market instance in this revision.

No external instruction in this revision may change `T`, `trading_fee_bps`, `maintenance_bps`, `initial_bps`, `liquidation_fee_bps`, `liquidation_fee_cap`, `min_liquidation_abs`, `MIN_INITIAL_DEPOSIT`, `MIN_NONZERO_MM_REQ`, `MIN_NONZERO_IM_REQ`, `I_floor`, or any other parameter fixed by §§1.4, 2.2, and 4.12.

A deployment that wishes to change any such value MUST migrate to a new market instance or future revision that defines an explicit safe update procedure. In particular, this revision has no runtime parameter-update instruction.

### 2.3 Materialized-account capacity

The engine MUST track the number of currently materialized account slots. That count MUST NOT exceed `MAX_MATERIALIZED_ACCOUNTS`.

A missing account is one whose slot is not currently materialized. Missing accounts MUST NOT be auto-materialized by `settle_account`, `withdraw`, `execute_trade`, `liquidate`, or `keeper_crank`.

Only the following path MAY materialize a missing account in this specification:

- a `deposit(i, amount, now_slot)` with `amount >= MIN_INITIAL_DEPOSIT`

Any implementation-defined alternative creation path is non-compliant unless it enforces an economically equivalent anti-spam threshold and preserves all account-initialization invariants of §2.5.

### 2.4 Canonical zero-position defaults

The canonical zero-position account defaults are:

- `basis_pos_q_i = 0`
- `a_basis_i = ADL_ONE`
- `k_snap_i = 0`
- `epoch_snap_i = 0`

These defaults are valid because all helpers that use side-attached snapshots MUST first require `basis_pos_q_i != 0`.

### 2.5 Account materialization

`materialize_account(i, slot_anchor)` MAY succeed only if the account is currently missing and materialized-account capacity remains below `MAX_MATERIALIZED_ACCOUNTS`.

On success, it MUST increment the materialized-account count and set:

- `C_i = 0`
- `PNL_i = 0`
- `R_i = 0`
- canonical zero-position defaults from §2.4
- `fee_credits_i = 0`
- `w_start_i = slot_anchor`
- `w_slope_i = 0`
- `last_fee_slot_i = slot_anchor`

### 2.6 Permissionless empty- or flat-dust-account reclamation

The engine MUST provide a permissionless reclamation path `reclaim_empty_account(i)`.

It MAY succeed only if all of the following hold:

- account `i` is materialized
- `0 <= C_i < MIN_INITIAL_DEPOSIT`
- `PNL_i == 0`
- `R_i == 0`
- `basis_pos_q_i == 0`
- `fee_credits_i <= 0`

On success, it MUST:

- if `C_i > 0`:
  - let `dust = C_i`
  - `set_capital(i, 0)`
  - `I = checked_add_u128(I, dust)`
- forgive any negative `fee_credits_i` by setting `fee_credits_i = 0`
- reset all local fields to canonical zero / anchored defaults
- mark the slot missing / reusable
- decrement the materialized-account count

This forgiveness is safe only because voluntary organic paths that would leave a flat account with negative exact `Eq_maint_raw_i` are forbidden by §10.5. Reclamation is therefore reserved for genuinely empty or economically dust-flat accounts whose remaining fee debt is uncollectible. A user who wishes to preserve a flat balance below `MIN_INITIAL_DEPOSIT` MUST withdraw it to zero or top it back up above the live-balance floor before a permissionless reclaim occurs.

A reclaimed empty or flat-dust account MUST contribute nothing to `C_tot`, `PNL_pos_tot`, `PNL_matured_pos_tot`, side counts, stale counts, or OI. Any swept dust capital becomes part of `I` and leaves `V` unchanged, so `C_tot + I` is conserved.

### 2.7 Initial market state

Market initialization MUST take, at minimum, `init_slot`, `init_oracle_price`, and configured fee / margin / insurance / materialization parameters.

At market initialization, the engine MUST set:

- `V = 0`
- `I = 0`
- `I_floor = configured I_floor`
- `C_tot = 0`
- `PNL_pos_tot = 0`
- `PNL_matured_pos_tot = 0`
- `current_slot = init_slot`
- `slot_last = init_slot`
- `P_last = init_oracle_price`
- `fund_px_last = init_oracle_price`
- `r_last = 0`
- `A_long = ADL_ONE`, `A_short = ADL_ONE`
- `K_long = 0`, `K_short = 0`
- `epoch_long = 0`, `epoch_short = 0`
- `K_epoch_start_long = 0`, `K_epoch_start_short = 0`
- `OI_eff_long = 0`, `OI_eff_short = 0`
- `mode_long = Normal`, `mode_short = Normal`
- `stored_pos_count_long = 0`, `stored_pos_count_short = 0`
- `stale_account_count_long = 0`, `stale_account_count_short = 0`
- `phantom_dust_bound_long_q = 0`, `phantom_dust_bound_short_q = 0`

### 2.8 Side modes and reset lifecycle

A side may be in one of three modes:

- `Normal`: ordinary operation
- `DrainOnly`: the side is live but has decayed below the safe precision threshold; OI on that side may decrease but MUST NOT increase
- `ResetPending`: the side has been fully drained and its prior epoch is awaiting stale-account reconciliation; no operation may increase OI on that side

`begin_full_drain_reset(side)` MAY succeed only if `OI_eff_side == 0`. It MUST:

1. set `K_epoch_start_side = K_side`
2. increment `epoch_side` by exactly `1`
3. set `A_side = ADL_ONE`
4. set `stale_account_count_side = stored_pos_count_side`
5. set `phantom_dust_bound_side_q = 0`
6. set `mode_side = ResetPending`

`finalize_side_reset(side)` MAY succeed only if all of the following hold:

- `mode_side == ResetPending`
- `OI_eff_side == 0`
- `stale_account_count_side == 0`
- `stored_pos_count_side == 0`

On success, it MUST set `mode_side = Normal`.

`maybe_finalize_ready_reset_sides_before_oi_increase()` MUST check each side independently and, if the `finalize_side_reset(side)` preconditions already hold, immediately finalize that side. It MUST NOT begin a new reset or mutate OI.

### 2.8.1 Epoch-gap invariant

For every materialized account with `basis_pos_q_i != 0` on side `s`, the engine MUST maintain exactly one of the following states:

- **current attachment:** `epoch_snap_i == epoch_s`, or
- **stale one-epoch lag:** `mode_s == ResetPending` and `epoch_snap_i + 1 == epoch_s`.

Epoch gaps larger than `1` are forbidden.

Informative preservation note: `begin_full_drain_reset(side)` increments the side epoch once and snapshots the still-stored positions as stale, while `finalize_side_reset(side)` is impossible until both `stale_account_count_side == 0` and `stored_pos_count_side == 0`. Because no OI-increasing path may attach a new nonzero basis on a `ResetPending` side, a second epoch increment cannot occur while an older stale basis from the previous epoch still exists.

---

## 3. Solvency, matured-profit haircut, and live equity

### 3.1 Residual backing available to matured junior profits

Define:

- `senior_sum = checked_add_u128(C_tot, I)`
- `Residual = max(0, V - senior_sum)`

Invariant: the engine MUST maintain `V >= senior_sum` at all times.

### 3.2 Matured positive-PnL aggregate

Define:

- `ReleasedPos_i = max(PNL_i, 0) - R_i`
- `PNL_matured_pos_tot = Σ ReleasedPos_i`

Fresh positive PnL that has not yet warmed up MUST contribute to `R_i` first and therefore MUST NOT immediately increase `PNL_matured_pos_tot`.

### 3.3 Global haircut ratio `h`

Let:

- if `PNL_matured_pos_tot == 0`, define `h = 1`
- else:
  - `h_num = min(Residual, PNL_matured_pos_tot)`
  - `h_den = PNL_matured_pos_tot`

For account `i` on a touched state:

- if `PNL_matured_pos_tot == 0`, `PNL_eff_matured_i = ReleasedPos_i`
- else `PNL_eff_matured_i = mul_div_floor_u128(ReleasedPos_i, h_num, h_den)`

Because each account is floored independently:

- `Σ PNL_eff_matured_i <= h_num <= Residual`

### 3.4 Live equity used by margin and liquidation

For account `i` on a touched state, first define the exact signed quantity used for initial-margin, withdrawal, and principal-conversion style checks in a transient widened signed domain:

- `Eq_init_base_i = (C_i as wide_signed) + min(PNL_i, 0) + (PNL_eff_matured_i as wide_signed)`

Then define:

- `Eq_init_raw_i = Eq_init_base_i - (FeeDebt_i as wide_signed)`
- `Eq_init_net_i = max(0, Eq_init_raw_i)`
- `Eq_maint_raw_i = (C_i as wide_signed) + (PNL_i as wide_signed) - (FeeDebt_i as wide_signed)`
- `Eq_net_i = max(0, Eq_maint_raw_i)`

Interpretation:

- `Eq_init_raw_i` is the exact widened signed quantity used for initial-margin and withdrawal-style approval checks. Fresh reserved PnL in `R_i` does **not** count here, and matured junior profit counts only through the global haircut of §3.3.
- `Eq_init_net_i` is a clamped nonnegative convenience quantity derived from `Eq_init_raw_i`. It MAY be exposed for reporting, but it MUST NOT be used where negative raw equity must be distinguished from zero, including risk-increasing trade approval and open-position withdrawal approval.
- `Eq_net_i` / `Eq_maint_raw_i` are the quantities used for maintenance-margin and liquidation checks. On a touched generating account, full local mark-to-market `PNL_i` counts here, whether currently released or still reserved.
- The global haircut remains a claim-conversion / initial-margin / withdrawal construct. It MUST NOT directly reduce another account's maintenance equity, and pure warmup release on unchanged `C_i`, `PNL_i`, and `fee_credits_i` MUST NOT by itself reduce `Eq_maint_raw_i`.
- strict risk-reducing buffer comparisons MUST use `Eq_maint_raw_i` (not `Eq_net_i`) so negative raw equity cannot be hidden by the outer `max(0, ·)` floor.

The signed quantities `Eq_init_base_i`, `Eq_init_raw_i`, and `Eq_maint_raw_i` MUST be computed in a transient widened signed type or an equivalent exact checked construction that preserves full mathematical ordering.

- Positive overflow of these exact widened intermediates is unreachable under the configured bounds and MUST fail conservatively if encountered.
- An implementation MAY project an exact negative value below `i128::MIN + 1` to `i128::MIN + 1` only for one-sided health checks that compare against `0` or another nonnegative threshold after the exact sign is already known.
- Such projection MUST NOT be used in any strict before/after raw maintenance-buffer comparison, including §10.5 step 29. Those comparisons MUST use the exact widened signed values without saturation or clamping.

### 3.5 Conservatism under pending A/K side effects and warmup

Because live haircut uses only matured positive PnL:

- pending positive mark / funding / ADL effects MUST NOT become initial-margin or withdrawal collateral until they are touched, reserved, and later warmed up according to §6
- on the touched generating account, local maintenance checks MAY use full local `PNL_i`, but only matured released positive PnL enters the global haircut denominator and only matured released positive PnL may be converted into principal via §7.4
- reserved fresh positive PnL MUST NOT enter another account's equity, the global haircut denominator, or any principal-conversion path before warmup release
- pending lazy ADL obligations MUST NOT be counted as backing in `Residual`

---

## 4. Canonical helpers

### 4.1 Checked scalar helpers

`checked_add_u128`, `checked_sub_u128`, `checked_add_i128`, `checked_sub_i128`, `checked_mul_u128`, `checked_mul_i128`, `checked_cast_i128`, and any equivalent low-level helper MUST either return the exact value or fail conservatively on overflow / underflow.

`checked_cast_i128(x)` means an exact cast from a bounded nonnegative integer to `i128`, or conservative failure if the cast would not fit.

### 4.2 `set_capital(i, new_C)`

When changing `C_i` from `old_C` to `new_C`, the engine MUST update `C_tot` by the signed delta in checked arithmetic and then set `C_i = new_C`.

### 4.3 `set_reserved_pnl(i, new_R)`

Preconditions:

- `new_R <= max(PNL_i, 0)`

Effects:

1. `old_pos = max(PNL_i, 0) as u128`
2. `old_rel = old_pos - R_i`
3. `new_rel = old_pos - new_R`
4. update `PNL_matured_pos_tot` by the exact delta from `old_rel` to `new_rel` using checked arithmetic
5. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`
6. set `R_i = new_R`

### 4.4 `set_pnl(i, new_PNL)`

When changing `PNL_i` from `old` to `new`, the engine MUST:

1. require `new != i128::MIN`
2. let `old_pos = max(old, 0) as u128`
3. let `old_R = R_i`
4. let `old_rel = old_pos - old_R`
5. let `new_pos = max(new, 0) as u128`
6. require `new_pos <= MAX_ACCOUNT_POSITIVE_PNL`
7. if `new_pos > old_pos`:
   - `reserve_add = new_pos - old_pos`
   - `new_R = checked_add_u128(old_R, reserve_add)`
   - require `new_R <= new_pos`
8. else:
   - `pos_loss = old_pos - new_pos`
   - `new_R = old_R.saturating_sub(pos_loss)`
   - require `new_R <= new_pos`
9. let `new_rel = new_pos - new_R`
10. update `PNL_pos_tot` by the exact delta from `old_pos` to `new_pos` using checked arithmetic
11. require resulting `PNL_pos_tot <= MAX_PNL_POS_TOT`
12. update `PNL_matured_pos_tot` by the exact delta from `old_rel` to `new_rel` using checked arithmetic
13. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`
14. set `PNL_i = new`
15. set `R_i = new_R`

**Caller obligation:** if `new_R > old_R`, the caller MUST invoke `restart_warmup_after_reserve_increase(i)` before returning from the routine that caused the positive-PnL increase.

### 4.4.1 `consume_released_pnl(i, x)`

This helper removes only matured released positive PnL and MUST leave `R_i` unchanged.

Preconditions:

- `x > 0`
- `x <= ReleasedPos_i`

Effects:

1. `old_pos = max(PNL_i, 0) as u128`
2. `old_R = R_i`
3. `old_rel = old_pos - old_R`
4. `new_pos = old_pos - x`
5. `new_rel = old_rel - x`
6. require `new_pos >= old_R`
7. update `PNL_pos_tot` by the exact delta from `old_pos` to `new_pos` using checked arithmetic
8. update `PNL_matured_pos_tot` by the exact delta from `old_rel` to `new_rel` using checked arithmetic
9. `PNL_i = checked_sub_i128(PNL_i, checked_cast_i128(x))`
10. require resulting `PNL_matured_pos_tot <= PNL_pos_tot`
11. leave `R_i` unchanged

This helper MUST be used for profit conversion. `set_pnl(i, PNL_i - x)` is non-compliant for that purpose because generic reserve-first loss ordering is intentionally reserved for market losses and other true PnL decreases, not for removing already-matured released profit.

### 4.5 `set_position_basis_q(i, new_basis_pos_q)`

When changing stored `basis_pos_q_i` from `old` to `new`, the engine MUST update `stored_pos_count_long` and `stored_pos_count_short` exactly once using the sign flags of `old` and `new`, then write `basis_pos_q_i = new`.

For a single logical position change, `set_position_basis_q` MUST be called exactly once with the final target. Passing through an intermediate zero value is not permitted.

### 4.6 `attach_effective_position(i, new_eff_pos_q)`

This helper MUST convert a current effective quantity into a new position basis at the current side state.

If the account currently has a nonzero same-epoch basis and this helper is about to discard that basis (by writing either `0` or a different nonzero basis), then the engine MUST first account for any orphaned unresolved same-epoch quantity remainder:

- let `s = side(basis_pos_q_i)`
- if `epoch_snap_i == epoch_s`, compute `rem = (abs(basis_pos_q_i) * A_s) mod a_basis_i` in exact arithmetic
- if `rem != 0`, invoke `inc_phantom_dust_bound(s)`

If `new_eff_pos_q == 0`, it MUST:

- `set_position_basis_q(i, 0)`
- reset snapshots to canonical zero-position defaults

If `new_eff_pos_q != 0`, it MUST:

- require `abs(new_eff_pos_q) <= MAX_POSITION_ABS_Q`
- `set_position_basis_q(i, new_eff_pos_q)`
- `a_basis_i = A_side(new_eff_pos_q)`
- `k_snap_i = K_side(new_eff_pos_q)`
- `epoch_snap_i = epoch_side(new_eff_pos_q)`

### 4.7 Phantom-dust helpers

- `inc_phantom_dust_bound(side)` increments `phantom_dust_bound_side_q` by exactly `1` q-unit using checked addition.
- `inc_phantom_dust_bound_by(side, amount_q)` increments `phantom_dust_bound_side_q` by exactly `amount_q` q-units using checked addition.

### 4.8 Exact math helpers (normative)

The engine MUST use the following exact helpers.

**Signed conservative floor division**

`floor_div_signed_conservative(n, d)`:

- require `d > 0`
- `q = trunc_toward_zero(n / d)`
- `r = n % d`
- if `n < 0` and `r != 0`, return `q - 1`
- else return `q`

**Positive checked ceiling division**

`ceil_div_positive_checked(n, d)`:

- require `d > 0`
- `q = n / d`
- `r = n % d`
- if `r != 0`, return checked(`q + 1`)
- else return `q`

**Exact multiply-divide floor for nonnegative inputs**

`mul_div_floor_u128(a, b, d)`:

- require `d > 0`
- compute the exact quotient `q = floor(a * b / d)`
- this MUST be exact even if the exact product `a * b` exceeds native `u128`
- require `q <= u128::MAX`
- return `q`

**Exact multiply-divide ceil for nonnegative inputs**

`mul_div_ceil_u128(a, b, d)`:

- require `d > 0`
- compute the exact quotient `q = ceil(a * b / d)`
- this MUST be exact even if the exact product `a * b` exceeds native `u128`
- require `q <= u128::MAX`
- return `q`

**Exact wide signed multiply-divide floor from K snapshots**

`wide_signed_mul_div_floor_from_k_pair(abs_basis_u128, k_then_i128, k_now_i128, den_u128)`:

- require `den_u128 > 0`
- compute the exact signed wide difference `k_diff = k_now_i128 - k_then_i128` in a transient wide signed type
- compute the exact wide magnitude `p = abs_basis_u128 * abs(k_diff)`
- let `q = floor(p / den_u128)`
- let `r = p mod den_u128`
- if `k_diff >= 0`, return `q` as positive `i128` (require representable)
- if `k_diff < 0`, return `-q` if `r == 0`, else return `-(q + 1)` to preserve mathematical floor semantics (require representable)

**Checked fee-debt conversion**

`fee_debt_u128_checked(fee_credits)`:

- require `fee_credits != i128::MIN`
- if `fee_credits >= 0`, return `0`
- else return `(-fee_credits) as u128`

**Saturating warmup multiply**

`saturating_mul_u128_u64(a, b)`:

- if `a == 0` or `b == 0`, return `0`
- if `a > u128::MAX / (b as u128)`, return `u128::MAX`
- else return `a * (b as u128)`

**Wide ADL quotient helper**

`wide_mul_div_ceil_u128_or_over_i128max(a, b, d)`:

- require `d > 0`
- compute the exact quotient `q = ceil(a * b / d)` in a transient wide type
- if `q > i128::MAX as u128`, return the tagged result `OverI128Magnitude`
- else return `Ok(q as u128)`

### 4.9 Warmup helpers

`restart_warmup_after_reserve_increase(i)` MUST:

1. if `T == 0`:
   - `set_reserved_pnl(i, 0)`
   - `w_slope_i = 0`
   - `w_start_i = current_slot`
   - return
2. if `R_i == 0`:
   - `w_slope_i = 0`
   - `w_start_i = current_slot`
   - return
3. set `w_slope_i = max(1, floor(R_i / T))`
4. set `w_start_i = current_slot`

`advance_profit_warmup(i)` MUST:

1. if `R_i == 0`:
   - `w_slope_i = 0`
   - `w_start_i = current_slot`
   - return
2. if `T == 0`:
   - `set_reserved_pnl(i, 0)`
   - `w_slope_i = 0`
   - `w_start_i = current_slot`
   - return
3. `elapsed = current_slot - w_start_i`
4. `release = min(R_i, saturating_mul_u128_u64(w_slope_i, elapsed))`
5. if `release > 0`, `set_reserved_pnl(i, R_i - release)`
6. if `R_i == 0`, set `w_slope_i = 0`
7. set `w_start_i = current_slot`

### 4.10 `charge_fee_to_insurance(i, fee_abs)`

Preconditions:

- `fee_abs <= MAX_PROTOCOL_FEE_ABS`

Effects:

1. `fee_paid = min(fee_abs, C_i)`
2. if `fee_paid > 0`:
   - `set_capital(i, C_i - fee_paid)`
   - `I = checked_add_u128(I, fee_paid)`
3. `fee_shortfall = fee_abs - fee_paid`
4. if `fee_shortfall > 0`:
   - `fee_credits_i = checked_sub_i128(fee_credits_i, fee_shortfall as i128)`

This helper MUST NOT mutate `PNL_i`, `PNL_pos_tot`, `PNL_matured_pos_tot`, or any `K_side`.

### 4.11 Insurance-loss helpers

`use_insurance_buffer(loss_abs)`:

1. precondition: `loss_abs > 0`
2. `available_I = I.saturating_sub(I_floor)`
3. `pay_I = min(loss_abs, available_I)`
4. `I = I - pay_I`
5. return `loss_abs - pay_I`

`record_uninsured_protocol_loss(loss_abs)`:

- precondition: `loss_abs > 0`
- no additional decrement to `V` or `I` occurs
- the uncovered loss remains represented as junior undercollateralization through `Residual` and `h`

`absorb_protocol_loss(loss_abs)`:

1. precondition: `loss_abs > 0`
2. `loss_rem = use_insurance_buffer(loss_abs)`
3. if `loss_rem > 0`, `record_uninsured_protocol_loss(loss_rem)`

### 4.12 Funding-rate recomputation helper

This revision defines a fully explicit consensus funding profile: the **zero-rate core profile**.

The engine MUST define the pure helper:

- `recompute_r_last_from_final_state()`

It MUST read only the final post-reset state of the current instruction and MUST store the next-interval rate for this revision as:

1. read the final post-reset state only; intermediate pre-reset state MUST be ignored
2. store `r_last = 0`

No other result is compliant in this revision.

Consequences:

- `|r_last| <= MAX_ABS_FUNDING_BPS_PER_SLOT` holds trivially
- repeated invocations are idempotent
- no compliant state transition in this revision can produce a nonzero `r_last`
- this revision has **no live funding-transfer branch**; any future nonzero-funding transfer arithmetic is outside the normative implementation surface of this document and MUST be introduced only by a future revision with a fully explicit formula and tests
- `fund_px_last` remains deterministic metadata and MUST equal the oracle price from the most recent successful `accrue_market_to`
---

## 5. Unified A/K side-index mechanics

### 5.1 Eager-equivalent event law

For one side of the book, a single eager global event on absolute fixed-point position `q_q >= 0` and realized PnL `p` has the form:

- `q_q' = α q_q`
- `p' = p + β * q_q / POS_SCALE`

where:

- `α ∈ [0, 1]` is the surviving-position fraction
- `β` is quote PnL per unit pre-event base position

The cumulative side indices compose as:

- `A_new = A_old * α`
- `K_new = K_old + A_old * β`

### 5.2 `effective_pos_q(i)`

For an account `i` with nonzero basis:

- let `s = side(basis_pos_q_i)`
- if `epoch_snap_i != epoch_s`, then `effective_pos_q(i) = 0` for current-market risk purposes until the account is touched and zeroed
- else `effective_abs_pos_q(i) = mul_div_floor_u128(abs(basis_pos_q_i) as u128, A_s, a_basis_i)`
- `effective_pos_q(i) = sign(basis_pos_q_i) * effective_abs_pos_q(i)`

If `basis_pos_q_i == 0`, define `effective_pos_q(i) = 0`.

### 5.2.1 Side-OI components of a signed effective position

For any signed fixed-point position `q` in q-units:

- `OI_long_component(q) = max(q, 0) as u128`
- `OI_short_component(q) = max(-q, 0) as u128`

Because every reachable effective position satisfies `|q| <= MAX_POSITION_ABS_Q < i128::MAX`, both casts are exact.

### 5.2.2 Exact bilateral trade side-OI after-values

For a bilateral trade with pre-trade effective positions `old_eff_pos_q_a`, `old_eff_pos_q_b` and candidate post-trade effective positions `new_eff_pos_q_a`, `new_eff_pos_q_b`, define:

- `old_long_a = OI_long_component(old_eff_pos_q_a)`
- `old_short_a = OI_short_component(old_eff_pos_q_a)`
- `old_long_b = OI_long_component(old_eff_pos_q_b)`
- `old_short_b = OI_short_component(old_eff_pos_q_b)`
- `new_long_a = OI_long_component(new_eff_pos_q_a)`
- `new_short_a = OI_short_component(new_eff_pos_q_a)`
- `new_long_b = OI_long_component(new_eff_pos_q_b)`
- `new_short_b = OI_short_component(new_eff_pos_q_b)`

Then the exact candidate side-OI after-values are:

- `OI_long_after_trade = (((OI_eff_long - old_long_a) - old_long_b) + new_long_a) + new_long_b`
- `OI_short_after_trade = (((OI_eff_short - old_short_a) - old_short_b) + new_short_a) + new_short_b`

All arithmetic above MUST use the checked helpers of §4.1.

A trade would increase net side OI on the long side iff `OI_long_after_trade > OI_eff_long`, and analogously for the short side.

When §10.5 uses these candidate after-values, the same exact `OI_long_after_trade` and `OI_short_after_trade` computed for constrained-side gating MUST later be written to `OI_eff_long` and `OI_eff_short`; heuristic reopen tests or alternate decompositions are non-compliant.

### 5.3 `settle_side_effects(i)`

When touching account `i`:

1. if `basis_pos_q_i == 0`, return immediately
2. let `s = side(basis_pos_q_i)`
3. let `den = checked_mul_u128(a_basis_i, POS_SCALE)`
4. if `epoch_snap_i == epoch_s` (same epoch):
   - `q_eff_new = mul_div_floor_u128(abs(basis_pos_q_i) as u128, A_s, a_basis_i)`
   - record `old_R = R_i`
   - `pnl_delta = wide_signed_mul_div_floor_from_k_pair(abs(basis_pos_q_i) as u128, k_snap_i, K_s, den)`
   - `set_pnl(i, checked_add_i128(PNL_i, pnl_delta))`
   - if `R_i > old_R`, invoke `restart_warmup_after_reserve_increase(i)`
   - if `q_eff_new == 0`:
     - `inc_phantom_dust_bound(s)`
     - `set_position_basis_q(i, 0)`
     - reset snapshots to canonical zero-position defaults
   - else:
     - leave `basis_pos_q_i` and `a_basis_i` unchanged
     - set `k_snap_i = K_s`
     - set `epoch_snap_i = epoch_s`
5. else (epoch mismatch):
   - require `mode_s == ResetPending`
   - require `epoch_snap_i + 1 == epoch_s`
   - record `old_R = R_i`
   - `pnl_delta = wide_signed_mul_div_floor_from_k_pair(abs(basis_pos_q_i) as u128, k_snap_i, K_epoch_start_s, den)`
   - `set_pnl(i, checked_add_i128(PNL_i, pnl_delta))`
   - if `R_i > old_R`, invoke `restart_warmup_after_reserve_increase(i)`
   - `set_position_basis_q(i, 0)`
   - decrement `stale_account_count_s` using checked subtraction
   - reset snapshots to canonical zero-position defaults

The `epoch_snap_i + 1 == epoch_s` precondition is justified by the invariant of §2.8.1; a larger gap is non-compliant state corruption.

### 5.4 `accrue_market_to(now_slot, oracle_price)`

Before any operation that depends on current market state, the engine MUST call `accrue_market_to(now_slot, oracle_price)`.

This helper intentionally uses the validated oracle-price sample as both:

- the mark-to-market price sample (`P_last`), and
- the deterministic funding-basis metadata sample (`fund_px_last`).

Under the zero-rate core profile of §4.12, `fund_px_last` is metadata only; no funding transfer is applied in this revision.

This helper MUST:

1. require trusted `now_slot >= slot_last`
2. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
3. snapshot `OI_long_0 = OI_eff_long` and `OI_short_0 = OI_eff_short`
4. compute signed one-shot `ΔP = (oracle_price as i128) - (P_last as i128)`
5. apply mark-to-market exactly once using the snapped side state:
   - if `OI_long_0 > 0`, `K_long = checked_add_i128(K_long, (A_long * ΔP))`
   - if `OI_short_0 > 0`, `K_short = checked_sub_i128(K_short, (A_short * ΔP))`
6. apply no funding transfer in this revision; `r_last` is always `0` under §4.12
7. update `slot_last = now_slot`
8. update `P_last = oracle_price`
9. update `fund_px_last = oracle_price`

Any future nonzero-funding revision MUST replace step 6 with a fully explicit normative funding-transfer rule.

### 5.5 Funding anti-retroactivity

Each standard-lifecycle instruction of §10 MUST invoke `recompute_r_last_from_final_state()` exactly once and only after any end-of-instruction reset handling specified by that instruction.

In this revision the helper always stores `0`, so the ordering has no economic effect beyond deterministic state shape. Any future nonzero-funding revision MUST preserve the same post-reset recomputation ordering when it introduces live funding transfers.

### 5.6 `enqueue_adl(ctx, liq_side, q_close_q, D)`

Suppose a bankrupt liquidation from side `liq_side` leaves an uncovered deficit `D >= 0` after the liquidated account's principal and realized PnL have been exhausted. `q_close_q` is the fixed-point base quantity removed from the liquidated side and MAY be zero.

Let `opp = opposite(liq_side)`.

This helper MUST perform the following in order:

1. if `q_close_q > 0`, decrement `OI_eff_liq_side` by `q_close_q` using checked subtraction
2. if `D > 0`, set `D_rem = use_insurance_buffer(D)`; else define `D_rem = 0`
3. read `OI = OI_eff_opp`
4. if `OI == 0`:
   - if `D_rem > 0`, `record_uninsured_protocol_loss(D_rem)`
   - if `OI_eff_liq_side == 0`, set both `ctx.pending_reset_liq_side = true` and `ctx.pending_reset_opp = true`
   - return
5. if `OI > 0` and `stored_pos_count_opp == 0`:
   - require `q_close_q <= OI`
   - let `OI_post = OI - q_close_q`
   - if `D_rem > 0`, `record_uninsured_protocol_loss(D_rem)`
   - set `OI_eff_opp = OI_post`
   - if `OI_post == 0`:
     - set `ctx.pending_reset_opp = true`
     - if `OI_eff_liq_side == 0`, set `ctx.pending_reset_liq_side = true`
   - return
6. otherwise (`OI > 0` and `stored_pos_count_opp > 0`):
   - require `q_close_q <= OI`
   - `A_old = A_opp`
   - `OI_post = OI - q_close_q`
7. if `D_rem > 0`:
   - let `adl_scale = checked_mul_u128(A_old, POS_SCALE)`
   - compute `delta_K_abs_result = wide_mul_div_ceil_u128_or_over_i128max(D_rem, adl_scale, OI)`
   - if `delta_K_abs_result == OverI128Magnitude`, `record_uninsured_protocol_loss(D_rem)`
   - else:
     - `delta_K_abs = unwrap(delta_K_abs_result)`
     - `delta_K_exact = -(delta_K_abs as i128)`
     - if `checked_add_i128(K_opp, delta_K_exact)` fails, `record_uninsured_protocol_loss(D_rem)`
     - else `K_opp = K_opp + delta_K_exact`
8. if `OI_post == 0`:
   - set `OI_eff_opp = 0`
   - set `ctx.pending_reset_opp = true`
   - if `OI_eff_liq_side == 0`, set `ctx.pending_reset_liq_side = true`
   - return
9. compute `A_prod_exact = checked_mul_u128(A_old, OI_post)`
10. `A_candidate = floor(A_prod_exact / OI)`
11. `A_trunc_rem = A_prod_exact mod OI`
12. if `A_candidate > 0`:
   - set `A_opp = A_candidate`
   - set `OI_eff_opp = OI_post`
   - if `A_trunc_rem != 0`:
     - `N_opp = stored_pos_count_opp as u128`
     - `global_a_dust_bound = checked_add_u128(N_opp, ceil_div_positive_checked(checked_add_u128(OI, N_opp), A_old))`
     - `inc_phantom_dust_bound_by(opp, global_a_dust_bound)`
   - if `A_opp < MIN_A_SIDE`, set `mode_opp = DrainOnly`
   - return
13. if `A_candidate == 0` while `OI_post > 0`, enter the precision-exhaustion terminal drain:
   - set `OI_eff_opp = 0`
   - set `OI_eff_liq_side = 0`
   - set both pending-reset flags true

Normative intent:

- Real bankruptcy losses MUST first consume the Insurance Fund down to `I_floor`.
- Only the remaining `D_rem` MAY be socialized through `K_opp` or left as junior undercollateralization.
- Quantity socialization MUST never assert-fail due to `A_side` rounding to zero.
- If `enqueue_adl` drives a side's authoritative `OI_eff_side` to `0`, that side MUST enter the reset lifecycle before any further live-OI-dependent processing, even when the liquidated side remains live.
- Under the maintained invariant `OI_eff_long == OI_eff_short` at `enqueue_adl` entry, the nested `if OI_eff_liq_side == 0` guards in steps 4, 5, and 8 are currently tautological whenever the enclosing branch has already driven the opposing side to `0`. They are retained as defensive structure and do not change reachable behavior in this revision.
- Real quote deficits MUST NOT be written into `K_opp` when there are no opposing stored positions left to realize that K change.

### 5.7 End-of-instruction reset handling

The engine MUST provide both:

- `schedule_end_of_instruction_resets(ctx)`
- `finalize_end_of_instruction_resets(ctx)`

`schedule_end_of_instruction_resets(ctx)` MUST be called exactly once at the end of each top-level instruction that can touch accounts, mutate side state, or liquidate.

It MUST perform the following in order:

1. **Bilateral-empty dust clearance**
   - if `stored_pos_count_long == 0` and `stored_pos_count_short == 0`:
     - `clear_bound_q = checked_add_u128(phantom_dust_bound_long_q, phantom_dust_bound_short_q)`
     - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0) or (phantom_dust_bound_short_q > 0)`
     - if `has_residual_clear_work`:
       - require `OI_eff_long == OI_eff_short`
       - if `OI_eff_long <= clear_bound_q` and `OI_eff_short <= clear_bound_q`:
         - set `OI_eff_long = 0`
         - set `OI_eff_short = 0`
         - set both pending-reset flags true
       - else fail conservatively
2. **Unilateral-empty dust clearance (long empty)**
   - else if `stored_pos_count_long == 0` and `stored_pos_count_short > 0`:
     - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_long_q > 0)`
     - if `has_residual_clear_work`:
       - require `OI_eff_long == OI_eff_short`
       - if `OI_eff_long <= phantom_dust_bound_long_q`:
         - set `OI_eff_long = 0`
         - set `OI_eff_short = 0`
         - set both pending-reset flags true
       - else fail conservatively
3. **Unilateral-empty dust clearance (short empty)**
   - else if `stored_pos_count_short == 0` and `stored_pos_count_long > 0`:
     - `has_residual_clear_work = (OI_eff_long > 0) or (OI_eff_short > 0) or (phantom_dust_bound_short_q > 0)`
     - if `has_residual_clear_work`:
       - require `OI_eff_long == OI_eff_short`
       - if `OI_eff_short <= phantom_dust_bound_short_q`:
         - set `OI_eff_long = 0`
         - set `OI_eff_short = 0`
         - set both pending-reset flags true
       - else fail conservatively
4. **DrainOnly zero-OI reset scheduling**
   - if `mode_long == DrainOnly and OI_eff_long == 0`, set `ctx.pending_reset_long = true`
   - if `mode_short == DrainOnly and OI_eff_short == 0`, set `ctx.pending_reset_short = true`

`finalize_end_of_instruction_resets(ctx)` MUST:

1. if `ctx.pending_reset_long` and `mode_long != ResetPending`, invoke `begin_full_drain_reset(long)`
2. if `ctx.pending_reset_short` and `mode_short != ResetPending`, invoke `begin_full_drain_reset(short)`
3. if `mode_long == ResetPending` and `OI_eff_long == 0` and `stale_account_count_long == 0` and `stored_pos_count_long == 0`, invoke `finalize_side_reset(long)`
4. if `mode_short == ResetPending` and `OI_eff_short == 0` and `stale_account_count_short == 0` and `stored_pos_count_short == 0`, invoke `finalize_side_reset(short)`

Once either pending-reset flag becomes true during a top-level instruction, that instruction MUST NOT perform any additional account touches, liquidations, or explicit position mutations that rely on live authoritative OI. It MUST proceed directly to end-of-instruction reset handling after finishing any already-started local bookkeeping that does not read or mutate live side exposure.

---

## 6. Warmup and matured-profit release

### 6.1 Parameter

- `T = warmup_period_slots`
- if `T == 0`, warmup is instantaneous

### 6.2 Semantics of `R_i`

`R_i` is the reserved portion of positive `PNL_i` that has not yet matured through warmup.

- `ReleasedPos_i = max(PNL_i, 0) - R_i`
- Only `ReleasedPos_i` contributes to `PNL_matured_pos_tot`, to live haircut, to `Eq_init_net_i`, and to profit conversion
- Reserved fresh positive PnL in `R_i` MAY contribute only to the generating account's maintenance checks
- `Eq_maint_raw_i` uses full local `PNL_i` on the touched generating account, so pure changes in composition between `ReleasedPos_i` and `R_i` do not by themselves change maintenance equity
- Fresh positive PnL MUST enter `R_i` first by the automatic reserve-increase rule in `set_pnl`

### 6.3 Warmup progress

Touched accounts MUST call `advance_profit_warmup(i)` before any logic that depends on current released positive PnL in that touch.

This helper releases previously reserved positive PnL according to the current slope and elapsed slots but never grants newly added reserve any retroactive maturity.

### 6.4 Anti-retroactivity

When `set_pnl` increases `R_i`, the caller MUST immediately invoke `restart_warmup_after_reserve_increase(i)`. This resets `w_start_i = current_slot` and recomputes `w_slope_i` from the new reserve, so newly generated profit cannot inherit old dormant maturity headroom.

### 6.5 Release slope preservation

When reserve decreases only because of `advance_profit_warmup(i)`, the engine MUST preserve the existing `w_slope_i` for the remaining reserve (unless the reserve reaches zero). This prevents repeated touches from creating exponential-decay maturity.

---

## 7. Loss settlement, flat-loss resolution, profit conversion, and fee-debt sweep

### 7.1 `settle_losses_from_principal(i)`

If `PNL_i < 0`, the engine MUST immediately attempt to settle from principal:

1. require `PNL_i != i128::MIN`
2. record `old_R = R_i`
3. `need = (-PNL_i) as u128`
4. `pay = min(need, C_i)`
5. apply:
   - `set_capital(i, C_i - pay)`
   - `set_pnl(i, checked_add_i128(PNL_i, pay as i128))`

Because `pay <= need = -PNL_i_before`, the post-write `PNL_i_after = PNL_i_before + pay` lies in `[PNL_i_before, 0]`. Therefore `max(PNL_i_after, 0) = 0`, no reserve can be added, and the helper MUST leave `R_i` unchanged. Implementations SHOULD assert `R_i == old_R` after the helper.

### 7.2 Open-position negative remainder

If after §7.1:

- `PNL_i < 0`, and
- `effective_pos_q(i) != 0`

then the account MUST remain liquidatable. It MUST NOT be silently zeroed or routed through flat-account loss absorption.

### 7.3 Flat-account negative remainder

If after §7.1:

- `PNL_i < 0`, and
- `effective_pos_q(i) == 0`

then the engine MUST:

1. call `absorb_protocol_loss((-PNL_i) as u128)`
2. `set_pnl(i, 0)`

This path is allowed only for truly flat accounts whose current-state side effects are already locally authoritative through `touch_account_full` or an equivalent already-touched liquidation subroutine. A pure `deposit` path that does not call `accrue_market_to` and does not make new current-state side effects authoritative MUST NOT invoke this path.

### 7.4 Profit conversion

Profit conversion removes matured released profit and converts only its haircutted backed portion into protected principal.

In this specification's automatic touch flow, this helper is invoked only on touched states with `basis_pos_q_i == 0`. Open-position accounts that want to voluntarily realize matured profit without closing may instead use the explicit `convert_released_pnl` instruction of §10.4.1.

On an eligible touched state, define `x = ReleasedPos_i`. If `x == 0`, do nothing.

Compute `y` using the pre-conversion haircut ratio from §3:

- because `x > 0` implies `PNL_matured_pos_tot > 0`, define `y = mul_div_floor_u128(x, h_num, h_den)`

Apply:

1. `consume_released_pnl(i, x)`
2. `set_capital(i, checked_add_u128(C_i, y))`
3. if `R_i == 0`:
   - `w_slope_i = 0`
   - `w_start_i = current_slot`
4. else leave the existing warmup schedule unchanged

Profit conversion MUST NOT reduce `R_i`. Any still-reserved warmup balance remains reserved and continues to mature only through §6.

### 7.5 Fee-debt sweep

After any operation that increases `C_i`, the engine MUST pay down fee debt as soon as that newly available capital is no longer senior-encumbered by all higher-seniority trading losses already attached to the account's locally authoritative state.

This means:

- sweep MUST occur immediately after profit conversion, because the conversion created new capital and the touched account's current-state trading losses have already been settled
- sweep MUST occur in `deposit` only after `settle_losses_from_principal`, and only when `basis_pos_q_i == 0` and `PNL_i >= 0`
- on a truly flat authoritative state, zero or positive `PNL_i` does not senior-encumber newly available capital; only a surviving negative `PNL_i` blocks the sweep
- a pure `deposit` into an account with `basis_pos_q_i != 0` MUST defer fee-debt sweep until a later full current-state touch, because unresolved A/K side effects are still senior to protocol fee collection from that capital
- sweep MUST NOT be deferred across instructions once capital is both present and no longer senior-encumbered
- a direct external repayment through `deposit_fee_credits` (§10.3.1) is **not** a capital sweep and does not pass through `C_i`; it directly increases `I` and reduces `fee_credits_i`

The sweep is:

1. `debt = fee_debt_u128_checked(fee_credits_i)`
2. `pay = min(debt, C_i)`
3. if `pay > 0`:
   - `set_capital(i, C_i - pay)`
   - `fee_credits_i = checked_add_i128(fee_credits_i, pay as i128)`
   - `I = checked_add_u128(I, pay)`

---

## 8. Fees

This revision has no separate `fee_revenue` bucket. All explicit fee collections and direct fee-credit repayments accrue into `I`.

### 8.1 Trading fees

Trading fees are explicit transfers to insurance and MUST NOT be socialized through `h` or `D`.

Define:

- `fee = mul_div_ceil_u128(trade_notional, trading_fee_bps, 10_000)`

with `0 <= trading_fee_bps <= MAX_TRADING_FEE_BPS`.

Rules:

- if `trading_fee_bps == 0` or `trade_notional == 0`, then `fee = 0`
- if `trading_fee_bps > 0` and `trade_notional > 0`, then `fee >= 1`

The fee MUST be charged using `charge_fee_to_insurance(i, fee)`.

Deployment guidance: even though the strict risk-reducing trade exemption of §10.5 now holds the explicit fee of the candidate trade constant for the before/after buffer comparison, high trading fees still worsen the actual post-trade state. Deployments that want voluntary partial de-risking to remain broadly usable SHOULD configure `trading_fee_bps` materially below `maintenance_bps`.

### 8.2 Account-local recurring maintenance fees (disabled in this revision)

Recurring account-local maintenance fees are disabled in this revision.

Normative consequences:

1. Implementations MUST NOT realize any recurring account-local maintenance fee.
2. No wall-clock passage may create fee debt except through explicitly specified protocol-fee entrypoints.
3. `last_fee_slot_i` remains deterministic metadata only.
4. `touch_account_full` MUST stamp `last_fee_slot_i = current_slot` so the reserved field remains monotonic and deterministic, but this stamp has no economic effect.
5. Any implementation-defined recurring maintenance-fee accumulator, event-segmentation method, or fee realization path is non-compliant with this revision.

### 8.3 Liquidation fees

The protocol MUST define:

- `liquidation_fee_bps` with `0 <= liquidation_fee_bps <= MAX_LIQUIDATION_FEE_BPS`
- `liquidation_fee_cap` with `0 <= liquidation_fee_cap <= MAX_PROTOCOL_FEE_ABS`
- `min_liquidation_abs` with `0 <= min_liquidation_abs <= liquidation_fee_cap`

For a liquidation that closes `q_close_q` at `oracle_price`, define:

- if `q_close_q == 0`, then `liq_fee = 0`
- else:
  - `closed_notional = mul_div_floor_u128(q_close_q, oracle_price, POS_SCALE)`
  - `liq_fee_raw = mul_div_ceil_u128(closed_notional, liquidation_fee_bps, 10_000)`
  - `liq_fee = min(max(liq_fee_raw, min_liquidation_abs), liquidation_fee_cap)`

The short-circuit is on `q_close_q`, not `closed_notional`. Therefore the minimum fee floor applies even when `closed_notional` floors to zero.

### 8.4 Fee debt as margin liability

`FeeDebt_i = fee_debt_u128_checked(fee_credits_i)`:

- MUST reduce `Eq_maint_raw_i`, `Eq_net_i`, `Eq_init_raw_i`, and therefore also the derived `Eq_init_net_i`
- MUST be swept whenever principal becomes available and is no longer senior-encumbered by already-realized trading losses on the same local state
- MUST NOT directly change `Residual`, `PNL_pos_tot`, or `PNL_matured_pos_tot`

---

## 9. Margin checks and liquidation

### 9.1 Margin requirements

After `touch_account_full(i, oracle_price, now_slot)`, define:

- `Notional_i = mul_div_floor_u128(abs(effective_pos_q(i)), oracle_price, POS_SCALE)`
- if `effective_pos_q(i) == 0`:
  - `MM_req_i = 0`
  - `IM_req_i = 0`
- else:
  - `MM_req_i = max(mul_div_floor_u128(Notional_i, maintenance_bps, 10_000), MIN_NONZERO_MM_REQ)`
  - `IM_req_i = max(mul_div_floor_u128(Notional_i, initial_bps, 10_000), MIN_NONZERO_IM_REQ)`

Healthy conditions:

- maintenance healthy if `Eq_net_i > MM_req_i as i128`
- initial-margin healthy if exact `Eq_init_raw_i >= (IM_req_i as wide_signed)` in the widened signed domain of §3.4

These absolute nonzero-position floors are a finite-capacity liveness safeguard. A microscopic open position MUST NOT evade both initial-margin and maintenance enforcement solely because proportional notional floors to zero.

### 9.2 Risk-increasing and strict risk-reducing trades

A trade for account `i` is **risk-increasing** when either:

1. `abs(new_eff_pos_q_i) > abs(old_eff_pos_q_i)`, or
2. the position sign flips across zero, or
3. `old_eff_pos_q_i == 0` and `new_eff_pos_q_i != 0`

A trade is **strictly risk-reducing** when:

- `old_eff_pos_q_i != 0`
- `new_eff_pos_q_i != 0`
- `sign(new_eff_pos_q_i) == sign(old_eff_pos_q_i)`
- `abs(new_eff_pos_q_i) < abs(old_eff_pos_q_i)`

### 9.3 Liquidation eligibility

An account is liquidatable when after a full `touch_account_full`:

- `effective_pos_q(i) != 0`, and
- `Eq_net_i <= MM_req_i as i128`

### 9.4 Partial liquidation

A liquidation MAY be partial only if it closes a strictly positive quantity smaller than the full remaining effective position:

- `0 < q_close_q < abs(old_eff_pos_q_i)`

A successful partial liquidation MUST:

1. use the current touched state
2. let `old_eff_pos_q_i = effective_pos_q(i)` and require `old_eff_pos_q_i != 0`
3. determine `liq_side = side(old_eff_pos_q_i)`
4. define `new_eff_abs_q = checked_sub_u128(abs(old_eff_pos_q_i), q_close_q)`
5. require `new_eff_abs_q > 0`
6. define `new_eff_pos_q_i = sign(old_eff_pos_q_i) * (new_eff_abs_q as i128)`
7. close `q_close_q` synthetically at `oracle_price` with zero execution-price slippage
8. apply the resulting position using `attach_effective_position(i, new_eff_pos_q_i)`
9. settle realized losses from principal via §7.1
10. compute `liq_fee` per §8.3 on the quantity actually closed
11. charge that fee using `charge_fee_to_insurance(i, liq_fee)`
12. invoke `enqueue_adl(ctx, liq_side, q_close_q, 0)` to decrease global OI and socialize quantity reduction
13. if either pending-reset flag becomes true in `ctx`, stop any further live-OI-dependent checks or mutations; only the remaining local post-step validation of step 14 may still run before end-of-instruction reset handling
14. require the resulting nonzero position to be maintenance healthy on the current post-step-12 state, i.e. recompute `Notional_i`, `MM_req_i`, `Eq_maint_raw_i`, and `Eq_net_i` from that current local state and require maintenance health under §9.1

The step-14 health check is a purely local post-partial validation and MUST still be evaluated even when step 13 has scheduled a pending reset. It uses only the post-step local maintenance quantities and oracle price; it does not depend on the matured-profit haircut ratio `h` or on any further live-OI mutation after `enqueue_adl`.

### 9.5 Full-close / bankruptcy liquidation

The engine MUST be able to perform a deterministic full-close liquidation on an already-touched liquidatable account. When the resulting post-close state leaves uncovered negative `PNL_i` after principal exhaustion and liquidation fees, that uncovered amount is the bankruptcy deficit handled below.

Full-close liquidation is a local subroutine on the current touched state. It MUST NOT call `touch_account_full` again.

It MUST:

1. use the current touched state
2. let `old_eff_pos_q_i = effective_pos_q(i)` and require `old_eff_pos_q_i != 0`
3. set `q_close_q = abs(old_eff_pos_q_i)`; full-close liquidation MUST strictly close the full remaining effective position
4. let `liq_side = side(old_eff_pos_q_i)`
5. because the close is synthetic, it MUST execute exactly at `oracle_price` with zero execution-price slippage
6. `attach_effective_position(i, 0)`
7. `OI_eff_liq_side` MUST NOT be decremented anywhere except through `enqueue_adl`
8. `settle_losses_from_principal(i)`
9. compute `liq_fee` per §8.3 and charge it via `charge_fee_to_insurance(i, liq_fee)`
10. determine the uncovered bankruptcy deficit `D`:
    - if `PNL_i < 0`, let `D = (-PNL_i) as u128`
    - else `D = 0`
11. if `q_close_q > 0` or `D > 0`, invoke `enqueue_adl(ctx, liq_side, q_close_q, D)`
12. if `D > 0`, `set_pnl(i, 0)`

### 9.6 Side-mode gating

Before any top-level instruction rejects an OI-increasing operation because a side is in `ResetPending`, it MUST first invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`.

Any operation that would increase net side OI on a side whose mode is `DrainOnly` or `ResetPending` MUST be rejected.

For `execute_trade`, this prospective check MUST use the exact bilateral candidate after-values of §5.2.2 on both sides. Open-only heuristics, single-account approximations, or any decomposition other than §5.2.2 are non-compliant.

---

## 10. External operations

### 10.0 Standard instruction lifecycle

Unless explicitly noted otherwise (for example `deposit`, `deposit_fee_credits`, `top_up_insurance_fund`, and `reclaim_empty_account`), an external state-mutating operation that accepts `oracle_price` and `now_slot` executes inside the same standard lifecycle:

1. validate trusted monotonic slot inputs and the validated oracle input required by that endpoint
2. initialize a fresh instruction context `ctx`
3. perform the endpoint's exact current-state inner execution
4. call `schedule_end_of_instruction_resets(ctx)` exactly once
5. call `finalize_end_of_instruction_resets(ctx)` exactly once
6. recompute `r_last` exactly once from the final post-reset state
7. if the instruction can mutate live side exposure, assert `OI_eff_long == OI_eff_short` at the end

This subsection is a condensation aid only. The endpoint subsections below remain the normative source of truth for exact call ordering, including any endpoint-specific exceptions or additional guards.

### 10.1 `touch_account_full(i, oracle_price, now_slot)`

Canonical settle routine for an existing materialized account. It MUST perform, in order:

1. require account `i` is materialized
2. require trusted `now_slot >= current_slot`
3. require trusted `now_slot >= slot_last`
4. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
5. set `current_slot = now_slot`
6. call `accrue_market_to(now_slot, oracle_price)`
7. call `advance_profit_warmup(i)`
8. call `settle_side_effects(i)`
9. call `settle_losses_from_principal(i)`
10. if `effective_pos_q(i) == 0` and `PNL_i < 0`, resolve uncovered flat loss via §7.3
11. set `last_fee_slot_i = current_slot`
12. if `basis_pos_q_i == 0`, convert matured released profits via §7.4
13. sweep fee debt per §7.5

`touch_account_full` MUST NOT itself begin a side reset.

### 10.2 `settle_account(i, oracle_price, now_slot)`

Standalone settle wrapper for an existing account.

Procedure:

1. initialize fresh instruction context `ctx`
2. `touch_account_full(i, oracle_price, now_slot)`
3. `schedule_end_of_instruction_resets(ctx)`
4. `finalize_end_of_instruction_resets(ctx)`
5. recompute `r_last` exactly once from the final post-reset state

This wrapper MUST NOT materialize a missing account.

### 10.3 `deposit(i, amount, now_slot)`

`deposit` is a pure capital-transfer instruction. It MUST NOT call `accrue_market_to`, MUST NOT mutate side state, and MUST NOT auto-touch unrelated accounts.

A pure deposit does **not** make unresolved A/K side effects locally authoritative. Therefore, for an account with `basis_pos_q_i != 0`, the deposit path MUST NOT treat the account as truly flat and MUST NOT sweep fee debt, because unresolved current-side trading losses remain senior until a later full current-state touch.

A pure deposit also MUST NOT decrement `I` or record uninsured protocol loss. Therefore, even on a currently flat stored state, if negative PnL remains after principal settlement the deposit path MUST leave that remainder in `PNL_i` for a later full accrued touch.

Procedure:

1. require trusted `now_slot >= current_slot`
2. if account `i` is missing:
   - require `amount >= MIN_INITIAL_DEPOSIT`
   - `materialize_account(i, now_slot)`
3. set `current_slot = now_slot`
4. require `checked_add_u128(V, amount) <= MAX_VAULT_TVL`
5. set `V = V + amount`
6. `set_capital(i, checked_add_u128(C_i, amount))`
7. `settle_losses_from_principal(i)`
8. MUST NOT invoke §7.3 or otherwise decrement `I`
9. if `basis_pos_q_i == 0` and `PNL_i >= 0`, sweep fee debt via §7.5

Because `deposit` cannot mutate OI, stored positions, stale-account counts, phantom-dust bounds, or side modes, it MAY omit §§5.7 end-of-instruction reset handling.

### 10.3.1 `deposit_fee_credits(i, amount, now_slot)`

`deposit_fee_credits` is a direct external repayment of account-local fee debt. It is **not** a capital deposit, does **not** pass through `C_i`, and therefore does not subordinate trading losses.

Procedure:

1. require account `i` is materialized
2. require trusted `now_slot >= current_slot`
3. set `current_slot = now_slot`
4. let `debt = fee_debt_u128_checked(fee_credits_i)`
5. let `pay = min(amount, debt)`
6. if `pay == 0`, return
7. require `checked_add_u128(V, pay) <= MAX_VAULT_TVL`
8. set `V = V + pay`
9. set `I = checked_add_u128(I, pay)`
10. set `fee_credits_i = checked_add_i128(fee_credits_i, pay as i128)`
11. require `fee_credits_i <= 0`

Normative consequences:

- the externally accounted repayment amount is exactly `pay`, not the user-specified `amount`
- any over-request above the outstanding debt is silently capped and MUST NOT create positive `fee_credits_i`
- the instruction MUST NOT call `accrue_market_to`
- the instruction MUST NOT mutate side state, `C_i`, `PNL_i`, `R_i`, or any aggregate other than `V` and `I`

### 10.3.2 `top_up_insurance_fund(amount, now_slot)`

`top_up_insurance_fund` is a direct external addition to the Insurance Fund and the vault. It does not credit any account principal.

Procedure:

1. require trusted `now_slot >= current_slot`
2. set `current_slot = now_slot`
3. require `checked_add_u128(V, amount) <= MAX_VAULT_TVL`
4. set `V = V + amount`
5. set `I = checked_add_u128(I, amount)`

This instruction MUST NOT call `accrue_market_to`, MUST NOT mutate any account-local state, and MUST NOT mutate side state.

### 10.4 `withdraw(i, amount, oracle_price, now_slot)`

The minimum live-balance dust floor applies to **all** withdrawals, not only truly flat ones. This is a finite-capacity liveness safeguard: a temporary dust position MUST NOT be able to bypass the floor and then return to a flat unreclaimable sub-`MIN_INITIAL_DEPOSIT` account.

Procedure:

1. require account `i` is materialized
2. initialize fresh instruction context `ctx`
3. `touch_account_full(i, oracle_price, now_slot)`
4. require `amount <= C_i`
5. require the post-withdraw capital `C_i - amount` is either `0` or `>= MIN_INITIAL_DEPOSIT`
6. if `effective_pos_q(i) != 0`, require post-withdraw initial-margin health on the hypothetical post-withdraw state where:
   - `C_i' = C_i - amount`
   - `V' = V - amount`
   - exact `Eq_init_raw_i` is recomputed from that hypothetical state and compared against `IM_req_i` in the widened signed domain of §3.4
   - all other touched-state quantities are unchanged
   - equivalently, because both `V` and `C_tot` decrease by the same `amount`, `Residual` and `h` are unchanged by the simulation
7. apply:
   - `set_capital(i, C_i - amount)`
   - `V = V - amount`
8. `schedule_end_of_instruction_resets(ctx)`
9. `finalize_end_of_instruction_resets(ctx)`
10. recompute `r_last` exactly once from the final post-reset state

### 10.4.1 `convert_released_pnl(i, x_req, oracle_price, now_slot)`

Explicit voluntary conversion of matured released positive PnL for an account that still has an open position.

This instruction exists because ordinary `touch_account_full` auto-conversion is intentionally flat-only. It allows a user with an open position to realize matured profit into protected principal on current state, accept the resulting maintenance-equity change on their own terms, and immediately sweep any outstanding fee debt from the new capital.

Procedure:

1. require account `i` is materialized
2. initialize fresh instruction context `ctx`
3. `touch_account_full(i, oracle_price, now_slot)`
4. if `basis_pos_q_i == 0`:
   - the ordinary touch flow has already auto-converted any released profit eligible on the now-flat state
   - `schedule_end_of_instruction_resets(ctx)`
   - `finalize_end_of_instruction_resets(ctx)`
   - recompute `r_last` exactly once from the final post-reset state
   - return
5. require `0 < x_req <= ReleasedPos_i`
6. compute `y` using the same pre-conversion haircut rule as §7.4:
   - because `x_req > 0` implies `PNL_matured_pos_tot > 0`, define `y = mul_div_floor_u128(x_req, h_num, h_den)`
7. `consume_released_pnl(i, x_req)`
8. `set_capital(i, checked_add_u128(C_i, y))`
9. sweep fee debt per §7.5
10. require the current post-step-9 state is maintenance healthy if `effective_pos_q(i) != 0`
11. `schedule_end_of_instruction_resets(ctx)`
12. `finalize_end_of_instruction_resets(ctx)`
13. recompute `r_last` exactly once from the final post-reset state

A failed post-conversion maintenance check MUST revert atomically. This instruction MUST NOT materialize a missing account.

### 10.5 `execute_trade(a, b, oracle_price, now_slot, size_q, exec_price)`

`size_q > 0` means account `a` buys base from account `b`.

Procedure:

1. require both accounts are materialized
2. require `a != b`
3. require trusted `now_slot >= current_slot`
4. require trusted `now_slot >= slot_last`
5. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
6. require validated `0 < exec_price <= MAX_ORACLE_PRICE`
7. require `0 < size_q <= MAX_TRADE_SIZE_Q`
8. compute `trade_notional = mul_div_floor_u128(size_q, exec_price, POS_SCALE)`
9. require `trade_notional <= MAX_ACCOUNT_NOTIONAL`
10. initialize fresh instruction context `ctx`
11. `touch_account_full(a, oracle_price, now_slot)`
12. `touch_account_full(b, oracle_price, now_slot)`
13. let `old_eff_pos_q_a = effective_pos_q(a)` and `old_eff_pos_q_b = effective_pos_q(b)`
14. let `MM_req_pre_a`, `MM_req_pre_b` be maintenance requirement on the post-touch pre-trade state
15. let `Eq_maint_raw_pre_a = Eq_maint_raw_a` and `Eq_maint_raw_pre_b = Eq_maint_raw_b` in the exact widened signed domain of §3.4
16. let `margin_buffer_pre_a = Eq_maint_raw_pre_a - (MM_req_pre_a as wide_signed)` and `margin_buffer_pre_b = Eq_maint_raw_pre_b - (MM_req_pre_b as wide_signed)` in the exact widened signed domain of §3.4
17. invoke `maybe_finalize_ready_reset_sides_before_oi_increase()`
18. define:
   - `new_eff_pos_q_a = checked_add_i128(old_eff_pos_q_a, size_q as i128)`
   - `new_eff_pos_q_b = checked_sub_i128(old_eff_pos_q_b, size_q as i128)`
19. require `abs(new_eff_pos_q_a) <= MAX_POSITION_ABS_Q` and `abs(new_eff_pos_q_b) <= MAX_POSITION_ABS_Q`
20. compute `OI_long_after_trade` and `OI_short_after_trade` exactly via §5.2.2 using `old_eff_pos_q_a`, `old_eff_pos_q_b`, `new_eff_pos_q_a`, and `new_eff_pos_q_b`; require `OI_long_after_trade <= MAX_OI_SIDE_Q` and `OI_short_after_trade <= MAX_OI_SIDE_Q`; reject if `mode_long ∈ {DrainOnly, ResetPending}` and `OI_long_after_trade > OI_eff_long`; reject if `mode_short ∈ {DrainOnly, ResetPending}` and `OI_short_after_trade > OI_eff_short`
21. apply immediate execution-slippage alignment PnL before fees:
   - `trade_pnl_num = checked_mul_i128(size_q as i128, (oracle_price as i128) - (exec_price as i128))`
   - `trade_pnl_a = floor_div_signed_conservative(trade_pnl_num, POS_SCALE)`
   - `trade_pnl_b = -trade_pnl_a`
   - record `old_R_a = R_a` and `old_R_b = R_b`
   - `set_pnl(a, checked_add_i128(PNL_a, trade_pnl_a))`
   - `set_pnl(b, checked_add_i128(PNL_b, trade_pnl_b))`
   - if `R_a > old_R_a`, invoke `restart_warmup_after_reserve_increase(a)`
   - if `R_b > old_R_b`, invoke `restart_warmup_after_reserve_increase(b)`
22. apply the resulting effective positions using `attach_effective_position(a, new_eff_pos_q_a)` and `attach_effective_position(b, new_eff_pos_q_b)`
23. update side OI atomically by writing the exact candidate after-values from step 20:
   - set `OI_eff_long = OI_long_after_trade`
   - set `OI_eff_short = OI_short_after_trade`
24. settle post-trade losses from principal for both accounts via §7.1
25. if `new_eff_pos_q_a == 0`, require `PNL_a >= 0` after step 24
26. if `new_eff_pos_q_b == 0`, require `PNL_b >= 0` after step 24
27. compute `fee = mul_div_ceil_u128(trade_notional, trading_fee_bps, 10_000)`
28. charge explicit trading fees using `charge_fee_to_insurance(a, fee)` and `charge_fee_to_insurance(b, fee)`
29. enforce post-trade margin for each account using the current post-step-28 state:
   - if the resulting effective position is zero:
     - the flat-account guard from steps 25–26 still applies, and
     - require exact `Eq_maint_raw_i >= 0` in the widened signed domain of §3.4 on the current post-step-28 state
   - else if the trade is risk-increasing for that account, require exact raw initial-margin healthy using `Eq_init_raw_i` and `IM_req_i` as defined in §9.1
   - else if the account is maintenance healthy using `Eq_net_i`, allow
   - else if the trade is strictly risk-reducing for that account, allow only if **both** of the following hold in the exact widened signed domain of §3.4:
     - the post-trade **fee-neutral** raw maintenance buffer `((Eq_maint_raw_i + (fee as wide_signed)) - (MM_req_i as wide_signed))` is strictly greater than the corresponding exact widened pre-trade raw maintenance buffer recorded in steps 15–16, and
     - the post-trade **fee-neutral** raw maintenance-equity shortfall below zero does not worsen, equivalently `min(Eq_maint_raw_i + (fee as wide_signed), 0) >= min(Eq_maint_raw_pre_i, 0)`
   - else reject

A bilateral trade is valid only if **both** participating accounts independently satisfy one of the permitted post-trade conditions above. If either account fails, the entire instruction MUST revert atomically; one counterparty's strict risk-reducing exemption never rescues the other.

This strict risk-reducing comparison is evaluated on the actual post-step-28 state but holds only the explicit fee of the candidate trade constant for the before/after comparison. Equivalently, it compares pre-trade raw maintenance buffer against post-trade raw maintenance buffer plus that same trade fee, so pure fee friction alone cannot make a genuinely de-risking trade fail the exemption. In addition, the fee-neutral raw maintenance-equity shortfall below zero must not worsen, so a large maintenance-requirement drop from a partial close cannot be used to mask newly created bad debt from execution slippage. All execution-slippage PnL, all position / notional changes, and all other current-state liabilities still remain in the comparison. Likewise, a voluntary organic flat close whose actual post-fee state would have negative exact `Eq_maint_raw_i` MUST still be rejected rather than exiting with unpaid fee debt that could later be forgiven by reclamation.
30. `schedule_end_of_instruction_resets(ctx)`
31. `finalize_end_of_instruction_resets(ctx)`
32. recompute `r_last` exactly once from the final post-reset state
33. assert `OI_eff_long == OI_eff_short`

### 10.6 `liquidate(i, oracle_price, now_slot, policy)`

`policy` MUST be one of:

- `FullClose`
- `ExactPartial(q_close_q)` where `0 < q_close_q < abs(old_eff_pos_q_i)` on the already-touched current state

No other liquidation-policy encoding is compliant in this revision.

Procedure:

1. require account `i` is materialized
2. initialize fresh instruction context `ctx`
3. `touch_account_full(i, oracle_price, now_slot)`
4. require liquidation eligibility from §9.3
5. if `policy == ExactPartial(q_close_q)`, attempt that exact partial-liquidation subroutine on the already-touched current state per §9.4, passing `ctx` through any `enqueue_adl` call; if any current-state validity check for that exact partial fails, reject
6. else (`policy == FullClose`), execute the full-close liquidation subroutine on the already-touched current state per §9.5, passing `ctx` through any `enqueue_adl` call
7. if any remaining nonzero position exists after liquidation, it MUST already have been reattached via `attach_effective_position`
8. `schedule_end_of_instruction_resets(ctx)`
9. `finalize_end_of_instruction_resets(ctx)`
10. recompute `r_last` exactly once from the final post-reset state
11. assert `OI_eff_long == OI_eff_short`

### 10.7 `reclaim_empty_account(i)`

Permissionless empty- or flat-dust-account recycling wrapper.

Procedure:

1. require account `i` is materialized
2. require all preconditions of §2.6 hold on the current state
3. execute the reclamation effects of §2.6

`reclaim_empty_account` MUST NOT call `accrue_market_to`, MUST NOT mutate side state, and MUST NOT materialize any account.

### 10.8 `keeper_crank(now_slot, oracle_price, ordered_candidates[], max_revalidations)`

`keeper_crank` is the minimal on-chain permissionless shortlist processor. Candidate discovery, ranking, deduplication, and sequential simulation MAY be performed entirely off chain. `ordered_candidates[]` is an untrusted keeper-supplied ordered list of existing account identifiers and MAY include optional liquidation-policy hints in the same `FullClose` / `ExactPartial(q_close_q)` format used by §10.6. The on-chain program MUST treat every candidate and order choice as advisory only. A liquidation-policy hint is advisory in the sense that it is untrusted and MUST be ignored unless it is current-state-valid under this section.

Procedure:

1. initialize fresh instruction context `ctx`
2. require trusted `now_slot >= current_slot`
3. require trusted `now_slot >= slot_last`
4. require validated `0 < oracle_price <= MAX_ORACLE_PRICE`
5. call `accrue_market_to(now_slot, oracle_price)` exactly once at the start
6. set `current_slot = now_slot`
7. let `attempts = 0`
8. for each candidate in keeper-supplied order:
   - if `attempts == max_revalidations`, break
   - if `ctx.pending_reset_long` or `ctx.pending_reset_short`, break
   - if candidate account is missing, continue
   - increment `attempts` by exactly `1`
   - perform one exact current-state revalidation attempt on that account by executing the same local state transition as `touch_account_full` on the already-accrued instruction state, namely the logic of §10.1 steps 7–13 in the same order; this local keeper helper MUST NOT call `accrue_market_to` again
   - if the account is liquidatable after that exact current-state touch and a current-state-valid liquidation-policy hint is present, the keeper MUST execute liquidation on the already-touched state using the same already-touched local liquidation execution as §§9.4–9.5 and §10.6 steps 4–7; the valid hint's exact policy is applied as-is, while an invalid or stale hint MUST be ignored; the keeper path MUST reuse `ctx`, MUST NOT repeat the touch, MUST NOT invoke end-of-instruction reset handling inside the loop, and MUST NOT nest a separate top-level instruction
   - if liquidation or the exact touch schedules a pending reset, break
9. `schedule_end_of_instruction_resets(ctx)`
10. `finalize_end_of_instruction_resets(ctx)`
11. recompute `r_last` exactly once from the final post-reset state
12. assert `OI_eff_long == OI_eff_short`

Rules:

- missing accounts MUST NOT be materialized
- `max_revalidations` measures normal exact current-state revalidation attempts on materialized accounts; missing-account skips do not count
- the engine MUST process candidates in keeper-supplied order except for the mandatory stop-on-pending-reset rule
- the engine MUST NOT impose any on-chain liquidation-first ordering across keeper-supplied candidates
- a candidate that proves safe or needs only cleanup after exact current-state touch still counts against `max_revalidations`
- a fatal conservative failure or invariant violation encountered during exact touch or liquidation remains a top-level instruction failure and MUST revert atomically; `max_revalidations` is not a sandbox against corruption

---

## 11. Permissionless off-chain shortlist keeper mode

This section is the sole normative specification for the optimized keeper path. Candidate discovery, ranking, deduplication, and sequential simulation MAY be performed entirely off chain. The protocol's on-chain safety derives only from exact current-state revalidation immediately before any liquidation write.

### 11.1 Core rules

1. The engine does **not** require any on-chain phase-1 search, barrier classifier, or no-false-negative scan proof.
2. `ordered_candidates[]` in §10.8 is keeper-supplied and untrusted. It MAY be stale, incomplete, duplicated, adversarially ordered, or produced by approximate heuristics.
3. Optional liquidation-policy hints are untrusted. They MUST be ignored unless they encode one of the §10.6 policies and pass the same exact current-state validity checks as the normal `liquidate` entrypoint. A current-state-valid hint is then applied exactly; otherwise that keeper attempt performs no liquidation action for that candidate.
4. The protocol MUST NOT require that a keeper discover *all* currently liquidatable accounts before it may process a useful subset.
5. Because `settle_account`, `liquidate`, `reclaim_empty_account`, and `keeper_crank` are permissionless, reset progress and dead-account recycling MUST remain possible without any mandatory on-chain scan order.

### 11.2 Exact current-state revalidation attempts

Let `max_revalidations` be the keeper's per-instruction budget measured in **exact current-state revalidation attempts**.

An exact current-state revalidation attempt begins when `keeper_crank` invokes the local exact-touch path on one materialized account after the single instruction-level `accrue_market_to(now_slot, oracle_price)` and `current_slot = now_slot` anchor.

It counts against `max_revalidations` once that materialized-account revalidation reaches a normal per-candidate outcome, including when the account:

- is liquidatable and is liquidated
- is touched and only cleanup happens
- is touched and proves safe
- is touched, remains liquidatable, but no valid current-state liquidation action is applied for that attempt

A pure missing-account skip does **not** count.

Inside `keeper_crank`, the per-candidate local exact-touch helper MUST be economically equivalent to `touch_account_full(i, oracle_price, now_slot)` on a state that has already been globally accrued once to `(now_slot, oracle_price)` at the start of the instruction. Concretely, for each materialized candidate it MUST execute the same local logic and in the same order as §10.1 steps 7–13, and it MUST NOT call `accrue_market_to` again for that account.

If the account is liquidatable after this local exact-touch path and a current-state-valid liquidation-policy hint is present, the keeper MUST invoke liquidation on the already-touched state using the same already-touched local liquidation execution as §§9.4–9.5 and §10.6 steps 4–7 and must apply that hint's exact policy. If no current-state-valid hint is present, that candidate receives no liquidation action in that attempt. The keeper path MUST NOT duplicate the touch, invoke end-of-instruction reset handling mid-loop, or nest a second top-level instruction.

A fatal conservative failure or invariant violation encountered after an exact-touch attempt begins is **not** a counted skip. It is a top-level instruction failure and reverts atomically under §0.

### 11.3 On-chain ordering constraints

The protocol MUST NOT impose a mandatory on-chain liquidation-first, cleanup-first, or priority-queue ordering across keeper-supplied candidates.

Inside `keeper_crank`, the only mandatory on-chain ordering constraints are:

1. the single initial `accrue_market_to(now_slot, oracle_price)` and trusted `current_slot = now_slot` anchor happen before per-candidate exact revalidation
2. materialized candidates are processed in keeper-supplied order
3. once either pending-reset flag becomes true, the instruction stops further candidate processing and proceeds directly to end-of-instruction reset handling

A stale or adversarial shortlist MAY waste that instruction's own `max_revalidations` budget or the submitting keeper's own call opportunity, but it MUST NOT permit an incorrect liquidation.

### 11.4 Honest-keeper guidance (non-normative)

An honest keeper SHOULD, when compute permits, simulate the same single `accrue_market_to(now_slot, oracle_price)` step off chain, then sequentially simulate the shortlisted touches and liquidations on the evolving simulated state before submission. This is recommended because liquidation ordering is path-dependent through `A_side`, `K_side`, `OI_eff_*`, side modes, and end-of-instruction reset stop conditions.

For off-chain ordering, an honest keeper SHOULD usually prioritize:

- reset-progress or dust-progress candidates that can unblock finalization on already-constrained sides
- opposite-side bankruptcy candidates **before** a touch that is expected to zero the last stored position on side `S` while phantom OI would remain on `S`, because once `stored_pos_count_S == 0` while phantom OI remains, further `D_rem` can no longer be written into `K_S` and is routed through uninsured protocol loss after insurance
- otherwise, higher expected uncovered deficit after insurance, larger maintenance shortfall, larger notional, and `DrainOnly`-side candidates ahead of otherwise similar `Normal`-side candidates

These `SHOULD` recommendations are operational guidance only, not consensus rules.

## 12. Required test properties (minimum)

An implementation MUST include tests that cover at least:

1. **Conservation:** `V >= C_tot + I` always, and `Σ PNL_eff_matured_i <= Residual`.
2. **Fresh-profit reservation:** a positive `set_pnl` increase raises `R_i` by the same positive delta and does not immediately increase `PNL_matured_pos_tot`.
3. **Oracle-manipulation haircut safety:** fresh, unwarmed manipulated PnL cannot dilute `h`, cannot satisfy initial-margin or withdrawal checks, and cannot reduce another account's equity before warmup release; it MAY only support the generating account's own maintenance equity.
4. **Warmup anti-retroactivity:** newly generated profit cannot inherit old dormant maturity headroom.
5. **Pure release slope preservation:** repeated touches do not create exponential-decay maturity.
6. **Same-epoch local settlement:** settlement of one account does not depend on any canonical-order prefix.
7. **Non-compounding quantity basis:** repeated same-epoch touches without explicit position mutation do not compound quantity-flooring loss.
8. **Dynamic dust bound:** after same-epoch zeroing events, basis replacements, and ADL multiplier truncations before a reset, authoritative OI on a side with no stored positions is bounded by that side's cumulative phantom-dust bound.
9. **Dust-clear scheduling:** dust clearance and reset initiation happen only at end of top-level instructions, never mid-instruction.
10. **Epoch-safe reset:** accounts cannot be attached to a new epoch before `begin_full_drain_reset` runs.
11. **Precision-exhaustion terminal drain:** if `A_candidate == 0` with `OI_post > 0`, the engine force-drains both sides instead of reverting.
12. **ADL representability fallback:** if `delta_K_abs` is non-representable or `K_opp + delta_K_exact` overflows, quantity socialization still proceeds and the remainder routes through `record_uninsured_protocol_loss`.
13. **Insurance-first deficit coverage:** `enqueue_adl` spends `I` down to `I_floor` before any remaining bankruptcy loss is socialized or left as junior undercollateralization.
14. **Unit consistency:** margin, notional, and fees use quote-token atomic units consistently.
15. **`set_pnl` aggregate safety:** positive-PnL updates do not overflow `PNL_pos_tot` or `PNL_matured_pos_tot`.
16. **`PNL_i == i128::MIN` forbidden:** every negation path is safe.
17. **Trading and liquidation fee shortfalls:** unpaid explicit fees become negative `fee_credits_i`, not `PNL_i` and not `D`.
18. **Zero-rate recomputation ordering:** every standard-lifecycle endpoint recomputes `r_last` exactly once and only from final post-reset state, and every compliant recomputation stores exactly `0`.
19. **Zero-rate funding surface determinism:** no compliant instruction in this revision applies a funding transfer, and `fund_px_last` always equals the oracle price from the most recent successful `accrue_market_to`.
20. **Flat-account negative remainder:** a flat account with negative `PNL_i` after principal exhaustion resolves through `absorb_protocol_loss` only in the allowed already-authoritative flat-account paths.
21. **Reset finalization:** after reconciling stale accounts, the side can leave `ResetPending` and accept fresh OI again.
22. **Deposit loss seniority:** in `deposit`, realized losses are settled from newly deposited principal before any outstanding fee debt is swept.
23. **Deposit materialization threshold:** a missing account cannot be materialized by a deposit smaller than `MIN_INITIAL_DEPOSIT`, while an existing materialized account may still receive smaller top-ups.
24. **Dust liquidation minimum fee:** if `q_close_q > 0` but `closed_notional` floors to zero, `liq_fee` still honors `min_liquidation_abs`.
25. **Risk-reducing trade exemption:** a strict non-flipping position reduction that improves the exact widened **fee-neutral** raw maintenance buffer is allowed even if the account remains below maintenance after the trade, but only if the same trade does not worsen the exact widened **fee-neutral** raw maintenance-equity shortfall below zero. A reduction whose fee-neutral raw maintenance buffer worsens, or whose fee-neutral negative raw maintenance equity becomes more negative, is rejected.
26. **Positive local PnL supports maintenance but not initial margin / withdrawal at face value:** on a touched generating account, maintenance uses full local `PNL_i`, so a freshly profitable account is not liquidated solely because profit is still warming up and pure warmup release on unchanged `PNL_i` does not reduce `Eq_maint_raw_i`; the same junior profit still cannot satisfy a risk-increasing initial-margin or withdrawal check except through the matured-haircutted component of exact `Eq_init_raw_i`.
27. **Reserve-loss ordering:** when positive `PNL_i` shrinks for true market-loss reasons, losses consume `R_i` before matured released positive PnL, so neutral price chop does not ratchet previously matured margin into reserve.
28. **Organic close bankruptcy guard:** a flat trade cannot bypass ADL by leaving negative `PNL_i` behind.
29. **Full-close liquidation requirement:** full-close liquidation always closes the full remaining effective position.
30. **Dead-account reclamation:** a flat account with `0 <= C_i < MIN_INITIAL_DEPOSIT`, zero `PNL_i`, zero `R_i`, zero basis, and nonpositive `fee_credits_i` can be reclaimed safely; any remaining dust capital is swept into `I` and the slot is reused.
31. **Missing-account safety:** `settle_account`, `withdraw`, `execute_trade`, `liquidate`, and `keeper_crank` do not materialize missing accounts.
32. **Standalone settle lifecycle:** `settle_account` can reconcile the last stale or dusty account and still trigger required reset scheduling/finalization and final-state funding recomputation.
33. **Off-chain shortlist stale/adversarial safety:** replaying or adversarially ordering an old shortlist cannot cause an incorrect liquidation, because `keeper_crank` revalidates each processed candidate on current state before any liquidation write.
34. **Keeper single global accrual:** `keeper_crank` calls `accrue_market_to(now_slot, oracle_price)` exactly once per instruction and per-candidate exact revalidation does not reaccrue the market.
35. **Keeper local-touch equivalence:** the per-candidate exact local touch used inside `keeper_crank` is economically equivalent to `touch_account_full` on the same already-accrued state.
36. **Keeper revalidation budget accounting:** `max_revalidations` bounds the number of normal exact current-state revalidation attempts on materialized accounts, including safe false positives and cleanup-only touches; missing-account skips do not count. Fatal conservative failures are instruction failures, not counted skips.
37. **No duplicate keeper touch before liquidation:** when `keeper_crank` liquidates a candidate, it does so from the already-touched current state and does not perform a second full touch of that same candidate inside the same attempt.
38. **Keeper local liquidation is not a nested top-level finalize:** the per-candidate keeper liquidation path executes only the already-touched local liquidation subroutine and does not call `schedule_end_of_instruction_resets`, `finalize_end_of_instruction_resets`, or `recompute_r_last_from_final_state()` mid-loop.
39. **Keeper candidate-order freedom:** the engine imposes no on-chain liquidation-first ordering across keeper-supplied candidates; a cleanup-first shortlist is processed in the keeper-supplied order unless a pending reset is scheduled.
40. **Keeper stop on pending reset:** once a candidate touch or liquidation schedules a pending reset, `keeper_crank` performs no further candidate processing before end-of-instruction reset handling.
41. **Permissionless reset or dust progress without on-chain scan:** targeted `settle_account` calls or targeted `keeper_crank` shortlists can reconcile stale accounts on a `ResetPending` side and can also clear targeted pre-reset dust-progress accounts on a side already within its phantom-dust-clear bound, without any on-chain phase-1 search.
42. **Post-reset funding recomputation in keeper:** `keeper_crank` recomputes `r_last` exactly once after final reset handling, and the stored value is exactly `0` under the zero-rate core profile.
43. **K-pair chronology correctness:** same-epoch and epoch-mismatch settlement call `wide_signed_mul_div_floor_from_k_pair(abs_basis, k_then, k_now, den)` in chronological order; a true loss cannot be settled as a gain due to swapped arguments.
44. **Deposit true-flat guard and latent-loss seniority:** a `deposit` into an account with `basis_pos_q_i != 0` neither routes unresolved negative PnL through §7.3 nor sweeps fee debt before a later full current-state touch.
45. **No duplicate full-close touch:** both the top-level `liquidate` path and the `keeper_crank` local liquidation path execute the already-touched full-close / bankruptcy liquidation subroutine without a second full touch or second deterministic `last_fee_slot_i` stamp.
46. **Zero-rate funding recomputation:** `recompute_r_last_from_final_state()` always stores `0` and never reverts.
47. **Keeper atomicity alignment:** a normal safe / cleanup / liquidated candidate counts against `max_revalidations`, but a fatal conservative failure during exact touch or liquidation reverts the whole instruction atomically rather than being treated as a counted skip.
48. **Exact raw maintenance-buffer comparison:** strict risk-reducing trade permission uses the exact widened signed pre/post raw maintenance buffers and cannot be satisfied solely because both sides of the comparison were clamped at the negative representation floor.
49. **Profit-conversion reserve preservation:** converting `ReleasedPos_i = x` leaves `R_i` unchanged and reduces both `PNL_pos_tot` and `PNL_matured_pos_tot` by exactly `x`; repeated settles cannot drain reserve faster than `advance_profit_warmup`.
50. **Flat-only automatic conversion:** an open-position `touch_account_full` does not automatically convert matured released profit into capital, while a truly flat touched state may convert it via §7.4.
51. **Universal withdrawal dust guard:** any withdrawal must leave either `0` capital or at least `MIN_INITIAL_DEPOSIT`; a materialize-open-dust-withdraw-close loop cannot end at a flat unreclaimable `C_i = 1` account.
52. **Explicit open-position profit conversion:** `convert_released_pnl` consumes only `ReleasedPos_i`, leaves `R_i` unchanged, sweeps fee debt from the new capital, and rejects atomically if the post-conversion open-position state is not maintenance healthy.
53. **Phantom-dust ADL ordering awareness:** if a keeper simulation zeroes the last stored position on a side while phantom OI remains, opposite-side bankruptcies processed after that point lose current-instruction K-socialization capacity; processing them before that zeroing touch preserves it.
54. **Exact-drain reset scheduling under OI symmetry:** whenever `enqueue_adl` reaches an opposing-zero branch (`OI == 0` after step 1, or `OI_post == 0`), the maintained `OI_eff_long == OI_eff_short` invariant implies the liquidated side is also authoritatively zero at that point, the required pending resets are scheduled, and subsequent close / liquidation attempts do not underflow against zero authoritative OI.
55. **Organic flat-close fee-debt guard:** if a trade would leave an account with resulting effective position `0` but exact post-fee `Eq_maint_raw_i < 0`, the instruction rejects atomically; a user cannot wash-trade away assets, exit flat with unpaid fee debt, and then reclaim the slot to forgive it. A profitable fast winner with positive reserved `R_i` and nonnegative exact post-fee `Eq_maint_raw_i` may still close risk to zero even though `Eq_init_raw_i` excludes that reserved profit.
56. **Exact raw initial-margin approval:** a risk-increasing trade or open-position withdrawal with exact `Eq_init_raw_i < IM_req_i` is rejected even if `Eq_init_net_i` would floor to `0` and the proportional notional term would otherwise floor low.
57. **Absolute nonzero-position margin floors:** any nonzero position faces at least `MIN_NONZERO_MM_REQ` and `MIN_NONZERO_IM_REQ`; a microscopic nonzero position cannot remain healthy or be newly opened solely because proportional notional floors to zero.
58. **Flat dust-capital reclamation:** a trade- or conversion-created flat account with `0 < C_i < MIN_INITIAL_DEPOSIT` cannot pin capacity permanently, because `reclaim_empty_account` may sweep that dust capital into `I` and recycle the slot.
59. **Epoch-gap invariant preservation:** every materialized nonzero-basis account is either attached to the current side epoch or lags by exactly one epoch while that side is `ResetPending`; a gap larger than one is rejected as corruption.
60. **Direct fee-credit repayment cap:** `deposit_fee_credits` applies only `min(amount, FeeDebt_i)`, never makes `fee_credits_i` positive, increases `V` and `I` by exactly the applied amount, and does not mutate `C_i` or side state.
61. **Insurance top-up bounded arithmetic:** `top_up_insurance_fund` uses checked addition, enforces `MAX_VAULT_TVL`, increases `V` and `I` by the same exact amount, and does not mutate any other state.
62. **Pure deposit no-insurance-draw:** `deposit` never calls `absorb_protocol_loss`, never decrements `I`, and leaves any surviving flat negative `PNL_i` in place for a later accrued touch.
63. **Bilateral trade approval atomicity:** if one trade counterparty qualifies under step 29 but the other fails every permitted branch, the entire trade reverts atomically.
64. **Exact trade OI decomposition and constrained-side gating:** §10.5 uses the exact bilateral candidate after-values of §5.2.2 both for constrained-side gating and for final OI writeback; sign flips are therefore handled as a same-side close plus opposite-side open without ambiguity.
65. **Liquidation policy determinism:** direct `liquidate` accepts only `FullClose` or `ExactPartial(q_close_q)`; keeper hints use the same format, valid keeper hints are applied exactly, and absent or invalid keeper hints cause no liquidation action for that candidate in that attempt.
66. **Flat authoritative deposit sweep:** on a flat authoritative state (`basis_pos_q_i == 0`) with `PNL_i >= 0`, `deposit` sweeps fee debt immediately after principal-loss settlement even when `PNL_i > 0` because of remaining warmup reserve or other positive flat PnL; only a surviving negative `PNL_i` blocks the sweep.
67. **Configuration immutability:** no runtime instruction in this revision can change `T`, fee parameters, margin parameters, liquidation parameters, `I_floor`, or the live-balance floors after initialization.
68. **Partial liquidation remainder nonzero:** any compliant partial liquidation satisfies `0 < q_close_q < abs(old_eff_pos_q_i)` and therefore produces strictly nonzero `new_eff_pos_q_i`; there is no zero-result partial-liquidation branch.
69. **Positive conversion denominator:** whenever flat auto-conversion or `convert_released_pnl` consumes `x > 0` released profit, `PNL_matured_pos_tot > 0` on that state and the haircut denominator is strictly positive.
70. **Partial-liquidation local health check survives reset scheduling:** if a partial liquidation reattaches a nonzero remainder and `enqueue_adl` schedules a pending reset in the same instruction, the instruction still evaluates the post-step local maintenance-health requirement of §9.4 on that remaining state before final reset handling; only further live-OI-dependent work is skipped.

## 13. Compatibility and upgrade notes

1. LP accounts and user accounts may share the same protected-principal and junior-profit mechanics.
2. The mandatory `O(1)` global aggregates for solvency are `C_tot`, `PNL_pos_tot`, and `PNL_matured_pos_tot`; the A/K side indices add `O(1)` state for lazy settlement.
3. This spec deliberately rejects hidden residual matching. Bankruptcy socialization occurs only through explicit Insurance Fund usage, explicit A/K state, or junior undercollateralization.
4. Any upgrade path from a version that did not maintain `R_i`, `PNL_matured_pos_tot`, `basis_pos_q_i`, `a_basis_i`, `stored_pos_count_*`, `stale_account_count_*`, or `phantom_dust_bound_*_q` consistently MUST complete migration before OI-increasing operations are re-enabled.
5. Any upgrade from an earlier integrated barrier-preview or addendum-based keeper design MAY drop the on-chain preview helper and barrier-scan logic once the exact current-state `keeper_crank` path and the shortlist-oriented tests from §12 are implemented.
6. Any future revision that wishes to re-enable nonzero funding or recurring maintenance fees MUST replace the zero-rate and fee-disabled rules of §§4.12 and 8.2 with a fully explicit normative formula, funding-transfer arithmetic, and test suite; implementation-defined formulas are non-compliant.
7. Any future revision that wishes to allow runtime parameter mutation MUST define an explicit safe update procedure that preserves warmup, margin, liquidation, and dust-floor invariants across the transition.

