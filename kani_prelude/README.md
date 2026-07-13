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

- `pk_eq_abstract` ≡ derived `Pubkey ==`
- `pk_cmp_abstract` ≡ derived `Pubkey cmp`
- `checked_div_abstract` ≡ `i64::checked_div`
- `mul_div_floor_u128` / `mul_div_ceil_u128` ≡ exact `a*b/d` (no-overflow regime)

Proofs run against a dependency-free local `Pubkey` model — a 32-byte newtype
with derived `Eq`/`Ord`, which is exactly what `anchor_lang::prelude::Pubkey`
is, so the lemmas transfer to the real type verbatim (see `src/lib.rs` module
docs). This keeps anchor-lang/solana-program out of the graph: no
version-unification, fast solving.

## Not proved here (over-approximating stubs)

The PDA / log / CPI stubs (Tiers 2/4) are deliberately weaker than the real
primitive (opaque symbolic address, no-op logging, assumed-success CPI) — sound
for safety properties by construction, with no equivalence to prove. They need
real solana-program types, so they live with the vendor template, not in this
dependency-free crate.

## Status

Chunk 1 of the #182 crate-extraction (see `docs/toolchain-backlog.md`). Not yet
wired into codegen or pinned in CI — those are later chunks.
