# Regression: duplicate `Accounts` struct names across modules

Reviewer-reported on the `v2.9-anchor-first-class` branch. Tracks
`accounts_struct_for_handler` in `crates/qedgen/src/anchor_adapt.rs`.

## What broke pre-fix

Two production-ish Anchor patterns combine here:

1. Two `pub struct Shared` declarations in different modules
   (`src/a/mod.rs` vs `src/b/mod.rs`) — different fields, different
   semantics, same ident.
2. Handlers that use the qualified path explicitly:
   `Context<crate::a::Shared>` vs `Context<crate::b::Shared>`.

`extract_accounts_type` returned only `path.segments.last().ident`,
so the adapter saw `"Shared"` for both handlers. `accounts_struct_for_handler`
then walked every `*.rs` under `src/` and picked the first
`pub struct Shared` — alphabetically that's `src/a/mod.rs`, so the
`heavy` handler was sealed against the wrong type.

This silently broke `qedgen-macros::accounts_struct_hash` recompution:
the user's `b::Shared` constraints could be edited freely without the
attribute firing `compile_error!` (it was checking `a::Shared`).

## What the fix does

- `extract_accounts_path` returns every segment of the qualifying
  path (`["crate", "b", "Shared"]`), not just the last ident.
- `accounts_struct_for_handler` normalizes the prefix (strips
  `crate`/`self`) and prioritizes files whose module path matches
  before falling back to the whole-tree walk.

`extract_accounts_type` is kept as a thin `.last()` wrapper for
display use (the breadcrumb comment in the rendered `.qedspec`).

## Repro

```bash
# Adapter should now seal `lite` against src/a/mod.rs and `heavy`
# against src/b/mod.rs:
./bin/qedgen adapt \
  --program examples/regressions/anchor-accounts-collision/ \
  --spec examples/regressions/anchor-accounts-collision/collision.qedspec

# The unit-level repro is in crates/qedgen/src/anchor_adapt.rs::tests:
#   compute_attributes_respects_qualified_accounts_path
```

Bare-path handlers (`Context<Shared>` with no qualifier) still hit
the historical first-match-wins fallback. A future fix can disambiguate
those via lib.rs use-tree resolution; the reviewer's repro is the
qualified-path case, and that's what this regression covers.
