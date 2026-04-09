import QEDGen.Solana.Spec

open QEDGen.Solana.SpecDSL
open QEDGen.Solana

/-!
# Property block tests

Tests that `property` generates correct predicate definitions and
per-operation preservation theorems.
-/

-- ============================================================================
-- 1. Percolator-style: conservation over arithmetic state
-- ============================================================================

qedspec RiskEngine where
  state
    authority : Pubkey
    vault : U64
    capital_total : U64
    insurance : U64

  operation deposit
    who: authority
    when: Active
    then: Active

  operation withdraw
    who: authority
    when: Active
    then: Active

  operation top_up_insurance
    who: authority
    when: Active
    then: Active

  property conservation "s.vault >= s.capital_total + s.insurance"
    preserved_by: deposit, withdraw, top_up_insurance

  property vault_positive "s.vault > 0"
    preserved_by: deposit

-- 1a. Predicate evaluates correctly on concrete state
example : RiskEngine.conservation
    { authority := ⟨0,0,0,0⟩, vault := 100, capital_total := 60,
      insurance := 30, status := .Active } := by
  unfold RiskEngine.conservation
  decide

-- 1b. Predicate fails on bad state
example : ¬ RiskEngine.conservation
    { authority := ⟨0,0,0,0⟩, vault := 50, capital_total := 60,
      insurance := 30, status := .Active } := by
  unfold RiskEngine.conservation
  decide

-- 1d. Preservation theorems exist with correct signatures
#check @RiskEngine.deposit.preserves_conservation
#check @RiskEngine.withdraw.preserves_conservation
#check @RiskEngine.top_up_insurance.preserves_conservation

-- 1e. vault_positive only scoped to deposit (not withdraw/top_up)
#check @RiskEngine.deposit.preserves_vault_positive

-- ============================================================================
-- 2. Escrow-style: property + CPI coexist
-- ============================================================================

qedspec TokenVault where
  state
    owner : Pubkey
    balance : U64

  operation deposit
    who: owner
    when: Active
    then: Active
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(source writable, vault writable, owner signer)

  operation withdraw
    who: owner
    when: Active
    then: Active
    calls: TOKEN_PROGRAM_ID DISC_TRANSFER(vault writable, destination writable, owner signer)

  property balance_bounded "valid_u64 s.balance"
    preserved_by: deposit, withdraw

-- 2a. Property coexists with CPI theorems
#check @TokenVault.deposit.cpi_correct
#check @TokenVault.deposit.preserves_balance_bounded
#check @TokenVault.withdraw.cpi_correct
#check @TokenVault.withdraw.preserves_balance_bounded

-- 2b. Property predicate uses library definitions
example : TokenVault.balance_bounded
    { owner := ⟨0,0,0,0⟩, balance := 42, status := .Active } := by
  show 42 ≤ U64_MAX
  decide

-- ============================================================================
-- 3. Multiple properties on same spec
-- ============================================================================

qedspec MultiProp where
  state
    admin : Pubkey
    x : U64
    y : U64

  operation increment
    who: admin
    when: Active
    then: Active

  property x_bounded "valid_u64 s.x"
    preserved_by: increment

  property y_bounded "valid_u64 s.y"
    preserved_by: increment

  property sum_bounded "s.x + s.y <= U64_MAX"
    preserved_by: increment

-- 3a. Three distinct preservation theorems for one operation
#check @MultiProp.increment.preserves_x_bounded
#check @MultiProp.increment.preserves_y_bounded
#check @MultiProp.increment.preserves_sum_bounded

-- 3b. sum_bounded evaluates correctly
example : MultiProp.sum_bounded
    { admin := ⟨0,0,0,0⟩, x := 100, y := 200, status := .Active } := by
  show 100 + 200 ≤ U64_MAX
  decide
