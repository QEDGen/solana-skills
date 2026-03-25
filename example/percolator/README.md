# Percolator

**EDUCATIONAL RESEARCH PROJECT — NOT PRODUCTION READY. NOT AUDITED. Do NOT use with real funds.**

A predictable alternative to ADL queues.

If you want the `xy = k` of perpetual futures risk engines -- something you can reason about, audit, and run without human intervention -- the cleanest move is simple: stop treating profit like money. Treat it like what it really is in a stressed exchange: a junior claim on a shared balance sheet.

> No user can ever withdraw more value than actually exists on the exchange balance sheet.

## Two Problems, Two Mechanisms

A perp exchange has two fairness problems:

1. **Exit fairness:** when the vault is stressed, who gets paid and how much?
2. **Overhang clearing:** when positions go bankrupt, how does the opposing side absorb the residual without deadlocking the market?

Percolator solves them with two independent mechanisms that compose cleanly:

- **H** (the haircut ratio) keeps all exits fair.
- **A/K** (the lazy side indices) keeps all residual overhang clearing fair, and guarantees markets always return to healthy.

---

## H: Fair Exits

Capital is senior. Profit is junior. A single global ratio determines how much profit is real.

```
Residual  = max(0, V - C_tot - I)

              min(Residual, PNL_matured_pos_tot)
    h     =  ----------------------------------
                    PNL_matured_pos_tot
```

If fully backed, `h = 1`. If stressed, `h < 1`. Every profitable account sees the same fraction of its *released* profit:

```
ReleasedPos_i   = max(PNL_i, 0) - R_i
effective_pnl_i = floor(ReleasedPos_i * h)
```

Fresh profit sits in a per-account reserve `R_i` and converts to released (matured) profit through a warmup period. Only matured profit enters the haircut denominator (`PNL_matured_pos_tot`) and the per-account effective PnL. This is the core oracle-manipulation defense — an attacker who spikes a price sees their unrealized gain locked in `R_i`, excluded from both the ratio and their withdrawable amount, until the warmup window passes.

No rankings, no queue priority, no first-come advantage. The floor rounding is conservative — the sum of all effective PnL never exceeds what exists in the vault.

When the system is stressed, `h` falls and less converts. When losses settle or buffers recover, `h` rises. Self-healing.

Flat accounts are always protected — `h` only gates profit extraction, never touches deposited capital.

---

## A/K: Fair Overhang Clearing

When a leveraged account goes bankrupt, two things need to happen: remove the position quantity from open interest, and distribute any uncovered deficit across the opposing side.

Traditional ADL queues pick specific counterparties and force-close them. Percolator replaces the queue with two global coefficients per side:

- **A** scales everyone's effective position equally.
- **K** accumulates all PnL events (mark, funding, deficit socialization) into one index.

```
effective_pos(i) = floor(basis_i * A / a_basis_i)
pnl_delta(i)     = floor(|basis_i| * (K - k_snap_i) / (a_basis_i * POS_SCALE))
```

When a liquidation reduces OI, `A` decreases — every account on that side shrinks by the same ratio. When a deficit is socialized, `K` shifts — every account absorbs the same per-unit loss.

No account is singled out. Settlement is O(1) per account and order-independent.

### Markets Always Return to Healthy

A/K guarantees forward progress through a deterministic cycle:

**DrainOnly** — when `A` drops below a precision threshold, no new OI can be added. Positions can only close.

**ResetPending** — when OI reaches zero, the engine snapshots `K`, increments the epoch, and resets `A` back to 1. Remaining accounts settle their residual PnL exactly once when next touched.

**Normal** — once all stale accounts have settled and OI is confirmed zero, the side reopens for trading with full precision.

No admin intervention. No governance vote. The state machine always makes progress.

---

## How They Compose

| | H | A/K |
|---|---|---|
| **Solves** | Exit fairness | Overhang clearing |
| **Math** | Pro-rata profit scaling | Pro-rata position/deficit scaling |
| **Triggered by** | Withdrawal or conversion | Bankrupt liquidation |
| **Recovery** | Automatic as Residual improves | Deterministic three-phase reset |

Together:
- No user can withdraw more than exists.
- No user is singled out for forced closure.
- Markets always recover.
- Flat accounts keep their deposits.

A/K fairness is exact for open-position economics. H fairness is exact only for the currently stored realized claim set, not for the economically "true" claim set you would get after globally cranking everyone.

---

## Open Source

Fork it, test it, send bug reports. Percolator is open research under Apache-2.0.

```bash
cargo install --locked kani-verifier
cargo kani setup
cargo kani
```

## References

- Tarun Chitra, *Autodeleveraging: Impossibilities and Optimization*, arXiv:2512.01112, 2025. https://arxiv.org/abs/2512.01112
