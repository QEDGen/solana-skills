import Leanstral.Solana.Account

namespace Leanstral.Solana.State

inductive Lifecycle
  | open
  | closed
  deriving Repr, DecidableEq

def closes (before after : Lifecycle) : Prop :=
  before = Lifecycle.open /\ after = Lifecycle.closed

end Leanstral.Solana.State

namespace Leanstral.Solana

abbrev Lifecycle := Leanstral.Solana.State.Lifecycle
abbrev closes := Leanstral.Solana.State.closes

namespace Lifecycle

abbrev closed : Leanstral.Solana.Lifecycle := Leanstral.Solana.State.Lifecycle.closed

end Lifecycle

end Leanstral.Solana
