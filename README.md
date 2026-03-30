<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/logo-light.png">
    <img src="docs/logo-dark.png" alt="QEDGen" width="260">
  </picture>
</p>

<h3 align="center">Prove your Solana code is correct. Mathematically.</h3>

<p align="center">
  <a href="https://qedgen.dev">Website</a> &middot;
  <a href="https://github.com/qedgen/solana-skills/blob/main/SKILL.md">Docs</a> &middot;
  <a href="https://github.com/qedgen/solana-skills/issues">Issues</a>
</p>

<p align="center">
  <a href="https://github.com/qedgen/solana-skills/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://qedgen.dev"><img src="https://img.shields.io/badge/site-qedgen.dev-38bdf8" alt="Website"></a>
</p>

---

An agent skill that formally verifies Solana programs by generating **Lean 4 proofs**. Your agent writes the proofs; Mistral's **Leanstral** model handles hard sub-goals.

```bash
npx skills add qedgen/solana-skills
```

> Works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any agent supporting the [Agent Skills](https://agentskills.io) spec.

## How it works

```
Your Code ──► Your Agent ──► SPEC.md ──► Lean 4 Proofs ──► lake build ──► ∎
                  │                           ▲
                  │                           │
                  └─── iterate on errors ─────┘
                         Leanstral fills
                         hard sub-goals
```

1. Your agent reads the program source and IDL
2. Generates a `SPEC.md` with verification goals and properties
3. Writes Lean 4 proofs against the QEDGen support library
4. Iterates on `lake build` errors until proofs compile
5. Calls `qedgen fill-sorry` for hard sub-goals (powered by Leanstral)

## What it verifies

| Property | Approach |
|---|---|
| **Access control** | Signer checks, authority constraints |
| **CPI correctness** | Correct parameters passed to each transfer (axiomatic, pure `rfl`) |
| **State machines** | Lifecycle correctness, one-shot safety |
| **Arithmetic safety** | Overflow/underflow for fixed-width integers |

CPI calls are axiomatic — we verify the program passes correct parameters. SPL Token internals and the Solana runtime are trusted.

## Setup

Export a [Mistral API key](https://console.mistral.ai) (free tier available):

```bash
export MISTRAL_API_KEY=your_key_here
```

The installer handles Rust, Lean/elan, the CLI binary, and global Mathlib cache automatically. First Mathlib build takes 15-45 min; subsequent builds reuse the cache.

## Usage

### Full pipeline

```bash
qedgen verify \
  --idl target/idl/my_program.json \
  --validate
```

### Generate from an existing prompt

```bash
qedgen generate \
  --prompt-file /tmp/analysis/property.prompt.txt \
  --output-dir /tmp/proof \
  --passes 4 \
  --validate
```

### Fill hard sub-goals

```bash
qedgen fill-sorry \
  --file formal_verification/Proofs/Hard.lean \
  --passes 3 \
  --validate
```

### Consolidate proofs

```bash
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir my_program/formal_verification
```

## Examples

### Rust / Anchor

- **[Escrow](examples/rust/escrow/)** — Token escrow with authority checks, CPI transfer verification, and lifecycle proofs
- **[Percolator](examples/rust/percolator/)** — Market maker with state machine verification and arithmetic safety

### sBPF Assembly

- **[Counter](examples/sbpf/counter/)** — Account counter with 3 verified validation guards (178 instructions)
- **[Tree](examples/sbpf/tree/)** — Red-black tree with 3 verified validation guards (498 instructions)
- **[Transfer](examples/sbpf/transfer/)** — Lamport transfer with account count and data length checks
- **[Slippage](examples/sbpf/slippage/)** — AMM slippage protection with overflow safety

## Requirements

- `MISTRAL_API_KEY` environment variable
- Rust toolchain (auto-installed if missing)
- Lean 4 / elan (auto-installed if missing)

Override the Mathlib cache location with `QEDGEN_VALIDATION_WORKSPACE`.

## License

[MIT](LICENSE)
