I need a single Lean 4 module that compiles under Lean 4 + Mathlib.

Return Lean code only.
Do not duplicate declarations.
Do not leave theorem bodies empty after `:= by`.
If a proof is incomplete, use `sorry` inside the proof body.
Prefer a smaller explicit model that compiles over a larger broken one.

## Common Tactic Patterns - READ CAREFULLY

### Rewrite Direction After Option.some.inj

When working with Option types and hypotheses of form `h : someFunc(...) = some result`:

**CRITICAL**: After `apply Option.some.inj at h`, the hypothesis transforms to: `inner_expression = result`

- If your goal contains `result` (the right-hand side), use `rw [← h]` with the LEFTWARD arrow
- If your goal contains `inner_expression` (the left-hand side), use `rw [h]` without arrow

Example:
```lean
-- Given: h : cancelPreservesBalances p_accounts ... = some p_accounts'
-- Goal: trackedTotal p_accounts = trackedTotal p_accounts'

-- Step 1: Unfold and inject
rw [cancelPreservesBalances] at h  -- h : some (p_accounts.map ...) = some p_accounts'
apply Option.some.inj at h         -- h : (p_accounts.map ...) = p_accounts'

-- Step 2: Rewrite in goal
-- Goal contains p_accounts' (right side of h), so use LEFTWARD arrow
rw [← h]  -- Replaces p_accounts' with (p_accounts.map ...)

-- Now goal is: trackedTotal p_accounts = trackedTotal (p_accounts.map ...)
```

**REMEMBER**: After `Option.some.inj`, you almost always need `rw [← h]` (with arrow) to substitute the `some` result in your goal.

### If-Expressions with Proof Bindings

- Use `if h : condition then ...` ONLY when you need the proof `h` in the then/else branches
- If you don't use `h`, write `if condition then ...` without the binding
- This avoids "unused variable" warnings

Example:
```lean
-- BAD: h is never used
if h : x = y then some () else none

-- GOOD: no unused variable
if x = y then some () else none

-- GOOD: h is actually used
if h : x = y then proof_using_h h else none
```

Here is an example of a Rust function and the desired Lean proof structure. Follow this format.

---

### EXAMPLE

#### Rust Source
```rust
pub fn checked_add(a: u64, b: u64) -> Option<u64> {
    a.checked_add(b)
}
```

#### Lean Proof
```lean
import Mathlib.Data.Nat.Basic
import Mathlib.Tactic

def checked_add (a b : Nat) : Option Nat :=
  if h : a + b < 2^64 then
    some (a + b)
  else
    none

theorem checked_add_correct (a b : Nat) :
  a + b < 2^64 → checked_add a b = some (a + b) := by
  intro h
  simp [checked_add, h]

theorem checked_add_overflow (a b : Nat) :
  a + b ≥ 2^64 → checked_add a b = none := by
  intro h
  simp [checked_add, h]
```
---

Now, generate a Lean proof for the following.

## Source Code
<paste the relevant code here>

## Property to Prove
<state exactly one property, or a very small set of closely related properties>

## Context
<account model, invariants, semantic assumptions, or proof boundaries>

## Output Requirements
1. Define the model types and executable transition functions first.
2. State the theorem only after the semantics are defined.
3. Use only Lean 4 / Mathlib identifiers you are confident exist.
4. Prefer concrete definitions over placeholders.
5. If the full request is too large, prove a sound subset cleanly instead of emitting broken code.
