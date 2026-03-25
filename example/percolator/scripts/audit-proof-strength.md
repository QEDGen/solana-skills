# Kani Proof Strength Audit Prompt

Use this prompt to analyze Kani proof harnesses for weakness, vacuity, collapse into
unit tests, or failure to achieve inductive strength.

---

Analyze each Kani proof harness for weakness. For every proof, determine:

## Criteria 1–5: Symbolic Testing Quality

1. **Input classification**: Is each input to the function-under-test concrete (hardcoded),
   symbolic (kani::any with kani::assume bounds), or derived (computed from other inputs)?
   A proof where ALL function inputs are concrete is a unit test, not a proof.

2. **Branch coverage**: Read the function-under-test and list every conditional branch
   (if/else, match arms, min/max, saturating ops that could clamp). For each branch,
   determine whether the proof's input constraints ALLOW the solver to reach both sides.
   Flag any branch that is locked to one side by concrete values or overly tight assumes.

3. **Invariant strength**: What does the proof actually assert?
   - valid_state() is weaker than canonical_inv() — flag proofs that use the weaker check
     when canonical_inv exists.
   - Post-condition assertions (like "pnl >= 0") without the full invariant are incomplete.
   - Assertions gated behind `if result.is_ok()` without a non-vacuity check on the Ok path
     may be vacuously true if the solver always takes the Err path.

4. **Vacuity risk**: Can the solver satisfy all kani::assume constraints AND reach the
   assertions? Watch for:
   - Contradictory assumes that make the proof trivially true
   - assume(canonical_inv(...)) on a hand-built state that might not satisfy it
   - assert_ok! on a path that might always error given the constraints

5. **Symbolic collapse**: Even with kani::any(), check if derived values collapse the
   symbolic range. Example: if vault = capital + insurance + pnl, and pnl is symbolic,
   but capital and insurance are concrete and large, the haircut ratio h may always be 1,
   never exercising the h < 1 branch.

## Criterion 6: Inductive Strength

A true inductive invariant proof has the form:

```
∀s: State.  INV(s)  ⟹  INV(f(s))
```

This requires the initial state `s` to be **fully symbolic** — any state satisfying INV,
not a state constructed from `RiskEngine::new()` with selected fields overwritten.

For each proof that asserts `canonical_inv` (or a component) pre- and post-operation,
evaluate whether it achieves inductive strength:

6a. **State construction method**: Does the proof start from a fully symbolic engine with
    `assume(INV)`, or does it call `RiskEngine::new(concrete_params)` and overwrite
    selected fields? The latter fixes hundreds of fields to concrete values (freelist
    topology, cursor positions, funding index, net_lp_pos, etc.), limiting the proof
    to states reachable from that specific construction. Fields outside the function's
    cone of influence are pruned by the solver automatically — a fully symbolic state
    does NOT create an intractably large problem.

6b. **Topology coverage**: Does the proof fix a specific account topology (e.g., exactly
    1 user, 0 LPs), or does it reason over arbitrary topologies? A proof with 1 account
    makes aggregate maintenance trivially correct (c_tot = capital, pnl_pos_tot = pnl).
    Multi-account interactions — where settling account i changes haircut_ratio affecting
    account j — are only testable with 2+ accounts having independent symbolic state.
    The ideal proof is **modular**: it reasons relative to one arbitrary target account
    and an abstract "rest of the system" represented by aggregate summary values.

6c. **Invariant decomposition**: Is canonical_inv checked monolithically, or are its
    components (inv_structural, inv_aggregates, inv_accounting, inv_mode, inv_per_account)
    proven to be preserved independently? Decomposition enables fully symbolic proofs
    because each component has a smaller cone of influence. For example, inv_accounting
    (vault >= c_tot + insurance) depends only on vault, c_tot, and insurance — not on
    individual account fields.

6d. **Loop elimination in invariant specs**: Do the invariant functions (inv_aggregates,
    inv_per_account, inv_structural) use `for idx in 0..MAX_ACCOUNTS` loops? These loops
    make `assume(canonical_inv(engine))` expensive for the solver. Can the invariant be
    reformulated as a loop-free property relative to one target account? For example,
    instead of checking `c_tot == Σ capital[i]`, check that `set_capital` maintains
    `c_tot' = c_tot - old_capital + new_capital` — a loop-free delta property.

6e. **Cone of influence**: List every field of RiskEngine that the function-under-test
    actually reads or writes (directly or via callees). Any field NOT in this set is
    outside the cone of influence — if the proof fixes it to a concrete value, that
    limits generality for no benefit. Flag proofs that construct concrete values for
    fields outside the cone.

6f. **Bounded ranges vs. full domain**: Does the proof constrain symbolic values to
    small ranges (e.g., `capital <= 1000`) when the function's correctness should hold
    for all u128 values? Bounded ranges are a symptom of constructive proofs that hit
    solver limits. An inductive proof with decomposed invariants should not need range
    bounds — the solver handles the full domain because only relevant bits survive
    cone-of-influence pruning.

## Classification

For each proof, output:
- **INDUCTIVE**: Fully symbolic initial state with assume(INV), decomposed invariant
  components, loop-free specs, modular account reasoning. The gold standard.
- **STRONG**: Symbolic inputs exercise all branches, canonical_inv checked, non-vacuous,
  but starts from a constructed state rather than fully symbolic. Good symbolic test,
  not a true inductive proof.
- **WEAK**: Symbolic inputs but misses branches or uses weaker invariant (list which).
- **UNIT TEST**: Concrete inputs, single execution path.
- **VACUOUS**: Assertions may never be reached.

## Recommendations

For each non-INDUCTIVE proof, include specific recommendations:

1. What fields are outside the cone of influence and can be left fully symbolic?
2. Which invariant components can be proven independently?
3. Can loop-based invariant checks be replaced with delta-based (loop-free) properties?
4. Can the proof be modularized to reason about one arbitrary account + abstract aggregates?
5. What is the minimal assume needed (which invariant components) for the specific function?
