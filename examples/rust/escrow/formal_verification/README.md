# Solana Program Lean Proofs

This directory contains formal verification proofs for the Solana program, generated using QEDGen.

## Building and Verifying

To build and verify all proofs:

```bash
lake build
```

This will verify all theorems and ensure they compile correctly.

## Structure

All proofs are contained in `Proofs.lean`, organized into namespaces to avoid naming conflicts:
- Each proof has its own namespace
- Shared definitions from the QEDGen Solana library are imported at the top
- The `lean_solana` directory contains the Solana modeling framework

## Generated Proofs

See `Proofs.lean` for the complete list of theorems and their proofs.
