-- Loom integration for sBPF WP reasoning
--
-- Connects our SbpfM (StateT State Id) to Loom's monad algebra framework:
-- - MAlgOrdered instance gives us the WP algebra automatically
-- - MAlg.lift provides the canonical WP function
-- - loom_solve can discharge arithmetic VCs via SMT (z3/cvc5)
--
-- This file bridges our lightweight wp function (SBPF/WP.lean) to Loom's
-- richer infrastructure. Import this when you need SMT-backed reasoning.

import QEDGen.Solana.SBPF.WP
import Loom.MonadAlgebras.Instances.StateT
import Loom.MonadAlgebras.Defs

namespace QEDGen.Solana.SBPF

/-! ## Loom integration

For SbpfM = StateT State Id, Loom automatically provides:
  MAlgOrdered (StateT State Id) (State → Prop)

This means MAlg.lift : SbpfM α → (α → State → Prop) → State → Prop
is available as the canonical WP function from the monad algebra. -/

/-- Our lightweight wp and Loom's MAlg.lift agree on SbpfM computations.
    This confirms the two WP definitions are compatible. -/
theorem wp_eq_lift (c : SbpfM α) (post : α → State → Prop) (s : State) :
    wp c post s = MAlg.lift c post s := by
  simp [wp, MAlg.lift, MAlg.μ, MAlgOrdered.μ, Functor.map, StateT.map]
  rfl

end QEDGen.Solana.SBPF
