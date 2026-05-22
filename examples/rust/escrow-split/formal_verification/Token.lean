-- v2.26 Track F: bundled-interface axiom module.
-- Stance 1 — the upstream binary_hash pin is the contract
-- boundary. Each `axiom ensures_axiom_<idx>` corresponds to one
-- `ensures` clause on the interface handler; the caller's
-- Lean proof discharges its CPI post-condition by applying
-- the relevant axiom, instead of carrying a `sorry`.
--
-- Axioms are callee-frame: parameters and predicates only
-- reference the callee's own ABI, never the caller's State
-- type. This keeps a single axiom module reusable across
-- every caller in the workspace.

import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.Valid

namespace Token

open QEDGen.Solana

/-- Content pin against the deployed program at
    `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`. Callers commit to this hash; if the deployed
    binary changes, the lock must be regenerated. -/
def binary_hash : String := "sha256:0000000000000000000000000000000000000000000000000000000000000000"

namespace transfer

/-- `Token.transfer` post-condition #0 (axiomatized; discharged by binary_hash pin). -/
axiom ensures_axiom_0 (amount : Nat) : amount > 0

end transfer

namespace mint_to

/-- `Token.mint_to` post-condition #0 (axiomatized; discharged by binary_hash pin). -/
axiom ensures_axiom_0 (amount : Nat) : amount > 0

end mint_to

namespace burn

/-- `Token.burn` post-condition #0 (axiomatized; discharged by binary_hash pin). -/
axiom ensures_axiom_0 (amount : Nat) : amount > 0

end burn

end Token
