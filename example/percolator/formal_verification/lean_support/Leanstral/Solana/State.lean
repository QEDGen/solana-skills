import Leanstral.Solana.Account

namespace Leanstral.Solana.State

inductive Lifecycle
  | open
  | closed
  deriving Repr, DecidableEq

def closes (before after : Lifecycle) : Prop :=
  before = Lifecycle.open /\ after = Lifecycle.closed

-- Lifecycle is irreversible: once closed, always closed
theorem closed_irreversible (p_lifecycle : Lifecycle) :
    p_lifecycle = Lifecycle.closed → p_lifecycle ≠ Lifecycle.open := by
  intro h
  rw [h]
  intro contra
  cases contra

-- Closes implies the result is closed
theorem closes_is_closed (p_before p_after : Lifecycle) :
    closes p_before p_after → p_after = Lifecycle.closed := by
  intro h
  exact h.2

-- Closes implies the original was open
theorem closes_was_open (p_before p_after : Lifecycle) :
    closes p_before p_after → p_before = Lifecycle.open := by
  intro h
  exact h.1

-- If already closed, cannot close again (closes requires open → closed)
theorem closed_cannot_close (p_after : Lifecycle) :
    ¬ closes Lifecycle.closed p_after := by
  intro h
  have := closes_was_open Lifecycle.closed p_after h
  cases this

-- Helper: decidable equality for lifecycle
theorem lifecycle_eq_or_ne (p_l1 p_l2 : Lifecycle) :
    p_l1 = p_l2 ∨ p_l1 ≠ p_l2 := by
  cases p_l1 <;> cases p_l2 <;> simp

end Leanstral.Solana.State

namespace Leanstral.Solana

abbrev Lifecycle := Leanstral.Solana.State.Lifecycle
abbrev closes := Leanstral.Solana.State.closes
abbrev closed_irreversible := Leanstral.Solana.State.closed_irreversible
abbrev closes_is_closed := Leanstral.Solana.State.closes_is_closed
abbrev closes_was_open := Leanstral.Solana.State.closes_was_open
abbrev closed_cannot_close := Leanstral.Solana.State.closed_cannot_close
abbrev lifecycle_eq_or_ne := Leanstral.Solana.State.lifecycle_eq_or_ne

namespace Lifecycle

abbrev «open» : Leanstral.Solana.Lifecycle := Leanstral.Solana.State.Lifecycle.open
abbrev closed : Leanstral.Solana.Lifecycle := Leanstral.Solana.State.Lifecycle.closed

end Lifecycle

end Leanstral.Solana
