-- Formal verification of the asm-slippage program
--
-- Source: asm-slippage.s — a slippage guard that rejects transactions
-- when the token balance drops below a minimum threshold.

import QEDGen.Solana.SBPF

namespace SlippageProofs

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

/-! ## Program transcription

Translate asm-slippage.s into a Program array. Jump targets are absolute
instruction indices: `end` label maps to index 4. -/

@[simp] def prog : Program := #[
  .ldx .dword .r3 .r1 0x2918,   -- 0: r3 = minimum_balance
  .ldx .dword .r4 .r1 0x00a0,   -- 1: r4 = token_account_balance
  .jge .r3 (.reg .r4) 4,        -- 2: if min >= bal, jump to error (index 4)
  .exit,                          -- 3: success (r0 = 0)
  .lddw .r1 0,                   -- 4: error msg addr
  .lddw .r2 17,                  -- 5: error msg len
  .call .sol_log_,                -- 6: log error
  .lddw .r0 1,                   -- 7: set error code
  .exit                           -- 8: error exit
]

/-! ## Property P1: slippage rejection

SPEC.md §3.1 P1: When minimum_balance >= token_account_balance,
the program MUST exit with code 1. -/

set_option maxHeartbeats 8000000 in
theorem rejects_insufficient_balance
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (effectiveAddr inputAddr 0x2918) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr 0x00a0) = tokenBal)
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
    (h_min : readU64 mem (effectiveAddr inputAddr 0x2918) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr 0x00a0) = tokenBal)
    (h_ok : minBal < tokenBal) :
    (execute prog (initState inputAddr mem) 10).exitCode = some 0 := by
  have h_not_ge : ¬(minBal ≥ tokenBal) := by omega
  sbpf_steps

end SlippageProofs
