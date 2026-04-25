# `qedgen adapt` — brownfield demo

Worked example of pulling a `.qedspec` skeleton out of an existing
Anchor program. Pairs with `qedgen adapt` (v2.9 M4.3).

## Layout

```
src/
  lib.rs                       # #[program] pub mod counter { ... }
  instructions/
    mod.rs
    initialize.rs              # pub fn handler(ctx, start) — actual body
    increment.rs               # pub fn handler(ctx, delta) — actual body
before.qedspec                 # raw output of `qedgen adapt`
after.qedspec                  # the same spec, with TODOs filled in
```

The program is a minimal counter (Anchor-scaffold style: `lib.rs`
forwards `instructions::<name>::handler(ctx, args)`). It exists only
to feed the adapter — there's no `Cargo.toml` because nothing here is
compiled. The adapter only reads source.

## Reproduce

```bash
qedgen adapt --program examples/anchor-brownfield-demo
```

The output matches `before.qedspec` byte-for-byte (snapshot-tested in
`crates/qedgen/src/anchor_adapt.rs`). Then the user (or their agent)
edits in:

  - the actual lifecycle (`Uninitialized → Active` here)
  - per-handler `auth`, `accounts`, `requires`, `effect` blocks
  - error variants discovered while filling in `requires`

…and ends up with `after.qedspec`. From there `qedgen check`,
`qedgen codegen`, etc. work the same as for any greenfield spec.

## What the adapter carries forward vs. what stays TODO

| Carries forward (from Rust source)                  | Stays TODO (semantic) |
|-----------------------------------------------------|-----------------------|
| handler names                                       | state machine variants and fields |
| typed arguments (Rust primitives + bare user types) | requires / ensures |
| `Context<X>` accounts struct name (as comment)      | effect bodies |
| breadcrumb to the actual handler body file          | error variants |
| classifier hint (inline / free-fn / method shape)   | auth / signers |

The split lines up with what's mechanically derivable: argument types
are in the function signature; semantic properties live in the
handler body and constraint attributes, which a reader (you, or a
coding agent with the body in scope) is faster at extracting than a
syntactic walker would be.
