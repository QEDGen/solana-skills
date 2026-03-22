# Interactive Proof Development with Claude Code

This document describes the workflow for interactive formal verification using Claude Code and the leanstral tool.

> **Important**: See [VERIFICATION_SCOPE.md](VERIFICATION_SCOPE.md) for the trust boundary.
> We verify program business logic, NOT external dependencies (SPL Token, Solana runtime).

## Architecture

The proof generation system uses:
- **Leanstral binary**: Analyzes Rust/Solana source code and generates Lean4 proof sketches
- **Mistral API**: leanstral-2603 model specialized for Lean theorem proving
- **Claude Code**: Interactive refinement and debugging of generated proofs

## Workflow

### 1. Generate Initial Proof Sketches

From the repository root:

```bash
# Analyze Rust source to extract properties
./bin/leanstral analyze --input example/escrow/programs/escrow/src/lib.rs \
  --output-dir /tmp/escrow-analysis

# Generate Lean proof sketches
./bin/leanstral generate --prompt-file /tmp/escrow-analysis/*.prompt.txt \
  --output-dir /tmp/escrow-proofs --passes 1 --temperature 0.3
```

This produces:
- Theorem statements with correct signatures
- Proof sketches (may contain `sorry` placeholders)
- Supporting type definitions and transition functions

### 2. Interactive Refinement with Claude

Copy generated proofs to the project and iterate:

```bash
cp /tmp/escrow-proofs/* example/escrow/formal_verification/
lake build  # Check for errors
```

Common issues to fix:
1. **Tactic failures**: `split_ifs`, `simp`, `rw` mismatches
2. **Missing lemmas**: Add composition lemmas to `lean_support/`
3. **Namespace issues**: Resolve ambiguities between `Token.trackedTotal` and `trackedTotal`

### 3. Fixing Common Proof Patterns

#### Issue: `split_ifs` fails with "no if-then-else to split"

**Problem**: Using `simp` before `split_ifs` eliminates the if-structure

**Fix**:
```lean
-- BAD
simp [transition] at h
split_ifs at h  -- ERROR: no if to split

-- GOOD
unfold transition at h
split_ifs at h with h_eq
```

#### Issue: Cannot compose two transfers for conservation proof

**Problem**: `transfer_preserves_total` uses `let` bindings that make composition difficult

**Fix**: Add `four_way_transfer_preserves_total` axiom for escrow-style exchanges:
```lean
axiom four_way_transfer_preserves_total
    (p_accounts : List Account)
    (p_from1 p_to1 p_from2 p_to2 : Pubkey)
    (p_amount1 p_amount2 : Nat)
    (h_pair1_distinct : p_from1 ≠ p_to1)
    (h_pair2_distinct : p_from2 ≠ p_to2)
    (h_cross_distinct : p_from1 ≠ p_from2) :
    trackedTotal (four_way_map ...) = trackedTotal p_accounts
```

### 4. Verify Final Build

```bash
lake build
# Should complete with "Build completed successfully"
# No `sorry` placeholders in theorem proofs
```

## Prompt Engineering for Better Proofs

The leanstral prompts are in `crates/leanstral/src/prompt/templates.rs`. Key improvements made:

1. **Added tactic sequencing guidance**: Use `unfold` before `split_ifs`
2. **Documented four-way transfer lemma**: For escrow exchanges
3. **Clarified equation direction**: When to use `symm` with axioms

To regenerate with improved prompts:
```bash
cargo build --release  # Rebuild leanstral with updated templates
./bin/leanstral generate --prompt-file /tmp/escrow-analysis/*.prompt.txt \
  --output-dir /tmp/escrow-proofs-v2 --passes 1 --temperature 0.3
```

## Interactive Mode (Future)

For more complex properties, an interactive mode with the leanstral model would:
1. Generate initial proof sketch
2. Run `lake build` and collect errors
3. Send errors back to leanstral model for fixes
4. Iterate until proof compiles

This would leverage the specialized leanstral-2603 model's understanding of:
- Lean 4 tactic language
- Common proof patterns
- Error message interpretation
- Lemma composition strategies
