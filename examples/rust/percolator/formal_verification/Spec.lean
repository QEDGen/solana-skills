import QEDGen.Solana.Spec

open QEDGen.Solana.SpecDSL

/-!
# Percolator Risk Engine — Spec-Driven Verification

A perpetual DEX risk engine managing protected principal, junior profit claims,
and lazy A/K side indices.

Core properties:
  - Conservation: V >= C_tot + I (vault covers all obligations)
  - Vault bounded: V <= MAX_VAULT_TVL
  - ADL lifecycle: Active → Draining → Resetting → Active

Effects use structured `field add/sub param` syntax — validated against
state fields at elaboration time. `sub` auto-generates underflow guards.
-/

qedspec Percolator where
  state
    authority : Pubkey
    V : U64
    C_tot : U64
    I : U64

  -- Deposit: user adds capital, V and C_tot increase by same amount
  operation deposit
    who: authority
    when: Active
    then: Active
    takes: amount U64
    guard: "s.V + amount ≤ 10000000000000000"
    effect: V add amount, C_tot add amount

  -- Withdraw: user removes capital, V and C_tot decrease by same amount
  -- Auto-generates: amount ≤ s.V ∧ amount ≤ s.C_tot
  operation withdraw
    who: authority
    when: Active
    then: Active
    takes: amount U64
    effect: V sub amount, C_tot sub amount

  -- Top up insurance: external deposit into insurance fund
  operation top_up_insurance
    who: authority
    when: Active
    then: Active
    takes: amount U64
    guard: "s.V + amount ≤ 10000000000000000"
    effect: V add amount, I add amount

  -- ADL lifecycle transitions (no state field mutations)
  operation trigger_adl
    who: authority
    when: Active
    then: Draining

  operation complete_drain
    who: authority
    when: Draining
    then: Resetting

  operation reset
    who: authority
    when: Resetting
    then: Active

  -- Conservation: vault covers all obligations
  property conservation "s.V ≥ s.C_tot + s.I"
    preserved_by: deposit, withdraw, top_up_insurance, trigger_adl, complete_drain, reset

  -- Vault bounded by MAX_VAULT_TVL
  property vault_bounded "s.V ≤ 10000000000000000"
    preserved_by: deposit, top_up_insurance
