# Leanstral Solana Skill

An agent skill that formally verifies Solana programs by generating Lean 4 proofs via Mistral's **Leanstral** model (`labs-leanstral-2603`).

## Installation

```bash
npx skills add abishekk92/leanstral-solana-skill
```

The installer automatically sets up Rust, Lean/elan, the CLI binary, and a global Mathlib cache.

## Setup

Export a free Mistral API key from [console.mistral.ai](https://console.mistral.ai):

```bash
export MISTRAL_API_KEY=your_key_here
```

## Usage

### Full pipeline (recommended)

```bash
leanstral verify \
  --idl target/idl/my_program.json \
  --input programs/my_program/src/lib.rs \
  --tests tests/my_program.ts \
  --output-dir /tmp/proofs \
  --top-k 3 \
  --validate \
  --repair-rounds 1
```

This analyzes the program, ranks candidate properties, generates proofs via pass@N sampling, validates them with `lake build`, and retries on compiler errors.

### Analysis only

```bash
leanstral analyze \
  --idl target/idl/my_program.json \
  --input programs/my_program/src/lib.rs \
  --tests tests/my_program.ts \
  --output-dir /tmp/analysis
```

Emits `analysis.json` with ranked property candidates and one prompt file per property.

### Generate from an existing prompt

```bash
leanstral generate \
  --prompt-file /tmp/analysis/property.prompt.txt \
  --output-dir /tmp/proof \
  --passes 4 \
  --validate
```

### Verify output

```bash
cd /tmp/proofs/<property_id>
lake build  # Success = formally verified
```

## What It Verifies

- **Access control** — signer checks, `has_one`, authority constraints
- **Conservation** — token totals preserved across transfers
- **State machines** — lifecycle correctness, one-shot safety
- **Arithmetic safety** — overflow/underflow for fixed-width integers

CPI parameters are verified for correct construction (program ID, distinct accounts, bounded amounts). SPL Token internals and Solana runtime are trusted axioms.

## Requirements

- `MISTRAL_API_KEY` environment variable
- Rust toolchain (auto-installed if missing)
- Lean 4 / elan (auto-installed if missing)

The first Mathlib build takes 15-45 minutes. Subsequent builds reuse the global cache. Override the cache location with `LEANSTRAL_VALIDATION_WORKSPACE`.

## Supported Agents

Works with any agent implementing the [Agent Skills spec](https://agentskills.io): Claude Code, Cursor, Windsurf, GitHub Copilot, and others.

## License

MIT
