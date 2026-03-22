# Solana Escrow Program Formal Verification

This directory contains formal verification proofs for the Solana escrow program, generated using the **leanstral tool** with LLM-based proof generation and interactive refinement.

## Building and Verifying

To build and verify all proofs:

```bash
lake build
```

This will verify all theorems and ensure they compile correctly.

## Structure

- **EscrowProofs.lean**: All proofs organized into namespaces
- **lean_support/**: Solana modeling framework (Account, Token, State, Authority)
- **claude.md**: Interactive proof development workflow for Claude Code users

## Proofs Included

The following properties are formally verified:

1. **Access Control**: Cancel and exchange operations require proper authorization
2. **Token Conservation**: Token balances are preserved across transfers
3. **State Machine**: Lifecycle transitions (open → closed) are correct
4. **Arithmetic Safety**: Numeric operations stay within bounds

See `EscrowProofs.lean` for complete theorem statements and proofs.

## Interactive Development

For details on the proof generation and refinement workflow, including:
- How to regenerate proofs from Rust source
- Common issues and fixes
- Prompt engineering for better proofs
- Future interactive mode with leanstral

See **[claude.md](claude.md)**
