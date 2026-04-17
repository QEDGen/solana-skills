import Mathlib.Algebra.BigOperators.Fin
import Mathlib.Logic.Function.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Tactic.Abel

/-!
# Indexed state support

Backs `Map[N] T` fields in `.qedspec` with `Fin n → α`. This keeps sums
(`∑ i : Fin n, ...`), quantifiers (`∀ i : Fin n, ...`), and updates
(`Function.update`) all first-class with Mathlib's big-operator API.

Used by QEDGen-generated Spec.lean files for per-account state like
percolator's `accounts : Map[MAX_ACCOUNTS] Account`.
-/

namespace QEDGen.Solana.IndexedState

/-- A bounded map from a finite index to values. Total function. -/
abbrev Map (n : Nat) (α : Type) : Type := Fin n → α

namespace Map

variable {n : Nat} {α : Type}

/-- Lookup at index `i`. -/
@[reducible] def get (m : Map n α) (i : Fin n) : α := m i

/-- Update index `i` to value `x`. Wraps `Function.update`. -/
@[reducible] def set (m : Map n α) (i : Fin n) (x : α) : Map n α :=
  Function.update m i x

@[simp] theorem get_set_same (m : Map n α) (i : Fin n) (x : α) :
    (m.set i x).get i = x := by
  simp [get, set]

@[simp] theorem get_set_other (m : Map n α) (i j : Fin n) (x : α) (h : j ≠ i) :
    (m.set i x).get j = m.get j := by
  simp [get, set, Function.update_of_ne h]

end Map

/-- Pointwise ordering for maps into an ordered type. -/
@[reducible] def Map.LePoint {n : Nat} {α : Type} [LE α] (m₁ m₂ : Map n α) : Prop :=
  ∀ i : Fin n, m₁ i ≤ m₂ i

/-- Sum over a map: `∑ i, m i`. Provided as a thin alias so generated
    Spec.lean code can use `Map.sum m` without pulling in Finset syntax. -/
@[reducible] def Map.sum {n : Nat} {β : Type} [AddCommMonoid β] (m : Map n β) : β :=
  ∑ i : Fin n, m i

/-- Projection through `Function.update` factors as `update` of the projected
    function. Key lemma for reasoning about `∑ j, proj (update f i v j)`. -/
theorem proj_update_eq
    {n : Nat} {β γ : Type}
    (f : Fin n → β) (i : Fin n) (v : β) (proj : β → γ) :
    (fun j : Fin n => proj (Function.update f i v j)) =
    Function.update (fun j => proj (f j)) i (proj v) := by
  funext j
  by_cases hji : j = i
  · subst hji; simp [Function.update_self]
  · simp [Function.update_of_ne hji]

/-- Bilinear sum-update identity — for any `AddCommMonoid γ`,
    `sum_after + old_val = sum_before + new_val`.
    Avoids truncating subtraction on `Nat` and works uniformly across
    monoids by stating both sides as sums. -/
theorem sum_update_proj_bilinear
    {n : Nat} {β γ : Type} [AddCommMonoid γ]
    (f : Fin n → β) (i : Fin n) (v : β) (proj : β → γ) :
    (∑ j : Fin n, proj (Function.update f i v j)) + proj (f i) =
    (∑ j : Fin n, proj (f j)) + proj v := by
  -- Reindex the LHS sum through the projected update.
  rw [show (∑ j : Fin n, proj (Function.update f i v j)) =
          ∑ j : Fin n, Function.update (fun j => proj (f j)) i (proj v) j
       from by rw [proj_update_eq f i v proj]]
  -- Extract the i-th term from both sums using `Finset.sum_erase_add`.
  rw [show ∑ j : Fin n, Function.update (fun j => proj (f j)) i (proj v) j =
          Function.update (fun j => proj (f j)) i (proj v) i +
            ∑ j ∈ (Finset.univ : Finset (Fin n)).erase i,
              Function.update (fun j => proj (f j)) i (proj v) j
       from (Finset.add_sum_erase _ _ (Finset.mem_univ i)).symm]
  rw [show (∑ j : Fin n, proj (f j)) =
          proj (f i) + ∑ j ∈ (Finset.univ : Finset (Fin n)).erase i, proj (f j)
       from (Finset.add_sum_erase _ _ (Finset.mem_univ i)).symm]
  -- The updated-at-i value IS proj v.
  rw [Function.update_self]
  -- The off-index terms in the update sum are just the original.
  rw [show ∑ j ∈ (Finset.univ : Finset (Fin n)).erase i,
          Function.update (fun j => proj (f j)) i (proj v) j =
          ∑ j ∈ (Finset.univ : Finset (Fin n)).erase i, proj (f j) from by
        apply Finset.sum_congr rfl
        intro j hj
        rw [Finset.mem_erase] at hj
        rw [Function.update_of_ne hj.1]]
  -- Both sides are commutatively equal; `abel` (AddCommMonoid normalizer) closes.
  abel

/-- Summing a projection through a `Function.update` is unchanged when the
    update doesn't alter that projection at the updated index. This is the
    backbone of conservation proofs when a handler's effect touches fields
    OTHER than those appearing in the sum. -/
theorem sum_update_proj_eq
    {n : Nat} {β : Type} {γ : Type} [AddCommMonoid γ]
    (f : Fin n → β) (i : Fin n) (v : β) (proj : β → γ)
    (h : proj v = proj (f i)) :
    (∑ j : Fin n, proj (Function.update f i v j)) =
    (∑ j : Fin n, proj (f j)) := by
  apply Finset.sum_congr rfl
  intros j _
  by_cases hji : j = i
  · subst hji; rw [Function.update_self]; exact h
  · rw [Function.update_of_ne hji]

end QEDGen.Solana.IndexedState
