-- Region-based memory frame for sBPF verification
--
-- sBPF memory is partitioned into regions (input < stack < heap).
-- Writes to one region don't affect reads from another.
--
-- This module provides:
--   1. Chain frame lemmas: strip N writes in one shot
--   2. The `mem_frame` tactic: automatic region-based write stripping
--   3. SbpfMem: optional region-typed wrapper for compositional proofs
--
-- The key optimization over strip_writes: mem_frame pre-unfolds STACK_START
-- once and uses two-premise frame lemmas (h_r + h_w) instead of trying
-- 20+ lemma alternatives with omega for each write layer.

import QEDGen.Solana.SBPF.Memory

namespace QEDGen.Solana.SBPF.Region

open QEDGen.Solana.SBPF.Memory

/-! ## Chain frame: strip N writes in one shot

`writeU64Chain mem writes` applies a list of U64 writes to memory.
Definitionally equal to nested writeU64 calls, so `change` works. -/

/-- Apply a list of U64 writes to memory.
    writeU64Chain mem [(a₁,v₁), (a₂,v₂)] = writeU64 (writeU64 mem a₁ v₁) a₂ v₂ -/
def writeU64Chain (mem : Mem) : List (Nat × Nat) → Mem
  | [] => mem
  | (a, v) :: rest => writeU64Chain (writeU64 mem a v) rest

@[simp] theorem writeU64Chain_nil (mem : Mem) :
    writeU64Chain mem [] = mem := rfl

@[simp] theorem writeU64Chain_cons (mem : Mem) (a v : Nat) (rest : List (Nat × Nat)) :
    writeU64Chain mem ((a, v) :: rest) = writeU64Chain (writeU64 mem a v) rest := rfl

/-- readU64 from below stack through a chain of U64 writes above stack. -/
theorem readU64_writeU64Chain_frame (mem : Mem) (rAddr : Nat) (writes : List (Nat × Nat))
    (h_r : rAddr + 8 ≤ STACK_START)
    (h_w : ∀ p ∈ writes, STACK_START ≤ p.1) :
    readU64 (writeU64Chain mem writes) rAddr = readU64 mem rAddr := by
  induction writes generalizing mem with
  | nil => rfl
  | cons hd tl ih =>
    dsimp only [writeU64Chain]
    have h_tl : ∀ p ∈ tl, STACK_START ≤ p.1 :=
      fun p hp => h_w p (List.mem_cons_of_mem _ hp)
    rw [ih (writeU64 mem hd.1 hd.2) h_tl]
    exact readU64_writeU64_frame _ _ _ _ h_r (h_w hd (List.mem_cons_self ..))

/-- readU32 from below stack through a chain of U64 writes above stack. -/
theorem readU32_writeU64Chain_frame (mem : Mem) (rAddr : Nat) (writes : List (Nat × Nat))
    (h_r : rAddr + 4 ≤ STACK_START)
    (h_w : ∀ p ∈ writes, STACK_START ≤ p.1) :
    readU32 (writeU64Chain mem writes) rAddr = readU32 mem rAddr := by
  induction writes generalizing mem with
  | nil => rfl
  | cons hd tl ih =>
    dsimp only [writeU64Chain]
    have h_tl : ∀ p ∈ tl, STACK_START ≤ p.1 :=
      fun p hp => h_w p (List.mem_cons_of_mem _ hp)
    rw [ih (writeU64 mem hd.1 hd.2) h_tl]
    exact readU32_writeU64_frame _ _ _ _ h_r (h_w hd (List.mem_cons_self ..))

/-- readU8 from below stack through a chain of U64 writes above stack. -/
theorem readU8_writeU64Chain_frame (mem : Mem) (rAddr : Nat) (writes : List (Nat × Nat))
    (h_r : rAddr + 1 ≤ STACK_START)
    (h_w : ∀ p ∈ writes, STACK_START ≤ p.1) :
    readU8 (writeU64Chain mem writes) rAddr = readU8 mem rAddr := by
  induction writes generalizing mem with
  | nil => rfl
  | cons hd tl ih =>
    dsimp only [writeU64Chain]
    have h_tl : ∀ p ∈ tl, STACK_START ≤ p.1 :=
      fun p hp => h_w p (List.mem_cons_of_mem _ hp)
    rw [ih (writeU64 mem hd.1 hd.2) h_tl]
    exact readU8_writeU64_frame _ _ _ _ h_r (h_w hd (List.mem_cons_self ..))

/-! ## mem_frame tactic

Strips write layers from read expressions using region separation.

Two modes:
1. **Below-above** (most common): read from input region (< STACK_START),
   write to stack (≥ STACK_START). Uses two-premise frame lemmas.
2. **Within-stack**: read and write both in stack at different offsets.
   Falls back to standard disjointness lemmas.

Key optimization: unfolds STACK_START once at the start instead of
per-alternative, then all omega calls work on pure numerals. -/

syntax "mem_frame" : tactic

macro_rules
  | `(tactic| mem_frame) => `(tactic| (
    -- Pre-unfold region constants so omega sees numerals
    try unfold STACK_START belowStack at *;
    -- Strip all write layers
    repeat (first
      -- Below-above frame: read below stack, write above stack
      -- Try U64 reads first (most common in sBPF)
      | rw [readU64_writeU64_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU64_writeU32_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU64_writeU16_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU64_writeU8_frame  _ _ _ _ (by omega) (by omega)]
      -- U32 reads
      | rw [readU32_writeU64_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU32_writeU32_frame _ _ _ _ (by omega) (by omega)]
      -- U8 reads
      | rw [readU8_writeU64_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU8_writeU32_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU8_writeU16_frame _ _ _ _ (by omega) (by omega)]
      | rw [readU8_writeU8_frame  _ _ _ _ (by omega) (by omega)]
      -- Within-stack: different addresses (no region shortcut)
      | rw [readU64_writeU64_disjoint _ _ _ _ (by omega)]
      | rw [readU64_writeU32_disjoint _ _ _ _ (by omega)]
      | rw [readU64_writeU8_disjoint  _ _ _ _ (by omega)]
      | rw [readU8_writeU64_outside  _ _ _ _ (by omega)]
      | rw [readU8_writeU32_outside  _ _ _ _ (by omega)]
      | rw [readU8_writeU8_disjoint  _ _ _ _ (by omega)]
      -- Same-address reads (for reading back written values)
      | rw [readU64_writeU64_same _ _ _ (by first | simp | omega)]
      | rw [readU32_writeU32_same _ _ _ (by first | simp | omega)]
      | rw [readU8_writeU8_same   _ _ _ (by first | simp | omega)])))

end QEDGen.Solana.SBPF.Region
