---
name: qedgen
description: Find the bugs your tests miss. Define what your Solana program must guarantee in a .qedspec — QEDGen validates it and generates tests, proofs, and CI to keep it fixed. Trigger when the user wants to verify code, write a .qedspec, generate tests or proofs, check program properties, or ship to mainnet with confidence. Also trigger on "qedgen", "qedspec", "verify my code", "prove correctness", "formal verification", "property testing".
---

# QEDGen — Spec-Driven Verification

You (Claude) help the user **specify** what their program must guarantee. The `.qedspec` is the deliverable — everything else (property tests, Kani harnesses, Lean proofs, Rust code) is derived from it. You iterate on the spec with the user, using the verification waterfall (proptest → Kani → Lean) to surface bugs as spec-level feedback.

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

After proofs compile and `qedgen check` passes, stamp the verified Rust handlers with `#[qed(verified)]` so any future drift — in either the handler body *or* the `.qedspec` — fails the build. Generated scaffolds (`$QEDGEN codegen --all`) already emit these attributes with hashes populated; only legacy/brownfield handlers need to be stamped by hand.

```rust
use qedgen_macros::qed;

#[qed(verified,
      spec = "percolator.qedspec",
      handler = "deposit",
      hash = "5af369bb254368d3",
      spec_hash = "6771efd5f76b268a")]
pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    guards::deposit(&ctx, amount)?;
    // user business logic
}
```

The two hashes run independently at compile time:

| Attribute arg | Covers | Fires when |
|---|---|---|
| `hash` | handler body + signature (modulo whitespace/comments/attrs) | user edits the handler body |
| `spec_hash` | raw text of `handler <name> { … }` in the `.qedspec` | spec changes the handler contract |

Both are **pure compile-time checks**: the macro expands to the original function unchanged (`cargo expand` to verify — zero runtime overhead, zero injected code).

**Setup**: omit a hash to have the macro tell you its current value:

```rust
#[qed(verified, spec = "percolator.qedspec", handler = "deposit")]
// ↑ cargo build prints: expected hash = "5af36...", spec_hash = "67719..."
```

Paste the printed hashes back into the attribute. Done.

### 4e. Reconcile spec drift (agent loop)

When a user edits the `.qedspec` after code/proofs are already written, QEDGen does **not** regenerate user-owned files (handler bodies, `Proofs.lean`). It surfaces drift via compile errors and orphan checks. You (Claude) are expected to reconcile.

**Files never clobbered by `qedgen codegen`:**
- `programs/<name>/src/lib.rs`, `programs/<name>/src/instructions/<handler>.rs` — user's business logic
- `formal_verification/Proofs.lean` — user's preservation proofs
- `tests/integration/*.rs` — user's integration test cases

**Files always regenerated:**
- `programs/<name>/src/guards.rs` — pure codegen from `requires` / `aborts_if`
- `formal_verification/Spec.lean` — types, transitions, property defs
- Kani and proptest harnesses

**Drift detection signals:**

| Signal | How to see it | What it tells you |
|---|---|---|
| Rust: `cargo build` error at `#[qed(...)]` | Compile error: "handler `X` spec contract changed. Expected: `abc`, Actual: `def`" | Spec's handler block changed — user's Rust handler needs review |
| Rust: `cargo build` error at `#[qed(...)]` with `hash` mismatch | Compile error: "function `X` has changed since verification" | User edited the function body — re-verify or update hash |
| Lean: `lake build` error referencing a missing symbol | "unknown identifier `deposit_guards`" or "unknown constant `Xyz`" | Spec renamed/removed something `Proofs.lean` references |
| Lean: `qedgen check` orphan/missing report | "orphan theorem `X`" / "missing obligation `Y`" | `Proofs.lean` has theorems for handlers that no longer exist, or is missing theorems for new handlers |

**Agent reconcile loop:**

```
1. Run `cargo build` in the generated program crate, `lake build` in formal_verification/
2. Also run:  $QEDGEN check --spec program.qedspec --json
3. For each drift signal:
     a. Read the CURRENT spec's handler/property block
     b. Read the CURRENT user file (handler body or theorem)
     c. Diff them — identify what semantic change the spec introduced
     d. Apply the minimal edit to the user file to restore consistency
     e. For Rust: update the `hash` and `spec_hash` attribute values to the new computed values (the compile error shows them)
     f. For Lean: add missing `theorem … := by sorry` stubs, delete orphan theorems, or rename per the spec
4. Re-run build commands; repeat until green
5. Fill any newly-introduced sorry stubs via the standard proof path (Step 4b → 4c)
```

**Rules for reconciliation:**

- **Never auto-update a hash without inspecting the diff first.** The `spec_hash` changing is the only signal the user has that the spec contract moved under them; silently bumping it bypasses the check.
- **Handler signature changes require a code review, not a mechanical patch.** If `deposit` gained a `deadline: u64` parameter in the spec, the Rust body may need real logic changes, not just a re-hash.
- **Orphan theorems are almost always safe to delete** once you've confirmed the spec no longer declares that handler/property. Missing theorems get stubbed with `sorry` and then filled.
- **If both hashes drift together**, the spec changed AND the user's body was edited in the same cycle — present this to the user before acting; don't silently prefer one side.

### 4f. Verify and report

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

**Every `qedgen codegen` or `qedgen check` invocation requires a git repo.** The CLI exits with a clear error if run outside a `.git` tree — this protects against accidental data loss from scaffolding into an unversioned directory. Run `git init` first if needed.

**Ownership model** (which files codegen owns vs. scaffolds once):

| File | Policy | Rationale |
|---|---|---|
| `formal_verification/Spec.lean` | always regenerated | Pure codegen: types, transitions, property defs |
| `formal_verification/Proofs.lean` | scaffold once, never touched | User-owned proof bodies |
| `programs/<name>/src/guards.rs` | always regenerated | Pure codegen from `requires` / `aborts_if` |
| `programs/<name>/src/lib.rs`, `instructions/<handler>.rs` | scaffold once, never touched | User-owned business logic |
| `tests/kani/*.rs`, `tests/proptest/*.rs` | always regenerated | Pure codegen; user customizations go in a sibling file |
| `tests/integration/*.rs` | scaffold once, never touched | User-owned test cases |

**Subsequent `qedgen codegen` runs** print skip advisories for user-owned files and always regenerate pure-codegen files. Drift between user-owned files and the spec is caught at `cargo build` / `lake build` time via the `#[qed(spec_hash=…)]` macro and `qedgen check` orphan reports — see **Step 4e** for the reconcile loop.

```bash
# Validation (spec design feedback — transient, agent-driven)
$QEDGEN check --spec program.qedspec                    # lint + coverage + orphan/missing theorems
$QEDGEN check --spec program.qedspec --json              # machine-readable for agent

# Generated (scaffolded or regenerated per ownership table above)
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

| Variable | Purpose | When needed |
|---|---|---|
| `MISTRAL_API_KEY` | Leanstral API access (`fill-sorry`, `generate`) | Lean proof sorry-filling |
| `ARISTOTLE_API_KEY` | Aristotle deep proof search | Hard sub-goals Leanstral can't solve |
| `QEDGEN_HOME` | Override global home directory (default: `~/.qedgen/`) | Always |
| `QEDGEN_VALIDATION_WORKSPACE` | Override validation workspace (default: `~/.qedgen/workspace/`) | Lean proofs |

API keys and Lean toolchain are **not required** for spec writing, validation (`check`), or code generation (`codegen`). They are only needed when filling proof obligations (`fill-sorry`, `generate`, `aristotle`). Prompt the user to set them up only when they reach the proof engineering phase.

## Error handling

- **First `lake build` is slow with Mathlib**: 15-45 min first time, cached after.
- **`could not resolve 'HEAD' to a commit`**: Remove `.lake/packages/mathlib`, run `lake update`.
- **Rate limiting (429)**: Built-in exponential backoff in `fill-sorry`.
- **`omega` fails on address disjointness**: See `references/sbpf.md` simp normalization.
- **`simp` timeout on sBPF proofs**: Check three performance rules in `references/sbpf.md`.
