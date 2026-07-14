//! qedgen-kani-prelude — the soundness core for QEDGen's Kani abstractions (#182).
//!
//! QEDGen's Kani harnesses replace a handful of Solana primitives that CBMC
//! bit-blasts wastefully with cheaper, sound abstractions wired via
//! `#[kani::stub]`. Historically each was emitted as a *string literal,
//! re-inlined into every generated harness*, its soundness argued once in prose.
//! This crate is the single source of truth: each abstraction lives here as
//! real, compile-checked code, and — where it is an *exact* abstraction —
//! carries a `#[kani::proof]` that machine-checks it against the primitive it
//! replaces.
//!
//! ## A dependency-free, byte-level API
//!
//! The public surface is deliberately typed over `[u8; 32]` / `i64` / `u128`,
//! never over `anchor_lang::prelude::Pubkey`. That is what makes this an
//! *importable* crate rather than a per-target regenerated blob: because it
//! names no Solana type, it needs no anchor-lang / solana-program dependency,
//! so there is no version to unify against the program under test. The
//! generated harness keeps its own `Pubkey`-typed stub target and calls in with
//! a one-line adapter over the program's own type:
//!
//! ```ignore
//! use qedgen_kani_prelude as kp;
//! fn pk_eq_abstract(a: &Pubkey, b: &Pubkey) -> bool {
//!     kp::wide_eq_32(a.to_bytes(), b.to_bytes())          // proven logic lives in the crate
//! }
//! #[kani::stub(<Pubkey as core::cmp::PartialEq>::eq, pk_eq_abstract)]
//! ```
//!
//! ## Why proving over `[u8; 32]` covers the real `Pubkey`
//!
//! solana / anchor `Pubkey` is `#[repr(transparent)] struct Pubkey([u8; 32])`
//! with **derived** `Eq`/`Ord`, and a derive on a newtype delegates straight to
//! the inner `[u8; 32]`'s own `==` / `cmp`. The proofs below check the
//! abstraction against exactly those array operations, so the lemma transfers to
//! `Pubkey` verbatim — while the crate stays dependency-free and fast to solve.
//!
//! ## Endianness
//!
//! Kani models a little-endian target (as does every Solana host). `wide_cmp_32`
//! `swap_bytes` reinterprets each little-endian `u128` half as big-endian so
//! tuple-lexicographic `u128` comparison reproduces the byte-lexicographic order
//! of the derived `Ord`. Equality is endianness-independent.
//!
//! ## Exact vs over-approximating vs axiomatized
//!
//! `wide_eq_32` / `wide_cmp_32` / `wide_eq_64` / `wide_cmp_64` /
//! `checked_div_i64` are **exact** — proved equal to the primitive they replace
//! on every input (sound both ways, so they change no verification result).
//! The log / CPI stubs (Tier 4) are deliberately *over-approximating* (no-op
//! logging, assumed-success CPI): sound for safety by construction, nothing to
//! prove equal, and they need real solana-program types — so they live with the
//! generated harness, not in this dependency-free crate. The Tier 2
//! trusted-crypto stubs (PDA derivation, hashes, secp256k1 recovery — #189)
//! are **axiomatized**: their `UfMap32`/`UfMap64` cores below are
//! deterministic by construction (machine-checked) and injective by a
//! collision-freedom `kani::assume` — see the Tier 2 section for the trust
//! argument.
#![cfg(kani)]

use core::cmp::Ordering;

// ---------------------------------------------------------------------------
// Tier 1 — opaque-token equality / ordering (#182). Reinterpret the 32 bytes as
// two u128 halves: 2 word-comparisons, NOT a 32-byte memcmp/lex loop (Kani
// unwind 2 vs >= 34). Verification-only, so the transmute never runs on-chain.
// ---------------------------------------------------------------------------

/// Abstract 32-byte equality — the reusable core of the `Pubkey ==` stub.
/// Reinterprets the bytes as two `u128` halves and compares (equal bytes ⇔
/// equal words, so endianness is irrelevant here). Proved equivalent to
/// elementwise `[u8; 32]` equality by [`wide_eq_32_agrees_with_array`].
#[allow(clippy::missing_transmute_annotations)]
pub fn wide_eq_32(a: [u8; 32], b: [u8; 32]) -> bool {
    let a: [u128; 2] = unsafe { core::mem::transmute(a) };
    let b: [u128; 2] = unsafe { core::mem::transmute(b) };
    a[0] == b[0] && a[1] == b[1]
}

/// Abstract 32-byte lexicographic ordering (byte 0 most significant = big-endian
/// u256) — the reusable core of the `Pubkey cmp` stub. Proved equivalent to the
/// derived `[u8; 32]` `cmp` by [`wide_cmp_32_agrees_with_array`].
#[allow(clippy::missing_transmute_annotations)]
pub fn wide_cmp_32(a: [u8; 32], b: [u8; 32]) -> Ordering {
    let a: [u128; 2] = unsafe { core::mem::transmute(a) };
    let b: [u128; 2] = unsafe { core::mem::transmute(b) };
    (a[0].swap_bytes(), a[1].swap_bytes()).cmp(&(b[0].swap_bytes(), b[1].swap_bytes()))
}

/// Abstract 64-byte equality (#191) — the T1 core for signature-width tokens
/// (`[u8; 64]`: ed25519/secp256k1 signatures, recovered secp pubkeys). Four
/// `u128` word-comparisons instead of a 64-byte memcmp/lex loop (Kani unwind
/// 2 vs ≥ 66). Proved equivalent to elementwise `[u8; 64]` equality by
/// [`wide_eq_64_agrees_with_array`].
#[allow(clippy::missing_transmute_annotations)]
pub fn wide_eq_64(a: [u8; 64], b: [u8; 64]) -> bool {
    let a: [u128; 4] = unsafe { core::mem::transmute(a) };
    let b: [u128; 4] = unsafe { core::mem::transmute(b) };
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2] && a[3] == b[3]
}

/// Abstract 64-byte lexicographic ordering (#191) — byte 0 most significant,
/// mirroring [`wide_cmp_32`]'s big-endian reinterpretation. Proved equivalent
/// to the derived `[u8; 64]` `cmp` by [`wide_cmp_64_agrees_with_array`].
#[allow(clippy::missing_transmute_annotations)]
pub fn wide_cmp_64(a: [u8; 64], b: [u8; 64]) -> Ordering {
    let a: [u128; 4] = unsafe { core::mem::transmute(a) };
    let b: [u128; 4] = unsafe { core::mem::transmute(b) };
    let key = |w: [u128; 4]| {
        (
            w[0].swap_bytes(),
            w[1].swap_bytes(),
            w[2].swap_bytes(),
            w[3].swap_bytes(),
        )
    };
    key(a).cmp(&key(b))
}

// ---------------------------------------------------------------------------
// Arithmetic tier — abstract `i64::checked_div` (#182). A symbolic 64-bit
// divisor forces CBMC/z3 to bit-blast a sequential divider that stalls; replace
// it with a fresh symbolic quotient pinned by division's EXACT contract
// (`a = q*b + r`, `|r| < |b|`, `sign(r) = sign(a)`, computed in i128 so the
// contract math can't overflow) plus the two real `None` cases. The quotient is
// unique for `b != 0`, so this is exact — see `checked_div_i64_agrees_with_std_bounded`.
// ---------------------------------------------------------------------------

/// Abstract `i64::checked_div` — the reusable core of the `checked_div` stub.
/// Returns a fresh symbolic quotient constrained by truncating division's exact
/// contract instead of invoking the divider circuit. Proved equal to
/// `a.checked_div(b)` (bounded) by [`checked_div_i64_agrees_with_std_bounded`].
pub fn checked_div_i64(a: i64, b: i64) -> Option<i64> {
    if b == 0 || (a == i64::MIN && b == -1) {
        return None; // the real `checked_div`'s two None cases
    }
    let q: i64 = kani::any();
    let (ai, bi, qi) = (a as i128, b as i128, q as i128);
    let r = ai - qi * bi; // remainder; i128 so it can't overflow
    kani::assume(r.abs() < bi.abs());
    kani::assume(r == 0 || (r > 0) == (ai > 0));
    Some(q)
}

// ---------------------------------------------------------------------------
// Saturating `mul_div` helpers — emitted today by the spec-model path
// (`kani_mir/prefix.rs`) when a guard references them. Not stubs of a std fn;
// each carries a machine-checked contract harness (#190): the SPEC side is
// stated multiplicatively (`q*d ≤ prod < (q+1)*d`), so only the helper's own
// divider runs — over bounded operands, the same encoding that makes
// `checked_div_i64_agrees_with_std_bounded` tractable. Full-width residual:
// see each harness's doc comment.
// ---------------------------------------------------------------------------

/// `floor(a*b/d)` with saturation on overflow; `0` when `d == 0`.
#[inline]
pub fn mul_div_floor_u128(a: u128, b: u128, d: u128) -> u128 {
    if d == 0 {
        return 0;
    }
    a.saturating_mul(b) / d
}

/// `ceil(a*b/d)` with saturation on overflow; `0` when `d == 0`.
#[inline]
pub fn mul_div_ceil_u128(a: u128, b: u128, d: u128) -> u128 {
    if d == 0 {
        return 0;
    }
    let prod = a.saturating_mul(b);
    if prod % d == 0 {
        prod / d
    } else {
        (prod / d).saturating_add(1)
    }
}

/// Nearest integer to `a*b/d`, with exact halves rounded upward; `0` when
/// `d == 0`. Quotient/remainder decomposition avoids an overflowing bias add.
#[inline]
pub fn mul_div_round_half_up_u128(a: u128, b: u128, d: u128) -> u128 {
    if d == 0 {
        return 0;
    }
    let prod = a.saturating_mul(b);
    let q = prod / d;
    let r = prod % d;
    let half_up_threshold = d / 2 + d % 2;
    if r >= half_up_threshold {
        q.saturating_add(1)
    } else {
        q
    }
}

/// `floor(a * bps / 10000)` split into quotient/remainder to keep the product
/// small; `u128::MAX` when `bps > 10000` (out of range).
#[inline]
pub fn mul_bps_floor_u128(a: u128, bps: u128) -> u128 {
    if bps > 10000 {
        return u128::MAX;
    }
    let b = (bps as u16) as u128;
    let q = a / 10000;
    let r = a % 10000;
    q.wrapping_mul(b).wrapping_add(r.wrapping_mul(b) / 10000)
}

// ---------------------------------------------------------------------------
// Tier 2 — deterministic uninterpreted functions with a collision-freedom
// axiom (#189). The Kani mirror of `lean_solana`'s 'Trust (axioms)' boundary
// for trusted crypto: PDA derivation (sha256 + bump search), sha256 / keccak /
// blake3, and secp256k1 recovery are exhaustively bit-blasted by CBMC when a
// harness reaches them, at zero verification value — the program must hold for
// ANY hash output; we never re-verify that sha256 is sha256.
//
// What each map models, and its trust argument:
//   * DETERMINISM is REAL (machine-checked, `ufmap32_is_deterministic`): the
//     map memoizes, so equal inputs return the SAME output — the property the
//     old fresh-`kani::any()` PDA stub lost, and the one programs actually
//     rely on (derive-then-compare).
//   * COLLISION-FREEDOM is an AXIOM (`kani::assume`): a fresh output is
//     assumed distinct from every previous output, making the modeled
//     function injective across the harness. This mirrors trusting sha256
//     collision resistance — exactly the assumption the Lean side's PDA/hash
//     axioms make (`lean_solana` axiom docs). It is an over-approximation in
//     one direction only: a real collision, were one findable, could only
//     REMOVE behaviors from the model, and BMC harnesses assert safety over
//     the modeled behaviors. Do not use these maps to prove a property that
//     HOLDS ONLY IF collisions exist.
//   * CAPACITY is fail-closed: exceeding `CAP` distinct inputs (or the 128-
//     byte key budget) is an `assert!` failure — the harness fails loudly and
//     names the fix, never silently degrades.
//
// Kani-mechanics notes: key bytes are packed with `copy_nonoverlapping`
// (CBMC models memcpy natively — no unwind cost), keys/values compare as
// `u128` words (no memcmp loops), and the memo scan is bounded by the
// CONCRETE `CAP` — a harness needs `#[kani::unwind(CAP + 2)]` or higher.
// ---------------------------------------------------------------------------

/// Fixed-size key for the T2 uninterpreted-function maps: parts are packed
/// with a length prefix (so `["ab","c"]` ≠ `["a","bc"]`) into a 128-byte
/// buffer compared as eight `u128` words. Overflow is fail-closed (`assert!`).
#[derive(Clone, Copy)]
pub struct UfKey {
    bytes: [u8; 128],
    used: usize,
}

impl UfKey {
    pub const fn new() -> Self {
        UfKey {
            bytes: [0u8; 128],
            used: 0,
        }
    }

    /// Append one part (length-prefixed). Fail-closed: a part longer than 255
    /// bytes or a key past 128 bytes is a harness assertion failure (raise the
    /// budget in the prelude), never a silent truncation.
    pub fn push(mut self, part: &[u8]) -> Self {
        assert!(
            part.len() <= 255,
            "qedgen_kani_prelude::UfKey: part exceeds 255 bytes"
        );
        assert!(
            self.used + 1 + part.len() <= 128,
            "qedgen_kani_prelude::UfKey: key exceeds the 128-byte budget"
        );
        self.bytes[self.used] = part.len() as u8;
        self.used += 1;
        // memcpy intrinsic — CBMC models it natively, no copy-loop to unwind.
        unsafe {
            core::ptr::copy_nonoverlapping(
                part.as_ptr(),
                self.bytes.as_mut_ptr().add(self.used),
                part.len(),
            );
        }
        self.used += part.len();
        self
    }

    /// The key as eight `u128` words (word-compares, no memcmp loop).
    #[allow(clippy::missing_transmute_annotations)]
    fn words(&self) -> [u128; 8] {
        unsafe { core::mem::transmute(self.bytes) }
    }
}

impl Default for UfKey {
    fn default() -> Self {
        Self::new()
    }
}

/// Eight explicit word-compares — NOT `[u128; 8] ==`, whose lexicographic
/// slice loop would reintroduce an unwind bound.
fn words_eq(a: &[u128; 8], b: &[u128; 8]) -> bool {
    a[0] == b[0]
        && a[1] == b[1]
        && a[2] == b[2]
        && a[3] == b[3]
        && a[4] == b[4]
        && a[5] == b[5]
        && a[6] == b[6]
        && a[7] == b[7]
}

macro_rules! define_ufmap {
    ($map:ident, $cell:ident, $width:literal, $wide_eq:ident) => {
        /// Deterministic uninterpreted function `UfKey → [u8; $width]` with a
        /// collision-freedom axiom — see the Tier 2 section docs for the trust
        /// argument. `CAP` bounds DISTINCT inputs per harness (fail-closed).
        pub struct $map<const CAP: usize> {
            keys: [[u128; 8]; CAP],
            vals: [[u8; $width]; CAP],
            len: usize,
        }

        impl<const CAP: usize> $map<CAP> {
            pub const fn new() -> Self {
                $map {
                    keys: [[0u128; 8]; CAP],
                    vals: [[0u8; $width]; CAP],
                    len: 0,
                }
            }

            /// Memoized apply: a seen key returns its recorded value
            /// (determinism, machine-checked); a fresh key draws a symbolic
            /// value ASSUMED distinct from every previous one
            /// (collision-freedom, axiom).
            pub fn apply(&mut self, key: UfKey) -> [u8; $width] {
                let words = key.words();
                for i in 0..self.len {
                    if words_eq(&self.keys[i], &words) {
                        return self.vals[i];
                    }
                }
                let fresh: [u8; $width] = kani::any();
                for i in 0..self.len {
                    kani::assume(!$wide_eq(fresh, self.vals[i])); // collision-freedom axiom
                }
                assert!(
                    self.len < CAP,
                    concat!(
                        "qedgen_kani_prelude::",
                        stringify!($map),
                        " capacity exhausted — raise CAP in the generated stub"
                    )
                );
                self.keys[self.len] = words;
                self.vals[self.len] = fresh;
                self.len += 1;
                fresh
            }
        }

        impl<const CAP: usize> Default for $map<CAP> {
            fn default() -> Self {
                Self::new()
            }
        }

        /// Interior-mutable, harness-`static`-friendly wrapper: generated
        /// stubs hold `static UF: $cell<CAP> = $cell::new();` and call
        /// `UF.apply(key)` with no `unsafe` at the call site. Sound under
        /// Kani: single-threaded, and `apply` never re-enters.
        pub struct $cell<const CAP: usize>(core::cell::UnsafeCell<$map<CAP>>);

        // Kani executes harnesses single-threaded; there is no data race to
        // guard against, and the crate never compiles off-kani.
        unsafe impl<const CAP: usize> Sync for $cell<CAP> {}

        impl<const CAP: usize> $cell<CAP> {
            pub const fn new() -> Self {
                $cell(core::cell::UnsafeCell::new($map::new()))
            }

            pub fn apply(&self, key: UfKey) -> [u8; $width] {
                unsafe { &mut *self.0.get() }.apply(key)
            }
        }
    };
}

define_ufmap!(UfMap32, UfCell32, 32, wide_eq_32);
define_ufmap!(UfMap64, UfCell64, 64, wide_eq_64);

// ===========================================================================
// Soundness proofs — each `exact` abstraction agrees with the primitive it
// replaces. Proved directly over `[u8; 32]` / `i64`, which (see module docs) is
// exactly what the real `Pubkey` derives delegate to. Run with `cargo kani`.
// ===========================================================================

/// T1: `wide_eq_32` ≡ derived `[u8; 32]` equality (= `Pubkey ==`), for all byte
/// pairs. The array `==` is a 32-element loop — hence `unwind(33)`; the
/// abstraction itself is unwind-free, which is the whole point.
#[kani::proof]
#[kani::unwind(33)]
fn wide_eq_32_agrees_with_array() {
    let a: [u8; 32] = kani::any();
    let b: [u8; 32] = kani::any();
    assert_eq!(wide_eq_32(a, b), a == b);
}

/// T1: `wide_cmp_32` ≡ derived `[u8; 32]` `cmp` (= `Pubkey cmp`), for all byte
/// pairs. Array `Ord` is a lexicographic loop — `unwind(33)`.
#[kani::proof]
#[kani::unwind(33)]
fn wide_cmp_32_agrees_with_array() {
    let a: [u8; 32] = kani::any();
    let b: [u8; 32] = kani::any();
    assert_eq!(wide_cmp_32(a, b), a.cmp(&b));
}

/// Arithmetic: `checked_div_i64` ≡ `i64::checked_div`, BOUNDED to 8-bit
/// operands. The direct-equality form is the most convincing, but comparing over
/// full 64-bit values forces CBMC through the very divider the abstraction
/// avoids; bounding `a`/`b` to `i8` range keeps it fast while still exercising
/// every sign combination, truncation-toward-zero, and both `None` cases. The
/// UNBOUNDED proof is a nonlinear/divider BMC wall — the same wall the deferred
/// `mul_div_*` proofs and the "Custom is a nonlinear-BMC wall" note in
/// docs/toolchain-backlog.md record — so unbounded soundness rests on the
/// documented contract argument, not a machine check.
#[kani::proof]
fn checked_div_i64_agrees_with_std_bounded() {
    let a: i64 = kani::any();
    let b: i64 = kani::any();
    kani::assume(a >= i8::MIN as i64 && a <= i8::MAX as i64);
    kani::assume(b >= i8::MIN as i64 && b <= i8::MAX as i64);
    assert_eq!(checked_div_i64(a, b), a.checked_div(b));
}

/// T1 (#191): `wide_eq_64` ≡ derived `[u8; 64]` equality, for all byte pairs.
/// The array `==` is a 64-element loop — hence `unwind(65)`; the abstraction
/// itself is unwind-free.
#[kani::proof]
#[kani::unwind(65)]
fn wide_eq_64_agrees_with_array() {
    let a: [u8; 64] = kani::any();
    let b: [u8; 64] = kani::any();
    assert_eq!(wide_eq_64(a, b), a == b);
}

/// T1 (#191): `wide_cmp_64` ≡ derived `[u8; 64]` `cmp`, for all byte pairs.
#[kani::proof]
#[kani::unwind(65)]
fn wide_cmp_64_agrees_with_array() {
    let a: [u8; 64] = kani::any();
    let b: [u8; 64] = kani::any();
    assert_eq!(wide_cmp_64(a, b), a.cmp(&b));
}

// ---------------------------------------------------------------------------
// #190 — `checked_div_i64` beyond the i8 box: the two `None` cases and the
// unit divisors hold at FULL 64-bit dividend width (a concrete divisor
// constant-propagates the divider circuit away; the `None` cases never reach
// it). Together with the i8-exhaustive harness above, the residual gap is
// "symbolic dividend × symbolic non-unit divisor at full width" — the
// nonlinear/divider BMC wall documented in docs/toolchain-backlog.md, covered
// by the quotient-uniqueness contract argument in `checked_div_i64`'s docs.
// ---------------------------------------------------------------------------

/// #190: both `None` cases at full width — `b == 0` for every `a`, and the
/// `(i64::MIN, -1)` overflow pair. Neither path executes a divider.
#[kani::proof]
fn checked_div_i64_none_cases_full_width() {
    let a: i64 = kani::any();
    assert_eq!(checked_div_i64(a, 0), a.checked_div(0));
    assert_eq!(checked_div_i64(i64::MIN, -1), i64::MIN.checked_div(-1));
}

/// #190: unit divisors at full dividend width — `b == 1` is the identity and
/// `b == -1` is negation (minus the `i64::MIN` overflow pair). The concrete
/// divisor constant-propagates the divider circuit, so the full 64-bit
/// dividend stays tractable.
#[kani::proof]
fn checked_div_i64_unit_divisors_full_width() {
    let a: i64 = kani::any();
    assert_eq!(checked_div_i64(a, 1), a.checked_div(1));
    kani::assume(a != i64::MIN);
    assert_eq!(checked_div_i64(a, -1), a.checked_div(-1));
}

// ---------------------------------------------------------------------------
// #190 — `mul_div_*` contract harnesses. The spec side is multiplicative
// (`q*d ≤ prod < (q+1)*d`), so only the helper's OWN divider runs; operands
// are bounded to u8 range (the same box that makes the `checked_div` proof
// tractable) except where a branch never divides (`d == 0`, `bps > 10000`) or
// divides by a CONSTANT (`mul_bps_floor`'s `/ 10000`), which hold at full or
// u64 width. Residual: the in-range division behavior between u8-bounded and
// full-width operands rests on the same documented divider argument as
// `checked_div_i64`.
// ---------------------------------------------------------------------------

/// #190: `mul_div_floor_u128` returns exactly `⌊a·b / d⌋` — stated as the
/// Euclidean bracketing `q*d ≤ a*b < q*d + d` over fully-symbolic u8-bounded
/// `a`/`b` and a CONCRETE divisor panel. A fully-symbolic divisor alongside a
/// symbolic dividend is the 128-bit divider wall (the harness ran > 3 min);
/// concrete divisors constant-propagate the circuit while the panel still
/// exercises d = 1, d > prod, remainder-zero and remainder-nonzero cases.
/// The complementary symbolic-DIVISOR shape (with a concrete dividend) is
/// covered by `mul_div_floor_u128_saturation_bounded_divisor` below.
#[kani::proof]
fn mul_div_floor_u128_floor_contract_bounded() {
    let a: u128 = kani::any();
    let b: u128 = kani::any();
    kani::assume(a <= u8::MAX as u128 && b <= u8::MAX as u128);
    let prod = a * b; // ≤ 255² — never saturates in this box
    for d in [1u128, 2, 3, 7, 10, 255, 70000] {
        let q = mul_div_floor_u128(a, b, d);
        assert!(q * d <= prod);
        assert!(prod < q * d + d);
    }
}

/// #190: `mul_div_ceil_u128` returns exactly `⌈a·b / d⌉` — `q*d ≥ a*b` and
/// `q*d - a*b < d`, over the same symbolic-operand × concrete-divisor panel
/// as the floor harness (see its doc for the divider-wall rationale).
#[kani::proof]
fn mul_div_ceil_u128_ceil_contract_bounded() {
    let a: u128 = kani::any();
    let b: u128 = kani::any();
    kani::assume(a <= u8::MAX as u128 && b <= u8::MAX as u128);
    let prod = a * b;
    for d in [1u128, 2, 3, 7, 10, 255, 70000] {
        let q = mul_div_ceil_u128(a, b, d);
        assert!(q * d >= prod);
        assert!(q * d - prod < d);
    }
}

/// `mul_div_round_half_up_u128` chooses floor below the half-way threshold
/// and ceiling at or above it, including odd denominators where no exact half
/// exists. The concrete divisor panel keeps the duplicated reference divider
/// tractable while covering even/odd and exact/non-exact cases.
#[kani::proof]
fn mul_div_round_half_up_contract_bounded() {
    let a: u128 = kani::any();
    let b: u128 = kani::any();
    kani::assume(a <= u8::MAX as u128 && b <= u8::MAX as u128);
    let prod = a * b;
    for d in [1u128, 2, 3, 7, 10, 255, 70000] {
        let floor = prod / d;
        let remainder = prod % d;
        let threshold = d / 2 + d % 2;
        let expected = if remainder >= threshold {
            floor + 1
        } else {
            floor
        };
        assert_eq!(mul_div_round_half_up_u128(a, b, d), expected);
    }
}

/// #190: the `d == 0` guard of both `mul_div` helpers returns 0 at FULL
/// operand width — the branch never reaches a divider.
#[kani::proof]
fn mul_div_zero_divisor_full_width() {
    let a: u128 = kani::any();
    let b: u128 = kani::any();
    assert_eq!(mul_div_floor_u128(a, b, 0), 0);
    assert_eq!(mul_div_ceil_u128(a, b, 0), 0);
    assert_eq!(mul_div_round_half_up_u128(a, b, 0), 0);
}

/// #190: the saturation branch — when `a·b` overflows u128, both helpers
/// divide the saturated product, so floor returns `u128::MAX / d` (concrete
/// dividend; u8-bounded divisor keeps the circuit constant-propagatable).
#[kani::proof]
fn mul_div_floor_u128_saturation_bounded_divisor() {
    let d: u128 = kani::any();
    kani::assume(d >= 1 && d <= u8::MAX as u128);
    // u128::MAX * 2 saturates; the helper must then divide the saturated MAX.
    assert_eq!(mul_div_floor_u128(u128::MAX, 2, d), u128::MAX / d);
}

/// #190: `mul_bps_floor_u128` returns exactly `⌊a·bps / 10000⌋` — stated as
/// the Euclidean bracketing `q·10^4 ≤ a·bps < q·10^4 + 10^4` (multiplication
/// only on the spec side) over a symbolic in-range `bps` and a CONCRETE `a`
/// panel spanning the quotient/remainder split's regimes: below / at / above
/// the 10^4 divisor, and up to a full u64-width dividend. The all-symbolic
/// direct-equality form pays for four 128-bit division circuits (both sides
/// divide) and exceeded the solver budget even u16-bounded; with `a`
/// concrete, `a / 10^4` and `a % 10^4` constant-fold and the one remaining
/// divider (`r·bps / 10^4`, a ≤ 27-bit dividend by a constant) closes — the
/// same shape as the floor/ceil panels above. Also pins the out-of-range
/// guard (`bps > 10000 → u128::MAX`) at full symbolic width.
#[kani::proof]
fn mul_bps_floor_u128_floor_contract_bounded() {
    let bps: u128 = kani::any();
    if bps <= 10000 {
        for a in [
            0u128,
            1,
            9999,
            10000,
            10001,
            65535,
            123_456_789,
            u64::MAX as u128,
        ] {
            // a·bps ≤ (2^64-1)·10^4 < 2^128 — the bracket math cannot overflow.
            let q = mul_bps_floor_u128(a, bps);
            assert!(q * 10000 <= a * bps);
            assert!(a * bps < q * 10000 + 10000);
        }
    } else {
        let a: u128 = kani::any();
        assert_eq!(mul_bps_floor_u128(a, bps), u128::MAX);
    }
}

// ---------------------------------------------------------------------------
// #189 — Tier 2 UfMap sanity harnesses. Determinism is a genuine machine
// check; the distinctness harness checks the collision-freedom AXIOM is wired
// (it asserts what `apply` assumes — see the Tier 2 section docs for why the
// axiom itself is trusted, not proven). CAP = 2 keeps the memo scan cheap.
// ---------------------------------------------------------------------------

/// #189: determinism (machine-checked) — applying the SAME key twice returns
/// the same value, even with a different key interleaved.
#[kani::proof]
#[kani::unwind(8)]
fn ufmap32_is_deterministic() {
    let mut uf: UfMap32<2> = UfMap32::new();
    let k1 = UfKey::new().push(&[1, 2, 3]);
    let k2 = UfKey::new().push(&[4]).push(&[5]);
    let v1 = uf.apply(k1);
    let _ = uf.apply(k2);
    assert!(wide_eq_32(uf.apply(k1), v1));
}

/// #189: key packing is length-prefixed — `["ab","c"]` and `["a","bc"]` are
/// DIFFERENT keys (same concatenated bytes), so they draw distinct values
/// under the collision-freedom axiom.
#[kani::proof]
#[kani::unwind(8)]
fn ufmap32_distinct_keys_distinct_values() {
    let mut uf: UfMap32<2> = UfMap32::new();
    let v1 = uf.apply(UfKey::new().push(b"ab").push(b"c"));
    let v2 = uf.apply(UfKey::new().push(b"a").push(b"bc"));
    assert!(!wide_eq_32(v1, v2));
}

/// #189: the 64-byte-valued map (secp256k1 recovery shape) — determinism and
/// axiom wiring in one harness.
#[kani::proof]
#[kani::unwind(8)]
fn ufmap64_deterministic_and_distinct() {
    let mut uf: UfMap64<2> = UfMap64::new();
    let k1 = UfKey::new().push(&[9; 32]);
    let k2 = UfKey::new().push(&[7; 32]);
    let v1 = uf.apply(k1);
    let v2 = uf.apply(k2);
    assert!(!wide_eq_64(v1, v2));
    assert!(wide_eq_64(uf.apply(k1), v1));
}
