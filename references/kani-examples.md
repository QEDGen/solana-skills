# Kani Examples

This file holds long-form Kani guidance that used to live in `SKILL.md`.
Keep the skill file focused on the operating contract; use this reference
when the user asks for concrete Kani harness patterns.

## Harness Strategy

Use Kani after the `.qedspec` is lint-clean and proptest is not enough.
Kani is best for bounded proofs over small symbolic states:

- Access-control rejection.
- Arithmetic safety.
- Conservation invariants.
- Isolation between accounts.
- State-machine transition guards.

Generated Kani harnesses model the spec, not a completed on-chain runtime.
For brownfield projects, prefer complementary harnesses that call real code
when the existing codebase already has helper infrastructure.

## Minimal Pattern

```rust
#[kani::proof]
fn deposit_preserves_total_assets() {
    let mut state: PoolState = kani::any();
    let amount: u64 = kani::any();

    kani::assume(amount > 0);
    kani::assume(state.total_assets <= u64::MAX - amount);

    let before = state.total_assets;
    let result = state.deposit(amount);

    if result.is_ok() {
        assert_eq!(state.total_assets, before + amount);
    }
}
```

## Arithmetic

Default `.qedspec` `+=` and `-=` effects lower to checked arithmetic in
the Kani model. Use explicit wrapping or saturating forms only when the
program intentionally uses those semantics.

For wide arithmetic that causes solver blowups, QEDGen can route selected
harnesses to z3 with `#[kani::solver(bin = "z3")]`. If z3 is missing, install
it before running the affected harnesses.

## Brownfield Rule

For existing projects, read existing tests before generating new harnesses.
Do not duplicate a large model when the program already exposes real state
constructors and transition helpers. Use those helpers and assert the spec
properties directly.
