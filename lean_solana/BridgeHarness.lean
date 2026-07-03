-- qedbridge test harness (A2b-2). The Bridge elaborator had no invocation
-- anywhere in the repo; this is the first one, so `qedbridge` is actually
-- exercised and its generated `.refines`/`.rejects`/encode/decode can be checked.
-- Standalone (not in lib roots): `cd lean_solana && lake env lean BridgeHarness.lean`.
import QEDGen.Solana.Bridge

namespace Vault
open SVM.SBPF SVM.SBPF.Memory

/-- Minimal vault account the bridge encodes: {owner: Pubkey, total: u64, bump: u8}. -/
structure State where
  owner : SVM.Pubkey.Pubkey
  total : Nat
  bump  : Nat

/-- The `increment` handler's abstract transition (`total += 1`). -/
def incrementTransition (s : State) (_signer : SVM.Pubkey.Pubkey) : Option State :=
  some { s with total := s.total + 1 }

end Vault

qedbridge Vault where
  input: r1
  fuel: 100
  layout
    owner Pubkey at 0
    total U64 at 32
    bump U8 at 40
  operations
    increment discriminator 0

-- Regression fixture for the A2b-2 elaborator port + post-leg discharge (both
-- done). The generated `.refines` (below) carries the `h_prog : cr.SatisfiedBy
-- progAt` / `h_exit` / `h_asm : AsmRefinesFieldUpdate …` / `h_pre` / … hyps plus
-- the per-field state-validity bounds (`hb_owner_0…`, `hb_total`, `hb_bump`), and
-- its body now closes SORRY-FREE: `BridgeAdapter.halts_zero_of_fieldUpdate` for
-- the halt, then the qedsvm#48 `CodecRead.lean` forward family
-- (`holdsFor_sepConj_right` → `holdsFor_codecCoarse_field` →
-- `readU64_of_holdsFor_memU64Is` / `readU8_of_holdsFor_memByteIs` /
-- `pubkeyAt_of_holdsFor_pubkeyIs`) for the post leg. The generated `.rejects`
-- now closes sorry-free too (qedsvm#40 / v0.9.0): it threads
-- `h_asm : AsmRefinesTransitionFault …` (one FAULT path of the
-- whole-transition bundle) and discharges through
-- `BridgeAdapter.faults_of_transitionFault` + `toSentinel_ne_zero`. So this
-- file now elaborates with exactly 1 `sorry` warning (decode_encode) and no
-- errors.
#check @Vault.Bridge.increment.refines
#check @Vault.Bridge.increment.rejects
#check @Vault.Bridge.encodeState
#check @Vault.Bridge.decodeState
-- `.refines` and `.rejects` are sorry-free: expect only the standard axioms
-- (propext / Classical.choice / Quot.sound), NO `sorryAx`.
#print axioms Vault.Bridge.increment.refines
#print axioms Vault.Bridge.increment.rejects
