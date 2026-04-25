# `qedgen adapt` — Squads-style fixture

Companion to `examples/anchor-brownfield-demo/`. Exercises the
`<Type>::<method>(ctx, args)` forwarder shape (Squads V4 convention
per `reference_anchor_patterns.md`).

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

Same v2.9 caveat as the Marinade-style fixture: `#[qed]` annotation
on impl methods lands in v2.10. Scaffold mode works fully.

## Reproduce

```bash
qedgen adapt --program examples/anchor-squads-style-demo
# matches before.qedspec byte-for-byte
```
