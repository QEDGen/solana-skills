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
//! ## Exact vs over-approximating
//!
//! `wide_eq_32` / `wide_cmp_32` / `checked_div_i64` are **exact** — proved equal
//! to the primitive they replace on every input (sound both ways, so they change
//! no verification result). The PDA / log / CPI stubs (Tiers 2/4) are
//! deliberately *over-approximating* (opaque symbolic address, no-op logging,
//! assumed-success CPI): sound for safety by construction, nothing to prove
//! equal, and they need real solana-program types — so they live with the
//! generated harness, not in this dependency-free crate.
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
// correctness proofs are DEFERRED (their spec compares against a symbolic u128
// divider — the same stall `checked_div_i64` sidesteps; a tractable proof needs
// the same divider-free contract encoding).
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
