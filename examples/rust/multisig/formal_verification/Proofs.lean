/-
Proofs.lean — user-owned preservation proofs.

`qedgen codegen` bootstraps this file once and never touches it again.
Spec.lean is regenerated; this file is durable. `qedgen check`
(and `qedgen reconcile`) flag orphan theorems (handler removed from
spec) and missing obligations (new `preserved_by` declared).
-/
import Spec

namespace Multisig

open QEDGen.Solana

-- Preservation obligations the spec expects.
-- Write each theorem against the signature generated in Spec.lean
-- (the handler's transition + the property predicate). Close with
-- tactics like `unfold`, `omega`, or `simp_all` as appropriate, or
-- `QEDGen.Solana.IndexedState.forall_update_pres` for per-account
-- invariants in Map-backed specs.
--
--   theorem threshold_bounded_preserved_by_approve
--   theorem threshold_bounded_preserved_by_cancel_proposal
--   theorem threshold_bounded_preserved_by_create_vault
--   theorem threshold_bounded_preserved_by_execute
--   theorem threshold_bounded_preserved_by_propose
--   theorem threshold_bounded_preserved_by_reject
--   theorem threshold_bounded_preserved_by_remove_member
--   theorem votes_bounded_preserved_by_approve
--   theorem votes_bounded_preserved_by_cancel_proposal
--   theorem votes_bounded_preserved_by_create_vault
--   theorem votes_bounded_preserved_by_execute
--   theorem votes_bounded_preserved_by_propose
--   theorem votes_bounded_preserved_by_reject
--   theorem votes_bounded_preserved_by_remove_member

end Multisig
