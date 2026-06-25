-- Validation scaffold for the corrected `.refines` statement (A2b-2, finding 1).
-- Hand-written vault analogue of what the Bridge elaborator should emit: the
-- discharge (`AsmRefinesFieldUpdate`) + the program constraint (`SatisfiedBy`)
-- become hypotheses, so the theorem is provable (vs the current free-`progAt`
-- statement, which is not). Proven via the adapter; the only remaining `sorry`
-- is the post `codecCoarse → encodeState` leg (qedsvm#48).
import QEDGen.Solana.BridgeAdapter

namespace RefinesShape
open SVM.SBPF SVM.SBPF.Memory SVM.Solana.Abstract QEDGen.Solana.BridgeAdapter

-- Minimal vault account State (named `Acct` so it doesn't shadow `SVM.SBPF.State`)
-- + the generated encodeState / Transition shapes.
structure Acct where
  owner : SVM.Pubkey.Pubkey
  total : Nat
  bump  : Nat

def incrementTransition (s : Acct) (_signer : Pubkey) : Option Acct :=
  some { s with total := s.total + 1 }

def encodeState (s : Acct) (addr : Nat) (mem : Mem) : Prop :=
  pubkeyAt mem (addr + 0) s.owner ∧
  readU64 mem (addr + 32) = s.total ∧
  readU8 mem (addr + 40) = s.bump

def FUEL : Nat := 100

/-- The corrected `.refines` for `increment`. Note the threaded program /
    discharge hypotheses (`h_prog`, `h_exit`, `h_asm`) that the current
    generated statement lacks. Discharged via `halts_zero_of_fieldUpdate`; the
    sole `sorry` is the post-field-list → `encodeState` conversion (qedsvm#48). -/
theorem increment_refines
    (progAt : Nat → Option Insn) (cr : CodeReq) (rr : RegionTable → Prop)
    (nSteps nCu exitPc : Nat) (setupPre setupPost : Assertion)
    (inputAddr : Nat) (rt : RegionTable) (mem : Mem)
    (s s' : Acct) (signer : Pubkey)
    (h_prog : cr.SatisfiedBy progAt)
    (h_exit : progAt exitPc = some .exit)
    (_h_encode : encodeState s inputAddr mem)
    (_h_guard : incrementTransition s signer = some s')
    (h_asm : AsmRefinesFieldUpdate cr nSteps nCu 0 exitPc rr inputAddr
              [(0, .pubkey s.owner), (32, .u64 s.total), (40, .byte s.bump)]
              [(0, .pubkey s'.owner), (32, .u64 s'.total), (40, .byte s'.bump)]
              setupPre setupPost)
    (h_pre : (setupPre ** codecCoarse inputAddr
                [(0, .pubkey s.owner), (32, .u64 s.total), (40, .byte s.bump)]).holdsFor
              (initState inputAddr mem rt))
    (h_cs : ∀ k : Nat, (executeFn progAt (initState inputAddr mem rt) k).callStack = [])
    (h_r0 : ∀ t : State,
      (setupPost ** codecCoarse inputAddr
        [(0, .pubkey s'.owner), (32, .u64 s'.total), (40, .byte s'.bump)]).holdsFor t →
      t.regs.get .r0 = 0)
    (h_fuel : nSteps + 1 ≤ FUEL)
    (h_bud : (initState inputAddr mem rt).cuConsumed + nSteps + nCu
              ≤ (initState inputAddr mem rt).cuBudget)
    (h_rr : rr (initState inputAddr mem rt).regions) :
    (executeFn progAt (initState inputAddr mem rt) FUEL).exitCode = some 0 ∧
    encodeState s' inputAddr (executeFn progAt (initState inputAddr mem rt) FUEL).mem := by
  have hpc : (initState inputAddr mem rt).pc = 0 := by simp [initState]
  have hrun : (initState inputAddr mem rt).exitCode = none := by simp [initState]
  obtain ⟨h_halt, _h_post⟩ :=
    halts_zero_of_fieldUpdate h_asm h_prog h_exit h_pre hpc hrun h_bud h_rr h_r0 h_cs FUEL h_fuel
  refine ⟨h_halt, ?_⟩
  -- `_h_post : (setupPost ** codecCoarse inputAddr postFields).holdsFor result`
  -- ⟹ `encodeState s' inputAddr result.mem`. This is the codecCoarse→read leg
  -- (qedsvm#48: `holdsFor_codecCoarse` / `holdsFor_memU64Is` family).
  sorry

end RefinesShape
