import Leanstral.Solana.Account

namespace Leanstral.Solana.Authority

open Leanstral.Solana.Account

def Authorized (required actual : Pubkey) : Prop :=
  required = actual

theorem authorized_refl (authority : Pubkey) : Authorized authority authority := by
  rfl

end Leanstral.Solana.Authority
