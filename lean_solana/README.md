# QEDGen.Solana Support Library

This directory contains the **canonical** Lean support library for Solana program verification.

## Purpose

Provides pre-bundled axioms and types that model:
- **Account**: Solana account structure (key, authority, balance)
- **Cpi**: Generic CPI envelope (invoke_signed model with AccountMeta)
- **State**: Lifecycle and state machine types
- **Authority**: Authorization predicates
- **Valid**: Numeric bounds and validity predicates

## Trust Boundary

These axioms represent the **trust boundary** between:
- ✅ **Program logic** (what we verify)
- ⚠️ **External dependencies** (what we trust)

CPI verification focuses on structural correctness: correct program, correct accounts
with correct signer/writable flags, and correct instruction discriminator. Parameter
serialization within instruction data is trusted (SDK territory).

## Usage in Projects

### Option 1: Copy to Project (Recommended)

When generating proofs for a project:

```bash
# The qedgen tool automatically copies this library
./bin/qedgen generate ... --output-dir /tmp/proofs
```

The generated Lean project will include a `lean_solana/` directory with this library.

### Option 2: Symlink (For Development)

If you're actively developing the support library:

```bash
cd your-project/formal_verification/
ln -s path/to/lean_solana .
```

## Adding New Axioms

When adding axioms that are reusable across **all** Solana programs:

1. Add to the appropriate module in `QEDGen/Solana/`
2. Export in the `QEDGen.Solana` namespace
3. Document the trust assumption
4. Test by rebuilding: `lake build`

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
- `QEDGen/Solana/Cpi.lean`: Generic CPI envelope (invoke_signed model)
- `QEDGen/Solana/State.lean`: Lifecycle and state machine types
- `QEDGen/Solana/Valid.lean`: Numeric bounds and validity predicates
- `test_lemmas.lean`: Smoke tests for type-checking axioms
