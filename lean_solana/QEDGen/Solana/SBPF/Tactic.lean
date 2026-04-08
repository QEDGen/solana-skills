-- Automation for sBPF proof unrolling
--
-- Provides simp improvements for effectiveAddr and readByWidth.

import QEDGen.Solana.SBPF.Execute

namespace QEDGen.Solana.SBPF

open Memory

/-! ## Simplification improvements -/

/-- effectiveAddr with non-negative offset reduces to plain Nat addition.
    Eliminates the Int.toNat roundtrip for the common case. -/
@[simp] theorem effectiveAddr_nat (base off : Nat) :
    effectiveAddr base (↑off) = base + off := by
  unfold effectiveAddr; omega

-- Make readByWidth auto-simplify (dispatches to readU8/readU16/readU32/readU64)
attribute [simp] Memory.readByWidth

end QEDGen.Solana.SBPF
