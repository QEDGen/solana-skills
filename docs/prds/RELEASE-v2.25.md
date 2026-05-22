# Release v2.25.0 — ref_impl, modifies-driven agent-fill, ensures-preservation Kani

Minor release. Lands the "spec the property, let Kani find the math" flow as
a coherent verification pattern. Three layers, each independently useful but
designed to compose:

1. **`ref_impl name (...) : T = <expr>`** — top-level reference
   implementations that `ensures` clauses can call. Lower to Lean `def`s and
   to Rust fns embedded in the Kani harness.
2. **`modifies [...]` agent-fill** — codegen emits structured `todo!()`
   sites for fields declared in `modifies` but absent from the `effect`
   block, with the relevant `ensures` clauses quoted as comments.
3. **Ensures-preservation Kani harnesses** — for every handler carrying
   `ensures` clauses, Kani verifies the spec-translated transition
   satisfies them against `(pre, post)` of the symbolic state.

Together: the spec author declares the write set + the contract; codegen
shows the agent where to fill the math; Kani checks the contract holds.
For the LP-deposit pattern that motivated this release (mul-div LP-share
math too complex to inline in the effect block), the user writes a
`ref_impl lp_out (...)` capturing the math once, `ensures` referencing
it pinning the post-state value, `modifies [lp_supply]` declaring the
write — and Kani / Lean both have everything they need.

## What's in

### Phase A — modifies-driven agent-fill (already shipped on `feat/v2.24-phase-a-modifies-fill`, rolling into v2.25)

When `modifies [X, Y]` declares fields that aren't written in the
`effect { ... }` block, codegen emits a structured agent-fill site for
each unwritten field, with the relevant `ensures` clauses quoted as
comments:

```rust
self.pool.pool_balance = self.pool.pool_balance.checked_add(amount)
    .ok_or(LpError::MathOverflow)?;
// QED agent-fill site: `lp_supply` is in `modifies` but not in `effect`.
//   Implement against the spec's ensures:
//     ensures post.lp_supply == pre.lp_supply + lp_out(pre.lp_supply, ...)
//   The Kani / proptest harness verifies the impl satisfies these
//   clauses against the pre-state captured before the call.
self.pool.lp_supply = todo!("compute lp_supply to satisfy ensures above");
```

When no `ensures` references the field, the comment swaps for a pointer
at the new `unconstrained_modifies` lint instead.

**P0 lint** `unconstrained_modifies` fires when `modifies [X]` + no
effect write + no ensures referencing X. That shape is *completely
unconstrained*: Lean frame conditions allow any post-value, Kani has
nothing to assert. Fix-it: add an ensures, or drop from modifies.

Gated to the legacy flat-fields path; multi-variant ADT specs route
through a separate emitter and need their own treatment (deferred).

### Phase B — ensures-preservation Kani harnesses

For every handler that carries `ensures <expr>` clauses, codegen emits
a Kani BMC harness alongside the existing property-preservation suite:

```rust
#[kani::proof]
fn verify_deposit_ensures_0() {
    let mut s = State { pool_balance: kani::any(), lp_supply: kani::any() };
    let amount_stablecoin: u64 = kani::any();
    kani::assume((amount_stablecoin > 0));
    let pre = s.clone();
    if deposit(&mut s, amount_stablecoin) {
        let post = &s;
        assert!(post.lp_supply == pre.lp_supply
                + lp_out(pre.lp_supply, pre.pool_balance, amount_stablecoin),
            "ensures clause 0 on deposit violated by spec-translated transition");
    }
}
```

A new `rust_expr_binary` field on `ParsedEnsures` carries the binary
rendering: `state.x` → `post.x`, `old(state.x)` → `pre.x`. The harness
snapshots `pre = s.clone()` after the guard assumes, then runs the
spec-translated transition, then asserts every ensures clause.

For specs where `modifies` declares fields the effect block doesn't
write, the transition leaves those fields unchanged. Kani surfaces a
counterexample where the ensures fails — *that counterexample is the
signal* that the user's Rust impl needs to fill the modifies-driven
`todo!()` site to satisfy the contract.

**Impl-targeted variant (calls the user's real Anchor handler) is
deferred to v2.26.** That requires bridging to Anchor / Pinocchio /
native account builders, which is a substantially larger framework
surface. v2.25 ships the spec-model variant because it surfaces the
contract gap immediately without any framework wiring.

### Phase C — `ref_impl` top-level construct

New DSL surface for naming intermediate expressions referenced from
`ensures` bodies:

```
ref_impl lp_out (s_lp_supply : U64) (s_pool_balance : U64) (amount : U64) : U64 =
  if s_lp_supply == 0
    then amount
    else (amount * s_lp_supply) / s_pool_balance

handler deposit (amount_stablecoin : U64) {
  modifies [pool_balance, lp_supply]
  effect { pool_balance += amount_stablecoin }
  ensures state.lp_supply == old(state.lp_supply)
                            + lp_out(old(state.lp_supply),
                                     old(state.pool_balance),
                                     amount_stablecoin)
}
```

Lowering:

  - **Lean**: emits `def lp_out (s_lp_supply : Nat) (s_pool_balance : Nat) (amount : Nat) : Nat := …`.
    Available to proofs as an ordinary computable function — `unfold lp_out` works in tactics.
  - **Kani harness**: emits a Rust `fn lp_out(...) -> u64 { … }` so the
    ensures-preservation assertion can call it.
  - **Rust handler codegen**: skips ref_impl entirely. It's a
    verification fixture, not part of the impl contract — the user's
    real impl can compute lp_out via the same expression or a
    semantically-equivalent variant (ceiling division, checked
    arithmetic, etc.), and Kani verifies they agree.

Ref impls are pure expressions over typed parameters; no state
mutation, no calls to other ref_impls (yet), no side effects. Names
declared as ref_impls are filtered out of the uninterpreted-helper
collection (would otherwise emit `opaque foo : T → Bool` conflicting
with the real `def`).

Naming chosen over the earlier "ghost" proposal because **the
construct is a reference implementation** the user's real Rust impl
is verified against — not a ghost variable.

## Migration

No action required. Existing specs that don't declare `ref_impl`,
don't use `modifies`, and don't have `ensures` clauses lint and
codegen exactly as before.

Specs that already use `modifies` get the new emission shape: each
listed field that isn't written by the effect block now generates a
structured `todo!()` site in the Rust handler. If the spec also
declares ensures, the new ensures-preservation Kani harness will
verify the contract; if it doesn't, the new `unconstrained_modifies`
P0 lint fires until the author adds an ensures or removes the field.

Specs that declared `ensures` clauses pre-v2.25 will now have those
clauses verified by Kani too. If the spec's effect block satisfies the
ensures, the new harness passes trivially. If not — the counterexample
exposes a real spec / contract inconsistency that was silently
unverified before.

## Tested against

  - `cargo test --release --bin qedgen` — 810/810 pass (1 new test)
  - `cargo test --release --test codegen_smoke -- --ignored` — 4/4
    scaffold smoke tests pass
  - `cargo fmt --check`, `cargo clippy --release -- -D warnings`,
    `bash scripts/check-readme-drift.sh` — clean
  - `qedgen check --regen-drift` — clean (percolator's kani.rs picked
    up 7 new ensures-preservation harnesses; regenerated)
  - Hand-verified LP-deposit + ref_impl flow end-to-end: spec parses,
    ref_impl emits as Lean `def` and Rust fn, ensures harness binds
    `(pre, post)` correctly, agent-fill site quotes the right
    contract clauses

## What's next

v2.26 candidates:

  - **Impl-targeted Kani** — bridge to Anchor / Pinocchio account
    builders so the harness calls the user's real handler, not the
    spec model. The framework-specific account-init code is the
    bulk of the surface; rest is a thin wrapper over today's
    ensures-preservation shape.
  - **Multi-variant ADT support for the modifies-driven fill site** —
    the current emission gates to the legacy flat-fields path. ADT
    state needs `emit_variant_state_handler_body` to learn the same
    pattern.
  - **`ref_impl` composition** — calls to other `ref_impl`s, recursive
    ref_impls, and ref_impls that take `Map[N] T` parameters all
    deferred. Today's surface covers scalar parameters which is the
    LP-shape sweet spot.
