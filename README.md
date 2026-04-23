<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/logo-light.png">
    <img src="docs/logo-dark.png" alt="QEDGen" width="260">
  </picture>
</p>

<h3 align="center">Proofs, not promises.</h3>
<p align="center"><em>Ship without fear.</em></p>

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
| **Deploy safety** | On-chain shape for Anchor programs — version fields, reserved padding, pinned discriminators, signer coverage, PDA seed continuity — via `qedgen readiness` and `qedgen check-upgrade` (ratchet). |

CPI calls are axiomatic — we verify the program passes correct parameters. SPL Token internals and the Solana runtime are trusted.

**Proofs prove correctness. Ratchet proves deployability.** The P-rule preflight (`qedgen readiness`) catches future-upgrade landmines in a single IDL before the first deploy; the R-rule diff (`qedgen check-upgrade`) catches every breaking change between an old and new IDL once the program is live.

## Quick start

```bash
# 1. Install
npx skills add qedgen/solana-skills

# 2. Initialize the project — records the spec path in .qed/config.json
qedgen init --name my_program --spec my_program.qedspec --quasar

# 3. Validate and generate artifacts (no --spec needed from inside the project)
qedgen check
qedgen codegen --all
```

`.qed/config.json` pins the spec location so subsequent commands don't need
`--spec <path>` — they walk up from the current directory, find the nearest
`.qed/`, and resolve. Explicit `--spec` still works when you want to point
at something specific.

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

sBPF-specific declarations (`instruction`, `pubkey`, per-instruction `errors`)
live inside `pragma sbpf { ... }` — the core DSL stays platform-agnostic, and
`qedgen` infers the assembly target from the pragma's presence.

```
spec Transfer

pragma sbpf {
  instruction transfer_sol { ... }
}
```

```bash
# Transpile sBPF assembly to Lean 4
qedgen asm2lean --input src/program.s --output formal_verification/Program.lean

# Verify sBPF proofs (checks source hash, regenerates if stale)
qedgen check --spec my_program.qedspec --asm src/program.s
```

### CPI contracts — `interface` + `call`

When your program invokes another (SPL Token, System Program, an AMM, …),
declare the callee's contract as an `interface` and write `call` at the
invocation site. The Rust side gets a real CPI builder; Lean proofs pick up
the callee's declared `ensures` as hypotheses.

```
interface Token {
  program_id "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
  handler transfer (amount : U64) {
    discriminant "0x03"
    accounts { from : writable, type token
               to   : writable, type token
               authority : signer }
    ensures amount > 0
  }
}

handler exchange : State.Open -> State.Closed {
  call Token.transfer(from = taker_ta, to = initializer_ta,
                      amount = taker_amount, authority = taker)
}
```

```bash
# Scaffold a Tier-0 interface from an Anchor IDL (shape only — no ensures)
qedgen interface --idl target/idl/jupiter.json --out interfaces/jupiter.qedspec

# Or vendor it into .qed/interfaces/<program>.qedspec (the canonical location
# for tool-managed library specs — pointed at by `.qed/config.json`)
qedgen interface --idl target/idl/jupiter.json --vendor
```

`qedgen check` emits `[shape_only_cpi]` for any `call` whose target lacks
`ensures`, making the gap between "my Rust compiles" and "my program is
verified" visible. See [docs/design/spec-composition.md](docs/design/spec-composition.md)
for the full tier model.

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

After verifying a function, stamp it with `#[qed(verified)]` to detect future changes — either to the function body *or* to its spec contract:

```rust
use qedgen_macros::qed;

#[qed(verified,
      spec = "my_program.qedspec",
      handler = "deposit",
      hash = "5af369bb254368d3",
      spec_hash = "c3d4e5f67890abcd")]
pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    guards::deposit(&ctx, amount)?;
    // user business logic
}
```

Both hashes are pure compile-time checks — the macro expands to the function unchanged, so there's zero runtime cost. `hash` fires when the body changes; `spec_hash` fires when the `.qedspec` handler block changes.

```bash
# Unified drift report — Rust handlers + Lean theorems vs spec
qedgen reconcile --spec my_program.qedspec --json

# Scan and stamp hashes on all #[qed(verified)] functions
qedgen check --spec my_program.qedspec --drift programs/src/ --update-hashes

# CI gate — exit 1 if any verified function has changed
qedgen check --spec my_program.qedspec --drift programs/src/

# Transitive drift — also check if callees of verified functions changed
qedgen check --spec my_program.qedspec --drift programs/src/ --deep
```

`qedgen reconcile` is the agent-friendly entry point: it combines Rust-side `spec_hash` mismatches with Lean-side orphan/missing theorem findings into one machine-readable report, ready for an LLM to consume and act on.

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
qedgen codegen --spec my_program.qedspec --ci --ci-ratchet target/idl/my_program.json  # + ratchet readiness lint on every build
```

### Deploy-safety lint (ratchet)

`qedgen readiness` runs before the first deploy: one Anchor IDL in, a verdict out (`READY`, `UNSAFE`, or `BREAKING`) plus every specific future-upgrade landmine it finds. `qedgen check-upgrade` runs on every subsequent release: diff the deployed IDL against the candidate and fail the build on any change that would silently corrupt on-chain state, break existing clients, or orphan PDAs.

```bash
# Pre-deploy — lint one IDL for mainnet-readiness
qedgen readiness --idl target/idl/my_program.json
qedgen readiness --idl target/idl/my_program.json --json          # machine-readable

# Post-deploy — diff old vs new and block breaking upgrades
qedgen check-upgrade --old ratchet.lock --new target/idl/my_program.json

# Acknowledge an intentional unsafe change
qedgen check-upgrade --old ratchet.lock --new target/idl/my_program.json \
  --unsafe allow-field-append --migrated-account EscrowState
```

Exit codes mirror ratchet's CLI conventions: `0 = additive/safe`, `1 = breaking`, `2 = unsafe`. Under the hood qedgen embeds [ratchet](https://github.com/saicharanpogul/ratchet) as a library, so the rule catalog stays in sync with upstream — run `ratchet list-rules` to see the full P-rule and R-rule set (22 rules at the time of writing).

**Why both.** qedgen's `#[qed(verified)]` hash-stamps the *function body*, so a rename of an `#[account]` struct compiles with a stale-but-valid proof even though the on-chain discriminator is now different and every existing account of that type is orphaned. `qedgen check-upgrade`'s `R006 account-discriminator-change` catches that class of failure; the proof layer alone doesn't look at it.

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
