# `qedgen adapt` — Marinade-style fixture

Adapter-output regression fixture. Locks down the `ctx.accounts.<method>(...)`
forwarder shape (the Marinade convention from `reference_anchor_patterns.md`),
distinct from the Anchor scaffold's `instructions::<name>::handler(ctx, args)`
covered by `examples/anchor-brownfield-demo/`.

Scope: this fixture asserts `qedgen adapt` byte-matches `before.qedspec`. The
end-to-end before→after agent-fill story is told once in the brownfield demo;
this fixture only proves the handler-shape detector emits a parseable spec.

The adapter:

1. Parses `src/lib.rs`, finds `#[program] pub mod stake`.
2. Sees each handler tail expression is `ctx.accounts.process(...)`,
   classifies as `AccountsMethod`.
3. Reads the `Context<X>` type from each handler signature, walks
   `src/` for an `impl X { pub fn process }` block, locks onto the
   `instructions/<name>.rs` file containing it.
4. Emits `before.qedspec` with `// method on <Type>` per handler and
   the file breadcrumb.

Method-shape handlers seal end-to-end, the same as free-fn shapes —
the proc-macro tries `syn::ItemFn` first and falls back to
`syn::ImplItemFn`, so `#[qed]` works in either position. Run
`qedgen adapt --program <path> --spec <stake.qedspec>` to get the
sealed attribute lines for paste, including the `accounts_*` triplet.

## Reproduce

```bash
qedgen adapt --program examples/regressions/anchor-adapter-shapes/marinade-style
# matches before.qedspec byte-for-byte
```
