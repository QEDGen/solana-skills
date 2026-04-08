-- Pubkey4: 32-byte public key as four U64 chunks
--
-- sBPF programs compare pubkeys by loading four 8-byte chunks and branching
-- on each pair. This module bundles the four chunks into a structure with
-- memory predicates and frame lemmas, reducing hypothesis threading in proofs.

import QEDGen.Solana.SBPF.Memory
import QEDGen.Solana.SBPF.Region

namespace QEDGen.Solana.SBPF

open Memory

/-- A 32-byte public key stored as four 8-byte (U64) chunks in little-endian memory.
    Matches the sBPF pattern of comparing pubkeys via four `ldx.dw` + `jne` pairs. -/
structure Pubkey4 where
  c0 : Nat
  c1 : Nat
  c2 : Nat
  c3 : Nat
  deriving DecidableEq, BEq, Repr, Inhabited

theorem Pubkey4.ext' {a b : Pubkey4}
    (h0 : a.c0 = b.c0) (h1 : a.c1 = b.c1) (h2 : a.c2 = b.c2) (h3 : a.c3 = b.c3) :
    a = b := by
  cases a; cases b; simp_all

/-- Two pubkeys differ iff at least one chunk differs. -/
theorem Pubkey4.ne_iff {a b : Pubkey4} :
    a ≠ b ↔ a.c0 ≠ b.c0 ∨ a.c1 ≠ b.c1 ∨ a.c2 ≠ b.c2 ∨ a.c3 ≠ b.c3 := by
  constructor
  · intro h
    if h0 : a.c0 = b.c0 then
      if h1 : a.c1 = b.c1 then
        if h2 : a.c2 = b.c2 then
          if h3 : a.c3 = b.c3 then
            exact absurd (Pubkey4.ext' h0 h1 h2 h3) h
          else exact Or.inr (Or.inr (Or.inr h3))
        else exact Or.inr (Or.inr (Or.inl h2))
      else exact Or.inr (Or.inl h1)
    else exact Or.inl h0
  · intro h heq; subst heq
    cases h with
    | inl h => exact h rfl
    | inr h => cases h with
      | inl h => exact h rfl
      | inr h => cases h with
        | inl h => exact h rfl
        | inr h => exact h rfl

/-! ## Memory predicates -/

/-- A pubkey's four chunks reside at consecutive 8-byte addresses starting at `base`. -/
def pubkeyAt (mem : Mem) (base : Nat) (pk : Pubkey4) : Prop :=
  readU64 mem base = pk.c0 ∧
  readU64 mem (base + 8) = pk.c1 ∧
  readU64 mem (base + 16) = pk.c2 ∧
  readU64 mem (base + 24) = pk.c3

/-- Memory equality preserves pubkeyAt. Use after proving `s'.mem = s.mem`
    for register-only instruction sections. -/
theorem pubkeyAt_of_mem_eq {mem₁ mem₂ : Mem} {base : Nat} {pk : Pubkey4}
    (h_eq : mem₂ = mem₁) (h : pubkeyAt mem₁ base pk) :
    pubkeyAt mem₂ base pk := h_eq ▸ h

/-- pubkeyAt survives a U64 write at a disjoint address.
    The write must not overlap [base, base+32). -/
theorem pubkeyAt_writeU64_disjoint {mem : Mem} {base wAddr val : Nat} {pk : Pubkey4}
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
theorem pubkeyAt_writeU64_frame {mem : Mem} {base wAddr val : Nat} {pk : Pubkey4}
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
theorem pubkeyAt_writeU64Chain_frame {mem : Mem} {base : Nat} {pk : Pubkey4}
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
