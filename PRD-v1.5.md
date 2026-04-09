# QEDGen v1.5 PRD — Spec-Driven Verification

## Problem

v1.4 treats SPEC.md as a verbose English document that Claude interprets to produce Lean proofs. This has three problems:

1. **Ambiguity**: Natural language specs are imprecise. Two agents (or the same agent twice) can interpret "only the maker can cancel" differently when generating code vs proofs.
2. **Drift**: SPEC.md and proofs are independent artifacts. Nothing enforces that every spec property has a corresponding theorem, or that they stay in sync as the program evolves.
3. **Friction**: Setting up a verified project requires 10+ manual steps — mkdir, write lakefile.lean, decide on Mathlib, wire up imports. No CI integration exists.

## Thesis

The spec is the source of truth for both code and proofs. When agents write most of the code, this is the natural trust model — the spec drives generation, and `lake build` enforces consistency. The spec language must be precise enough to generate theorem signatures mechanically, but readable enough that a human can review and approve it in minutes.

## Release goals

### G1: Lean spec macros — replace English specs with checkable declarations

A Lean 4 macro library (`QEDGen.Solana.Spec`) that lets humans write:

```lean
import QEDGen.Solana.Spec

qedspec Escrow where

  state
    maker         : Pubkey
    src_mint      : Pubkey
    dst_mint      : Pubkey
    amount        : U64
    taker_amount  : U64
    status        : Open | Completed | Cancelled

  operation cancel
    who: maker
    when: Open
    then: Cancelled
    effect: transfer escrow_token → maker

  operation exchange
    who: taker
    when: Open
    then: Completed
    effect: transfer escrow_token → taker, transfer taker_token → maker

  invariant token_conservation
    over: [escrow_token, maker_token, taker_token]
    sum amount is constant across exchange

  invariant lifecycle
    Open → Completed | Cancelled
    terminal: Completed, Cancelled

  trust
    spl_token, solana_runtime, anchor_framework
```

The `qedspec` macro expands to:
- State structure definition
- Transition functions (`Option StateType`)
- Theorem *signatures* with `sorry` bodies — one per `operation` × property category, one per `invariant`
- A `#check_spec` command that fails if any generated theorem is missing a proof

This is Lean, so `lake build` validates it. The spec and proofs live in the same project. No separate toolchain, no translation bugs.

**What the macro does NOT generate**: proof bodies. The agent (or Leanstral/Aristotle) fills those in. The macro generates the contract; the prover fulfills it.

**Surface syntax principles**:
- No braces, no `==`, no type annotations beyond field names
- Reads like a structured document, not code
- Fixed vocabulary: `who`, `when`, `then`, `effect`, `over`, `trust`
- Lifecycle expressed as arrows, not predicates

### G2: `qedgen init` — zero-manual-step project setup

```bash
# Anchor project
qedgen init --name escrow

# sBPF project (runs asm2lean automatically)
qedgen init --name dropset --asm src/dropset.s

# With Mathlib (for advanced tactics)
qedgen init --name engine --mathlib
```

Generates:
```
formal_verification/
  lakefile.lean          # Pre-configured with qedgenSupport (+ Mathlib if --mathlib)
  lean-toolchain         # Pinned to current supported version
  Spec.lean              # Skeleton qedspec block (human fills in)
  Proofs.lean            # Root import
  Proofs/                # Empty, ready for agent-written proofs
  .gitignore             # .lake/
```

With `--asm`: also runs `asm2lean`, adds the generated module to lakefile.lean, and wires up imports.

### G3: Binary pinning for sBPF

`asm2lean` embeds a SHA-256 hash of the source `.s` file into the generated Lean module:

```lean
/-- Source: src/dropset.s
    SHA-256: a1b2c3d4e5f6... -/
@[simp] def sourceHash : String := "a1b2c3d4e5f6..."
```

New command to verify the full pipeline:

```bash
# Check hash, regenerate if stale, run lake build
qedgen verify --asm src/dropset.s --proofs formal_verification/
```

Behavior:
1. Hash the current `.s` file
2. Compare against `sourceHash` in the generated Lean module
3. If match: `lake build` only
4. If mismatch: re-run `asm2lean`, then `lake build`
5. Exit code 0 = verified, non-zero = proofs broken or stale

This is the command CI calls.

### G4: `qedgen check` — spec coverage

```bash
qedgen check --spec formal_verification/Spec.lean --proofs formal_verification/Proofs/
```

Parses the `qedspec` block, extracts generated theorem names, and checks each has a non-sorry proof in the proofs directory. Reports:

```
Escrow spec coverage:
  cancel_access_control        ✓ proven
  exchange_access_control      ✓ proven
  cancel_closes_escrow         ✓ proven
  exchange_closes_escrow       ✓ proven
  token_conservation           ✗ sorry
  lifecycle_terminal           ✗ missing

4/6 properties verified
```

This is a fast check (grep + parse, no `lake build`). Useful for progress tracking and CI status comments.

### G5: Automatic sorry escalation

```bash
qedgen fill-sorry --file Proofs/Hard.lean --escalate
```

Behavior:
1. Try Leanstral (fast, seconds) with configured passes
2. If sorry markers remain after all passes: automatically submit to Aristotle
3. Poll until completion, download result
4. Run `lake build` to validate

Single command, no project ID management. The `--escalate` flag opts into the full pipeline.

### G6: CI integration

New command to generate a GitHub Actions workflow:

```bash
qedgen ci --output .github/workflows/verify.yml
```

Generates a workflow that:

```yaml
name: Formal Verification

on:
  push:
    branches: [main]
    paths:
      - 'src/**'
      - 'formal_verification/**'
  pull_request:
    paths:
      - 'src/**'
      - 'formal_verification/**'

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Lean
        uses: leanprover/lean4-action@v1

      - name: Cache Mathlib
        uses: actions/cache@v4
        with:
          path: formal_verification/.lake
          key: mathlib-${{ hashFiles('formal_verification/lean-toolchain') }}

      - name: Install qedgen
        run: cargo install --path crates/qedgen

      # sBPF projects: verify binary hasn't drifted
      - name: Verify sBPF binary
        run: qedgen verify --asm src/program.s --proofs formal_verification/
        if: hashFiles('src/*.s') != ''

      # All projects: build proofs
      - name: Build proofs
        run: cd formal_verification && lake build

      # Report coverage
      - name: Check spec coverage
        run: qedgen check --spec formal_verification/Spec.lean --proofs formal_verification/Proofs/
```

The generated workflow is a starting point. Projects customize paths, add matrix builds for multiple programs, etc.

## Migration from v1.4

### SPEC.md → Spec.lean

The English SPEC.md doesn't disappear overnight. Migration path:

1. **New projects**: use `qedgen init`, write `Spec.lean` directly with `qedspec` macro
2. **Existing projects**: keep SPEC.md as documentation. Agent reads SPEC.md, writes equivalent `Spec.lean` using the macro, human approves. Proofs are updated to fulfill the macro-generated theorem signatures.

The skill workflow (SKILL.md) changes:
- Step 1 (understand program): unchanged — scoping quiz, IDL, or existing spec
- Step 3 (write SPEC.md): becomes "write Spec.lean using `qedspec` macro"
- Step 4 (set up project): replaced by `qedgen init`
- Step 5 (write proofs): unchanged, but theorem signatures come from the macro now
- Step 7 (verify): `lake build` validates both spec and proofs in one step

### Backward compatibility

- `qedgen spec --idl` continues to work, generates SPEC.md as before
- New flag: `qedgen spec --idl <path> --lean` generates a `Spec.lean` with `qedspec` block instead
- Existing projects without `qedspec` macro still work — the macro is opt-in

## Scope and sequencing

### P0 — Ship first

| Feature | Effort | Why P0 |
|---------|--------|--------|
| `qedspec` macro library | Large | Everything else depends on the spec being machine-checkable |
| `qedgen init` | Small | Removes 10 manual steps, unblocks new users immediately |
| Binary pinning in `asm2lean` | Small | Hash embedding + `verify` command. Enables trustworthy CI |

### P1 — Ship soon after

| Feature | Effort | Why P1 |
|---------|--------|--------|
| `qedgen check` (coverage) | Medium | Needs macro to be stable before building on top |
| `qedgen ci` (workflow gen) | Small | Template generation, depends on `verify` existing |
| `--escalate` flag for fill-sorry | Medium | Leanstral → Aristotle pipeline plumbing |

### P2 — Nice to have

| Feature | Effort | Why P2 |
|---------|--------|--------|
| `qedgen spec --idl --lean` | Medium | Generates `qedspec` block from IDL. Useful but not blocking |
| SPEC.md → Spec.lean migration tool | Medium | Convenience for brownfield projects |

## Non-goals

- **Anchor/Rust → Lean extraction**: the spec-as-source-of-truth model makes this unnecessary when agents write code. For brownfield, the sBPF path via `asm2lean` is the stronger guarantee.
- **Incremental lake build**: Lake handles caching. Not our problem.
- **Mathlib auto-detection**: simple enough to document. `--mathlib` flag on init is sufficient.
- **Standalone DSL / spec language**: rejected in favor of Lean macros. One toolchain, no translation step, `lake build` validates everything.

## Open questions

1. **Effect clause precision + invariant expressiveness**: These two are the core of the DSL design. The target is SQL-like: declarative, small vocabulary, expressive enough for real programs without understanding Lean internals. This will require iterative prototyping against real Anchor programs (escrow, AMMs, vaults) to find the right level of abstraction. Expect multiple rounds of "write spec → see what theorems it generates → adjust syntax." Open to experimentation — the macro system makes iteration cheap since we can change the surface syntax without changing the proof infrastructure.

3. ~~**sBPF spec integration**~~: Resolved. Keep sBPF separate for now. sBPF already has a strong truth link via `asm2lean` — the model *is* the bytecode. The `qedspec` macro targets higher-level languages (Rust/Anchor) where there's no mechanical extraction and the spec is the only thing keeping the agent honest. Revisit unification after experimenting with the macro on real Anchor programs.

4. ~~**Spec versioning**~~: Resolved. The spec is a `.lean` file checked into git alongside the proofs. `git log` / `git diff` is the audit trail. No special versioning mechanism needed.

## Success criteria

- A new user can go from zero to a verified escrow program without manually creating any project files
- `lake build` fails if spec properties don't have proofs
- `lake build` fails if the sBPF binary changed without proof regeneration
- CI can enforce all of the above with a single generated workflow
- A human can read and approve a `qedspec` block in under 5 minutes for a typical Solana program
