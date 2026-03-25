# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Leanstral is a Claude Code skill for formally verifying Solana programs using Lean 4 proofs. Claude (the local LLM) drives proof writing directly — reading code, writing Lean models/theorems/proofs, and iterating on `lake build` errors. Leanstral (Mistral's theorem prover) is called only for hard sub-goals via `fill-sorry`.

**Core workflow**: Claude reads source → writes SPEC.md → writes Lean 4 proofs → `lake build` → iterates → calls `leanstral fill-sorry` for hard sub-goals

## Build and Development Commands

### Build the CLI

```bash
# Build leanstral binary (outputs to ./bin/leanstral)
cargo build --release

# Build just the Lean support library
cd crates/leanstral/lean_support
lake build
```

### Run Tests

```bash
# Rust unit tests
cargo test

# Test Lean support library axioms
cd crates/leanstral/lean_support
lake env lean test_lemmas.lean

# Build the example escrow verification
cd example/escrow/formal_verification
lake build                # Verify all proofs compile
```

### Leanstral Commands

```bash
# Set up global validation workspace (first time: 15-45 min for Mathlib)
leanstral setup

# Generate proofs from a prompt file (used by Claude internally)
leanstral generate \
  --prompt-file /tmp/proof/prompt.txt \
  --output-dir /tmp/proof \
  --passes 3 \
  --temperature 0.3 \
  --validate

# Fill sorry markers in a Lean file (Claude calls this for hard sub-goals)
leanstral fill-sorry \
  --file formal_verification/Proofs/Hard.lean \
  --passes 3 \
  --validate

# Generate a draft SPEC.md from an Anchor IDL
leanstral spec --idl target/idl/program.json --output-dir ./formal_verification

# Consolidate multiple proof projects into single project
leanstral consolidate \
  --input-dir /tmp/proofs \
  --output-dir formal_verification
```

## Architecture

### Crate Structure

**`crates/leanstral/`** - Single crate: CLI and Mistral API client
- `main.rs` - CLI entry points (generate, fill-sorry, spec, consolidate, setup)
- `api.rs` - Mistral API client, pass@N sampling, sorry-filling, retry logic
- `validate.rs` - Lake build validation in persistent workspace
- `project.rs` - Lean project scaffolding generation
- `consolidate.rs` - Merges multiple proof projects
- `spec.rs` - SPEC.md generation from Anchor IDL

**`crates/leanstral/lean_support/`** - Canonical Lean axioms for Solana
- `Leanstral/Solana/Account.lean` - Account structure
- `Leanstral/Solana/Token.lean` - Token operations and conservation axioms
- `Leanstral/Solana/Authority.lean` - Authorization predicates
- `Leanstral/Solana/State.lean` - Lifecycle and state machines

### Key Design Decisions

**Why Claude-driven (not pipeline-driven)?**
- Claude reads code context and writes proofs directly — no lossy analyzer step
- Proof patterns generalize across programs without per-property prompt templates
- Claude iterates on `lake build` errors naturally
- Scales to large programs without combinatorial prompt explosion

**Why Leanstral only for sorry-filling?**
- Full module generation requires too much context (import ordering, namespace management)
- Focused sorry-filling gives Leanstral maximum signal with minimal noise
- Claude handles the modeling/structuring; Leanstral handles hard tactic proofs

**Why pass@N sampling?**
- Leanstral is non-deterministic; multiple attempts increase success rate
- Validation selects compilable proof over heuristics (sorry count)

**Why persistent validation workspace?**
- Lake's first Mathlib build takes 15-45 minutes
- Reusing `.lake/packages/` avoids repeated Mathlib compilation
- Location: platform cache dir or `LEANSTRAL_VALIDATION_WORKSPACE`

**Why axioms instead of proving SPL Token?**
- Verification scope: program logic only (see VERIFICATION_SCOPE.md)
- Trust boundary: SPL Token, Solana runtime, CPI mechanics
- Pragmatic: keeps proofs tractable and completion time reasonable

## Verification Scope

**What we verify:**
- Authorization (signer checks, constraints)
- Conservation (token totals preserved)
- State machines (lifecycle, one-shot safety)
- Arithmetic safety (overflow/underflow)
- CPI correctness (parameters match intent)

**What we trust (axioms):**
- SPL Token implementation
- Solana runtime (PDA derivation, account ownership)
- CPI mechanics
- Anchor framework

See `example/escrow/formal_verification/VERIFICATION_SCOPE.md` for details.

## Common Development Tasks

### Adding New Axioms

When a proof pattern is reusable across programs:

1. Add to `crates/leanstral/lean_support/Leanstral/Solana/Token.lean` (or other module)
2. Document the trust assumption with a comment
3. Export in `Leanstral.lean`
4. Update SKILL.md support library API section
5. Test: `cd crates/leanstral/lean_support && lake build`

### Debugging Failed Proofs

If `lake build` fails:
1. Read the error output directly
2. Common issues:
   - `split_ifs` fails → use `unfold` before `split_ifs`
   - `omega could not prove` → unfold named predicates in BOTH hypothesis and goal: `unfold pred at h ⊢`
   - `no goals to be solved` → remove redundant tactic (e.g., `· contradiction` after auto-closed branch)
   - `unexpected token 'open'` → use `«open»` quoting for Lean keywords
   - Namespace collision → check `open` statements
3. Fix the proof and re-run `lake build`

## Environment Variables

- `MISTRAL_API_KEY` - Required for `fill-sorry` and `generate` commands
- `LEANSTRAL_VALIDATION_WORKSPACE` - Override validation workspace path (default: platform cache dir)

## Common Lean Proof Patterns

### Tactic Sequencing
```lean
-- BAD: simp eliminates if-structure
simp [transition] at h
split_ifs at h  -- ERROR

-- GOOD: unfold preserves structure
unfold transition at h
split_ifs at h with h_eq
```

### Conservation Proofs
```lean
-- CRITICAL: unfold named predicate in BOTH hypothesis and goal
unfold conservation at h_inv ⊢
omega
```

### CPI Correctness (pure rfl)
```lean
theorem cpi_correct (ctx : Context) :
    let cpi := build_cpi ctx
    cpi.program = TOKEN_PROGRAM_ID ∧ cpi.amount = ctx.amount := by
  unfold build_cpi
  exact ⟨rfl, rfl⟩
```

## Output Artifacts

After `leanstral generate`:
```
/tmp/proof/
├── Best.lean              # Selected best completion
├── metadata.json          # Rankings, timings, tokens
├── prompt.txt             # Prompt sent to Leanstral
├── attempts/
│   ├── completion_0.lean
│   ├── completion_0_raw.txt
│   └── ...
└── validation/
    └── completion_0.log   # Lake build log
```

## Notes

- First Lean build is expensive (15-45 min for Mathlib). Run `leanstral setup` first.
- If `lake build` fails with "could not resolve 'HEAD' to a commit", remove `.lake/packages/mathlib` and run `lake update`.
- Binary is built to `./bin/leanstral`, not `target/release/leanstral`.
- The SKILL.md file defines the full proof-writing workflow that Claude follows.
