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

-- Regression fixture for the A2b-2 elaborator port (now done). The generated
-- `.refines` (below) carries the `h_prog : cr.SatisfiedBy progAt` / `h_exit` /
-- `h_asm : AsmRefinesFieldUpdate …` / `h_pre` / … hypotheses (cf.
-- `RefinesShape.lean`) and its body discharges via
-- `BridgeAdapter.halts_zero_of_fieldUpdate`, leaving the single post-leg `sorry`
-- (qedsvm#48). The `#check` documents that corrected signature; this file
-- elaborates with exactly 3 `sorry` warnings (decode_encode + refines + rejects)
-- and no errors. The PRE-port statement was unprovable: it quantified over a free
-- `progAt` with no `cr.SatisfiedBy` hypothesis (refinement for *any* program).
#check @Vault.Bridge.increment.refines
#check @Vault.Bridge.encodeState
#check @Vault.Bridge.decodeState
