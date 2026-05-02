/-
Proofs.lean — user-owned preservation proofs for Multisig.

`qedgen codegen` bootstraps this file once and never touches it again.
Spec.lean is regenerated; this file is durable. `qedgen check`
(and `qedgen reconcile`) flag orphan theorems (handler removed from
spec) and missing obligations (new `preserved_by` declared).
-/
import Spec

namespace Multisig

open QEDGen.Solana

-- =========================================================================
-- threshold_bounded (s.threshold ≤ s.member_count ∧ s.threshold > 0)
-- =========================================================================
-- create_vault sets both fields under guard; everything else either leaves
-- them untouched or only decrements member_count under a guard that proves
-- the new value still ≥ threshold.

theorem threshold_bounded_preserved_by_create_vault
    (s s' : State) (signer : Pubkey) (threshold member_count : Nat)
    (_h_inv : threshold_bounded s)
    (h : create_vaultTransition s signer threshold member_count = some s') :
    threshold_bounded s' := by
  unfold create_vaultTransition at h
  split_ifs at h with hg
  cases h
  unfold threshold_bounded
  exact ⟨hg.2.2.1.2, hg.2.2.1.1⟩

theorem threshold_bounded_preserved_by_propose
    (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s)
    (h : proposeTransition s signer = some s') :
    threshold_bounded s' := by
  unfold proposeTransition at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_approve
    (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS)
    (h_inv : threshold_bounded s)
    (h : approveTransition s signer member_index = some s') :
    threshold_bounded s' := by
  unfold approveTransition at h
  simp only at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_reject
    (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS)
    (h_inv : threshold_bounded s)
    (h : rejectTransition s signer member_index = some s') :
    threshold_bounded s' := by
  unfold rejectTransition at h
  simp only at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_execute
    (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS)
    (h_inv : threshold_bounded s)
    (h : executeTransition s signer member_index = some s') :
    threshold_bounded s' := by
  unfold executeTransition at h
  simp only at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_cancel_proposal
    (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s)
    (h : cancel_proposalTransition s signer = some s') :
    threshold_bounded s' := by
  unfold cancel_proposalTransition at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_remove_member
    (s s' : State) (signer : Pubkey)
    (h_inv : threshold_bounded s)
    (h : remove_memberTransition s signer = some s') :
    threshold_bounded s' := by
  unfold remove_memberTransition at h
  split_ifs at h with hg
  cases h
  obtain ⟨_h_thresh_le, h_thresh_pos⟩ := h_inv
  -- hg.2.2.1 : s.member_count > s.threshold  ⇒  s.threshold ≤ s.member_count - 1
  -- threshold itself is untouched, so positivity carries. dsimp reduces
  -- the `{ s with ... }`.field projections so omega can see the integers.
  refine ⟨?_, ?_⟩
  · dsimp only; omega
  · dsimp only; exact h_thresh_pos

-- =========================================================================
-- votes_bounded (s.approval_count + s.rejection_count ≤ s.member_count)
-- =========================================================================
-- The spec restricts `preserved_by` to handlers that preserve this property
-- from `votes_bounded` alone: create_vault, propose, execute, cancel_proposal,
-- remove_member — all of which either zero out both counters or hold them
-- constant under a guard. `approve` and `reject` increment counters by 1 each
-- and would need an auxiliary invariant linking the running totals to the
-- per-slot `voted` bitmap; see the comment on `votes_bounded` in
-- multisig.qedspec for why those obligations are excluded.

theorem votes_bounded_preserved_by_create_vault
    (s s' : State) (signer : Pubkey) (threshold member_count : Nat)
    (_h_inv : votes_bounded s)
    (h : create_vaultTransition s signer threshold member_count = some s') :
    votes_bounded s' := by
  unfold create_vaultTransition at h
  split_ifs at h
  cases h
  unfold votes_bounded
  -- s'.approval_count = 0, s'.rejection_count = 0 ⇒ 0 ≤ s'.member_count
  dsimp only; omega

theorem votes_bounded_preserved_by_propose
    (s s' : State) (signer : Pubkey)
    (_h_inv : votes_bounded s)
    (h : proposeTransition s signer = some s') :
    votes_bounded s' := by
  unfold proposeTransition at h
  split_ifs at h
  cases h
  unfold votes_bounded
  dsimp only; omega

theorem votes_bounded_preserved_by_execute
    (s s' : State) (signer : Pubkey) (member_index : Fin MAX_MEMBERS)
    (_h_inv : votes_bounded s)
    (h : executeTransition s signer member_index = some s') :
    votes_bounded s' := by
  unfold executeTransition at h
  simp only at h
  split_ifs at h
  cases h
  unfold votes_bounded
  dsimp only; omega

theorem votes_bounded_preserved_by_cancel_proposal
    (s s' : State) (signer : Pubkey)
    (_h_inv : votes_bounded s)
    (h : cancel_proposalTransition s signer = some s') :
    votes_bounded s' := by
  unfold cancel_proposalTransition at h
  split_ifs at h
  cases h
  unfold votes_bounded
  dsimp only; omega

theorem votes_bounded_preserved_by_remove_member
    (s s' : State) (signer : Pubkey)
    (_h_inv : votes_bounded s)
    (h : remove_memberTransition s signer = some s') :
    votes_bounded s' := by
  unfold remove_memberTransition at h
  split_ifs at h with hg
  cases h
  unfold votes_bounded
  -- Guard: approval_count = 0 ∧ rejection_count = 0 zeros both counters
  -- (independently of member_count's decrement). dsimp reduces struct
  -- projections so omega can use the guard equalities directly.
  obtain ⟨_h_creator, _h_status, _h_mc_gt, h_app_zero, h_rej_zero⟩ := hg
  dsimp only
  omega

end Multisig
