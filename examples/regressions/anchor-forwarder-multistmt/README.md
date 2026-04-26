# Regression: multi-statement Anchor forwarders

Reviewer-reported on the `v2.9-anchor-first-class` branch. Tracks the
classifier in `crates/qedgen/src/anchor_resolver.rs`.

## What broke pre-fix

`classify_forwarder` only accepted single-expression bodies. The
ubiquitous Anchor scaffold shape

```rust
pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
    instructions::buy::handler(ctx, amount)?;
    Ok(())
}
```

has two statements (`<call>?;` and `Ok(())`), so it was classified as
`Inline` and the adapter sealed the wrapper bytes in `src/lib.rs`
instead of `src/instructions/buy.rs`. Downstream effects:

- `qedgen check --anchor-project` falsely reported the handler's
  spec effects as missing (the wrapper only forwards — it has no
  effect on state).
- `qedgen adapt --spec` emitted a `#[qed]` attribute whose body hash
  matched the wrapper, not the real handler body, hiding drift in the
  real handler.

The advertised `--handler <name>=<rust_path>` escape hatch only ran
on classifier `Unrecognized` cases, so users with this shape had no
way to route around the misclassification.

## What the fix does

`extract_forwarder_tail` (new) accepts the two-stmt `<call>?; Ok(())`
shape and the single-stmt `<call>?` (try-tail) shape as pure
forwarder plumbing. `resolve_with_override` now also fires when an
override is supplied even if the classifier landed elsewhere — the
user's intent is "treat this as a forwarder pointing at <path>", and
that intent always wins.

Genuine multi-statement bodies (e.g. `fence` below, with a `msg!`
between the call and return) stay classified `Inline` so user logic
keeps flowing into the body hash.

## Repro

```bash
# Adapter should report buy + settle as `free-fn forwarder` shape and
# fence as `inline body in the #[program] mod`.
./bin/qedgen adapt --program examples/regressions/anchor-forwarder-multistmt/

# The classifier-level repro is in
# crates/qedgen/src/anchor_resolver.rs::tests:
#   classifies_two_stmt_propagate_then_ok_as_forwarder
#   classifies_two_stmt_with_return_ok_as_forwarder
#   classifies_try_tail_as_forwarder
#   classifies_three_stmt_forwarder_as_inline   (Inline-stays-Inline)
#   classifies_let_binding_then_ok_as_inline    (Inline-stays-Inline)
```

The fixture is shape-only — no `Cargo.toml`. The adapter only needs
`src/lib.rs` plus the modules it forwards to.
