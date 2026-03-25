# QEDGen.Solana Support Library

This directory contains the **canonical** Lean support library for Solana program verification.

## Purpose

Provides pre-bundled axioms and types that model:
- **Account**: Solana account structure (key, authority, balance)
- **Token**: SPL Token operations as trusted axioms
- **State**: Lifecycle and state machine types
- **Authority**: Authorization predicates

## Trust Boundary

These axioms represent the **trust boundary** between:
- ✅ **Program logic** (what we verify)
- ⚠️ **External dependencies** (what we trust)

See `QEDGen/Solana/Token.lean` for detailed documentation of trust assumptions.

## Usage in Projects

### Option 1: Copy to Project (Recommended)

When generating proofs for a project:

```bash
# The qedgen tool automatically copies this library
./bin/qedgen analyze --input program.rs --output-dir /tmp/analysis
./bin/qedgen generate ... --output-dir /tmp/proofs
```

The generated Lean project will include a `lean_support/` directory with this library.

### Option 2: Symlink (For Development)

If you're actively developing the support library:

```bash
cd your-project/formal_verification/
ln -s ../../../crates/qedgen/lean_support .
```

## Pre-Bundled Axioms

### Token Conservation

```lean
-- Single transfer preserves total
axiom transfer_preserves_total : ...

-- Four-way transfer (escrow-style exchanges) preserves total
axiom four_way_transfer_preserves_total : ...

-- Balance updates with zero delta preserve total
axiom balance_update_preserves_total : ...
```

### Utility Lemmas

```lean
-- List operations on tracked totals
axiom trackedTotal_cons : ...
axiom trackedTotal_append : ...
theorem trackedTotal_map_id : ...
```

## Adding New Axioms

When adding axioms that are reusable across **all** Solana programs:

1. Add to the appropriate module in `QEDGen/Solana/`
2. Export in the `QEDGen.Solana` namespace
3. Document the trust assumption
4. Update `templates.rs` to include in support API documentation
5. Test by rebuilding: `lake build`

## Design Principles

1. **Axioms, not proofs**: External dependencies are trusted, not verified
2. **Minimal surface**: Only include what's needed for common Solana patterns
3. **Composable**: Axioms should compose to prove program properties
4. **Clear trust boundary**: Document what we verify vs. what we trust

## Testing

```bash
# Build the support library
lake build

# Test that all axioms are well-typed
lake env lean test_lemmas.lean
```

## Files

- `lakefile.lean`: Lake build configuration
- `QEDGen.lean`: Root module that exports everything
- `QEDGen/Solana/Account.lean`: Account structure and field access
- `QEDGen/Solana/Token.lean`: Token operations and conservation axioms
- `QEDGen/Solana/State.lean`: Lifecycle and state machine types
- `QEDGen/Solana/Authority.lean`: Authorization predicates
- `test_lemmas.lean`: Smoke tests for type-checking axioms
