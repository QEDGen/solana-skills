# `qedgen adapt` — Squads-style fixture

Adapter-output regression fixture. Locks down the
`<Type>::<method>(ctx, args)` forwarder shape (Squads V4 convention per
`reference_anchor_patterns.md`), distinct from the free-fn forwarder
covered by `examples/anchor-brownfield-demo/`.

Scope: this fixture asserts `qedgen adapt` byte-matches `before.qedspec`. The
end-to-end before→after agent-fill story is told once in the brownfield demo;
this fixture only proves the handler-shape detector emits a parseable spec.

The adapter:

1. Parses `src/lib.rs`, finds `#[program] pub mod multisig`.
2. Sees each handler tail expression is `MultisigCreate::multisig_create(ctx, ...)`,
   notes the `MultisigCreate` segment is PascalCase, classifies as
   `TypeAssoc`.
3. Walks `src/` for `impl MultisigCreate { pub fn multisig_create }`,
   locks onto `src/lib.rs` (impls inline with the `#[program]` mod
   here for compactness — `find_impl_method` handles either layout).
4. Emits `before.qedspec` with `// method on MultisigCreate` per
   handler and the file breadcrumb.

Same shape as the Marinade-style fixture: method-shape handlers seal
end-to-end via the proc-macro's `ItemFn`/`ImplItemFn` fallback.
`qedgen adapt --spec` emits sealed attributes ready to paste above
the impl methods.

## Reproduce

```bash
qedgen adapt --program examples/regressions/anchor-adapter-shapes/squads-style
# matches before.qedspec byte-for-byte
```
