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
    (h_rr : rr (initState inputAddr mem rt).regions)
    -- State-validity bounds: the abstract post-state fields respect their
    -- u64 / u8 / pubkey-limb widths. In the real pipeline these come from the
    -- spec's `Valid s'` invariant. They reconcile the forward codec bridges
    -- (which normalize: `readU64 = v % 2^64`, `readU8 = v % 256`) with the raw
    -- value `encodeState` asserts.
    (hb_owner0 : s'.owner.c0 < 2 ^ 64) (hb_owner1 : s'.owner.c1 < 2 ^ 64)
    (hb_owner2 : s'.owner.c2 < 2 ^ 64) (hb_owner3 : s'.owner.c3 < 2 ^ 64)
    (hb_total : s'.total < 2 ^ 64) (hb_bump : s'.bump < 256) :
    (executeFn progAt (initState inputAddr mem rt) FUEL).exitCode = some 0 ∧
    encodeState s' inputAddr (executeFn progAt (initState inputAddr mem rt) FUEL).mem := by
  have hpc : (initState inputAddr mem rt).pc = 0 := by simp [initState]
  have hrun : (initState inputAddr mem rt).exitCode = none := by simp [initState]
  obtain ⟨h_halt, h_post⟩ :=
    halts_zero_of_fieldUpdate h_asm h_prog h_exit h_pre hpc hrun h_bud h_rr h_r0 h_cs FUEL h_fuel
  refine ⟨h_halt, ?_⟩
  -- Post leg (qedsvm#48): drop the `setupPost` frame, extract each field's
  -- coarse atom from `codecCoarse`, then bridge each atom to the read that
  -- `encodeState` asserts (`CodecRead.lean` forward family).
  have hcodec := holdsFor_sepConj_right h_post
  unfold encodeState
  refine ⟨?_, ?_, ?_⟩
  · have ho := holdsFor_codecCoarse_field _ hcodec
      (show (0, FieldVal.pubkey s'.owner) ∈ _ by simp)
    simp only [FieldVal.coarse] at ho
    exact pubkeyAt_of_holdsFor_pubkeyIs hb_owner0 hb_owner1 hb_owner2 hb_owner3 ho
  · have ht := holdsFor_codecCoarse_field _ hcodec
      (show (32, FieldVal.u64 s'.total) ∈ _ by simp)
    simp only [FieldVal.coarse] at ht
    rw [readU64_of_holdsFor_memU64Is ht, Nat.mod_eq_of_lt hb_total]
  · have hbp := holdsFor_codecCoarse_field _ hcodec
      (show (40, FieldVal.byte s'.bump) ∈ _ by simp)
    simp only [FieldVal.coarse] at hbp
    rw [readU8_of_holdsFor_memByteIs hbp, Nat.mod_eq_of_lt hb_bump]

end RefinesShape
