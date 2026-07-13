# qedgen-kani-prelude

The soundness core for QEDGen's Kani abstractions (#182) — the Kani twin of
`lean_solana/`.

QEDGen's `--kani` / `--kani-impl` harnesses replace a few Solana primitives that
CBMC bit-blasts wastefully (`Pubkey` `==`/`cmp`, `i64::checked_div`, the
`mul_div` helpers) with cheaper, sound abstractions wired via `#[kani::stub]`.
This crate holds those abstractions **once**, machine-checked sound, instead of
re-inlining them into every generated harness.

## Run the proofs

```bash
cd kani_prelude
cargo kani            # all soundness proofs (dependency-free, fast)
```

Requires `cargo-kani` (developed against 0.67.0). Verification-only: a plain
`cargo build` compiles an empty crate (`#![cfg(kani)]`); the bodies and proofs
exist only under `cargo kani`.

## What is proved

Each **exact** abstraction is checked equal to the primitive it replaces on
every input (sound both ways, so it changes no verification result):

- `wide_eq_32` / `wide_cmp_32` ≡ derived `[u8; 32]` `==` / `cmp` (= `Pubkey`)
- `wide_eq_64` / `wide_cmp_64` ≡ derived `[u8; 64]` `==` / `cmp` (#191 —
  signature-width tokens)
- `checked_div_i64` ≡ `i64::checked_div` — i8-exhaustive, plus full-width
  `None` cases and full-width unit divisors (#190)
- `mul_div_floor_u128` / `mul_div_ceil_u128` — Euclidean floor/ceil contract
  (`q·d ≤ a·b < q·d + d` and the ceil dual) over symbolic u8-bounded `a`/`b`
  × a concrete divisor panel, plus full-width zero-divisor and a
  symbolic-divisor saturation branch (#190)
- `mul_bps_floor_u128` — the floor contract over a symbolic in-range `bps` ×
  a concrete dividend panel up to u64 width, plus the full-width out-of-range
  guard (#190)
- `UfMap32` / `UfMap64` **determinism** (memoized apply) and length-prefixed
  key packing (#189)

**Residual (documented, not machine-checked):** full-width division behavior
between the bounded boxes rests on the quotient-uniqueness contract argument
in `checked_div_i64`'s docs — the nonlinear/divider BMC wall recorded in
`docs/toolchain-backlog.md`.

Proofs run against a dependency-free local `Pubkey` model — a 32-byte newtype
with derived `Eq`/`Ord`, which is exactly what `anchor_lang::prelude::Pubkey`
is, so the lemmas transfer to the real type verbatim (see `src/lib.rs` module
docs). This keeps anchor-lang/solana-program out of the graph: no
version-unification, fast solving.

## Axioms (Tier 2 — #189)

`UfMap32` / `UfMap64` model trusted crypto (PDA derivation, sha256 / keccak /
blake3, secp256k1 recovery) as *deterministic uninterpreted functions*:

- **Determinism is real** — the map memoizes, machine-checked
  (`ufmap32_is_deterministic`).
- **Collision-freedom is an axiom** — a fresh output is `kani::assume`d
  distinct from every previous output. This mirrors trusting sha256 collision
  resistance, exactly the assumption `lean_solana`'s PDA/hash axioms make. Do
  not use these maps to prove a property that *holds only if* collisions
  exist.
- **Capacity is fail-closed** — exceeding `CAP` distinct inputs or the
  128-byte key budget is an `assert!` failure, never silent degradation.

## Not proved here (over-approximating stubs)

The log / CPI stubs (Tier 4) are deliberately weaker than the real primitive
(no-op logging, assumed-success CPI) — sound for safety properties by
construction, with no equivalence to prove. They need real solana-program
types, so they live with the generated harness, not in this dependency-free
crate. The PDA / hash / secp stubs emitted by codegen are thin adapters over
the `UfCell32` / `UfCell64` wrappers here.

## Status

Wired into codegen: `--kani-impl` emits `#[kani::stub]` adapters over this
crate (Pubkey T1 by detection; PDA / hash / secp256k1 / div by pragma opt-in),
and `qedgen` vendors the crate beside any generated harness that references
it. See `docs/toolchain-backlog.md` for the #182 history.
