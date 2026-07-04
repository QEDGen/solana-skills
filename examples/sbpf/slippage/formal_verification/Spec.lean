-- Formal verification of the asm-slippage program
--
-- Source: asm-slippage.s — a slippage guard that rejects transactions
-- when the token balance drops below a minimum threshold.

import SVM.SBPF
import Program

namespace SlippageProofs

open SVM.SBPF
open SVM.SBPF.Memory
open SlippageProg

/-! ## effectiveAddr lemmas -/

private theorem ea_min (b : Nat) : effectiveAddr b MINIMUM_BALANCE = b + 10520 := by
  unfold effectiveAddr MINIMUM_BALANCE; omega
private theorem ea_tok (b : Nat) : effectiveAddr b TOKEN_ACCOUNT_BALANCE = b + 160 := by
  unfold effectiveAddr TOKEN_ACCOUNT_BALANCE; omega

/-! ## Property P1: slippage rejection

SPEC.md §3.1 P1: When minimum_balance >= token_account_balance,
the program MUST exit with code 1.

The reject path logs "Slippage exceeded" before exiting. qedsvm ≥ v0.9.0
models `sol_log_` faithfully (guarded 17-byte read at r1, log push) instead
of a no-op, so the proof needs the message region readable: `h_rt_log`
covers `RODATA_e` (the asm2lean-laid-out address of the `.rodata` string
in the program region — see the layout note in Program.lean). -/

set_option maxHeartbeats 800000 in
theorem rejects_insufficient_balance
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (minBal tokenBal : Nat)
    (h_rt_min : rt.containsRange (inputAddr + 10520) 8 = true)
    (h_rt_tok : rt.containsRange (inputAddr + 160) 8 = true)
    (h_rt_log : rt.containsRange RODATA_e RODATA_e_LEN = true)
    (h_min : readU64 mem (inputAddr + 10520) = minBal)
    (h_tok : readU64 mem (inputAddr + 160) = tokenBal)
    (h_slip : minBal ≥ tokenBal) :
    (executeFn progAt (initState inputAddr mem rt) 10).exitCode = some 1 := by
  wp_exec [progAt] [ea_min, ea_tok]

/-! ## Property P2: slippage acceptance

SPEC.md §3.1 P2: When minimum_balance < token_account_balance,
the program MUST exit with code 0. -/

set_option maxHeartbeats 800000 in
theorem accepts_sufficient_balance
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (minBal tokenBal : Nat)
    (h_rt_min : rt.containsRange (inputAddr + 10520) 8 = true)
    (h_rt_tok : rt.containsRange (inputAddr + 160) 8 = true)
    (h_min : readU64 mem (inputAddr + 10520) = minBal)
    (h_tok : readU64 mem (inputAddr + 160) = tokenBal)
    (h_ok : minBal < tokenBal) :
    (executeFn progAt (initState inputAddr mem rt) 10).exitCode = some 0 := by
  have h_not_ge : ¬(minBal ≥ tokenBal) := by omega
  wp_exec [progAt] [ea_min, ea_tok]

end SlippageProofs
