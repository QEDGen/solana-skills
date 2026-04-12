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

  operation deposit
    doc: "User adds capital — V and C_tot increase by same amount"
    who: authority
    when: Active
    then: Active
    takes: amount U64
    guard: "s.V + amount ≤ 10000000000000000"
    effect: V add amount, C_tot add amount

  operation withdraw
    doc: "User removes capital — V and C_tot decrease by same amount"
    who: authority
    when: Active
    then: Active
    takes: amount U64
    effect: V sub amount, C_tot sub amount

  operation top_up_insurance
    doc: "External deposit into insurance fund"
    who: authority
    when: Active
    then: Active
    takes: amount U64
    guard: "s.V + amount ≤ 10000000000000000"
    effect: V add amount, I add amount

  operation trigger_adl
    doc: "Begin auto-deleveraging cycle"
    who: authority
    when: Active
    then: Draining

  operation complete_drain
    doc: "Complete the drain phase of ADL"
    who: authority
    when: Draining
    then: Resetting

  operation reset
    doc: "Reset risk engine after ADL cycle"
    who: authority
    when: Resetting
    then: Active

  -- Conservation: vault covers all obligations
  property conservation "s.V ≥ s.C_tot + s.I"
    preserved_by: deposit, withdraw, top_up_insurance, trigger_adl, complete_drain, reset

  -- Vault bounded by MAX_VAULT_TVL
  property vault_bounded "s.V ≤ 10000000000000000"
    preserved_by: deposit, top_up_insurance
