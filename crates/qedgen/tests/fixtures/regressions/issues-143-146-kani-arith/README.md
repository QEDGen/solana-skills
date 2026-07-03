# Issues #143–#146 — compound effect RHS + predicate arithmetic

Regression fixture for xeroc's Kani-codegen quartet (filed 2026-07-02
against v2.38.0). One spec exercises all four shapes; the pinning tests
live in `kani_mir/tests.rs`
(`compound_effect_rhs_and_arith_predicates_render_soundly`) and
`lean_gen_mir/tests.rs` (`compound_effect_rhs_lean_is_fully_state_qualified`).

| Issue | Shape in `vault.qedspec` | Broken output (v2.38.0) | Fixed output |
|-------|--------------------------|--------------------------|--------------|
| #143 | `fee := bps_mul(amount, rate)` | `s.fee = (bps_mul (amount) (rate));` — ML application syntax, bare `rate` | `s.fee = bps_mul(amount, s.rate);` |
| #144 | `cut := if flag == 1 then … else 0` | `(if flag = 1 then … else 0)` — ML conditional | `(if s.flag == 1 { … } else { 0 })` |
| #145 | `ref_impl bps_mul … = mul_div_floor(…)` | calls `mul_div_floor_u128` but never emits the definition; u128 body vs u64 signature | helper emitted + `(…) as u64` narrowing |
| #146 | `residual := fee - cut`, `requires now >= start + period`, property `(cut + residual) == fee` | bare Rust arithmetic → spurious Kani overflow/underflow failures on unconstrained state | effect RHS: `checked_sub` → `return false`; guard/property comparisons evaluate in u128 (exactly the Lean `Nat` model) |

Root cause (all four): the adapter's `render_effect` rendered compound
effect RHS via `expr_to_lean` with an empty type env and no
canonicalization, and that single string flowed verbatim into MIR
`Expr.rust` (`Expr::from_raw`) and out through every backend. The Lean
backend was bitten too: `residual := fee - cut` rendered as
`s.fee - cut` (heuristic front-prefix only) — `cut` unbound.

The fix renders effect RHS per-backend from the canonicalized typed AST
(`render_effect_rhs_forms`), threads the Rust form through
`ParsedHandler::effects_rust` → MIR `Expr.rust`, and adds math-exact
predicate rendering (`RustOpts::widen_arith`) plus checked effect-RHS
lowering (`RustOpts::checked_arith`).

The Lean/harness underflow divergence (Lean `Nat` monus vs `checked_sub`
rejection) is closed by #148: the Lean transition auto-emits bound
guards for bare arithmetic in effect values (`s.cut ≤ s.fee` here), so
both models reject the underflow path. See
`lean_gen_mir/transitions.rs::effect_tree_bound_conds`.
