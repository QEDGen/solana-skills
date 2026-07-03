import Mathlib.Tactic
import QEDGen.Solana.Valid

/-!
# U128 Arithmetic Helpers

Mathlib-powered lemmas for reasoning about u128 arithmetic in Solana programs.
Addresses the common DeFi pattern where u64 values are widened to u128 for
intermediate calculations (price * amount, fee computations, accumulators).

## Key lemmas

- `mul_u64_fits_u128`: widening multiply always fits
- `fixed_point_u128`: price * amount / decimals fits
- `accum_u128_add_u64`: accumulator + delta with overflow check
- `mul_bounded_u128`: custom bounds for non-u64 factors

## Usage

```
open QEDGen.Solana.Arithmetic
```
-/

namespace QEDGen.Solana.Arithmetic

open QEDGen.Solana.Valid

-- ============================================================
-- Widening multiplication
-- ============================================================

/-- The core DeFi lemma: multiplying two u64 values always fits in u128.
    This models `(a as u128) * (b as u128)` in Rust. -/
theorem mul_u64_fits_u128 {a b : Nat} (ha : valid_u64 a) (hb : valid_u64 b) :
    valid_u128 (a * b) := by
  change a * b ≤ 340282366920938463463374607431768211455
  have ha : a ≤ 18446744073709551615 := ha
  have hb : b ≤ 18446744073709551615 := hb
  calc a * b ≤ 18446744073709551615 * 18446744073709551615 := Nat.mul_le_mul ha hb
    _ ≤ 340282366920938463463374607431768211455 := by norm_num

/-- Multiplying two u32 values fits in u64. -/
theorem mul_u32_fits_u64 {a b : Nat} (ha : valid_u32 a) (hb : valid_u32 b) :
    valid_u64 (a * b) := by
  change a * b ≤ 18446744073709551615
  have ha : a ≤ 4294967295 := ha
  have hb : b ≤ 4294967295 := hb
  calc a * b ≤ 4294967295 * 4294967295 := Nat.mul_le_mul ha hb
    _ ≤ 18446744073709551615 := by norm_num

-- ============================================================
-- Widening addition
-- ============================================================

/-- Adding two u64 values always fits in u128. -/
theorem add_u64_fits_u128 {a b : Nat} (ha : valid_u64 a) (hb : valid_u64 b) :
    valid_u128 (a + b) := by
  change a + b ≤ 340282366920938463463374607431768211455
  have : a ≤ 18446744073709551615 := ha
  have : b ≤ 18446744073709551615 := hb
  omega

/-- Adding two u32 values fits in u64. -/
theorem add_u32_fits_u64 {a b : Nat} (ha : valid_u32 a) (hb : valid_u32 b) :
    valid_u64 (a + b) := by
  change a + b ≤ 18446744073709551615
  have : a ≤ 4294967295 := ha
  have : b ≤ 4294967295 := hb
  omega

-- ============================================================
-- Casting (widening)
-- ============================================================

/-- A valid u64 is trivially a valid u128. Models `x as u128`. -/
theorem u64_as_u128 {n : Nat} (h : valid_u64 n) : valid_u128 n := by
  change n ≤ 340282366920938463463374607431768211455
  have : n ≤ 18446744073709551615 := h
  omega

/-- A valid u32 is trivially a valid u64. -/
theorem u32_as_u64 {n : Nat} (h : valid_u32 n) : valid_u64 n := by
  change n ≤ 18446744073709551615
  have : n ≤ 4294967295 := h
  omega

-- ============================================================
-- Checked arithmetic (with explicit overflow guard)
-- ============================================================

/-- Checked u64 addition: if the caller proves no overflow, result is valid. -/
theorem checked_add_u64 {a b : Nat}
    (_ : valid_u64 a) (_ : valid_u64 b)
    (h_no_overflow : a + b ≤ U64_MAX) :
    valid_u64 (a + b) :=
  h_no_overflow

/-- Checked u128 addition. -/
theorem checked_add_u128 {a b : Nat}
    (_ : valid_u128 a) (_ : valid_u128 b)
    (h_no_overflow : a + b ≤ U128_MAX) :
    valid_u128 (a + b) :=
  h_no_overflow

/-- Checked u64 multiplication. -/
theorem checked_mul_u64 {a b : Nat}
    (_ : valid_u64 a) (_ : valid_u64 b)
    (h_no_overflow : a * b ≤ U64_MAX) :
    valid_u64 (a * b) :=
  h_no_overflow

/-- Checked u128 multiplication. -/
theorem checked_mul_u128 {a b : Nat}
    (_ : valid_u128 a) (_ : valid_u128 b)
    (h_no_overflow : a * b ≤ U128_MAX) :
    valid_u128 (a * b) :=
  h_no_overflow

-- ============================================================
-- Division (always safe — never increases)
-- ============================================================

/-- Division never increases a value, so valid_u128 is preserved. -/
theorem div_preserves_u128 {n d : Nat} (h : valid_u128 n) (_ : d > 0) :
    valid_u128 (n / d) :=
  le_trans (Nat.div_le_self n d) h

/-- Division preserves valid_u64. -/
theorem div_preserves_u64 {n d : Nat} (h : valid_u64 n) (_ : d > 0) :
    valid_u64 (n / d) :=
  le_trans (Nat.div_le_self n d) h

-- ============================================================
-- Fixed-point arithmetic (the key DeFi pattern)
-- ============================================================

/-- price * amount / 10^decimals fits in u128 when price and amount are u64.
    This is the standard token value calculation. -/
theorem fixed_point_u128 {price amount decimals : Nat}
    (hp : valid_u64 price) (ha : valid_u64 amount)
    (hd : decimals > 0) :
    valid_u128 (price * amount / decimals) :=
  div_preserves_u128 (mul_u64_fits_u128 hp ha) hd

/-- Two-step fixed-point: (a * b / d1) * c / d2, where a,b,c are u64.
    Common in multi-hop swaps or compound fee calculations.
    Requires proving the intermediate (a*b/d1) * c doesn't exceed u128. -/
theorem fixed_point_two_step {a b c d1 d2 : Nat}
    (_ : valid_u64 a) (_ : valid_u64 b) (_ : valid_u64 c)
    (_ : d1 > 0) (hd2 : d2 > 0)
    (h_inter : a * b / d1 * c ≤ U128_MAX) :
    valid_u128 (a * b / d1 * c / d2) :=
  div_preserves_u128 h_inter hd2

-- ============================================================
-- Accumulator patterns
-- ============================================================

/-- Adding a u64 delta to a u128 accumulator, with overflow check. -/
theorem accum_u128_add_u64 {total delta : Nat}
    (_ : valid_u128 total) (_ : valid_u64 delta)
    (h_no_overflow : total + delta ≤ U128_MAX) :
    valid_u128 (total + delta) :=
  h_no_overflow

/-- Subtracting from a u128 accumulator always stays valid. -/
theorem accum_u128_sub {total delta : Nat}
    (ht : valid_u128 total) (_ : delta ≤ total) :
    valid_u128 (total - delta) := by
  change total - delta ≤ 340282366920938463463374607431768211455
  have : total ≤ 340282366920938463463374607431768211455 := ht
  omega

-- ============================================================
-- Generic bounded multiplication
-- ============================================================

/-- If you know tighter bounds than u64, use them directly.
    Prove bound_a * bound_b ≤ U128_MAX (often by norm_num). -/
theorem mul_bounded_u128 {a b bound_a bound_b : Nat}
    (ha : a ≤ bound_a) (hb : b ≤ bound_b)
    (h_prod : bound_a * bound_b ≤ U128_MAX) :
    valid_u128 (a * b) :=
  le_trans (Nat.mul_le_mul ha hb) h_prod

/-- Variant with u64 bounds for one factor and custom for the other. -/
theorem mul_u64_bounded_u128 {a b bound_b : Nat}
    (ha : valid_u64 a) (hb : b ≤ bound_b)
    (h_prod : U64_MAX * bound_b ≤ U128_MAX) :
    valid_u128 (a * b) :=
  mul_bounded_u128 ha hb h_prod

end QEDGen.Solana.Arithmetic
