-- sBPF v3 JMP32 fixture (#429): proofs over the asm2lean lift of guard.s.
--
-- Both guards are 32-bit jumps. They compare only the low 32 bits of each
-- register, so the tag check passes for any u64 whose low word is 1.
--
-- Each loaded value is bound to a variable (`h_ld_*`), as in the other sBPF
-- examples. Each branch outcome is stated as a `jump32Holds` fact, which
-- `wp_exec` uses to pick the branch.

import SVM.SBPF
import Program

namespace GuardProofs

open SVM.SBPF
open SVM.SBPF.Memory
open GuardProg

/-! ## P1: low tag word is not 1 → error 1 (jne32, immediate source)

   Path: 0 → 1 → 2 → 6 → 7 -/

set_option maxHeartbeats 800000 in
theorem rejects_bad_tag
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (tag limit : Nat)
    (h_rt_tag : rt.containsRange (inputAddr + 96) 8 = true)
    (h_rt_lim : rt.containsRange (inputAddr + 104) 8 = true)
    (h_ld_tag : readU64 mem (inputAddr + 96) = tag)
    (h_ld_lim : readU64 mem (inputAddr + 104) = limit)
    (h_tag : tag % U32_MODULUS ≠ TAG) :
    (executeFn progAt (initState inputAddr mem rt) 8).exitCode = some E_TAG := by
  have h_jne : jump32Holds .ne tag TAG = true := by
    simp only [jump32Holds, TAG, U32_MODULUS] at h_tag ⊢; simp; omega
  wp_exec [progAt] [U32_MODULUS]

/-! ## P2: tag passes, limit's low word below the tag's → error 2
   (jlt32, register source)

   Path: 0 → 1 → 2 → 3 → 8 → 9 -/

set_option maxHeartbeats 800000 in
theorem rejects_limit_below_tag
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (tag limit : Nat)
    (h_rt_tag : rt.containsRange (inputAddr + 96) 8 = true)
    (h_rt_lim : rt.containsRange (inputAddr + 104) 8 = true)
    (h_ld_tag : readU64 mem (inputAddr + 96) = tag)
    (h_ld_lim : readU64 mem (inputAddr + 104) = limit)
    (h_tag : tag % U32_MODULUS = TAG)
    (h_lim : limit % U32_MODULUS < tag % U32_MODULUS) :
    (executeFn progAt (initState inputAddr mem rt) 8).exitCode = some E_ORDER := by
  have h_jne : jump32Holds .ne tag TAG = false := by
    simp only [jump32Holds, TAG, U32_MODULUS] at h_tag ⊢; simp; omega
  have h_jlt : jump32Holds .lt limit tag = true := by
    simp only [jump32Holds, U32_MODULUS] at h_lim ⊢; simp; omega
  wp_exec [progAt] [U32_MODULUS]

/-! ## P3: both guards pass → success

   Path: 0 → 1 → 2 → 3 → 4 → 5 -/

set_option maxHeartbeats 800000 in
theorem accepts_valid
    (inputAddr : Nat) (mem : Mem) (rt : RegionTable)
    (tag limit : Nat)
    (h_rt_tag : rt.containsRange (inputAddr + 96) 8 = true)
    (h_rt_lim : rt.containsRange (inputAddr + 104) 8 = true)
    (h_ld_tag : readU64 mem (inputAddr + 96) = tag)
    (h_ld_lim : readU64 mem (inputAddr + 104) = limit)
    (h_tag : tag % U32_MODULUS = TAG)
    (h_lim : tag % U32_MODULUS ≤ limit % U32_MODULUS) :
    (executeFn progAt (initState inputAddr mem rt) 8).exitCode = some 0 := by
  have h_jne : jump32Holds .ne tag TAG = false := by
    simp only [jump32Holds, TAG, U32_MODULUS] at h_tag ⊢; simp; omega
  have h_jlt : jump32Holds .lt limit tag = false := by
    simp only [jump32Holds, U32_MODULUS] at h_lim ⊢; simp; omega
  wp_exec [progAt] [U32_MODULUS]

/-! ## The high word is ignored

   A tag of `0x1_0000_0001` passes the 32-bit check. A 64-bit `jne` would
   reject it. -/

example : jump32Holds .ne 0x1_0000_0001 TAG = false := by decide

end GuardProofs
