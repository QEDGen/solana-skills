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

/-! ## Property P1: slippage rejection

SPEC.md §3.1 P1: When minimum_balance >= token_account_balance,
the program MUST exit with code 1. -/

set_option maxHeartbeats 8000000 in
theorem rejects_insufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (effectiveAddr inputAddr MINIMUM_BALANCE) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr TOKEN_ACCOUNT_BALANCE) = tokenBal)
    (h_slip : minBal ≥ tokenBal) :
    (execute prog (initState inputAddr mem) 10).exitCode = some 1 := by
  sbpf_steps

/-! ## Property P2: slippage acceptance

SPEC.md §3.1 P2: When minimum_balance < token_account_balance,
the program MUST exit with code 0. -/

set_option maxHeartbeats 4000000 in
theorem accepts_sufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (effectiveAddr inputAddr MINIMUM_BALANCE) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr TOKEN_ACCOUNT_BALANCE) = tokenBal)
    (h_ok : minBal < tokenBal) :
    (execute prog (initState inputAddr mem) 10).exitCode = some 0 := by
  have h_not_ge : ¬(minBal ≥ tokenBal) := by omega
  sbpf_steps

end SlippageProofs
