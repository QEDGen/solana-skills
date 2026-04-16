<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/logo-light.png">
    <img src="docs/logo-dark.png" alt="QEDGen" width="260">
  </picture>
</p>

<h3 align="center">Proofs, not promises.</h3>
<p align="center"><em>Ship to mainnet without fear.</em></p>

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

Write what your Solana program must guarantee in a `.qedspec` file. QEDGen validates the spec, finds bugs your tests miss, then generates everything needed to keep them fixed: **property tests**, **Kani harnesses**, **Lean 4 proofs**, **program code**, and **CI workflows** — all from a single source of truth. Supports **Anchor**, **Quasar**, and **sBPF assembly**.

```bash
npx skills add qedgen/solana-skills
```

> Works with Claude Code, Cursor, Windsurf, GitHub Copilot, and any agent supporting the [Agent Skills](https://agentskills.io) spec.

## How it works

```
.qedspec ──► check (validate spec) ──► codegen --all ──► lake build ──► ∎
                │                          │                  ▲       │
                ├── lint (instant)          ├── Rust skeleton  │       ├─► Leanstral (fast)
                ├── proptest (~100ms)       ├── Lean proofs    │       └─► Aristotle (deep)
                └── lean-gen (seconds)      ├── Kani harnesses └── iterate
                                            └── tests
```

1. **Define guarantees** — write a `.qedspec` describing what your program must guarantee, or let your agent generate one from the code or IDL
2. **Validate** — `qedgen check` runs the verification waterfall: lint catches structural issues, property tests find counterexamples in milliseconds, Lean catches what tests can't
3. **Generate** — `qedgen codegen --all` produces program code, test harnesses, Lean proofs, and CI workflows from the single spec
4. **Prove** — your agent fills proof obligations; Leanstral handles routine sub-goals (seconds), Aristotle handles the hardest ones (minutes–hours)

## What it verifies

| Property | Approach |
|---|---|
| **Access control** | Signer checks, authority constraints |
| **CPI correctness** | Correct program, accounts, flags, and discriminator for each invocation (axiomatic, pure `rfl`) |
| **State machines** | Lifecycle correctness, one-shot safety |
| **Conservation** | Custom invariants (token totals, vault bounds) preserved across operations |
| **Arithmetic safety** | Overflow/underflow for fixed-width integers, U64 bounds |
| **Input validation** | Account count, duplicates, data length, discriminators, parameter bounds — each guard maps to a specific error exit |
| **Memory correctness** | Stack/heap disjointness, pointer arithmetic (sBPF) |
| **PDA integrity** | Program-derived address derivation and 4-chunk comparison (sBPF) |

CPI calls are axiomatic — we verify the program passes correct parameters. SPL Token internals and the Solana runtime are trusted.

## Quick start

```bash
# 1. Install
npx skills add qedgen/solana-skills

# 2. Write a spec and validate it
qedgen check --spec my_program.qedspec

# 3. Generate all artifacts
qedgen codegen --spec my_program.qedspec --all
```

Lean proofs, Kani harnesses, and API keys are set up automatically when first needed. To configure them manually:

```bash
# Lean + Mathlib (only needed for formal proofs)
qedgen setup --mathlib

# API keys (only needed for sorry-filling and deep proof search)
export MISTRAL_API_KEY=your_key_here                    # https://console.mistral.ai (free tier available)
export ARISTOTLE_API_KEY=your_key_here                  # https://aristotle.harmonic.fun
```

## Usage

### Existing programs (brownfield)

```bash
# Generate a .qedspec scaffold from your Anchor IDL
qedgen spec --idl target/idl/my_program.json --format qedspec

# Review and complete the TODO items in the generated .qedspec
# Then use the same pipeline as greenfield:
qedgen init --name my_program
qedgen codegen --spec my_program.qedspec --lean
cd formal_verification && lake build
```

The generated `.qedspec` auto-derives state fields, operations, contexts, PDAs, and errors from the IDL. Guards, effects, lifecycle transitions, and properties are stubbed with TODOs for you or your agent to fill in.

### Spec-driven pipeline

```bash
# Initialize a new verification project from a .qedspec
qedgen init --name my_program

# Validate the spec (lint + coverage)
qedgen check --spec my_program.qedspec
qedgen check --spec my_program.qedspec --json           # machine-readable output

# Generate all committed artifacts from .qedspec
qedgen codegen --spec my_program.qedspec --all          # everything: Rust, Lean, Kani, tests

# Or generate selectively
qedgen codegen --spec my_program.qedspec                # Quasar Rust skeleton only
qedgen codegen --spec my_program.qedspec --lean         # + Lean proofs
qedgen codegen --spec my_program.qedspec --kani         # + Kani harnesses
qedgen codegen --spec my_program.qedspec --test         # + unit tests
qedgen codegen --spec my_program.qedspec --proptest     # + proptest harnesses
qedgen codegen --spec my_program.qedspec --integration  # + QuasarSVM integration tests

# Check with drift detection and verification report
qedgen check --spec my_program.qedspec --coverage       # operation × property matrix
qedgen check --spec my_program.qedspec --explain        # Markdown verification report
qedgen check --spec my_program.qedspec --code ./programs --kani ./tests/kani.rs  # drift detection
```

### sBPF verification

```bash
# Transpile sBPF assembly to Lean 4
qedgen asm2lean --input src/program.s --output formal_verification/Program.lean

# Verify sBPF proofs (checks source hash, regenerates if stale)
qedgen check --spec my_program.qedspec --asm src/program.s
```

### Generate proofs from a prompt

```bash
qedgen generate \
  --prompt-file /tmp/analysis/property.prompt.txt \
  --output-dir /tmp/proof \
  --passes 4 \
  --validate
```

### Fill hard sub-goals

```bash
# Leanstral (fast, seconds)
qedgen fill-sorry \
  --file formal_verification/Spec.lean \
  --passes 3 \
  --validate

# Auto-escalate to Aristotle if sorry markers remain
qedgen fill-sorry \
  --file formal_verification/Spec.lean \
  --passes 3 \
  --validate \
  --escalate
```

### Aristotle (when Leanstral fails)

```bash
# Submit and wait inline
qedgen aristotle submit --project-dir formal_verification --wait

# Or submit, detach, and poll later
qedgen aristotle submit --project-dir formal_verification
qedgen aristotle status <project-id> --wait --output-dir formal_verification

# List / cancel
qedgen aristotle list
qedgen aristotle cancel <project-id>
```

### Verification drift detection

After verifying a function, stamp it with `#[qed(verified)]` to detect future changes:

```rust
use qedgen_macros::qed;

#[qed(verified, hash = "5af369bb254368d3")]
pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    // Any change → compile_error! (with proc macro)
    // Any change → exit 1 (with CLI)
}
```

```bash
# Scan and stamp hashes on all #[qed(verified)] functions
qedgen check --spec my_program.qedspec --drift programs/src/ --update-hashes

# CI gate — exit 1 if any verified function has changed
qedgen check --spec my_program.qedspec --drift programs/src/

# Transitive drift — also check if callees of verified functions changed
qedgen check --spec my_program.qedspec --drift programs/src/ --deep
```

### Consolidate proofs

```bash
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir my_program/formal_verification
```

### Generate CI workflow

```bash
qedgen codegen --spec my_program.qedspec --ci                    # Lean-only verification workflow
qedgen codegen --spec my_program.qedspec --ci --ci-asm src/program.s  # Add sBPF source hash check
```

## Examples

### Rust / Anchor

- **[Escrow](examples/rust/escrow/)** — Token escrow with lifecycle proofs
- **[Lending](examples/rust/lending/)** — Lending pool with multi-account state
- **[Multisig](examples/rust/multisig/)** — Multi-signature vault with voting
- **[Percolator](examples/rust/percolator/)** — Perpetual DEX risk engine

### sBPF Assembly

- **[Counter](examples/sbpf/counter/)** — PDA counter
- **[Tree](examples/sbpf/tree/)** — Red-black tree
- **[Dropset](examples/sbpf/dropset/)** — On-chain order book
- **[Transfer](examples/sbpf/transfer/)** — SOL transfer via System Program CPI
- **[Slippage](examples/sbpf/slippage/)** — AMM slippage guard

## Requirements

- Rust toolchain (auto-installed if missing)

The following are only needed when working with Lean proofs and are set up automatically on first use:

- Lean 4 / elan — for `lake build` and formal proofs
- `MISTRAL_API_KEY` — for `fill-sorry` and `generate` ([console.mistral.ai](https://console.mistral.ai), free tier available)
- `ARISTOTLE_API_KEY` — for `aristotle` deep proof search ([aristotle.harmonic.fun](https://aristotle.harmonic.fun))

### Environment variables

| Variable | Purpose | When needed |
|---|---|---|
| `MISTRAL_API_KEY` | Leanstral API access (`fill-sorry`, `generate`) | Lean proofs |
| `ARISTOTLE_API_KEY` | Aristotle long-running proof search | Hard sub-goals |
| `QEDGEN_HOME` | Override global home directory (default: `~/.qedgen`) | Always |
| `QEDGEN_VALIDATION_WORKSPACE` | Override validation workspace path | Lean proofs |

## License

[MIT](LICENSE)
