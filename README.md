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

Spec-driven verification for Solana programs — **Rust/Anchor**, **Quasar**, and **sBPF assembly**. Describe what your program must guarantee in a `.qedspec`. QEDGen validates it and generates everything: **property tests**, **Kani proof harnesses**, **Lean 4 proofs**, **program code**, and **CI workflows** — all from one spec.

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

1. Write a `.qedspec` file defining your program's properties, or let your agent generate one from the IDL
2. Run `qedgen check` to validate the spec (lint + proptest + lean-gen)
3. Run `qedgen codegen --all` to generate all committed artifacts
4. Your agent writes Lean 4 proofs against the QEDGen support library
5. Iterates on `lake build` errors until proofs compile
6. Calls `qedgen fill-sorry` for hard sub-goals (Leanstral — seconds)
7. Escalates to `qedgen aristotle submit` when Leanstral fails (Aristotle — minutes to hours)

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

# 2. Set API keys
export MISTRAL_API_KEY=your_key_here                    # https://console.mistral.ai (free tier available)
export ARISTOTLE_API_KEY=your_key_here                  # https://aristotle.harmonic.fun

# 3. Run setup (installs Lean, Rust — first run takes a few minutes)
qedgen setup                    # minimal setup
qedgen setup --mathlib          # include Mathlib (adds 15-45 min for cache download)

# 4. Verify an example
cd examples/sbpf/dropset/formal_verification && lake build
```

If `lake build` completes with no errors, your setup is working. Every theorem compiled = every property proven.

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

- **[Escrow](examples/rust/escrow/)** — Token escrow with authority checks, CPI transfer verification, and lifecycle proofs
- **[Lending](examples/rust/lending/)** — Lending protocol verification
- **[Multisig](examples/rust/multisig/)** — Multi-signature verification
- **[Percolator](examples/rust/percolator/)** — Market maker with state machine verification and arithmetic safety

### sBPF Assembly

- **[Counter](examples/sbpf/counter/)** — Account counter with 3 verified validation guards (178 instructions)
- **[Tree](examples/sbpf/tree/)** — Red-black tree with 3 verified validation guards (498 instructions)
- **[Dropset](examples/sbpf/dropset/)** — On-chain order book RegisterMarket — 13/13 security properties verified (180 instructions)
- **[Transfer](examples/sbpf/transfer/)** — Lamport transfer with account count and data length checks
- **[Slippage](examples/sbpf/slippage/)** — AMM slippage protection with overflow safety

## Requirements

- Rust toolchain (auto-installed if missing)
- Lean 4 / elan (auto-installed if missing)
- `MISTRAL_API_KEY` — for `fill-sorry` and `generate` ([console.mistral.ai](https://console.mistral.ai), free tier available)
- `ARISTOTLE_API_KEY` — for `aristotle` commands ([aristotle.harmonic.fun](https://aristotle.harmonic.fun))

Both API keys are optional — `qedgen` works without them, but unresolved sub-goals will remain as `sorry` markers in proofs.

### Environment variables

| Variable | Purpose |
|---|---|
| `MISTRAL_API_KEY` | Leanstral API access (`fill-sorry`, `generate`) |
| `ARISTOTLE_API_KEY` | Aristotle long-running proof search |
| `QEDGEN_HOME` | Override global home directory (default: `~/.qedgen`) |
| `QEDGEN_VALIDATION_WORKSPACE` | Override validation workspace path |

## License

[MIT](LICENSE)
