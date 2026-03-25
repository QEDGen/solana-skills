import QEDGen.Solana.Account

namespace QEDGen.Solana.Authority

open QEDGen.Solana.Account

def Authorized (required actual : Pubkey) : Prop :=
  required = actual

theorem authorized_refl (authority : Pubkey) : Authorized authority authority := by
  rfl

end QEDGen.Solana.Authority
