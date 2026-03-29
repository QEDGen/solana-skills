# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

QEDGen is a Claude Code skill for formally verifying Solana programs using Lean 4 proofs. Claude (the local LLM) drives proof writing directly — reading code, writing Lean models/theorems/proofs, and iterating on `lake build` errors. Leanstral (Mistral's theorem prover) is called only for hard sub-goals via `fill-sorry`.

**Core workflow**: Claude reads source → writes SPEC.md → writes Lean 4 proofs → `lake build` → iterates → calls `qedgen fill-sorry` for hard sub-goals

## Build and Development Commands

### Build the CLI

```bash
# Build qedgen binary (outputs to ./bin/qedgen)
cargo build --release

# Build just the Lean support library
cd crates/qedgen/lean_support
lake build
```

### Run Tests

```bash
# Rust unit tests
cargo test

# Test Lean support library axioms
cd crates/qedgen/lean_support
lake env lean test_lemmas.lean

# Build the example escrow verification
cd example/escrow/formal_verification
lake build                # Verify all proofs compile
```

### QEDGen Commands

```bash
# Set up global validation workspace (first time: 15-45 min for Mathlib)
qedgen setup

# Generate proofs from a prompt file (used by Claude internally)
qedgen generate \
  --prompt-file /tmp/proof/prompt.txt \
  --output-dir /tmp/proof \
  --passes 3 \
  --temperature 0.3 \
  --validate

# Fill sorry markers in a Lean file (Claude calls this for hard sub-goals)
qedgen fill-sorry \
  --file formal_verification/Proofs/Hard.lean \
  --passes 3 \
  --validate

# Generate a draft SPEC.md from an Anchor IDL
qedgen spec --idl target/idl/program.json --output-dir ./formal_verification

# Consolidate multiple proof projects into single project
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir formal_verification

# Transpile sBPF assembly to Lean 4 program module
qedgen asm2lean \
  --input example/transfer/src/transfer.s \
  --output formal_verification/TransferProg.lean \
  --namespace TransferProg
```

## Architecture

### Crate Structure

**`crates/qedgen/`** - Single crate: CLI and Mistral API client
- `main.rs` - CLI entry points (generate, fill-sorry, spec, consolidate, asm2lean, setup)
- `api.rs` - Mistral API client, pass@N sampling, sorry-filling, retry logic
- `asm2lean.rs` - sBPF assembly → Lean 4 transpiler (parses `.s`, emits program module)
- `validate.rs` - Lake build validation in persistent workspace
- `project.rs` - Lean project scaffolding generation
- `consolidate.rs` - Merges multiple proof projects
- `spec.rs` - SPEC.md generation from Anchor IDL

**`crates/qedgen/lean_support/`** - Canonical Lean axioms for Solana
- `QEDGen/Solana/Account.lean` - Account structure
- `QEDGen/Solana/Authority.lean` - Authorization predicates
- `QEDGen/Solana/Cpi.lean` - Generic CPI envelope (invoke_signed model)
- `QEDGen/Solana/State.lean` - Lifecycle and state machines
- `QEDGen/Solana/Valid.lean` - Numeric bounds and validity predicates

### Key Design Decisions

**Why Claude-driven (not pipeline-driven)?**
- Claude reads code context and writes proofs directly — no lossy analyzer step
- Proof patterns generalize across programs without per-property prompt templates
- Claude iterates on `lake build` errors naturally
- Scales to large programs without combinatorial prompt explosion

**Why Leanstral model only for sorry-filling?**
- Full module generation requires too much context (import ordering, namespace management)
- Focused sorry-filling gives Leanstral maximum signal with minimal noise
- Claude handles the modeling/structuring; Leanstral handles hard tactic proofs

**Why pass@N sampling?**
- The Leanstral model is non-deterministic; multiple attempts increase success rate
- Validation selects compilable proof over heuristics (sorry count)

**Why persistent validation workspace?**
- Lake's first Mathlib build takes 15-45 minutes
- Reusing `.lake/packages/` avoids repeated Mathlib compilation
- Location: platform cache dir or `QEDGEN_VALIDATION_WORKSPACE`

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
- CPI correctness (program, accounts, discriminator match intent)

**What we trust (axioms):**
- SPL Token implementation
- Solana runtime (PDA derivation, account ownership)
- CPI mechanics
- Anchor framework

See `example/escrow/formal_verification/VERIFICATION_SCOPE.md` for details.

## Common Development Tasks

### Adding New Axioms

When a proof pattern is reusable across programs:

1. Add to the appropriate module in `crates/qedgen/lean_support/QEDGen/Solana/`
2. Document the trust assumption with a comment
3. Export in `QEDGen.lean`
4. Update SKILL.md support library API section
5. Test: `cd crates/qedgen/lean_support && lake build`

### Debugging Failed Proofs

If `lake build` fails:
1. Read the error output directly
2. Common issues:
   - `split_ifs` fails → use `unfold` before `split_ifs`
   - `omega could not prove` → unfold named predicates in BOTH hypothesis and goal: `unfold pred at h ⊢`
   - `no goals to be solved` → remove redundant tactic (e.g., `· contradiction` after auto-closed branch)
   - `unexpected token 'open'` → use `«open»` quoting for Lean keywords
   - Namespace collision → check `open` statements
   - `simp` timeout on sBPF proofs → see **sBPF simp performance** section below
3. Fix the proof and re-run `lake build`

### sBPF Proof Workflow

For sBPF assembly programs, use `qedgen asm2lean` to generate the program module instead of hand-transcribing:

```bash
qedgen asm2lean --input src/program.s --output formal_verification/Prog.lean
```

Then write proofs in a separate file that imports the generated module:

```lean
import QEDGen.Solana.SBPF
import Prog
open Prog

theorem my_property ... :=
    (execute prog (initState inputAddr mem) FUEL).exitCode = some CODE := by
  sbpf_steps
```

The `sbpf_steps` tactic automates sBPF execution unrolling via `simp`. It requires `@[simp]` on `prog`.

### sBPF simp Performance (Critical)

The `sbpf_steps` tactic is extremely sensitive to how constants are typed and named. Violations cause exponential blowup (seconds → hours).

**Rule 1: Offset constants MUST be `Int`, not `Nat`.**
`effectiveAddr` takes `(off : Int)`. With `Nat` offsets, Lean inserts a `Nat → Int` coercion that `simp` cannot efficiently process.
```lean
-- BAD: causes simp timeout
abbrev MY_OFFSET : Nat := 80

-- GOOD: matches effectiveAddr signature directly
abbrev MY_OFFSET : Int := 80
```

**Rule 2: Named constants in `prog` MUST match hypothesis names.**
`simp` uses syntactic matching. If `prog` has a raw numeric but the hypothesis uses a named constant, `simp` must unfold the constant at every subterm at every step.
```lean
-- BAD: prog has 80, hypothesis has MY_OFFSET — simp must unfold at each step
@[simp] def prog := #[ .ldx .dword .r2 .r1 80, ... ]
theorem t ... (h : readU64 mem (effectiveAddr inputAddr MY_OFFSET) = v) ...

-- GOOD: both use MY_OFFSET — syntactic match, instant
@[simp] def prog := #[ .ldx .dword .r2 .r1 MY_OFFSET, ... ]
theorem t ... (h : readU64 mem (effectiveAddr inputAddr MY_OFFSET) = v) ...
```

**Rule 3: `@[simp]` on `prog` is required.** The tactic needs to evaluate `prog[n]?` at each step.

The `qedgen asm2lean` command handles Rules 1-3 automatically: it emits `Int`-typed offsets, `Nat`-typed non-offsets, named constants in the `prog` array, and `@[simp]` on `prog`.

## Environment Variables

- `MISTRAL_API_KEY` - Required for `fill-sorry` and `generate` commands
- `QEDGEN_VALIDATION_WORKSPACE` - Override validation workspace path (default: platform cache dir)

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
-- Build a generic CpiInstruction (models invoke_signed)
def build_cpi (ctx : Context) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [⟨ctx.src, false, true⟩, ⟨ctx.dst, false, true⟩, ⟨ctx.auth, true, false⟩]
  , data := [DISC_TRANSFER] }

theorem cpi_correct (ctx : Context) :
    let cpi := build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.src false true ∧
    accountAt cpi 1 ctx.dst false true ∧
    accountAt cpi 2 ctx.auth true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩
```

## Output Artifacts

After `qedgen generate`:
```
/tmp/proof/
├── Best.lean              # Selected best completion
├── metadata.json          # Rankings, timings, tokens
├── prompt.txt             # Prompt sent to Leanstral model
├── attempts/
│   ├── completion_0.lean
│   ├── completion_0_raw.txt
│   └── ...
└── validation/
    └── completion_0.log   # Lake build log
```

## Notes

- First Lean build is expensive (15-45 min for Mathlib). Run `qedgen setup` first.
- If `lake build` fails with "could not resolve 'HEAD' to a commit", remove `.lake/packages/mathlib` and run `lake update`.
- Binary is built to `./bin/qedgen`, not `target/release/qedgen`.
- The SKILL.md file defines the full proof-writing workflow that Claude follows.
