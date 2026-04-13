-- Integration test: qedspec + qedbridge with an inline sBPF program
-- Validates the full workflow: spec → bridge → type-correct refinement theorems
--
-- Note: the wp_exec proof filling step is validated separately in
-- examples/sbpf/slippage/formal_verification/ where the program module
-- is pre-compiled (required for kernel depth limits).

import QEDGen

open QEDGen.Solana
open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

-- ============================================================================
-- Step 1: Inline minimal program (mimics slippage guard)
--
-- Reads two U64 values from memory, compares them, exits 0 or 1.
-- prog[0]: ldx.dw r3, r1, 0x2918  (load min_balance)
-- prog[1]: ldx.dw r4, r1, 0xa0    (load token_balance)
-- prog[2]: jge r3, r4, +4          (if min >= tok, jump to error)
-- prog[3]: exit                     (exit 0 — success)
-- prog[4-8]: error path (exit 1)
-- ============================================================================

namespace TestProg

abbrev TOKEN_BALANCE_OFF : Int := 0xa0
abbrev MIN_BALANCE_OFF : Int := 0x2918

@[simp] def progAt : Nat → Option QEDGen.Solana.SBPF.Insn
  | 0 => some (.ldx .dword .r3 .r1 MIN_BALANCE_OFF)
  | 1 => some (.ldx .dword .r4 .r1 TOKEN_BALANCE_OFF)
  | 2 => some (.jge .r3 (.reg .r4) 4)
  | 3 => some .exit
  | 4 => some (.lddw .r1 0)
  | 5 => some (.lddw .r2 17)
  | 6 => some (.call .sol_log_)
  | 7 => some (.lddw .r0 1)
  | 8 => some .exit
  | _ => none

end TestProg

-- ============================================================================
-- Step 2: Abstract spec for the guard
-- ============================================================================

qedspec SlippageGuard where
  state
    token_balance : U64
    min_balance : U64
  operation check
    guard: "s.min_balance < s.token_balance"

-- ============================================================================
-- Step 3: Bridge connecting spec to sBPF memory layout
-- ============================================================================

qedbridge SlippageGuard where
  input: r1
  fuel: 10
  layout
    token_balance U64 at 160
    min_balance U64 at 10520
  operations
    check discriminator 0

-- ============================================================================
-- Step 4: Verify generated definitions and types
-- ============================================================================

-- Bridge constants
#check @SlippageGuard.Bridge.TOKEN_BALANCE_OFF   -- Nat
#check @SlippageGuard.Bridge.MIN_BALANCE_OFF     -- Nat
#check @SlippageGuard.Bridge.FUEL                -- Nat

-- Encode/decode
#check @SlippageGuard.Bridge.encodeState  -- State → Nat → Mem → Prop
#check @SlippageGuard.Bridge.decodeState  -- Nat → Mem → State
#check @SlippageGuard.Bridge.decode_encode

-- Refinement theorems have the correct types
#check @SlippageGuard.Bridge.check.refines TestProg.progAt
#check @SlippageGuard.Bridge.check.rejects TestProg.progAt

-- ============================================================================
-- Step 5: Prove bridge decomposition (spec → memory reads)
--
-- This demonstrates the bridge connection: given encodeState and a failed
-- guard, we derive concrete memory inequalities.
-- ============================================================================

theorem bridge_decomposes
    (inputAddr : Nat) (mem : Mem)
    (s : SlippageGuard.State) (signer : Pubkey)
    (h_encode : SlippageGuard.Bridge.encodeState s inputAddr mem)
    (h_fail : SlippageGuard.checkTransition s signer = none) :
    readU64 mem (inputAddr + 10520) ≥ readU64 mem (inputAddr + 160) := by
  unfold SlippageGuard.Bridge.encodeState at h_encode
  obtain ⟨h_tok, h_min⟩ := h_encode
  have h_not_lt : ¬(s.min_balance < s.token_balance) := by
    intro h_lt; unfold SlippageGuard.checkTransition at h_fail; simp [h_lt] at h_fail
  rw [h_tok, h_min]; omega
