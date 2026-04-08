-- Pubkey memory predicates and frame lemmas for sBPF verification
--
-- sBPF programs compare pubkeys by loading four 8-byte chunks and branching
-- on each pair. This module provides memory predicates and frame lemmas
-- for the unified Pubkey type (defined in QEDGen.Solana.Account).

import QEDGen.Solana.Account
import QEDGen.Solana.SBPF.Memory
import QEDGen.Solana.SBPF.Region

namespace QEDGen.Solana.SBPF

open Memory
open QEDGen.Solana.Account

/-! ## Memory predicates -/

/-- A pubkey's four chunks reside at consecutive 8-byte addresses starting at `base`. -/
def pubkeyAt (mem : Mem) (base : Nat) (pk : Pubkey) : Prop :=
  readU64 mem base = pk.c0 ∧
  readU64 mem (base + 8) = pk.c1 ∧
  readU64 mem (base + 16) = pk.c2 ∧
  readU64 mem (base + 24) = pk.c3

/-- Memory equality preserves pubkeyAt. Use after proving `s'.mem = s.mem`
    for register-only instruction sections. -/
theorem pubkeyAt_of_mem_eq {mem₁ mem₂ : Mem} {base : Nat} {pk : Pubkey}
    (h_eq : mem₂ = mem₁) (h : pubkeyAt mem₁ base pk) :
    pubkeyAt mem₂ base pk := h_eq ▸ h

/-- pubkeyAt survives a U64 write at a disjoint address.
    The write must not overlap [base, base+32). -/
theorem pubkeyAt_writeU64_disjoint {mem : Mem} {base wAddr val : Nat} {pk : Pubkey}
    (h : pubkeyAt mem base pk)
    (hd : wAddr + 8 ≤ base ∨ base + 32 ≤ wAddr) :
    pubkeyAt (writeU64 mem wAddr val) base pk := by
  obtain ⟨h0, h1, h2, h3⟩ := h
  refine ⟨?_, ?_, ?_, ?_⟩
  · rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]; exact h0
  · rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]; exact h1
  · rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]; exact h2
  · rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]; exact h3

/-- pubkeyAt survives a U64 stack write when the pubkey is in the input region.
    Two-premise version: pubkey below STACK_START, write at or above STACK_START. -/
theorem pubkeyAt_writeU64_frame {mem : Mem} {base wAddr val : Nat} {pk : Pubkey}
    (h : pubkeyAt mem base pk)
    (h_r : base + 32 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    pubkeyAt (writeU64 mem wAddr val) base pk := by
  obtain ⟨h0, h1, h2, h3⟩ := h
  refine ⟨?_, ?_, ?_, ?_⟩
  · rw [readU64_writeU64_frame _ _ _ _ (by omega) h_w]; exact h0
  · rw [readU64_writeU64_frame _ _ _ _ (by omega) h_w]; exact h1
  · rw [readU64_writeU64_frame _ _ _ _ (by omega) h_w]; exact h2
  · rw [readU64_writeU64_frame _ _ _ _ (by omega) h_w]; exact h3

/-- pubkeyAt survives a chain of U64 stack writes. -/
theorem pubkeyAt_writeU64Chain_frame {mem : Mem} {base : Nat} {pk : Pubkey}
    (writes : List (Nat × Nat))
    (h : pubkeyAt mem base pk)
    (h_r : base + 32 ≤ STACK_START)
    (h_w : ∀ p ∈ writes, STACK_START ≤ p.1) :
    pubkeyAt (Region.writeU64Chain mem writes) base pk := by
  induction writes generalizing mem with
  | nil => exact h
  | cons hd tl ih =>
    dsimp only [Region.writeU64Chain]
    have h_tl : ∀ p ∈ tl, STACK_START ≤ p.1 :=
      fun p hp => h_w p (List.mem_cons_of_mem _ hp)
    exact ih (pubkeyAt_writeU64_frame h h_r (h_w hd (List.mem_cons_self ..))) h_tl

end QEDGen.Solana.SBPF
