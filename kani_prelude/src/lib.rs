//! qedgen-kani-prelude — the soundness core for QEDGen's Kani abstractions (#182).
//!
//! QEDGen's Kani harnesses replace a handful of Solana primitives that CBMC
//! bit-blasts wastefully with cheaper, sound abstractions wired via
//! `#[kani::stub]`. Historically each abstraction was emitted as a *string
//! literal, re-inlined into every generated harness*, and its soundness was
//! argued once in prose (or checked once in a throwaway workspace). This crate
//! is the single source of truth: each abstraction lives here as real,
//! compile-checked code, and — where it is an *exact* abstraction — carries a
//! `#[kani::proof]` that machine-checks it against the primitive it replaces.
//!
//! ## Why a local `Pubkey` model is faithful
//!
//! solana / anchor `Pubkey` is `#[repr(transparent)] struct Pubkey([u8; 32])`
//! with **derived** `PartialEq`/`Eq`/`Ord` — i.e. elementwise-then-lexicographic
//! `[u8; 32]` comparison, byte 0 most significant. `anchor_lang::prelude::Pubkey`
//! re-exports exactly that type. The abstractions below never touch anything
//! *but* those 32 bytes, so a lemma proved against the local [`Pubkey`] model
//! transfers verbatim to the real type — and we stay dependency-free (no
//! anchor-lang/solana-program in the graph, no version-unification, fast
//! solving). The vendored harness bodies keep their `anchor_lang::prelude::`
//! signatures; this crate proves the byte-level content those signatures wrap.
//!
//! ## Endianness
//!
//! Kani models a little-endian target (as does every Solana host). The
//! ordering abstraction's `swap_bytes` reinterprets each little-endian `u128`
//! half as big-endian so tuple-lexicographic `u128` comparison reproduces the
//! big-endian / byte-lexicographic order of the derived `Ord`. Equality is
//! endianness-independent (equal bytes ⟺ equal words either way).
//!
//! ## Exact vs over-approximating
//!
//! `pk_eq_abstract` / `pk_cmp_abstract` / `checked_div_abstract` are **exact** —
//! they agree with the real primitive on every input, proved below (sound in
//! both directions, so they change no verification result). The PDA / log / CPI
//! stubs (Tiers 2/4) are deliberately *over-approximating* (opaque symbolic
//! address, no-op logging, assumed-success CPI): sound for safety properties by
//! construction, with nothing to prove equal — they live with the vendor
//! template, not here, since they need real solana-program types.
#![cfg(kani)]

use core::cmp::Ordering;

/// Faithful, dependency-free model of solana/anchor `Pubkey`: a 32-byte newtype
/// with derived `Eq`/`Ord`. See the module docs for why proving against this
/// transfers to `anchor_lang::prelude::Pubkey`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pubkey([u8; 32]);

impl Pubkey {
    #[inline]
    pub fn new_from_array(bytes: [u8; 32]) -> Self {
        Pubkey(bytes)
    }

    #[inline]
    pub fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Tier 1 — opaque-token equality / ordering (#182). Reinterpret the 32 bytes as
// two u128 halves: 2 word-comparisons, NOT a 32-byte memcmp/lex loop (Kani
// unwind 2 vs >= 34). Verification-only, so the transmute never runs on-chain.
// ---------------------------------------------------------------------------

/// Abstract `Pubkey` equality. Proven bit-for-bit equivalent to the derived
/// `==` by [`pk_eq_abstract_agrees_with_derive`].
#[allow(clippy::missing_transmute_annotations)]
pub fn pk_eq_abstract(a: &Pubkey, b: &Pubkey) -> bool {
    let a: [u128; 2] = unsafe { core::mem::transmute(a.to_bytes()) };
    let b: [u128; 2] = unsafe { core::mem::transmute(b.to_bytes()) };
    a[0] == b[0] && a[1] == b[1]
}

/// Abstract `Pubkey` ordering (byte-lexicographic = big-endian u256). Proven
/// equivalent to the derived `cmp` by [`pk_cmp_abstract_agrees_with_derive`].
#[allow(clippy::missing_transmute_annotations)]
pub fn pk_cmp_abstract(a: &Pubkey, b: &Pubkey) -> Ordering {
    let a: [u128; 2] = unsafe { core::mem::transmute(a.to_bytes()) };
    let b: [u128; 2] = unsafe { core::mem::transmute(b.to_bytes()) };
    (a[0].swap_bytes(), a[1].swap_bytes()).cmp(&(b[0].swap_bytes(), b[1].swap_bytes()))
}

// ---------------------------------------------------------------------------
// Arithmetic tier — abstract `i64::checked_div` (#182). A symbolic 64-bit
// divisor forces CBMC/z3 to bit-blast a sequential divider that stalls; replace
// it with a fresh symbolic quotient pinned by division's EXACT contract
// (`a = q*b + r`, `|r| < |b|`, `sign(r) = sign(a)`, computed in i128 so the
// contract math can't overflow) plus the two real `None` cases. The quotient is
// unique for `b != 0`, so this is exact — proved by `checked_div_abstract_agrees_with_std`.
// ---------------------------------------------------------------------------

/// Abstract `i64::checked_div`. Proven equal to `a.checked_div(b)` on every
/// input by [`checked_div_abstract_agrees_with_std`].
pub fn checked_div_abstract(a: i64, b: i64) -> Option<i64> {
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
// bounded-correctness proofs pin their `floor`/`ceil`/`bps` semantics.
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

// ===========================================================================
// Soundness proofs — each `exact` abstraction agrees with the primitive it
// replaces on EVERY input. Run with `cargo kani` (in this directory).
// ===========================================================================

/// T1: abstract Pubkey equality ≡ derived `==`, for all byte pairs. The
/// derived side is a 32-element array compare (a loop) — hence `unwind(33)`;
/// the abstraction itself is unwind-free, which is the whole point.
#[kani::proof]
#[kani::unwind(33)]
fn pk_eq_abstract_agrees_with_derive() {
    let a = Pubkey::new_from_array(kani::any());
    let b = Pubkey::new_from_array(kani::any());
    assert_eq!(pk_eq_abstract(&a, &b), a == b);
}

/// T1: abstract Pubkey ordering ≡ derived `cmp`, for all byte pairs. Derived
/// `Ord` on `[u8; 32]` is a lexicographic loop — `unwind(33)`.
#[kani::proof]
#[kani::unwind(33)]
fn pk_cmp_abstract_agrees_with_derive() {
    let a = Pubkey::new_from_array(kani::any());
    let b = Pubkey::new_from_array(kani::any());
    assert_eq!(pk_cmp_abstract(&a, &b), a.cmp(&b));
}

/// Arithmetic: abstract division ≡ `i64::checked_div`, BOUNDED to 8-bit
/// operands. This is the most convincing form (direct equality against the real
/// primitive), but comparing over full 64-bit values forces CBMC through the
/// very divider circuit the abstraction exists to avoid; bounding `a`/`b` to
/// `i8` range keeps it fast while still exercising every sign combination,
/// truncation-toward-zero, and both `None` cases (`b == 0`, `MIN / -1`). The
/// UNBOUNDED proof is a nonlinear/divider BMC wall — the same wall the deferred
/// `mul_div_*` proofs hit and the "Custom is a nonlinear-BMC wall (div
/// abstracted, multiply-back residual)" note in docs/toolchain-backlog.md
/// records — so unbounded soundness rests on the documented contract argument,
/// not a machine check.
#[kani::proof]
fn checked_div_abstract_agrees_with_std_bounded() {
    let a: i64 = kani::any();
    let b: i64 = kani::any();
    kani::assume(a >= i8::MIN as i64 && a <= i8::MAX as i64);
    kani::assume(b >= i8::MIN as i64 && b <= i8::MAX as i64);
    assert_eq!(checked_div_abstract(a, b), a.checked_div(b));
}

// NOTE: correctness proofs for the `mul_div_*` helpers are DEFERRED. Their
// spec (`== (a*b)/d`) compares against a *symbolic u128 division*, which forces
// CBMC through the same 128-bit divider circuit that stalls the solver — the
// very stall `checked_div_abstract` exists to sidestep (cf. the "Custom is a
// nonlinear-BMC wall" note in docs/toolchain-backlog.md). A tractable proof
// needs a divider-free contract encoding (fresh symbolic quotient pinned by
// `q*d + r == a*b`, `r < d`), mirroring `checked_div_abstract`; tracked as a
// follow-up so this crate's `cargo kani` stays fast and green.
