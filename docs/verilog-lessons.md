# Lessons from Verilog/SystemVerilog Formal Verification for QEDGen

## 1. SystemVerilog Assertions (SVA) — Temporal Properties

**Hardware concept.** SVA distinguishes three assertion types: `assert property` (must hold), `assume property` (constrains inputs/environment), `cover property` (must be reachable). Properties can express temporal sequences: `a |-> ##[1:3] b` means "if `a`, then `b` within 1-3 cycles." This enables liveness ("eventually responds"), bounded response ("completes within N cycles"), and fairness ("every requester eventually gets served").

**Mapping to Solana.** QEDGen's `.qedspec` currently expresses safety properties (invariants that hold after every operation) but lacks temporal/sequencing properties. Solana programs have multi-transaction lifecycles (escrow: initialize → deposit → exchange → cancel). The spec can say "if state is X, guard G must hold" but cannot say "after initialize, exchange must become reachable within N operations" (bounded liveness) or "cancel is always reachable from any non-terminal state" (fairness).

**Action item.** Add three property kinds to `.qedspec`:
- `assert` (existing: universal invariants)
- `assume` (environment constraints — e.g., "clock always advances," "oracle price is bounded") — these become hypotheses in Lean proofs and preconditions in Kani harnesses rather than things to prove
- `cover` (reachability targets — e.g., "the happy path `Init → Active → Settled` is reachable") — these generate Lean existential proofs (`∃ trace, ...`) and Kani reachability checks (`kani::cover!`)

Also add a `lifecycle_sequence` block for bounded temporal properties: "from state A, state B is reachable in at most K operations." This generates a bounded induction proof in Lean.

---

## 2. Formal Property Verification Strategies

**Hardware concept.** Tools like JasperGold use k-induction (prove base case for k steps, then inductive step), abstraction (replace complex sub-blocks with simpler models), decomposition (split a property into sub-properties on sub-blocks), and case splitting (enumerate over a finite control variable).

**Mapping to Solana.** QEDGen currently generates monolithic per-property proofs. For complex programs with many operations, proof terms grow large and `lake build` times increase. k-induction maps directly to multi-step lifecycle proofs. Abstraction maps to our existing axiom trust boundary (SPL Token is axiomatized). Decomposition maps to splitting proofs by operation.

**Action item.** Implement automatic proof decomposition in the Lean code generator:
1. For conservation properties, generate one sub-lemma per operation, then a master theorem that case-splits on the operation discriminant and applies each sub-lemma.
2. For lifecycle properties, use k-induction: prove the invariant holds at init (base), then prove that any single valid transition preserves it (inductive step).
3. Document which axioms constitute the abstraction boundary and generate an explicit `axiom_manifest` in the Lean output listing every `axiom` or `sorry` with its trust justification.

---

## 3. Coverage-Driven Verification

**Hardware concept.** Hardware verification tracks functional coverage (which states/transitions were exercised), assertion coverage (which assertions fired), and proof convergence. A verification plan maps requirements to coverage points.

**Mapping to Solana.** QEDGen's current coverage model is binary: the proof compiles or it doesn't. There is no tracking of which operations each property covers, which state transitions are exercised, or which guards are tested.

**Action item.** Generate a verification coverage matrix automatically from the `.qedspec`:
- Rows: every `(operation, guard, effect)` triple
- Columns: every property
- Cell: whether the property's proof references that triple
- Compute coverage percentage and emit warnings for uncovered operations/guards

Concretely, this is a static analysis pass over the generated Lean: check which operation-specific lemmas are referenced by which property proofs. Emit a `coverage.json` and a human-readable table. Flag any operation with zero property coverage as a verification gap.

---

## 4. Assume-Guarantee Reasoning

**Hardware concept.** Large SoCs are verified block-by-block. Block A's outputs are assumptions for Block B's inputs, and vice versa. The composition theorem: if A guarantees G given assumption P, and B guarantees P given assumption G, then A||B satisfies both.

**Mapping to Solana.** Solana programs compose via CPI. Currently QEDGen axiomatizes CPI targets. But for user-composed programs (e.g., a vault calling an AMM), the axiom approach doesn't scale — you need to verify the composition.

**Action item.** Add a `cpi_contract` block to `.qedspec`:
```
cpi_contract transfer_tokens {
  assume: src.amount >= amount
  guarantee: src.amount' = src.amount - amount AND dst.amount' = dst.amount + amount
}
```
The callee's proof must discharge the guarantee. The caller's proof may assume it, provided it discharges the assumption. The Lean generator emits the guarantee as a theorem in the callee's project and as a hypothesis in the caller's proof.

---

## 5. Clock Domain Crossing → Cross-Program State Consistency

**Hardware concept.** CDC analysis verifies that signals crossing clock domains are properly synchronized. The core issue: state observed in one domain may be stale or transitioning in another.

**Mapping to Solana.** The analog is cross-program state consistency: Program A reads an account that Program B can modify in the same slot. "Stale state" = reading an account whose invariants were established by a different program, which may have been modified since.

**Action item.** Add a `shared_account` annotation to `.qedspec` that marks accounts readable by external programs. For these accounts, automatically generate a staleness property: "the property holds even if the shared account's value is any value satisfying its type constraints." In Lean, this means universally quantifying over the shared account's fields rather than constraining them to post-operation values.

---

## 6. Linting and Static Analysis

**Hardware concept.** Verilog linters catch combinational loops, inferred latches, undriven/unread signals, width mismatches, and sensitivity list incompleteness.

**Action items — new lint rules:**
1. **Unreachable state** (analog: undriven net) — a lifecycle state with no incoming transition. Always a spec bug.
2. **Dead guard** (analog: dead code) — a guard condition subsumed by another on the same operation, so it never independently blocks.
3. **Missing signer check** (analog: missing clock enable) — an operation that modifies state but has no `signer` guard.
4. **Unbounded arithmetic** (analog: width mismatch) — an effect performing arithmetic with no overflow guard.
5. **Write-without-read** (analog: write-only register) — a field written in effects but never read in guards or properties.
6. **Circular lifecycle** (analog: combinational loop) — a lifecycle where state A→B→A with no terminal state.

---

## 7. Equivalence Checking

**Hardware concept.** Formal equivalence checking (FEC) verifies that two representations (RTL vs gate-level netlist) are functionally identical for all inputs.

**Mapping to Solana.** This maps directly to spec-vs-implementation consistency. The spec says `effect: vault.amount += deposit_amount`; the Rust code does `checked_add`.

**Action item.** For brownfield programs, generate Kani harnesses that are equivalence checks — asserting the Rust function's output matches the spec's effect for all inputs within the Kani bound:
```rust
#[kani::proof]
fn equiv_check_deposit() {
    let input = kani::any::<DepositInput>();
    let pre_state = kani::any::<State>();
    kani::assume(/* spec guards */);
    let spec_post = spec_effect(pre_state, input);
    let impl_post = actual_deposit(pre_state, input);
    assert_eq!(spec_post, impl_post);
}
```

---

## Priority Ranking (effort-to-impact)

1. **Coverage matrix** (area 3) — low effort, immediately reveals spec gaps, no Lean changes needed
2. **New lint rules** (area 6) — extends existing linter, catches real bugs at spec time
3. **Proof decomposition** (area 2) — reduces proof size and build time for complex programs
4. **Assume/cover/assert property kinds** (area 1) — makes the spec language more expressive
5. **Equivalence checking harnesses** (area 7) — critical for brownfield adoption
6. **CPI assume-guarantee contracts** (area 4) — needed when verifying composed programs
7. **Shared account staleness** (area 5) — niche but important for DeFi programs reading oracles
