-- sBPF State Monad: monadic interface over the sBPF machine state
--
-- Defines SbpfM as StateT State Id, providing primitive operations
-- (getReg, setReg, loadByWidth, storeByWidth, etc.) that compose
-- monadically. Loom's MAlgOrdered instance for StateT gives us
-- weakest-precondition reasoning automatically.

import QEDGen.Solana.SBPF.Execute
import QEDGen.Solana.SBPF.Memory

namespace QEDGen.Solana.SBPF

open Memory

/-! ## Core monad -/

/-- The sBPF state monad: state transformer over machine state.
    Loom provides `MAlgOrdered (StateT State Id) (State → Prop)` automatically. -/
abbrev SbpfM (α : Type) := StateT State Id α

/-! ## Primitive state operations

All defined as direct functions (not using do/get/modify) so that
`simp` can reduce them without unfolding monadic infrastructure. -/

@[simp] def getReg (r : Reg) : SbpfM Nat :=
  fun s => (s.regs.get r, s)

@[simp] def setReg (r : Reg) (v : Nat) : SbpfM PUnit :=
  fun s => ((), { s with regs := s.regs.set r v })

@[simp] def getMem : SbpfM Mem :=
  fun s => (s.mem, s)

@[simp] def setMem (m : Mem) : SbpfM PUnit :=
  fun s => ((), { s with mem := m })

@[simp] def getPc : SbpfM Nat :=
  fun s => (s.pc, s)

@[simp] def setPc (pc : Nat) : SbpfM PUnit :=
  fun s => ((), { s with pc := pc })

@[simp] def getExitCode : SbpfM (Option Nat) :=
  fun s => (s.exitCode, s)

@[simp] def setExit (code : Nat) : SbpfM PUnit :=
  fun s => ((), { s with exitCode := some code })

/-! ## Derived operations -/

@[simp] def advancePc : SbpfM PUnit :=
  fun s => ((), { s with pc := s.pc + 1 })

@[simp] def resolveSrcM (src : Src) : SbpfM Nat :=
  fun s => (resolveSrc s.regs src, s)

/-! ## Memory operations -/

@[simp] def loadByWidthM (addr : Nat) (w : Width) : SbpfM Nat :=
  fun s => (readByWidth s.mem addr w, s)

@[simp] def storeByWidthM (addr : Nat) (val : Nat) (w : Width) : SbpfM PUnit :=
  fun s => ((), { s with mem := writeByWidth s.mem addr val w })

end QEDGen.Solana.SBPF
