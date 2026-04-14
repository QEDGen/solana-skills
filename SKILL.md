---
name: qedgen
description: Formally verify programs by writing Lean 4 proofs. Trigger this skill whenever the user wants to formally verify code, generate Lean 4 proofs, prove properties about algorithms or smart contracts, verify invariants, convert program logic into formal specifications, or anything involving Lean 4 and formal verification. Also trigger when the user mentions "qedgen", "lean proof", "formal proof", "verify my code", "prove correctness", "formal verification", or wants mathematical guarantees about their implementation.
---

# QEDGen — Agent-Driven Formal Verification

You (Claude) are the proof engineer. You read the codebase, write Lean 4 models and proofs, iterate on compiler errors, and call external theorem provers only for hard sub-goals you cannot fill yourself.

**Reference files** (read on demand — do NOT load all at once):
- `references/qedspec-dsl.md` — qedspec, qedguards, qedbridge DSL syntax
- `references/support-library.md` — Types, constants, functions, lemmas, arithmetic helpers
- `references/proof-patterns.md` — Access control, CPI, state machine, conservation patterns + tactic rules
- `references/sbpf.md` — sBPF assembly verification: asm2lean, wp_exec, memory, simp performance
- `references/cli.md` — Full CLI reference with all commands and flags

## Important: how to run qedgen

All `qedgen` commands MUST be run via the wrapper script. Set this once:
```bash
QEDGEN="$HOME/.agents/skills/qedgen/tools/qedgen"
```

## Architecture

```
You (Claude)                          Leanstral (fast)        Aristotle (deep)
  ├── Read spec / source code           ├── Fill sorry          ├── Long-running agent
  ├── Write Lean 4 models               └── Suggest tactics     └── Hard sub-goals
  ├── Write theorem statements                                     (minutes-hours)
  ├── Write proof attempts
  ├── Run `lake build`, read errors
  ├── Fix and iterate
  └── Generate code (codegen/kani/test)

Spec-driven pipeline:
  qedspec ──→ Spec.lean ──→ Lean proofs (formal)
          └──→ Quasar Rust program (codegen)
          └──→ Kani harnesses (bounded model checking)
          └──→ Unit tests + integration tests
```

## Step 1: Understand the program

**Always read the source code first.** Then classify the project:

### Returning qedgen project (`.qedspec` exists)
Read the `.qedspec` alongside the source. The `.qedspec` is the spec source of truth. On secondary runs, skip to Step 4. (`Spec.lean` is generated from the `.qedspec` — never treat it as the primary source.)

### Brownfield project (existing code, no `.qedspec`)
An existing Rust program being onboarded to qedgen for the first time.

1. **Read the source code** — Understand the program's state, instructions, guards, and invariants directly from Rust source. This is always the ground truth.
2. **Check for existing Rust tests** — Look for `tests/`, `#[cfg(test)]`, or `#[cfg(kani)]` modules. Existing tests reveal the program's testing patterns, helper infrastructure, and what's already covered. These shape how you write Kani harnesses (see "Brownfield projects" under Code generation).
3. **IDL exists** (`target/idl/<program>.json`) → Run `$QEDGEN spec --idl <path> --format qedspec` to generate a `.qedspec` scaffold, then review TODO items against the source and run `$QEDGEN lean-gen` to produce `Spec.lean`.
4. **Rust source only** (no IDL, no framework) → Extract the spec from source using LSP. See "Writing a .qedspec from Rust source" below.
5. **Nothing to start from** → Read the source code and ask scoping questions (Step 2).

### Writing a .qedspec from Rust source

For native Rust programs without an IDL (no Anchor/Quasar), use LSP and source reading to extract the program structure into a `.qedspec`. Work through these in order:

1. **Find the entry point** — Look for `process_instruction` or the instruction dispatcher. Use LSP to find all match arms or handler functions. Each handler becomes an `operation` block.

2. **Find state structs** — Search for structs that are serialized/deserialized from account data (look for `borsh::BorshDeserialize`, `Pack`, or manual byte parsing). Each becomes a `state {}` or `account {} ` block. Map field types: `u64` → `U64`, `Pubkey` → `Pubkey`, etc.

3. **Find account validation** — In each handler, identify which accounts are checked for `is_signer`, which are writable, and any PDA derivations (`Pubkey::find_program_address`). These map to `context {}` entries with `Signer`, `mut`, `seeds()`, `bump`.

4. **Find guards** — Look for early-return error checks (`if !condition { return Err(...) }`). These become `guard` clauses. Map error codes to an `errors [...]` block.

5. **Find state mutations** — Track which fields are modified in each handler. These become `effect {}` blocks (`field = value`, `field += value`, `field -= value`).

6. **Find CPI calls** — Look for `invoke` or `invoke_signed`. These become `calls` clauses with the target program, discriminator, and account list.

7. **Infer lifecycle** — If there's an init handler that creates the account and a close/cancel handler that closes it, use `lifecycle [Uninitialized, Active, Closed]`. Map each handler to `when`/`then` transitions.

Write the `.qedspec` file at the program root, then run `$QEDGEN lean-gen` to produce `Spec.lean`.

## Step 2: Scope the verification

If no spec was found, run a short interactive quiz — one question at a time:

**Q1: "What does this program need to guarantee above all else?"**
- Authorization / access control
- Tokens are never lost / correct routing
- One-shot safety / no replay
- Arithmetic safety / no overflow
- Conservation (e.g., vault >= total claims)

**Q2: "Which scenario worries you most?"** — generate concrete risk scenarios.

**Q3: "Does the program make any assumptions that aren't enforced on-chain?"**

Ask questions **one at a time**. Wait for the answer before the next.

## Step 3: Write Spec.lean

Write `formal_verification/Spec.lean` using the `qedspec` macro. See `references/qedspec-dsl.md` for full DSL syntax.

For sBPF assembly programs, use `qedguards` instead. See `references/qedspec-dsl.md`.

Present the spec to the user and get confirmation before proceeding.

### Lint the spec (iterative guide)

After writing or editing a .qedspec, **always** lint:

```bash
$QEDGEN lint --spec <path-to-qedspec> --json
```

Lint output is priority-ordered (1=security, 2=correctness, 3=completeness, 4=quality, 5=polish). Work through findings top-down:

1. **Priority 1-2 (security/correctness)**: Present each finding to the user. Offer to apply the suggested fix. Apply and re-lint.
2. **Priority 3 (completeness)**: Ask the user whether each suggested property/invariant matches their intent. Write the ones they confirm.
3. **Priority 4-5 (quality/polish)**: Mention these as optional improvements. Apply if the user agrees.

Re-run lint after each round of fixes until clean or only priority 5 items remain.

**Do not skip this step.** A spec with priority 1-2 warnings will generate code that can't be fully verified.

## Step 4: Set up the Lean project

```bash
# Anchor/Rust project
$QEDGEN init --name escrow

# sBPF project (runs asm2lean automatically)
$QEDGEN init --name dropset --asm src/dropset.s

# With Mathlib (for u128 arithmetic helpers)
$QEDGEN init --name engine --mathlib

# With Quasar codegen pipeline
$QEDGEN init --name counter --quasar
```

Generated structure:
```
formal_verification/
  lakefile.lean          # Pre-configured (+ Mathlib if --mathlib)
  lean-toolchain
  Spec.lean              # Skeleton qedspec
  Proofs.lean            # Root import
  Proofs/
  lean_solana/           # Embedded support library
  .gitignore
```

### When to add Mathlib

- **sBPF proofs do NOT need Mathlib.** Built-in tactics handle everything.
- **Anchor/Rust proofs MAY need Mathlib** for u128 arithmetic, `linarith`, `ring`, or `norm_num`.
- **Rule of thumb:** Start without. Add `--mathlib` if `lake build` fails on a missing tactic or lemma. Mathlib adds ~8GB and 15-45 min first build.

## Step 5: Write Lean proofs

This is the core step. You write Lean 4 directly — models, transitions, theorems, and proofs.

For each property in the spec:
1. Define the state as a Lean structure
2. Define the transition as `Option StateType` (return `none` on precondition failure)
3. State the theorem matching the spec property
4. Write the proof
5. Run `lake build` and iterate on errors

See `references/proof-patterns.md` for patterns (access control, CPI, state machine, conservation, arithmetic) and tactic rules.

See `references/support-library.md` for the full API (types, constants, functions, lemmas).

For sBPF assembly programs, see `references/sbpf.md` for `wp_exec` tactic usage, memory axioms, and simp performance rules.

### Key tactic rules (quick reference)

| Do | Don't |
|---|---|
| `unfold f at h` before `split_ifs` | `simp [f] at h` before `split_ifs` (kills if-structure) |
| `unfold pred at h_inv ⊢` for named predicates | `unfold pred` only in goal (omega can't see hypotheses) |
| `cases h` after `split_ifs` on `some = some` | `injection h` (unnecessary) |
| `omega` for linear arithmetic | `norm_num` for linear goals |

## Step 6: Call Leanstral / Aristotle for hard sub-goals

When you have `sorry` markers you cannot fill after 2-3 attempts:

```bash
# Try Leanstral first (fast, seconds)
$QEDGEN fill-sorry --file formal_verification/Proofs/Hard.lean --validate

# Auto-chain: Leanstral -> Aristotle if sorry remains
$QEDGEN fill-sorry --file formal_verification/Proofs/Hard.lean --escalate

# Or manually escalate to Aristotle (minutes-hours)
$QEDGEN aristotle submit --project-dir formal_verification --wait
```

**When to use which:**
- **Leanstral** (`fill-sorry`): Fast (seconds). Try first.
- **Aristotle** (`aristotle submit`): Slow but powerful. Use when Leanstral fails.
- If both fail: simplify the theorem or split into smaller lemmas.

See `references/cli.md` for all Aristotle subcommands.

## Step 7: Verify and report

```bash
# Build proofs
cd formal_verification && lake build

# Check spec coverage (accepts .qedspec or Spec.lean)
$QEDGEN check --spec program.qedspec --proofs formal_verification/Proofs/

# For sBPF: verify binary hasn't drifted from proofs
$QEDGEN verify --asm src/program.s --proofs formal_verification/

# Human-readable verification report
$QEDGEN explain --spec program.qedspec --proofs formal_verification/
```

`qedgen check` reports per-theorem status: **Proven**, **Sorry**, or **Missing**.

**IMPORTANT**: Always run `qedgen check` automatically after `lake build` succeeds with zero errors. Present the verification results to the user immediately — do not wait for them to ask. This is the final deliverable.

### Unified drift detection

When code or Kani harnesses are generated from the spec:

```bash
$QEDGEN check --spec Spec.lean --proofs Proofs/ --code programs/my_program/ --kani tests/kani.rs
```

### CI integration

```bash
$QEDGEN ci --output .github/workflows/verify.yml
```

## Code generation pipeline

The spec drives code generation across multiple layers:

```bash
$QEDGEN codegen --spec program.qedspec --output-dir programs/my_program/   # Quasar program
$QEDGEN kani --spec program.qedspec --output tests/kani.rs                  # Kani harnesses
$QEDGEN test --spec program.qedspec --output src/tests.rs                   # Unit tests
$QEDGEN integration-test --spec program.qedspec --output src/integration_tests.rs
$QEDGEN lean-gen --spec program.qedspec --output formal_verification/Spec.lean
$QEDGEN coverage --spec program.qedspec                                     # Verification matrix
$QEDGEN lint --spec program.qedspec                                         # Spec completeness lint
```

With `qedgen init --quasar`, all of these are generated automatically.

### v2.0 spec features

The `.qedspec` DSL supports these advanced verification blocks:

**`aborts_if`** — declare when an operation must reject, with a named error:
```
operation withdraw {
  guard state.C_tot >= amount
  aborts_if state.C_tot < amount with InsufficientFunds
  effect { V -= amount; C_tot -= amount }
}
```

**`cover`** — prove a sequence of operations is reachable:
```
cover happy_path {
  trace [deposit, withdraw]
}
```

**`liveness`** — prove bounded reachability between lifecycle states:
```
liveness drain_completes {
  from Draining
  leads_to Active
  via [complete_drain, reset]
  within 2
}
```

**`environment`** — prove properties hold under external state changes:
```
environment oracle_update {
  mutates oracle_price : U64
  constraint oracle_price > 0
}
```

**Proof decomposition** — properties now generate per-operation sub-lemmas (with `sorry`) plus an auto-proven master theorem. Users prove the sub-lemmas individually.

**Auto-overflow** — operations with `add` effects automatically generate overflow safety obligations in both Lean and Kani.

**`qedgen coverage`** — prints a verification matrix (operations × properties) showing coverage gaps.

See `references/qedspec-dsl.md` for full syntax reference.

### Brownfield projects — leveraging existing tests

For brownfield projects (existing codebase with existing tests), **always check for existing Rust tests before generating new ones.** Look for:

1. `tests/` directory — Kani proofs, unit tests, integration tests, fuzz tests
2. `src/tests.rs` or inline `#[cfg(test)]` modules
3. Shared test helpers (`tests/common/mod.rs`, test param factories)

**When existing tests are found:**

- **Read them first.** Understand the test patterns, helper infrastructure, and what's already covered.
- **Generate complementary Kani harnesses** that call the real program code — import actual structs, call real methods, check real invariants. Do NOT generate self-contained models that duplicate what the implementation already expresses.
- **Reuse existing test helpers** (param factories, setup functions, shared constants) rather than creating new ones.
- **Fill gaps** — use the `.qedspec` properties to identify which invariants lack Kani coverage, then write harnesses for those using the existing patterns.

**Example — brownfield Kani harness (percolator-style):**
```rust
#![cfg(kani)]
mod common;
use common::*;

#[kani::proof]
#[kani::unwind(34)]
#[kani::solver(cadical)]
fn deposit_preserves_conservation() {
    let mut engine = RiskEngine::new(zero_fee_params());
    let idx = engine.add_user(0).unwrap();
    let amount: u32 = kani::any();
    kani::assume(amount > 0 && amount <= 10_000_000);
    engine.deposit(idx, amount as u128, DEFAULT_ORACLE, DEFAULT_SLOT).unwrap();
    assert!(engine.check_conservation());
}
```

**Greenfield Kani harnesses** (self-contained models from `$QEDGEN kani`) are for new projects where no Rust implementation exists yet. They model the spec's state machine independently of any framework types.

## Git hygiene

**Never commit build artifacts:**
- `.lake/`, `build/`, `lake-packages/`, `lean_solana/.lake/`, `lean_solana/build/`

The generated `.gitignore` excludes these. When committing verification files:
- **Do** commit: `.lean` source files, `lakefile.lean`, `lean-toolchain`, `.gitignore`, `SPEC.md`
- **Never** commit: `.lake/`, `build/`, or any directory containing `.olean` files
- **Never** use `git add -A` or `git add .` — always stage specific `.lean` files by name

## Environment

| Variable | Purpose |
|---|---|
| `MISTRAL_API_KEY` | Required for `fill-sorry` and `generate` |
| `ARISTOTLE_API_KEY` | Required for `aristotle` commands |
| `QEDGEN_HOME` | Override global home directory (default: `~/.qedgen/`) |
| `QEDGEN_VALIDATION_WORKSPACE` | Override validation workspace (default: `~/.qedgen/workspace/`) |

## Error handling

- **First `lake build` is slow with Mathlib**: 15-45 min first time, cached after.
- **`could not resolve 'HEAD' to a commit`**: Remove `.lake/packages/mathlib`, run `lake update`.
- **Rate limiting (429)**: Built-in exponential backoff in `fill-sorry`.
- **`omega` fails on address disjointness**: See `references/sbpf.md` simp normalization.
- **`simp` timeout on sBPF proofs**: Check three performance rules in `references/sbpf.md`.
