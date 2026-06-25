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

-- Regression probe for the A2b-2 elaborator port. The CURRENT generated
-- `.refines` (below) quantifies over a free `progAt` with NO `cr.SatisfiedBy
-- progAt` hypothesis — i.e. it asserts refinement for *any* program, so it is
-- only `sorry`-provable (finding 1). After the port this signature should gain
-- the `h_prog`/`h_exit`/`h_asm`/… hypotheses (cf. `RefinesShape.lean`) and its
-- body should close via `BridgeAdapter.halts_zero_of_fieldUpdate` modulo qedsvm#48.
#check @Vault.Bridge.increment.refines
#check @Vault.Bridge.encodeState
#check @Vault.Bridge.decodeState
