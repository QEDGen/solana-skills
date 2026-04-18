/-
Proofs.lean — user-owned preservation proofs.

`qedgen codegen` bootstraps this file once and never touches it again.
Spec.lean is regenerated; this file is durable. `qedgen check`
(and `qedgen reconcile`) flag orphan theorems (handler removed from
spec) and missing obligations (new `preserved_by` declared).
-/
import Spec

namespace Escrow

open QEDGen.Solana

-- No preservation obligations declared by the spec.
-- Add `property <name> preserved_by [...]` blocks to the `.qedspec`
-- and `qedgen check` will list the new obligations here.

end Escrow
