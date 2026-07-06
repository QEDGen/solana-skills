# QEDGen v2.39.0 — effect RHS rendered per-backend from the typed AST (#143–#146)

**Status:** release-prep (on `chore/release-v2.39.0`). **Theme:** xeroc's
v2.38.0 Kani-codegen quartet (#143 #144 #145 #146), fixed at one root cause
in PR #147, plus the crucible wallet-lamport guard broadening merged to
`main` since v2.38.0.

## What shipped

### 1. Effect RHS renders per-backend from the canonicalized typed AST (#143, #144)

The adapter's `render_effect` used to lower compound effect RHS **once, in
Lean form, with an empty type env and no canonicalization**; that single
string flowed verbatim into MIR `Expr.rust` (`Expr::from_raw`) and out
through every backend. Consequences: ref_impl calls emitted ML application
syntax (`(bps_mul (amount) (rate))`), `if … then … else` passed through
unlowered, state fields lost their receiver — and the **Lean backend was
broken by the same string** (`residual := fee - cut` rendered `s.fee - cut`
with `cut` unbound; the downstream heuristic only prefixes the front of
the string).

Now `render_effect_rhs_forms` canonicalizes the RHS AST (the #139
`canon.rs` seam) and renders both forms — Lean via `expr_to_lean` with the
real env, Rust via `expr_to_rust` — threading the Rust form through
`ParsedHandler::effects_rust` / `ParsedEffectArm::effects_rust` →
`lower_effects` → MIR `Expr.rust`. Simple RHS shapes (params, literals,
bare fields) keep the legacy single-form string, so binder-specific
resolution downstream (`resolve_value`, Anchor `mechanize_effect`) is
byte-identical — the Anchor/Quasar scaffold outputs did not change.

### 2. `mul_div_floor` in ref_impl bodies (#145)

Two bugs: `guards_use_math_helpers` only probed requires/aborts/ensures/
lets — a `mul_div_floor` used *only* inside a `ref_impl` body never
triggered the inline `mul_div_floor_u128` helper emission — and even with
the helper present, the generated fn had a u128-typed body against its
declared-width signature. The gate now probes ref_impl bodies + effect
RHS, and the ref_impl body narrows back to the declared return width
(`(…) as u64`), the same precedent as `let X = mul_div_floor(…)`.

### 3. Checked / math-exact arithmetic in the harness lane (#146)

- **Predicates** (requires / properties / ensures): new math-exact render
  (`RustOpts::widen_arith`), stored in parallel `rust_expr_math` /
  `rust_expression_math` / `rust_expr_binary_math` fields (MIR
  `Expr::rust_math` / `rust_binary_math`) and preferred by the
  Kani/proptest emitters. Arithmetic inside comparisons evaluates in
  u128/i128 — `-` on unsigned kinds saturates (Nat monus), `/`·`%` follow
  Lean's total-function convention — so the predicate computes exactly
  what the Lean `Nat` model computes and can never overflow-panic on
  unconstrained symbolic state. The `(a * b) / 10000` shape is exempt so
  the solver-tuned `mul_bps_floor_u128` rewrite keeps firing.
- **Effect RHS**: bare `+`/`-`/`*`/`/`/`%` in `:=` bodies lowers checked
  (`RustOpts::checked_arith` → `(a).checked_sub(b)?` inside an
  `(|| Some(…))()` closure; `None` → transition returns `false`),
  extending the v2.7 checked `+=`/`-=` doctrine.
- **Documented divergence** (`references/qedspec-dsl.md` § Bare arithmetic
  in `:=` RHS and predicates): Lean `Nat` subtraction in a `:=` RHS is
  monus while the harness rejects on underflow; auto bound-guards on the
  Lean transition are follow-up (#148). Workaround: write the guard
  explicitly (`requires fee >= cut`) — both models then agree.

### 4. Crucible wallet-lamport guard (merged to `main` since v2.38.0)

The brownfield lamport-inflation guard's tracked set broadened from signer
keypairs to **every fuzzer-controlled fixture keypair** (signers AND
non-signer writables, PDA vault excluded) — a drain crediting a plain
writable `recipient` (the textbook missing-authority withdraw, where no
signer gains) now fires `assert_no_wallet_inflation`.

## Compatibility notes

- No CLI change. No `.qedspec` DSL syntax change — the DSL reference gains
  the bare-arithmetic semantics section.
- Generated **Anchor/Quasar scaffold** output: unchanged beyond the
  version-tag re-stamp.
- Generated **Kani/proptest harnesses** change where specs use compound
  effect RHS or arithmetic inside predicates: previously-broken shapes now
  compile; arithmetic predicates render widened. Bundled multisig /
  perp-dex harnesses + the 5 affected snapshots regenerated.
- Generated **Lean** changes only for compound effect RHS (previously
  emitted unbound bare fields — did not elaborate).
- Regression fixture: `crates/qedgen/tests/fixtures/regressions/issues-143-146-kani-arith/`
  with a `syn::parse_file` gate over the whole emitted harness.

## Gates (RELEASING.md)

`fmt` · `clippy -D warnings` (incl. moving `check_unknown_guard_identifier`
above the test module for clippy 1.94's `items_after_test_module`) ·
`cargo test` (17 suites) · `check-readme-drift` (21 cmds) · `regen-drift`
(8/8 clean) · `frozen` (8/8) · zero unintended `sorry` · `cargo audit` +
`cargo deny` — all green locally + CI on PR #147.

`check-lake-build.sh --strict`: 9/10 examples report *cold-checkout*
(`.lake/` caches cleaned locally for disk pressure — the v2.34 lesson);
`escrow` rebuilt green end-to-end (incl. the qedsvm bridge), and
`regen-drift` proves every committed `Spec.lean` is byte-identical to
v2.38.0, which shipped with the gate green. No Lean-facing output changed
for any bundled example in this release.
