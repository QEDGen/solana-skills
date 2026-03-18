# V1 Scope

This project is intentionally scoped to a small proof surface.

## Supported Direction

- IDL-first Anchor program analysis
- proof planning at the instruction/account/state level
- Lean support library for common Solana semantics
- one property at a time
- explicit local validation and repair
- explicit coverage accounting

## Supported Property Classes

- access control
- conservation
- close / one-shot lifecycle safety
- basic arithmetic safety

## Supported Semantic Surface

- account authority and writable checks
- token-style tracked balances
- simple lifecycle transitions
- instruction-level preconditions and postconditions

## Explicit Non-Goals

- full SVM / sBPF semantics
- arbitrary CPI-heavy protocol verification
- one-shot full-program proofs with no decomposition
- pretending unsupported cases are fully proved

## Product Expectation

The skill should return a proof bundle with explicit coverage:

- planned obligations
- validated obligations
- partially proved obligations
- failed obligations
- unsupported obligations

The honest output is more important than claiming complete proof coverage when that is not achievable.
