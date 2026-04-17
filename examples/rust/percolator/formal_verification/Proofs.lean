/-
Proofs.lean — user-owned preservation proofs for Percolator.

Codegen (`qedgen codegen`) never writes to this file. It's the
durable home for hand-written and agent-written proof bodies, while
`Spec.lean` is regenerated whenever the `.qedspec` changes.

Conventions:
  - Each preservation obligation `<property>_preserved_by_<handler>`
    declared here; codegen guarantees the matching transition+predicate
    defs live in `Spec`.
  - Liveness theorems follow the same pattern.
  - If `Spec.lean` adds/renames a property or handler and a theorem here
    becomes stale, the rename shows up as a compile error — fix or move
    the theorem to match.

Until M13 (git-native regen) lands, this split is the stop-gap
durability story.
-/
import Spec

namespace Percolator

open QEDGen.Solana
open QEDGen.Solana.IndexedState

-- =========================================================================
-- Conservation preservation — 9 handlers that touch state
-- =========================================================================

theorem conservation_preserved_by_add_user (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : add_userTransition s signer i = some s') :
    conservation s' := by
  unfold add_userTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  rw [QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 1 } Account.capital rfl,
      QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 1 } Account.reserved_pnl rfl]
  exact h_inv

theorem conservation_preserved_by_add_lp (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : add_lpTransition s signer i = some s') :
    conservation s' := by
  unfold add_lpTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  rw [QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 1 } Account.capital rfl,
      QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 1 } Account.reserved_pnl rfl]
  exact h_inv

theorem conservation_preserved_by_reclaim_empty_account (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : reclaim_empty_accountTransition s signer i = some s') :
    conservation s' := by
  unfold reclaim_empty_accountTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  rw [QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 0 } Account.capital rfl,
      QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 0 } Account.reserved_pnl rfl]
  exact h_inv

theorem conservation_preserved_by_close_account (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : close_accountTransition s signer i = some s') :
    conservation s' := by
  unfold close_accountTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  set newAcc : Account := { (s.accounts i) with capital := 0, active := 0 } with hNew
  have hrpnl : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).reserved_pnl) =
      (∑ j : AccountIdx, (s.accounts j).reserved_pnl) :=
    QEDGen.Solana.IndexedState.sum_update_proj_eq
      s.accounts i newAcc Account.reserved_pnl rfl
  have hbil : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).capital)
        + (s.accounts i).capital =
      (∑ j : AccountIdx, (s.accounts j).capital) + 0 :=
    QEDGen.Solana.IndexedState.sum_update_proj_bilinear
      s.accounts i newAcc Account.capital
  set Scap := ∑ j : AccountIdx, (s.accounts j).capital
  set Srpnl := ∑ j : AccountIdx, (s.accounts j).reserved_pnl
  omega

theorem conservation_preserved_by_deposit (s s' : State) (signer : Pubkey) i amount
    (h_inv : conservation s) (h : depositTransition s signer i amount = some s') :
    conservation s' := by
  unfold depositTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  set newAcc : Account := { (s.accounts i) with capital := (s.accounts i).capital + amount } with hNew
  have hrpnl : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).reserved_pnl) =
      (∑ j : AccountIdx, (s.accounts j).reserved_pnl) :=
    QEDGen.Solana.IndexedState.sum_update_proj_eq
      s.accounts i newAcc Account.reserved_pnl rfl
  have hbil : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).capital)
        + (s.accounts i).capital =
      (∑ j : AccountIdx, (s.accounts j).capital) + ((s.accounts i).capital + amount) :=
    QEDGen.Solana.IndexedState.sum_update_proj_bilinear
      s.accounts i newAcc Account.capital
  set Scap := ∑ j : AccountIdx, (s.accounts j).capital
  set Srpnl := ∑ j : AccountIdx, (s.accounts j).reserved_pnl
  omega

theorem conservation_preserved_by_withdraw (s s' : State) (signer : Pubkey) i amount
    (h_inv : conservation s) (h : withdrawTransition s signer i amount = some s') :
    conservation s' := by
  unfold withdrawTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  set newAcc : Account := { (s.accounts i) with capital := (s.accounts i).capital - amount } with hNew
  have hrpnl : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).reserved_pnl) =
      (∑ j : AccountIdx, (s.accounts j).reserved_pnl) :=
    QEDGen.Solana.IndexedState.sum_update_proj_eq
      s.accounts i newAcc Account.reserved_pnl rfl
  have hbil : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).capital)
        + (s.accounts i).capital =
      (∑ j : AccountIdx, (s.accounts j).capital) + ((s.accounts i).capital - amount) :=
    QEDGen.Solana.IndexedState.sum_update_proj_bilinear
      s.accounts i newAcc Account.capital
  set Scap := ∑ j : AccountIdx, (s.accounts j).capital
  set Srpnl := ∑ j : AccountIdx, (s.accounts j).reserved_pnl
  -- `hg` gives `(s.accounts i).capital ≥ amount`; omega uses it for Nat subtraction.
  omega

theorem conservation_preserved_by_deposit_fee_credits (s s' : State) (signer : Pubkey) i amount
    (h_inv : conservation s) (h : deposit_fee_creditsTransition s signer i amount = some s') :
    conservation s' := by
  unfold deposit_fee_creditsTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  rw [QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with fee_credits := (s.accounts i).fee_credits + amount } Account.capital rfl,
      QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with fee_credits := (s.accounts i).fee_credits + amount } Account.reserved_pnl rfl]
  set Scap := ∑ j : AccountIdx, (s.accounts j).capital
  set Srpnl := ∑ j : AccountIdx, (s.accounts j).reserved_pnl
  omega

theorem conservation_preserved_by_convert_released_pnl (s s' : State) (signer : Pubkey) i x
    (h_inv : conservation s) (h : convert_released_pnlTransition s signer i x = some s') :
    conservation s' := by
  unfold convert_released_pnlTransition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  set newAcc : Account := { (s.accounts i) with reserved_pnl := (s.accounts i).reserved_pnl - x } with hNew
  have hcap : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).capital) =
      (∑ j : AccountIdx, (s.accounts j).capital) :=
    QEDGen.Solana.IndexedState.sum_update_proj_eq
      s.accounts i newAcc Account.capital rfl
  have hbil : (∑ j : AccountIdx,
        (Function.update s.accounts i newAcc j).reserved_pnl)
        + (s.accounts i).reserved_pnl =
      (∑ j : AccountIdx, (s.accounts j).reserved_pnl) + ((s.accounts i).reserved_pnl - x) :=
    QEDGen.Solana.IndexedState.sum_update_proj_bilinear
      s.accounts i newAcc Account.reserved_pnl
  set Scap := ∑ j : AccountIdx, (s.accounts j).capital
  set Srpnl := ∑ j : AccountIdx, (s.accounts j).reserved_pnl
  -- `(s.accounts i).reserved_pnl ≥ x` from hg; omega uses it.
  omega

theorem conservation_preserved_by_liquidate_case_1 (s s' : State) (signer : Pubkey) i
    (h_inv : conservation s) (h : liquidate_case_1Transition s signer i = some s') :
    conservation s' := by
  unfold liquidate_case_1Transition at h
  split_ifs at h with hg
  cases h
  unfold conservation at h_inv ⊢
  dsimp only
  rw [QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 0 } Account.capital rfl,
      QEDGen.Solana.IndexedState.sum_update_proj_eq s.accounts i
        { (s.accounts i) with active := 0 } Account.reserved_pnl rfl]
  exact h_inv

-- =========================================================================
-- account_solvent preservation — per-index case analysis
-- =========================================================================

-- Pattern used throughout: `forall_update_pres` reduces per-account invariants
-- to discharging the new value at the updated index; off-index slots inherit
-- from `h_inv` automatically.

theorem account_solvent_preserved_by_deposit (s s' : State) (signer : Pubkey) i amount
    (h_inv : account_solvent s) (h : depositTransition s signer i amount = some s') :
    account_solvent s' := by
  unfold depositTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  refine QEDGen.Solana.IndexedState.forall_update_pres
    (P := fun a : Account => a.active = 1 → (a.capital : Int) + a.pnl ≥ 0)
    s.accounts _ _ h_inv ?_
  intro h_active
  have h_orig := h_inv i h_active
  push_cast at h_orig ⊢
  omega

-- The add_user/add_lp handlers require `cap + pnl >= 0` as a precondition,
-- so the invariant holds by construction on the activated slot.
theorem account_solvent_preserved_by_add_user (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : add_userTransition s signer i = some s') :
    account_solvent s' := by
  unfold add_userTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  refine QEDGen.Solana.IndexedState.forall_update_pres
    (P := fun a : Account => a.active = 1 → (a.capital : Int) + a.pnl ≥ 0)
    s.accounts _ _ h_inv ?_
  intro _h_active
  dsimp only
  omega

theorem account_solvent_preserved_by_add_lp (s s' : State) (signer : Pubkey) i
    (h_inv : account_solvent s) (h : add_lpTransition s signer i = some s') :
    account_solvent s' := by
  unfold add_lpTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  refine QEDGen.Solana.IndexedState.forall_update_pres
    (P := fun a : Account => a.active = 1 → (a.capital : Int) + a.pnl ≥ 0)
    s.accounts _ _ h_inv ?_
  intro _h_active
  dsimp only
  omega

-- The withdraw handler requires `cap + pnl ≥ amount` in addition to
-- `cap ≥ amount`, so post-state equity `(cap - amount) + pnl ≥ 0`.
theorem account_solvent_preserved_by_withdraw (s s' : State) (signer : Pubkey) i amount
    (h_inv : account_solvent s) (h : withdrawTransition s signer i amount = some s') :
    account_solvent s' := by
  unfold withdrawTransition at h
  split_ifs at h with hg
  cases h
  unfold account_solvent at h_inv ⊢
  refine QEDGen.Solana.IndexedState.forall_update_pres
    (P := fun a : Account => a.active = 1 → (a.capital : Int) + a.pnl ≥ 0)
    s.accounts _ _ h_inv ?_
  intro h_active
  have h_orig := h_inv i h_active
  push_cast at h_orig ⊢
  omega

-- =========================================================================
-- Liveness: Draining ~> Active within 2 ops.
--
-- The liveness obligation is an existential: from any Draining state there
-- exist operations that reach Active in ≤ 2 steps. Codegen currently emits
-- a malformed universal form; this hand-written statement is the correct
-- one and will be replaced once lean_gen's render_liveness is fixed.
-- =========================================================================

def applyOps (s : State) (signer : Pubkey) : List Operation → Option State
  | [] => some s
  | op :: rest => match applyOp s signer op with
    | some s' => applyOps s' signer rest
    | none => none

theorem liveness_drain_completes (s : State) (signer : Pubkey)
    (h_signer : signer = s.authority) (h_start : s.status = .Draining) :
    ∃ ops s', ops.length ≤ 2 ∧ applyOps s signer ops = some s' ∧ s'.status = .Active := by
  -- Witnesses: apply [complete_drain, reset] from Draining.
  refine ⟨[.complete_drain, .reset], { s with status := .Active }, ?_, ?_, ?_⟩
  · decide
  · -- complete_drain (Draining → Resetting) then reset (Resetting → Active).
    show applyOps s signer [.complete_drain, .reset] = _
    unfold applyOps applyOp complete_drainTransition
    simp [h_signer, h_start]
    unfold applyOps applyOp resetTransition
    simp [applyOps]
  · rfl

end Percolator
