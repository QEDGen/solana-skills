/-
Proofs.lean — user-owned preservation proofs for Multisig.

`qedgen codegen` bootstraps this file once and never touches it again.
Spec.lean is regenerated; this file is durable. `qedgen check`
(and `qedgen reconcile`) flag orphan theorems (handler removed from
spec) and missing obligations (new `preserved_by` declared).

Spec.lean owns each obligation statement as `def <name>_stmt : Prop`;
every theorem here types against its `_stmt`, so a statement cannot
drift from the spec — only the proof body is hand-written.
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

theorem threshold_bounded_preserved_by_create_vault :
    threshold_bounded_preserved_by_create_vault_stmt := by
  intro s s' signer threshold member_count _h_inv h
  unfold create_vaultTransition at h
  split_ifs at h with hg
  cases h
  unfold threshold_bounded
  exact ⟨hg.2.2.1.2, hg.2.2.1.1⟩

theorem threshold_bounded_preserved_by_propose :
    threshold_bounded_preserved_by_propose_stmt := by
  intro s s' signer h_inv h
  unfold proposeTransition at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_approve :
    threshold_bounded_preserved_by_approve_stmt := by
  intro s s' signer member_index h_inv h
  unfold approveTransition at h
  simp only at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_reject :
    threshold_bounded_preserved_by_reject_stmt := by
  intro s s' signer member_index h_inv h
  unfold rejectTransition at h
  simp only at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_execute :
    threshold_bounded_preserved_by_execute_stmt := by
  intro s s' signer member_index h_inv h
  unfold executeTransition at h
  simp only at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_cancel_proposal :
    threshold_bounded_preserved_by_cancel_proposal_stmt := by
  intro s s' signer h_inv h
  unfold cancel_proposalTransition at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_add_member :
    threshold_bounded_preserved_by_add_member_stmt := by
  intro s s' signer member_index member_pubkey h_inv h
  -- add_member rewrites `members`/`status` only; threshold and member_count
  -- pass through untouched.
  unfold add_memberTransition at h
  split_ifs at h
  cases h
  exact h_inv

theorem threshold_bounded_preserved_by_remove_member :
    threshold_bounded_preserved_by_remove_member_stmt := by
  intro s s' signer h_inv h
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

theorem votes_bounded_preserved_by_create_vault :
    votes_bounded_preserved_by_create_vault_stmt := by
  intro s s' signer threshold member_count _h_inv h
  unfold create_vaultTransition at h
  split_ifs at h
  cases h
  unfold votes_bounded
  -- s'.approval_count = 0, s'.rejection_count = 0 ⇒ 0 ≤ s'.member_count
  dsimp only; omega

theorem votes_bounded_preserved_by_propose :
    votes_bounded_preserved_by_propose_stmt := by
  intro s s' signer _h_inv h
  unfold proposeTransition at h
  split_ifs at h
  cases h
  unfold votes_bounded
  dsimp only; omega

theorem votes_bounded_preserved_by_execute :
    votes_bounded_preserved_by_execute_stmt := by
  intro s s' signer member_index _h_inv h
  unfold executeTransition at h
  simp only at h
  split_ifs at h
  cases h
  unfold votes_bounded
  dsimp only; omega

theorem votes_bounded_preserved_by_cancel_proposal :
    votes_bounded_preserved_by_cancel_proposal_stmt := by
  intro s s' signer _h_inv h
  unfold cancel_proposalTransition at h
  split_ifs at h
  cases h
  unfold votes_bounded
  dsimp only; omega

theorem votes_bounded_preserved_by_remove_member :
    votes_bounded_preserved_by_remove_member_stmt := by
  intro s s' signer _h_inv h
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
