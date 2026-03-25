# QEDGen Solana Skills

An agent skill that formally verifies Solana programs by generating Lean 4 proofs. Your agent writes the proofs; Mistral's **Leanstral** model handles hard sub-goals.

## Installation

```bash
npx skills add QEDGen/solana-skills
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
qedgen verify \
  --idl target/idl/my_program.json \
  --validate
```

The IDL is the primary input. Optional flags: `--input` (Rust source, passed to the LLM as context but not parsed) and `--tests` (test files, used as hint signals for property ranking).

This analyzes the program, ranks candidate properties, generates proofs via pass@N sampling, validates them with `lake build`, and retries on compiler errors.

### Analysis only

```bash
qedgen analyze \
  --idl target/idl/my_program.json
```

Emits `analysis.json` with ranked property candidates and one prompt file per property.

### Generate from an existing prompt

```bash
qedgen generate \
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

### Consolidate proofs

```bash
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir my_program/formal_verification
```

Merges validated `Best.lean` files into a single Lean project with namespaced proofs.

## What It Verifies

- **Access control** — signer checks, authority constraints
- **CPI correctness** — correct parameters passed to each transfer (axiomatic, pure `rfl`)
- **State machines** — lifecycle correctness, one-shot safety
- **Arithmetic safety** — overflow/underflow for fixed-width integers

CPI calls are treated as axiomatic (external to business logic). We verify the program passes correct parameters — SPL Token internals and Solana runtime are trusted.

## Requirements

- `MISTRAL_API_KEY` environment variable
- Rust toolchain (auto-installed if missing)
- Lean 4 / elan (auto-installed if missing)

The first Mathlib build takes 15-45 minutes. Subsequent builds reuse the global cache. Override the cache location with `QEDGEN_VALIDATION_WORKSPACE`.

## Supported Agents

Works with any agent implementing the [Agent Skills spec](https://agentskills.io): Claude Code, Cursor, Windsurf, GitHub Copilot, and others.

## License

MIT
