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

The `.qedspec` is the **single source of truth** — the only artifact committed to the user's repo. Everything else (proptests, Lean theorems, generated code) is derived and disposable.

```
Two phases:

Phase 1 — Spec Design (interactive, all artifacts transient)
  You + User ──→ iterate on .qedspec
    ├── Lint         — structural validation           (instant)
    ├── Proptest     — random counterexamples           (~100ms)
    └── lean-gen     — type-level provability check     (~seconds)
  Deliverable: .qedspec (committed)

Phase 2 — Proof Engineering (on demand, formal certificates)
  .qedspec ──→ Spec.lean ──→ Lean proofs ──→ lake build
    ├── Leanstral    — fast sorry-filling               (seconds)
    └── Aristotle    — deep agentic proof search         (minutes-hours)
  Deliverable: zero-sorry Lean project + #[qed(verified)] stamps

Spec-driven pipeline (all generated from the same .qedspec):
  qedspec ──→ Proptest harnesses    (transient, /tmp)
          ──→ Kani harnesses        (generated)
          ──→ Spec.lean             (generated)
          ──→ Quasar Rust program   (generated)
          ──→ Unit + integration tests (generated)
```

## Step 1: Understand the program

**Always read the source code first.** Then classify the project:

### Returning qedgen project (`.qedspec` exists)
Read the `.qedspec` alongside the source. The `.qedspec` is the spec source of truth — `Spec.lean` is generated from it, never treat it as the primary source. On returning visits, re-enter the spec design loop (Step 3) to validate or extend the spec, or proceed directly to proof engineering (Step 4) if the user wants formal certificates.

### Brownfield project (existing code, no `.qedspec`)
An existing Rust program being onboarded to qedgen for the first time.

1. **Read the source code** — Understand the program's state, instructions, guards, and invariants directly from Rust source. This is always the ground truth.
2. **Check for existing Rust tests** — Look for `tests/`, `#[cfg(test)]`, or `#[cfg(kani)]` modules. Existing tests reveal the program's testing patterns, helper infrastructure, and what's already covered. These shape how you write Kani harnesses (see "Brownfield projects" under Code generation).
3. **IDL exists** (`target/idl/<program>.json`) → Run `$QEDGEN spec --idl <path> --format qedspec` to generate a `.qedspec` scaffold, then review TODO items against the source and run `$QEDGEN codegen --spec <path> --lean` to produce `Spec.lean`.
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

Write the `.qedspec` file at the program root, then run `$QEDGEN codegen --spec <path> --lean` to produce `Spec.lean`.

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

## Step 3: Write and refine the .qedspec

The spec design phase is an **interactive loop**. You iterate on the `.qedspec` with the user, using lint, proptest, and lean-gen as internal feedback tools. All verification artifacts (proptests, Lean theorems, Spec.lean) are **transient intermediary files** — generated to `/tmp`, used for validation, then discarded. Only the `.qedspec` gets committed to the user's repo.

```
┌─────────────────────────────────────────────────────────────┐
│                   Spec Design Loop                          │
│                                                             │
│   Write/edit .qedspec                                       │
│        │                                                    │
│        ▼                                                    │
│   Lint ──→ priority 1-2 findings? ──→ fix spec, re-lint    │
│        │                                                    │
│        ▼                                                    │
│   Proptest (transient) ──→ counterexample? ──→ fix spec     │
│        │                                                    │
│        ▼                                                    │
│   lean-gen + lake build (transient) ──→ unprovable? ──→ fix │
│        │                                                    │
│        ▼                                                    │
│   Present findings to user as spec-level issues             │
│        │                                                    │
│        ▼                                                    │
│   User confirms ──→ finalize .qedspec                       │
│                                                             │
│   Deliverable: the .qedspec file (committed to repo)        │
│   Everything else: disposable agent workspace               │
└─────────────────────────────────────────────────────────────┘
```

### 3a. Write the initial .qedspec

Write the `.qedspec` at the program root. See `references/qedspec-dsl.md` for full DSL syntax. For sBPF assembly programs, use `qedguards` instead.

Present the spec to the user and get confirmation before proceeding to validation.

### 3b. Lint (structural validation)

After writing or editing a .qedspec, **always** lint:

```bash
$QEDGEN check --spec <path-to-qedspec> --json
```

Lint output is priority-ordered (1=security, 2=correctness, 3=completeness, 4=quality, 5=polish). Work through findings top-down:

1. **Priority 1-2 (security/correctness)**: Present each finding to the user. Offer to apply the suggested fix. Apply and re-lint.
2. **Priority 3 (completeness)**: Ask the user whether each suggested property/invariant matches their intent. Write the ones they confirm.
3. **Priority 4-5 (quality/polish)**: Mention these as optional improvements. Apply if the user agrees.

Re-run lint after each round of fixes until clean or only priority 5 items remain.

### 3c. Proptest (fast counterexamples)

Run proptest to catch spec-level bugs in ~100ms. All proptest artifacts are **transient** — generated, run, and discarded within `/tmp`.

```bash
# Generate harness
$QEDGEN codegen --spec <path-to-qedspec> --proptest --proptest-output /tmp/proptest_harness.rs

# Run in a scratch project (create once per session, reuse across specs)
mkdir -p /tmp/proptest_runner/tests /tmp/proptest_runner/src
touch /tmp/proptest_runner/src/lib.rs
cat > /tmp/proptest_runner/Cargo.toml << 'EOF'
[package]
name = "proptest-runner"
version = "0.1.0"
edition = "2021"
[dev-dependencies]
proptest = "1"
EOF

# Copy and run (repeat for each spec change)
cp /tmp/proptest_harness.rs /tmp/proptest_runner/tests/proptest.rs
cd /tmp/proptest_runner && cargo test --test proptest
```

**Interpreting failures:**
- **Preservation test fails**: A handler violates a declared property — missing guard or overflow.
- **Overflow test fails**: An `add`/`sub` effect wraps around without a guard.
- **Guard test fails**: Guard logic has a bug (shouldn't happen if lint is clean).

### 3d. lean-gen + lake build (type-level validation)

Generate Lean theorems and attempt to build. This catches spec issues that proptest misses — theorems that are logically unprovable indicate a spec bug, not a proof engineering problem.

```bash
# Generate Spec.lean to /tmp
$QEDGEN codegen --spec <path-to-qedspec> --lean --lean-output /tmp/lean_check/Spec.lean

# Quick validation build
$QEDGEN init --name check --output-dir /tmp/lean_check
cp /tmp/lean_check/Spec.lean /tmp/lean_check/formal_verification/Spec.lean
cd /tmp/lean_check/formal_verification && lake build
```

If `lake build` reveals structurally unprovable theorems (not just missing tactics — the theorem statement itself is wrong), that's feedback about the spec. Fix the `.qedspec` and re-run.

### 3e. Present findings as spec-level issues

**The user does not need to know which tier found the bug.** Present all findings as spec-level issues:

- "Your `deposit` handler can overflow `total_deposits`, which violates `pool_solvency`. Add a guard: `guard total_deposits + amount >= total_deposits`."
- "The `withdraw` handler's effect `V -= amount` can underflow when `V < amount`. The guard `C_tot >= amount` doesn't protect `V`."
- "The `approve` transition allows re-approval — `approval_count` increments without checking if this signer already approved."

The verification tier that found it (proptest, lean-gen, lint) is an implementation detail. Fix the spec, re-run the loop.

### 3f. Finalize

When all tiers pass clean, present the final `.qedspec` to the user. This is the deliverable — it gets committed to the repo. Everything else (proptests, Lean files, build artifacts) is disposed.

## Step 4: Proof engineering (on demand)

Steps 4-8 apply when the user wants **formal proof certificates** — mathematical guarantees backed by Lean 4 proofs. This is a separate phase from spec design. The `.qedspec` is the input; Lean proofs are the output.

Not every project needs this. The `.qedspec` alone (validated by lint + proptest) provides significant assurance. Full formal proofs are for high-stakes programs (DeFi vaults, bridges, token contracts) where the cost of a bug justifies the investment.

### 4a. Set up the Lean project

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
  Spec.lean              # Generated from .qedspec via codegen --lean
  lean_solana/           # Embedded support library
  .gitignore
```

Generate `Spec.lean` from the finalized `.qedspec`:
```bash
$QEDGEN codegen --spec program.qedspec --lean --lean-output formal_verification/Spec.lean
```

**When to add Mathlib:**
- **sBPF proofs do NOT need Mathlib.** Built-in tactics handle everything.
- **Anchor/Rust proofs MAY need Mathlib** for u128 arithmetic, `linarith`, `ring`, or `norm_num`.
- **Rule of thumb:** Start without. Add `--mathlib` if `lake build` fails on a missing tactic or lemma. Mathlib adds ~8GB and 15-45 min first build.

### 4b. Write Lean proofs

You write Lean 4 directly — filling the `sorry` markers generated by `codegen --lean`.

For each property in the spec:
1. The state structure and transitions are already generated
2. Theorem statements are already generated (with `sorry`)
3. Write the proof for each theorem
4. Run `lake build` and iterate on errors

See `references/proof-patterns.md` for patterns (access control, CPI, state machine, conservation, arithmetic) and tactic rules.

See `references/support-library.md` for the full API (types, constants, functions, lemmas).

For sBPF assembly programs, see `references/sbpf.md` for `wp_exec` tactic usage, memory axioms, and simp performance rules.

**Key tactic rules (quick reference):**

| Do | Don't |
|---|---|
| `unfold f at h` before `split_ifs` | `simp [f] at h` before `split_ifs` (kills if-structure) |
| `unfold pred at h_inv ⊢` for named predicates | `unfold pred` only in goal (omega can't see hypotheses) |
| `cases h` after `split_ifs` on `some = some` | `injection h` (unnecessary) |
| `omega` for linear arithmetic | `norm_num` for linear goals |

### 4c. Call Leanstral / Aristotle for hard sub-goals

When you have `sorry` markers you cannot fill after 2-3 attempts:

```bash
# Try Leanstral first (fast, seconds)
$QEDGEN fill-sorry --file formal_verification/Spec.lean --validate

# Auto-chain: Leanstral -> Aristotle if sorry remains
$QEDGEN fill-sorry --file formal_verification/Spec.lean --escalate

# Or manually escalate to Aristotle (minutes-hours)
$QEDGEN aristotle submit --project-dir formal_verification --wait
```

**When to use which:**
- **Leanstral** (`fill-sorry`): Fast (seconds). Try first.
- **Aristotle** (`aristotle submit`): Slow but powerful. Use when Leanstral fails.
- If both fail: simplify the theorem or split into smaller lemmas.

See `references/cli.md` for all Aristotle subcommands.

### 4d. Stamp verified code

After proofs compile and `qedgen check` passes, stamp the verified Rust handlers with `#[qed(verified)]` to detect future drift:

```bash
# Add #[qed(verified)] to handler functions, then stamp hashes
$QEDGEN check --spec program.qedspec --drift programs/src/ --update-hashes
```

This adds content hashes to every `#[qed(verified)]` annotation. If anyone later modifies a verified function:
- **At compile time** (with `qedgen-macros`): `compile_error!` — the program won't build
- **In CI** (with `qedgen check --drift`): `exit 1` — the pipeline fails

**Adding annotations:**

```rust
use qedgen_macros::qed;

#[qed(verified, hash = "5af369bb254368d3")]
pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    // ...
}
```

The hash covers the function signature + body, excluding attributes and comments. It changes when the body, parameters, or return type change. It does NOT change for whitespace, formatting, or comment changes.

### 4e. Verify and report

```bash
# Build proofs
cd formal_verification && lake build

# Check spec coverage (accepts .qedspec or Spec.lean)
$QEDGEN check --spec program.qedspec --proofs formal_verification/

# For sBPF: verify binary hasn't drifted from proofs
$QEDGEN check --spec program.qedspec --asm src/program.s

# Human-readable verification report
$QEDGEN check --spec program.qedspec --explain

# Coverage matrix
$QEDGEN check --spec program.qedspec --coverage
```

`qedgen check` reports per-theorem status: **Proven**, **Sorry**, or **Missing**.

**IMPORTANT**: Always run `qedgen check` automatically after `lake build` succeeds with zero errors. Present the verification results to the user immediately — do not wait for them to ask. This is the final deliverable.

**Unified drift detection** (when code or Kani harnesses are generated from the spec):

```bash
$QEDGEN check --spec program.qedspec --code programs/my_program/ --kani tests/kani.rs
```

**CI integration:**

```bash
# Generate CI workflow
$QEDGEN codegen --spec program.qedspec --ci

# Or add drift detection to existing CI
$QEDGEN check --spec program.qedspec --drift programs/src/   # exits 1 on drift
```

## Code generation pipeline

Everything is derived from the `.qedspec`. Some outputs are transient (used during spec design, then discarded), others are generated into the project.

```bash
# Validation (spec design feedback — transient, agent-driven)
$QEDGEN check --spec program.qedspec                    # lint + coverage (default)
$QEDGEN check --spec program.qedspec --json              # machine-readable for agent

# Generated (derived from spec, written to project)
$QEDGEN codegen --spec program.qedspec --all             # everything at once
$QEDGEN codegen --spec program.qedspec --lean            # Lean proofs only
$QEDGEN codegen --spec program.qedspec --kani            # Kani harnesses only
$QEDGEN codegen --spec program.qedspec --test            # Unit tests only
$QEDGEN codegen --spec program.qedspec --proptest        # Proptest harnesses only
$QEDGEN codegen --spec program.qedspec --integration     # Integration tests only
```

With `qedgen init --quasar`, the generated outputs are created automatically.

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

**`qedgen check --coverage`** — prints a verification matrix (operations × properties) showing coverage gaps.

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

**Greenfield Kani harnesses** (self-contained models from `$QEDGEN codegen --kani`) are for new projects where no Rust implementation exists yet. They model the spec's state machine independently of any framework types.

## Git hygiene

The `.qedspec` is the primary committed artifact. Everything else is derived.

**Always commit:**
- `.qedspec` file (the spec source of truth)

**Commit when doing full proof engineering (Step 4):**
- `formal_verification/*.lean` source files, `lakefile.lean`, `lean-toolchain`, `.gitignore`
- Generated Rust code (`codegen`, `kani`, `test` outputs)

**Never commit:**
- Transient intermediary files (proptests, scratch Lean builds in `/tmp`)
- `.lake/`, `build/`, `lake-packages/`, `lean_solana/.lake/`, `lean_solana/build/`
- Any directory containing `.olean` files
- Never use `git add -A` or `git add .` — always stage specific files by name

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
