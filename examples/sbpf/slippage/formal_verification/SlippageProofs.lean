-- Formal verification of the asm-slippage program
--
-- Source: asm-slippage.s — a slippage guard that rejects transactions
-- when the token balance drops below a minimum threshold.

import QEDGen.Solana.SBPF
import SlippageProg

namespace SlippageProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open SlippageProg

/-! ## effectiveAddr lemmas -/

private theorem ea_min (b : Nat) : effectiveAddr b MINIMUM_BALANCE = b + 10520 := by
  unfold effectiveAddr MINIMUM_BALANCE; omega
private theorem ea_tok (b : Nat) : effectiveAddr b TOKEN_ACCOUNT_BALANCE = b + 160 := by
  unfold effectiveAddr TOKEN_ACCOUNT_BALANCE; omega

/-! ## Property P1: slippage rejection

SPEC.md §3.1 P1: When minimum_balance >= token_account_balance,
the program MUST exit with code 1. -/

set_option maxHeartbeats 800000 in
theorem rejects_insufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (inputAddr + 10520) = minBal)
    (h_tok : readU64 mem (inputAddr + 160) = tokenBal)
    (h_slip : minBal ≥ tokenBal) :
    (executeFn progAt (initState inputAddr mem) 10).exitCode = some 1 := by
  wp_exec [progAt] [ea_min, ea_tok]

/-! ## Property P2: slippage acceptance

SPEC.md §3.1 P2: When minimum_balance < token_account_balance,
the program MUST exit with code 0. -/

set_option maxHeartbeats 800000 in
theorem accepts_sufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (inputAddr + 10520) = minBal)
    (h_tok : readU64 mem (inputAddr + 160) = tokenBal)
    (h_ok : minBal < tokenBal) :
    (executeFn progAt (initState inputAddr mem) 10).exitCode = some 0 := by
  have h_not_ge : ¬(minBal ≥ tokenBal) := by omega
  wp_exec [progAt] [ea_min, ea_tok]

end SlippageProofs
