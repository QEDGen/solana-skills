---
name: qedgen
description: Find the bugs your tests miss. Define what your Solana program must guarantee in a .qedspec — QEDGen validates it and generates tests, proofs, and CI to keep it fixed. Trigger when the user wants to verify code, write a .qedspec, generate tests or proofs, check program properties, or ship to mainnet with confidence. Also trigger on "qedgen", "qedspec", "verify my code", "prove correctness", "formal verification", "property testing".
---

# QEDGen — Spec-Driven Verification

You (Claude) help the user **specify** what their program must guarantee. The `.qedspec` is the deliverable — everything else (property tests, Kani harnesses, Lean proofs, Rust code) is derived from it. You iterate on the spec with the user, using the verification waterfall (proptest → Kani → Lean) to surface bugs as spec-level feedback.

**Reference files** (read on demand — do NOT load all at once):
- `references/qedspec-dsl.md` — qedspec, qedguards, qedbridge DSL syntax
- `references/qedspec-imports.md` — `import` keyword, `qed.toml`, `qed.lock`, `--frozen`, `--check-upstream` (v2.8+)
- `references/adversarial-probes.md` — Probe taxonomy the agent walks against the spec (§3a.5) — a checklist, not a CLI command
- `references/cli.md` — Full CLI reference with all commands and flags
- `references/support-library.md` — Lean types, constants, lemmas, arithmetic helpers (Phase 2 only)
- `references/proof-patterns.md` — Lean proof patterns + tactic rules (Phase 2 only)
- `references/sbpf.md` — sBPF assembly verification: asm2lean, wp_exec, memory, simp performance (sBPF is Lean-mandatory)

## Important: how to run qedgen

All `qedgen` commands MUST be run via the wrapper script. Set this once:
```bash
QEDGEN="$HOME/.agents/skills/qedgen/tools/qedgen"
```

## Architecture

The `.qedspec` is the **single source of truth** — the only artifact committed to the user's repo. Everything else (proptests, Lean theorems, generated code) is derived and disposable.

```
Three phases:

Phase 1 — Spec Design (interactive, all artifacts transient)
  You + User ──→ iterate on .qedspec
    ├── Lint               — structural validation              (instant)
    ├── Adversarial probe  — agent walks the probe taxonomy     (instant)
    ├── Proptest           — random counterexamples              (~100ms)
    ├── Kani BMC           — bounded-model counterexamples       (~seconds–minutes)
    └── lean-gen (opt.)    — type-level provability sanity      (~seconds)
  Mindset: architectural AND adversarial. Declare what must be true; the
  probes + counterexample tiers hunt for what breaks it. Kani+proptest are
  the spec's defenses, not a post-ship check.
  Deliverable: .qedspec (committed)
  Covered in: Step 1–3 below.

Phase 2 — Formal Proofs (on demand, for exotic math only)
  .qedspec ──→ Spec.lean ──→ Lean proofs ──→ lake build
    ├── Leanstral    — fast sorry-filling               (seconds)
    └── Aristotle    — deep agentic proof search         (minutes-hours)
  When to enter: the program has obligations Kani/proptest can't discharge
  — DeFi curve math, wide-arithmetic solvency, novel cryptographic
  primitives, or inductive sBPF bytecode proofs. Most programs finish at
  Phase 1.
  Mindset: tactical. Read Lean errors, select proof patterns, route hard
  sub-goals to the right backend. Switch references/proof-patterns.md
  and references/sbpf.md into active context.
  Deliverable: zero-sorry Lean project + #[qed(verified)] stamps
  Covered in: Step 4 below.

Phase 3 — Audit / drift maintenance (ongoing, post-ship)
  Verified code ──→ `qedgen reconcile` ──→ drift report
    ├── spec_hash mismatch       — handler body drifted from spec
    ├── orphan / missing theorem — Lean proofs out of sync with spec
    └── upstream_binary_hash     — library interface (SPL Token, …) moved
  Mindset: skeptical review. Read-mostly, verify claims, flag gaps.
  Deliverable: no unresolved drift; verified artifacts remain trustworthy.

Spec-driven pipeline (all generated from the same .qedspec):
  qedspec ──→ Proptest harnesses    (transient, /tmp)
          ──→ Kani harnesses        (generated)
          ──→ Spec.lean             (generated — Phase 2 only)
          ──→ Anchor Rust program   (generated — uses `anchor_lang::prelude::*`)
          ──→ Unit + integration tests (generated)
```

Phase boundaries are meaningful: each phase has a distinct mindset,
distinct tooling focus, and a distinct reference pack. When transitioning,
load only the references relevant to the new phase — keep context tight.

## Step 1: Understand the program

**Always read the source code first.** Then classify the project:

### Returning qedgen project (`.qedspec` exists)
Read the `.qedspec` alongside the source. The `.qedspec` is the spec source of truth — `Spec.lean` is generated from it, never treat it as the primary source. On returning visits, re-enter the spec design loop (Step 3) to validate or extend the spec, or proceed directly to proof engineering (Step 4) if the user wants formal certificates.

### Brownfield project (existing code, no `.qedspec`)
An existing Rust program being onboarded to qedgen for the first time.

1. **Read the source code** — Understand the program's state, instructions, guards, and invariants directly from Rust source. This is always the ground truth.
2. **Check for existing Rust tests** — Look for `tests/`, `#[cfg(test)]`, or `#[cfg(kani)]` modules. Existing tests reveal the program's testing patterns, helper infrastructure, and what's already covered. These shape how you write Kani harnesses (see "Brownfield projects" under Code generation).
3. **Anchor program** (`#[program] pub mod` exists) → Run `$QEDGEN adapt --program <crate-dir>` to scaffold a `.qedspec` from source. The adapter follows each `pub fn` in the `#[program]` mod to its actual handler body, picks up typed arguments + the `Context<X>` accounts struct + `#[error_code]` variants, and leaves a path breadcrumb to each handler. Edit the generated TODOs (lifecycle, requires, effect, transfers), then continue at Step 2 / 3.
4. **IDL exists** (`target/idl/<program>.json`, no source) → Run `$QEDGEN spec --idl <path> --format qedspec` for an ABI-only scaffold. Less precise than `qedgen adapt` (no handler bodies, no error variants), but works when only the IDL is available.
5. **Native / non-Anchor Rust source** (no `#[program]`) → Extract the spec using LSP. See "Writing a .qedspec from Rust source" below.
6. **Nothing to start from** → Read the source code and ask scoping questions (Step 2).

#### After the .qedspec is filled in: the `#[qed]` drift loop

Once the spec covers each handler, paint `#[qed]` attributes on the handlers so future body edits trip a compile error:

```
$QEDGEN adapt --program <crate-dir> --spec <path-to>.qedspec
```

Emits one `#[qed(verified, spec = ..., handler = ..., hash = ..., spec_hash = ..., accounts = ..., accounts_file = ..., accounts_hash = ...)]` line per spec handler with the matching source path. Paste each above its handler `pub fn`. The `qedgen-macros` crate recomputes every hash at compile time — body edits, spec edits, or `#[derive(Accounts)]` constraint edits without a re-run print a diff and break the build until you re-paste. All four forwarder shapes (inline, free-fn, type-associated method, accounts-method) seal end-to-end; the proc-macro tries `syn::ItemFn` and falls back to `syn::ImplItemFn`, so attaching `#[qed]` to a method inside an `impl` block works the same as on a free fn. The `accounts*` triplet is included automatically whenever the adapter can find the `Context<X>` struct in source.

Cross-check spec coverage against the live program in CI:

```
$QEDGEN check --spec <path> --anchor-project <crate-dir>
```

Two gates fire here:
- **Handler coverage.** Errors when the spec declares a handler the program doesn't have, or vice versa.
- **Effect coverage.** Heuristic lint: for each spec effect, asserts the corresponding Rust handler body contains at least one assignment-like mutation whose LHS leaf matches the effect's field name. Catches the "I added a spec effect but forgot to wire the code" footgun.

Pure read; pairs with `--frozen` for full CI gating. For handlers with custom dispatcher shapes the classifier can't follow automatically (runtime lookup tables, closure calls, non-path tails), use `qedgen adapt --handler <name>=<rust_path>` to point at the actual implementation manually (repeatable per handler).

### Writing a .qedspec from Rust source

For native Rust programs without an IDL (no Anchor framework), use LSP and source reading to extract the program structure into a `.qedspec`. Work through these in order:

1. **Find the entry point** — Look for `process_instruction` or the instruction dispatcher. Use LSP to find all match arms or handler functions. Each handler becomes an `operation` block.

2. **Find state structs** — Search for structs that are serialized/deserialized from account data (look for `borsh::BorshDeserialize`, `Pack`, or manual byte parsing). Each becomes a `state {}` or `account {} ` block. Map field types: `u64` → `U64`, `Pubkey` → `Pubkey`, etc.

3. **Find account validation** — In each handler, identify which accounts are checked for `is_signer`, which are writable, and any PDA derivations (`Pubkey::find_program_address`). These map to `context {}` entries with `Signer`, `mut`, `seeds()`, `bump`.

4. **Find guards** — Look for early-return error checks (`if !condition { return Err(...) }`). These become `guard` clauses. Map error codes to a top-level `type Error | Name | ...` ADT. (The `errors [...]` list-sugar is v2.5-restricted to inside `pragma sbpf { ... }`.)

5. **Find state mutations** — Track which fields are modified in each handler. These become `effect {}` blocks (`field = value`, `field += value`, `field -= value`).

6. **Find CPI calls** — Look for `invoke` or `invoke_signed`. Declare the callee's contract as an `interface Name { ... }` block (program_id, handler discriminant, accounts, optional `ensures`), then write `call Name.handler(name = expr, ...)` at each invocation site. For Anchor/SPL Token programs, use `qedgen interface --idl target/idl/<program>.json --out interfaces/<program>.qedspec` to scaffold a Tier-0 interface. See `interfaces/spl_token.qedspec` for the canonical SPL Token Tier-1 interface.

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

The spec design phase is an **architectural and adversarial interactive loop**. Draft the `.qedspec`, walk the probe taxonomy against it for attack surfaces the user didn't enumerate, then exercise it with counterexample tiers (proptest first, Kani BMC second) to find anything the probes missed. Lint, probes, proptest, and Kani are spec-design tools — not post-ship validation. All harnesses and transient Lean builds live in `/tmp`. Only the `.qedspec` gets committed.

```
┌─────────────────────────────────────────────────────────────────┐
│                   Spec Design Loop                              │
│                                                                 │
│   Write/edit .qedspec                                           │
│        │                                                        │
│        ▼                                                        │
│   Adversarial probe (auto) ──→ attack surface? ──→ add obligations
│        │                                                        │
│        ▼                                                        │
│   Lint ──→ priority 1-2 findings? ──→ fix spec, re-lint         │
│        │                                                        │
│        ▼                                                        │
│   Proptest (~100ms) ──→ counterexample? ──→ fix spec            │
│        │                                                        │
│        ▼                                                        │
│   Kani BMC (~seconds–minutes) ──→ counterexample? ──→ fix spec  │
│        │                                                        │
│        ▼                                                        │
│   lean-gen + lake build (optional) ──→ unprovable? ──→ fix      │
│        │                                                        │
│        ▼                                                        │
│   Present findings to user as spec-level issues                 │
│        │                                                        │
│        ▼                                                        │
│   User confirms ──→ finalize .qedspec                           │
│                                                                 │
│   Deliverable: the .qedspec file (committed to repo)            │
│   Everything else: disposable agent workspace                   │
└─────────────────────────────────────────────────────────────────┘
```

### 3a. Write the initial .qedspec

Write the `.qedspec` at the program root. See `references/qedspec-dsl.md` for full DSL syntax. Key v2.6 constructs:

- **`pragma sbpf { ... }`** wraps sBPF-specific declarations (`instruction`, `pubkey`, per-instruction `errors`). The pragma's presence also selects the assembly target — no explicit `target` keyword.
- **`interface Name { ... }` + `call Name.handler(...)`** declare CPI contracts. Generate Tier-0 scaffolds from Anchor IDLs with `qedgen interface --idl target/idl/<program>.json`. Tier-1 (hand-authored `ensures`) strengthens the caller's Lean proof.
- **Multi-file specs** — `qedgen check --spec <dir>` accepts a directory of `.qedspec` fragments all declaring the same `spec Name`. Merged in sorted-path order. See `examples/rust/escrow-split/` for the convention.
- **`let x = v in body`** — ML-style expression binding inside `ensures`/`requires`/effect RHS. Use when naming a computed quantity makes the post-condition clearer: `ensures let delta = old(state.balance) - state.balance in delta == amount`.

For sBPF assembly programs, use `qedguards` instead of the core DSL for guard/property bodies.

Present the spec to the user and get confirmation before proceeding to validation.

### 3a.5. Adversarial probe (agent-walked attack-surface checklist)

Before running lint or counterexample tiers, walk the probe taxonomy
in `references/adversarial-probes.md` against the spec and source —
looking for attack surfaces the user may not have enumerated. This
is a checklist the agent executes in-session, not a `qedgen probe`
CLI command; a real command is a future release scope. Users typically don't
think through authorization-bypass scenarios, close-reinit vectors,
or narrowing-cast corruption on their own; the probe list is what
you apply for them.

See `references/adversarial-probes.md` for the full taxonomy. Classes:

- **Arithmetic & casting** — narrowing casts from generic/const params,
  unchecked mul/add/sub on attacker-influenced operands, usize subtraction
  with external-mutation reachability.
- **State machine & close-safety** — replay, re-init, close without
  discriminator scrub, reads after an external resize.
- **Borrow / aliasing protocol** — release-then-reacquire losing writes,
  borrow-state assumptions that don't hold at the loader boundary.
- **Authorization** — missing signer/owner checks, PDA seed uniqueness.
- **Conservation** — sum-across-accounts invariants with partial updates.
- **CPI execution** — account ordering, realloc lamport disjointness,
  data-increase bounds.

For each probe that hits:

1. Add the corresponding negative-path obligation to the spec —
   `aborts_if`, `cover`, or a property — so proptest/Kani have a
   concrete target.
2. Record the hit in `.qed/plan/findings/NNN-<probe-slug>.md` (see
   **Learning capture** below) with the pattern shape, not the incident.
3. Surface *only* the findings to the user: "I noticed X could Y; I've
   added an `aborts_if` to defend. Confirm or adjust." Keep the probe
   list itself out of the conversation.

Probes that don't hit leave no trace. Silent when the spec is sound,
loud when it isn't.

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

### 3c. Counterexample hunting (proptest → Kani BMC)

Two adversarial tiers, both transient (generated, run, discarded in `/tmp`).
Proptest runs on every spec edit — it's ~100ms and catches most
counterexamples by random sampling. Kani runs when proptest is green
— it's bounded-model and exhaustive within its unwind limits, slower
(seconds to minutes) but deterministic.

#### 3c.1 Proptest (~100ms, every spec edit)

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

#### 3c.2 Kani BMC (~seconds–minutes, after proptest passes)

Run Kani once proptest is green. Kani is deterministic — it finds
everything within its unwind bound — but costs more wall-time per run.
Worth the cost: bounded-model verification of arithmetic and state
transitions is the single strongest defense short of full Lean proofs,
and it holds for the overwhelming majority of program-level obligations.

```bash
# Generate harness
$QEDGEN codegen --spec <path-to-qedspec> --kani --kani-output /tmp/kani_harness.rs

# Run in a scratch project with Kani installed
mkdir -p /tmp/kani_runner/src
cat > /tmp/kani_runner/Cargo.toml << 'EOF'
[package]
name = "kani-runner"
version = "0.1.0"
edition = "2021"
EOF
cp /tmp/kani_harness.rs /tmp/kani_runner/src/lib.rs
cd /tmp/kani_runner && cargo kani
```

**Solver selection.** Kani's CBMC default (CaDiCaL) handles layout and
small arithmetic well but chokes on wide types (u128) and symbolic
div/rem. When a harness times out or takes minutes, swap solvers per
property class:

| Property class                          | Solver     | Attribute                       |
|-----------------------------------------|------------|---------------------------------|
| Layout, bit-fiddling, u8/u16/u32 math   | CaDiCaL    | (default — no attribute needed) |
| u128 mul/add/sub, signed div/rem        | Z3         | `#[kani::solver(z3)]`           |
| Long-tail / mixed                       | kissat     | `#[kani::solver(kissat)]`       |
| Floating-point adjacent                 | bitwuzla   | `#[kani::solver(bitwuzla)]`     |
| Fallback                                | cvc5       | `#[kani::solver(cvc5)]`         |

Start with CaDiCaL. If a harness is slow (>30s for a bounded property),
add `#[kani::solver(z3)]` — the 128-bit arithmetic surface that would
otherwise require Lean verifies cleanly under Z3 in seconds. This is
the single most common reach-for-a-flag moment in Kani usage; don't
spend wall-time on CaDiCaL timeouts.

**Interpreting failures.** Kani prints a concrete counterexample — the
input that violated the assertion. Treat it as a spec bug report:
either add the missing guard, or narrow the property's precondition.
Record the class of bug (not the specific input) in
`.qed/plan/findings/` so the shape informs future probes.

### 3d. lean-gen (optional — type-level sanity)

**Skip unless the project is Phase 2 bound** (DeFi/crypto math, sBPF).
Proptest + Kani are sufficient for Phase 1 programs.

For Phase-2-bound projects, generate the Lean type stubs and run
`lake build` on the statements only. This catches spec bugs where the
theorem statement itself is nonsense (e.g. a conservation property that
doesn't type-check against the declared state). It does **not** attempt
proofs — `sorry` is fine here.

```bash
# Generate Spec.lean to /tmp
$QEDGEN codegen --spec <path-to-qedspec> --lean --lean-output /tmp/lean_check/Spec.lean

# Quick validation build (types only, proofs are sorry)
$QEDGEN init --name check --output-dir /tmp/lean_check
cp /tmp/lean_check/Spec.lean /tmp/lean_check/formal_verification/Spec.lean
cd /tmp/lean_check/formal_verification && lake build
```

A structurally unprovable theorem is feedback about the spec — fix the
`.qedspec` and re-run. Tactic failures are Phase 2 territory; a file
full of `sorry` that type-checks is the expected output of this step.

### 3e. Present findings as spec-level issues

**The user does not need to know which tier found the bug.** Present all findings as spec-level issues:

- "Your `deposit` handler can overflow `total_deposits`, which violates `pool_solvency`. Add a guard: `guard total_deposits + amount >= total_deposits`."
- "The `withdraw` handler's effect `V -= amount` can underflow when `V < amount`. The guard `C_tot >= amount` doesn't protect `V`."
- "The `approve` transition allows re-approval — `approval_count` increments without checking if this signer already approved."

The verification tier that found it (proptest, lean-gen, lint) is an implementation detail. Fix the spec, re-run the loop.

### 3f. Finalize

When all tiers pass clean, present the final `.qedspec` to the user. This is the deliverable — it gets committed to the repo. Everything else (proptests, Lean files, build artifacts) is disposed.

## Step 4: Proof engineering (on demand)

Step 4 applies when the user wants **formal proof certificates** —
mathematical guarantees backed by Lean 4 proofs. The `.qedspec` is the
input; Lean proofs are the output.

**Most programs don't need Phase 2.** A `.qedspec` validated by lint +
adversarial probes + proptest + Kani BMC in Phase 1 is the finish line
for the majority of Solana programs — authorization, conservation at
u64/u128 bounds, lifecycle, CPI shape, close-safety, and narrowing-cast
defenses all verify cleanly without leaving the SAT/SMT world.

Enter Phase 2 when the program has obligations those tiers can't
discharge:

- **DeFi numerical invariants** — AMM curve math, bonding curves,
  fee-conservation across wide arithmetic, solvency under compound
  operations. Mathlib (`Finset`, `BigOperators`, `ring`, `linarith`,
  `norm_num`) earns its 8GB here.
- **New cryptographic primitives** — hash-based constructions,
  signature schemes, commitment schemes whose correctness depends on
  mathematical properties rather than byte-level equivalence.
- **Inductive bytecode proofs** — sBPF assembly programs. sBPF is
  Lean-mandatory by construction: the SVM interpreter lives in the
  Lean support library, there's no Kani substitute. See
  `references/sbpf.md`.

**Drift caution.** The `QEDGen.Solana.*` support library intentionally
models a narrow, stable surface: program IDs, SPL Token discriminators,
account/CPI structure, `Lifecycle`, Mathlib-backed arithmetic helpers.
It does **not** ship Solana-runtime axioms (rent formula, ABI layout,
borrow-state protocol, PDA ownership rules, CPI execution semantics).
Those track a moving target and would rot faster than they pay off. If
a proof seems to need such axioms, it's asking for a boundary Phase 2
won't give you — stay in Phase 1 with Kani on the specific scenario.

**Don't hedge Phase 2 with Mathlib cost caveats.** If the target's
properties need Phase 2 (DeFi math, cryptographic primitives, sBPF
bytecode), then the toolchain is Mathlib — not an option. Phrasing
like *"we could burn 30 minutes for nothing"* or *"try a forced
`.qedspec` first to avoid the Mathlib install"* is actively harmful:
it pushes users into ceremonial specs that force qedgen into shapes
it's not designed for. The 8 GB / 15-45 min first build is one-time
**shared** infrastructure — every future Phase 2 project on the
machine reuses it in seconds. Recommend `qedgen setup --mathlib` once
and move on. Surface Error handling links if the user hits a
first-build issue, but don't pre-weigh the install against doing
nothing.

### 4a. Set up the Lean project

```bash
# Anchor/Rust project — --spec pins the authored qedspec location in .qed/config.json
$QEDGEN init --name escrow --spec escrow.qedspec

# sBPF project (runs asm2lean automatically)
$QEDGEN init --name dropset --spec dropset.qedspec --asm src/dropset.s

# With Mathlib (for u128 arithmetic helpers)
$QEDGEN init --name engine --spec engine.qedspec --mathlib

# With the full Anchor handler + Kani codegen pipeline. `--target`
# selects the framework: `anchor` is implemented today; `quasar` and
# `pinocchio` are reserved for v2.10+ and error cleanly when selected.
$QEDGEN init --name counter --spec counter.qedspec --target anchor
```

After init, subsequent commands find the spec automatically by walking up
from the current directory to the nearest `.qed/config.json` — no
`--spec <path>` needed on `qedgen check` or `qedgen codegen` from inside
the project. Explicit `--spec` still overrides when you want to point at
something different.

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
- **sBPF proofs do NOT need Mathlib.** Built-in tactics handle bytecode semantics.
- **DeFi / crypto proofs DO need Mathlib** — `Finset`, `BigOperators`, `ring`, `linarith`, `norm_num`, u128 reasoning. If you're in Phase 2 for DeFi math or a cryptographic primitive, pass `--mathlib` on the first `qedgen init`. Don't hedge.
- **The 8 GB / 15–45 min is one-time shared infrastructure, not a per-project cost.** `qedgen setup --mathlib` populates `~/.qedgen/workspace/` once; every subsequent `qedgen init --mathlib` references that shared install in seconds instead of re-fetching. Disk is cheap; the Lake cache persists across every future Phase 2 session in the workspace.
- **First-build failure is rare and recoverable** — see Error handling below if `lake update` needs to re-resolve.

**Ergonomic setup** (recommended once per machine, before any Phase 2 work):

```bash
$QEDGEN setup --mathlib    # one-time, 15-45 min
```

After this, every `qedgen init --mathlib` emits a lakefile pointing at the
shared install. If the shared workspace isn't populated, init falls back
to a fresh git fetch and prints a hint pointing at `setup --mathlib`.

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
$QEDGEN check --spec program.qedspec --code programs/my_program/ --kani programs/tests/kani.rs
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
| `programs/tests/kani.rs`, `programs/tests/proptest.rs` | always regenerated | Pure codegen; user customizations go in a sibling file. Layout changed in v2.6 so `cargo kani --tests` / `cargo test --test proptest` resolve the program's `Cargo.toml` directly. |
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

With `qedgen init --target anchor`, the generated outputs are created automatically.

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

### v2.5 spec features

**`pragma <name> { ... }`** — platform-specific namespace. sBPF constructs
(`instruction`, `pubkey`, list-form `errors`) are pragma-only; they won't
parse at the top level. `pragma sbpf { ... }` also selects the assembly
target (no explicit `target` keyword — that was removed in v2.5).

**`interface Name { ... }` + `call Target.handler(name = expr, ...)`** —
declarative CPI contracts. Three tiers: shape-only (Tier 0, from IDL),
hand-authored `requires` / `ensures` (Tier 1, e.g. SPL Token library at
`interfaces/spl_token.qedspec`), and imported qedspec (Tier 2, v2.6+).
Tier-1 clauses bind proptest, Kani, and — when the caller has a Lean
side — Lean obligations from the same source.
`qedgen check` emits `[shape_only_cpi]` for calls to Tier-0 interfaces —
the visible gap between "my Rust compiles" and "my program is verified."

**Caution when growing Tier-1.** Keep `requires`/`ensures` **semantic**
(`amount > 0`, `from != to`, `authority signs`) rather than
**implementation-tracking** (specific fee math, byte layouts, particular
error codes). Implementation details drift with upstream releases;
semantic constraints don't. When in doubt, prefer fewer clauses plus a
Kani harness against the deployed binary over more clauses that need
rewriting when the upstream ships a patch.

**`qedgen interface --idl <path>`** — scaffold a Tier-0 interface from an
Anchor IDL. Honest output: no `ensures` (IDL doesn't carry semantics),
TODO-stubbed `upstream` block (author fills in after running harnesses
against the deployed program).

**Multi-file specs** — `qedgen check --spec <dir>` accepts a directory of
fragments. Every `.qedspec` under the path must declare the same `spec
Name`; items merge in sorted-path order. Convention: `state.qedspec`,
`handlers/<name>.qedspec`, `properties.qedspec`, `interfaces/*.qedspec`.

**`let x = v in body`** — ML-style expression binding inside
`ensures`/`requires`/effect RHS. Lowers to Lean's `let x := v; body`.

See `references/qedspec-dsl.md` for full syntax reference.

### v2.6 spec features

**`+=` / `-=` default to checked arithmetic** in the Kani model. On
overflow the transition returns `false`, matching the
`checked_add(..).ok_or(MathOverflow)?` pattern deployed Anchor programs
use. Proptest retains wrapping arithmetic for bounded exploration.
Before v2.6 the Kani model emitted bare `+=`, flagging overflow on every
unbounded pre-state — a spec-model artifact that didn't match real code.

**`state { fields }` sugar** — shorthand for a single-record spec.
Accepts comma- or newline-separated fields; desugars to
`type State = { ... }`.

**Single-line `accounts { a : signer, b : writable, ... }`** parses
alongside the multi-line form.

**`implies`** in property bodies lowers to `(!a) || (b)` in generated
Rust (Kani/proptest). **`forall` / `exists`** over **U8 or I8** lower to
`(T::MIN..=T::MAX).all(|v| body)` / `.any(|v| body)` — exhaustive check,
≤256 iterations, works in both proptest and Kani. Larger types (U16+)
still emit a `/* QEDGEN_UNSUPPORTED_QUANTIFIER */` sentinel and the
property fn returns `true`; `qedgen check` fires a P1
`unchecked_quantifier` warning so the gap is visible rather than silent.

**Quantified-property preservation theorems** (property body contains
`∀`/`∃`) default to `sorry` in generated Lean — `omega` cannot prove
universal goals. Fill these via the standard escalation ladder:
Leanstral first (`qedgen fill-sorry`), then Aristotle for harder goals.

**Multi-variant ADTs with shared field names** deduplicate when
flattened into the state model (first-variant wins on name collisions).
Proper enum+match codegen is roadmap.

**Generated harness paths moved inside the program package**
(`programs/tests/kani.rs`, `programs/tests/proptest.rs`). `qedgen
verify` and `qedgen codegen` defaults agree, so `cargo kani --tests` /
`cargo test --test proptest` resolve `programs/Cargo.toml` without a
hand-authored root shim.

**Generated `Cargo.toml` points at real deps** with clear TODO
scaffolding: `anchor-lang` + `anchor-spl` as runtime framework,
`qedgen-macros` as a git-dep tied to the release tag.

**`verify_*_rejects_invalid` harnesses fold every `requires` clause**
into the `kani::assume`. Zero preconditions → harness skipped entirely
(no vacuous pass).

**`verify_*_effects` splits per-field** — one harness per (handler,
field) pair. Arithmetic-heavy RHS (`*`/`/`) switches the solver to
`minisat`, which handles bit-blasted multiplication that cadical wedges
on.

**`qedgen init` refuses to nest `formal_verification/formal_verification/`.**
Run it from the project root, or pass `--output-dir .`.

See `references/qedspec-dsl.md`  "What changed in v2.6" for the full
list plus before/after examples.

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

## Learning capture (`.qed/plan/`)

`qedgen init` scaffolds `.qed/plan/` — a committed, agent-maintained
ledger of what qedgen caught, what it missed, and what reviewers
surfaced after the fact. This is **qedgen-team telemetry**: the data
shapes future probes, lints, and DSL features. The signal is what matters;
business specifics stay out.

**Layout** (subdirectories created lazily on first write):

```
.qed/
  config.json                       # existing project metadata
  plan/
    README.md                       # conventions (seeded by init)
    findings/NNN-<slug>.md          # patterns caught / missed
    sessions/YYYY-MM-DD-<topic>.md  # session summaries at boundaries
    gaps.md                         # "qedgen didn't catch X; Y did"
    reviewers.md                    # external-review feedback
    scoping.md                      # moments we recommended NOT using qedgen
```

**When to write.** As you work:

- Every adversarial probe hit (§3a.5) → `findings/NNN-<probe-slug>.md`
  with the pattern shape and the obligation that was added.
- Every Kani counterexample → `findings/NNN-<class>.md` with the bug
  class (not the specific input).
- Every time the user overrides a suggestion, corrects a miss, or
  points out a probe should have fired → `gaps.md` entry with a
  one-line hypothesis for what would catch it next time.
- Every external reviewer finding (audit, security firm, ad-hoc
  teammate) → `reviewers.md` entry, pattern-tagged.
- Meaningful session boundaries (spec finalized, proofs shipped, bug
  resolved) → `sessions/YYYY-MM-DD-<topic>.md` with three fields: what
  we tried, what worked, what we'd do differently.
- **Every scoping decision where you recommended NOT engaging qedgen's
  default flow** → `scoping.md` entry with four fields: target shape
  in one sentence, why `.qedspec` didn't fit structurally, what you
  recommended instead (Phase 2 direct, skip, forced-shim), and a
  one-line hypothesis for what DSL extension would unlock the class.
  This is the richest signal for DSL evolution — it fires even when
  qedgen was never run. Write it **before** starting the recommended
  path, not after, so the reasoning doesn't evaporate.

**What to write.** Capture **patterns**, not specifics.

- **Good:** *"Generic const parameter flowed into an `as u16` cast
  without a force-evaluated compile-time bound — silent wrap at the
  65,536th push. Probe P-arith/narrowing-cast fired; added
  `aborts_if MAX > U16_MAX with NarrowingWrap`."*
- **Bad:** *"Alice's vault overflowed when she deposited 2^16 times."*

Scrub account keys, pubkeys, user identities, dollar values, and
project-specific identifiers before writing.

**Telemetry (future release).** A future `qedgen telemetry push` will
upload entries anonymised, opt-in. Until that command ships, `.qed/plan/`
is local to the repo — inspect, edit, or delete any entry before it
ever leaves the machine.

## Git hygiene

The `.qedspec` is the primary committed artifact. Everything else is derived.

**Always commit:**
- `.qedspec` file (the spec source of truth)
- `.qed/config.json` and `.qed/plan/` (project metadata + learning ledger)

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
