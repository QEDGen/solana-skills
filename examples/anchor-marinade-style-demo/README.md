# `qedgen adapt` — Marinade-style fixture

Companion to `examples/anchor-brownfield-demo/`. Same pipeline, but
exercises the `ctx.accounts.<method>(...)` forwarder shape (the
Marinade convention from `reference_anchor_patterns.md`) instead of
the Anchor scaffold's `instructions::<name>::handler(ctx, args)`.

The adapter:

1. Parses `src/lib.rs`, finds `#[program] pub mod stake`.
2. Sees each handler tail expression is `ctx.accounts.process(...)`,
   classifies as `AccountsMethod`.
3. Reads the `Context<X>` type from each handler signature, walks
   `src/` for an `impl X { pub fn process }` block, locks onto the
   `instructions/<name>.rs` file containing it.
4. Emits `before.qedspec` with `// method on <Type>` per handler and
   the file breadcrumb.

`#[qed]` annotation is **not** supported on impl methods in v2.9 — the
proc-macro currently parses only `syn::ItemFn`, not `syn::ImplItemFn`.
Method-shape handlers ride the scaffold path; the drift loop arrives
in v2.10.

## Reproduce

```bash
qedgen adapt --program examples/anchor-marinade-style-demo
# matches before.qedspec byte-for-byte
```
