# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

QEDGen is a Claude Code skill for spec-driven verification of Solana programs. The `.qedspec` is the single source of truth — QEDGen validates it (proptest, Kani, Lean) and generates all downstream artifacts (Rust code, test harnesses, Lean proofs, CI workflows). Leanstral and Aristotle handle hard proof sub-goals when escalated.

**Core workflow**: User describes intent → agent writes `.qedspec` → `qedgen check` validates (lint + proptest + Lean) → iterate on spec → `qedgen codegen --all` generates committed artifacts → `#[qed(verified)]` stamps verified code

## Primary user interface: agent + skill

QEDGen's UX is **agent-first**, not CLI-first. The end user interacts with:

1. **The SKILL** (`.claude/skills/qedgen/SKILL.md` + this file) — declarative guidance that shapes Claude's behavior when working with `.qedspec` files.
2. **Agents** — Claude (orchestrator), Leanstral (fast sorry-filling), Aristotle (long-running proof search). The CLI (`qedgen …`) is the *interface between agents and artifacts*, not a user-facing tool in its own right.

**Proof-filling escalation order** (default → last resort):

1. **Mechanical → codegen template** (`lean_gen.rs`): trivial preservation, vacuous cases from aborting branches, scalar-arithmetic goals closable by `omega`, `forall`-over-unchanged-Map via `Function.update_of_ne`.
2. **Non-mechanical but tractable → local LLM** (the LLM driving this session — Claude Code, Codex, or similar). Most real Lean proof bodies — case analysis, Mathlib lemma selection, sum-update rewrites, per-handler structural proofs — are well within a frontier LLM's reach and should be written directly in-context, not shelled out.
3. **Hard → Leanstral** (`qedgen fill-sorry`): when the local LLM has tried a few passes and still can't close the goal. Fast, non-deterministic, pass@N sampling.
4. **Last resort → Aristotle** (`qedgen aristotle submit`): agentic proof search measured in minutes to hours. Only when Leanstral has failed after multiple passes.

**v2.8 G3 — CPI ensures-as-axiom theorems**: when a handler does `call Interface.handler(...)`, codegen emits a per-call-site theorem whose statement is the callee's `ensures` substituted with the call-site arguments. v2.26 Slice 4a (Track F) replaces the `by sorry` body with `exact <Iface>.<handler>.ensures_axiom_<idx> <args>` for Tier-1/2 callees (interfaces declaring `ensures` AND an `upstream { binary_hash }` pin). A sibling `<Iface>.lean` module emits alongside `Spec.lean` carrying `def binary_hash` + `axiom ensures_axiom_<idx>` per declared ensures clause. Tier-0 callees (no ensures) keep the `by sorry` shape and fire the P1 lint `cpi_no_callee_ensures` to surface the upstream contract gap. v3.0 (stance 2) replaces the bundled axioms with imported callee proofs alongside the Anchor adapter that needs them. Until then, treat the axiom-discharged Tier-1/2 theorems as "verified by the imported `upstream { binary_hash }` pin, not by a Lean proof."

**v2.26 — first-class interfaces, end to end**: the `interface` declaration is a verification participant across all three backends. (1) Lean `render_cpi_theorems` applies the bundled axiom for Tier-1/2 callees (above). (2) Kani harnesses — both the v2.25 ensures-preservation shape (`kani.rs`) and the v2.26 impl-targeted shape (`kani_impl.rs`) — emit `kani::assume(<callee_ensures, substituted>)` after every CPI call site whose callee declares ensures, via the shared `cpi_substitute::substitute_callee_ensures_{lean,rust_binary}` helper. (3) `qedgen verify --check-upstream` promotes pin mismatches to CRIT-severity Findings (P2 in `check --frozen` by default, `--strict` escalates, `--upstream-stale-ok` suppresses for offline dev). Bundled SPL Token + System Program + Metaplex Token Metadata stdlib resolves `import X from "spl"` / `"system"` / `"metaplex"` via `crates/qedgen/data/interfaces/` with no `qed.toml` entry needed. Multi-CPI handlers whose callee ensures touch the same caller-state field fire the `multi_cpi_same_field` P2 lint + a `// WARNING: multi-CPI ordering` breadcrumb in the generated harness — per-call snapshot frames are v3.0-class.

**v2.26 — impl-targeted Kani harness (`--kani-impl`, opt-in)**: `kani_impl.rs` emits `programs/tests/kani_impl.rs` calling the user's REAL Anchor handler against a symbolic `Accounts` context (PDA-derived addresses bind via spec-declared seeds, account-data fields are `kani::any()`). Two trigger conditions: (a) explicit `--kani-impl`, or (b) auto-trigger when any handler has `modifies ⊋ effect.lhs` OR any `ref_impl` carries potentially-overflowing arithmetic over bounded-numeric params (`ref_impl_unbounded_arith` P2 lint). Anchor target only in v2.26; Pinocchio + native deferred to v2.27.

**v2.26 — `result` keyword on interface handler return types**: optional `-> <ident> : <Type>` syntax names the binder used in callee ensures (`handler absorb (amount : U64) -> burned : U64 { ensures burned <= amount }`). The substitution helper maps the declared binder to the caller's `let X = call ...` name. Bare `-> Type` (no binder) defaults to the literal `result` for back-compat.

**Code- and test-filling escalation order** (v2.4+, same shape as proofs):

1. **Mechanical → codegen template** (`codegen.rs::mechanize_effect`): scalar effects with simple RHS (`field := param`, `field += literal`, `field -= constant`) become real Rust; fully-mechanizable handlers ship as `Ok(())` with no `todo!()`.
2. **Non-mechanical but tractable → local LLM**: events (payload binding from spec event schema), token transfers (CPI builder shape), complex effect RHS (match/arith), and integration-test assertions (post-state checks, lifecycle chains). Run `qedgen codegen --fill` / `--fill-tests` to get one structured prompt per remaining `todo!()` site, then edit in-session.
3. **Last resort → spec refinement**: if the LLM can't fill the body from the prompt, the spec is under-specified. Add the missing detail (event field bindings, transfer authority chain, declared invariant) and re-run codegen. This is the Rust analog of "add a DSL feature that eliminates the proof obligation structurally".

**`qedgen verify` runs the generated harnesses** (v2.4+): `--proptest` shells `cargo test --release`, `--kani` shells `cargo kani --tests`, `--lean` shells `lake build`. With no backend flags, `verify` auto-detects every backend whose artifact is on disk and runs them all; failures surface verbatim with summarized diagnostics so the agent can act on them. Closes the loop that `qedgen check` opens (check validates the spec; verify validates the implementation).

**Design implications:**
- A new DSL feature that *eliminates* a proof obligation structurally (e.g. sum types making vacuous cases literal) is always preferable to a new proof template or a sorry to shell out.
- When a proof template can't handle a case, emit `sorry` with a comment documenting the obligation — don't bury it in complex tactics that might spuriously close.
- Don't pre-shell to Leanstral/Aristotle from code that a local LLM can handle. Escalation is when you've tried; not when you expect to need to.
- Routing between Leanstral and Aristotle is agent-decided per SKILL.md heuristics, not hardcoded in the CLI. The same applies to code/test fills: `--fill` emits prompts to stdout; the in-session agent decides when to call out (it almost never needs to).

## Build and Development Commands

### Build the CLI

```bash
# Build qedgen binary and copy to ./bin/qedgen
cargo build --release && cp target/release/qedgen bin/qedgen

# Build just the Lean support library
cd lean_solana
lake build
```

### Run Tests

```bash
# Rust unit tests
cargo test

# Test Lean support library axioms
cd lean_solana
lake env lean test_lemmas.lean

# Build the example escrow verification
cd examples/rust/escrow/formal_verification
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
  --file formal_verification/Spec.lean \
  --passes 3 \
  --validate

# Validate a spec (lint, coverage, drift)
qedgen check --spec program.qedspec                     # lint + coverage
qedgen check --spec program.qedspec --json              # machine-readable
qedgen check --spec program.qedspec --explain           # Markdown report
qedgen check --spec program.qedspec --drift src/        # drift detection
qedgen check --spec program.qedspec --drift src/ --deep # transitive drift

# Generate committed artifacts from a .qedspec
qedgen codegen --spec program.qedspec --all             # everything
qedgen codegen --spec program.qedspec --lean            # Lean proofs only
qedgen codegen --spec program.qedspec --kani            # Kani harnesses
qedgen codegen --spec program.qedspec --proptest        # proptest harnesses

# Agent-fill prompts for unfilled handlers (v2.4+)
qedgen codegen --spec program.qedspec --fill                       # all handlers
qedgen codegen --spec program.qedspec --fill --handler initialize  # one handler
qedgen codegen --spec program.qedspec --fill-tests                 # integration test sites

# Run the generated harnesses against the implementation (v2.4+)
qedgen verify --spec program.qedspec                    # auto-detect: every backend on disk
qedgen verify --spec program.qedspec --proptest         # cargo test --release proptest
qedgen verify --spec program.qedspec --kani             # cargo kani --tests
qedgen verify --spec program.qedspec --lean             # lake build
qedgen verify --spec program.qedspec --json             # machine-readable for CI

# Scaffold a .qedspec from an Anchor IDL
qedgen spec --idl target/idl/program.json --output-dir ./formal_verification

# Consolidate multiple proof projects into single project
qedgen consolidate \
  --input-dir /tmp/proofs \
  --output-dir formal_verification

# Transpile sBPF assembly to Lean 4 program module
qedgen asm2lean \
  --input examples/sbpf/transfer/src/transfer.s \
  --output formal_verification/Program.lean \
  --namespace Program
```

## Architecture

### Crate Structure

**`crates/qedgen-macros/`** - Proc macro crate: compile-time drift detection
- `lib.rs` - `#[qed]` attribute macro entry point, dispatches on keyword
- `verified.rs` - Content hash computation + `compile_error!` on drift

**`crates/qedgen/`** - Main crate: CLI, parsers, code generators
- `main.rs` - CLI entry points (init, check, codegen, generate, fill-sorry, aristotle, spec, asm2lean, setup, consolidate)
- `chumsky_parser.rs` - chumsky parser for `.qedspec` files (produces typed AST via `chumsky_adapter.rs`)
- `check.rs` - Spec validation: lint, coverage matrix, drift detection
- `lean_gen.rs` - Lean 4 code generation from parsed spec (Rust + sBPF renderers)
- `codegen.rs` - Rust program skeleton generation from spec (Anchor and Quasar targets fully supported; `--target pinocchio` reserves the CLI surface but is not yet implemented and errors at the init dispatcher)
- `pinocchio_probe.rs` - v2.19 Pinocchio audit site enumerator. Scans `*.rs` under `src/` for 10 site kinds (`BorrowUnchecked`, `BytemuckCall`, `RawPtrCastFromAccount`, `CustomLoadCall`, `TryIntoUnwrapOnSlice`, `SetLamportsArith`, `SetAmountArith`, `IndexedAccountAccess`, `IndexedDataSlice`, `SafetyComment`), parses adjacent `// SAFETY:` comments, emits a `PinocchioCatalogue` JSON. Maps each site to a candidate `Finding` paired with both `Reproducer::MolluskPrompt` and `Reproducer::MiriPrompt`. Routed via `qedgen probe --program <path>` (auto-detect) or `--runtime pinocchio` (explicit).
- `miri_verify.rs` - v2.19 Miri verify backend. Discovers `.qed/probes/pinocchio/*/repro_miri.rs`, shells `cargo +nightly miri test`, parses UB / aliasing / overflow / `SAFETY claim STALE` markers into structured `MiriDiagnostic`s. Dual-execution divergence detection (Miri-fail / Mollusk-pass) surfaces as `Category::ExecutionDivergence` (Critical).
- `kani.rs` - Kani BMC harness generation
- `proptest_gen.rs` - Proptest harness generation
- `unit_test.rs` - Unit test generation
- `integration_test.rs` - in-process SVM integration test generation
- `init.rs` - Project scaffolding (`qedgen init`, `.qed/` directory)
- `api.rs` - Mistral API client, pass@N sampling, sorry-filling, retry logic
- `aristotle.rs` - Aristotle (Harmonic) client for long-running proof search
- `asm2lean.rs` - sBPF assembly → Lean 4 transpiler (parses `.s`, emits program module)
- `deps.rs` - Point-of-use dependency checks (Lean, Kani)
- `validate.rs` - Lake build validation in persistent workspace
- `drift.rs` - `#[qed(verified)]` drift detection: scan Rust source, compute hashes, report/update
- `idl2spec.rs` - Anchor IDL → `.qedspec` scaffold generation
- `fingerprint.rs` - Spec section hashing for generated artifact staleness detection
- `project.rs` - Lean project scaffolding generation
- `consolidate.rs` - Merges multiple proof projects
- `idl.rs` - Anchor IDL parsing + first-pass pattern inference (consumed by `idl2spec` and `interface_gen`)

**`lean_solana/`** - Standalone Lean 4 library: Solana axioms (QEDGen.Solana)
- `QEDGen/Solana/Account.lean` - Account structure
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

See `examples/rust/escrow/formal_verification/VERIFICATION_SCOPE.md` for details.

## Common Development Tasks

### Adding New Axioms

When a proof pattern is reusable across programs:

1. Add to the appropriate module in `lean_solana/QEDGen/Solana/`
2. Document the trust assumption with a comment
3. Export in `QEDGen.lean`
4. Update SKILL.md support library API section
5. Test: `cd lean_solana && lake build`

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
   - `omega` fails on address disjointness after stack writes → normalize hypotheses with `simp [wrapAdd, toU64, ...]` (not `simp only`) so they match the goal form. Step-level simp applies `@[simp]` lemmas (modular identity, numeric evaluation) that `simp only` misses.
3. Fix the proof and re-run `lake build`

### sBPF Proof Workflow

For sBPF assembly programs, use `qedgen asm2lean` to generate the program module instead of hand-transcribing:

```bash
qedgen asm2lean --input src/program.s --output formal_verification/Program.lean
```

Then write proofs in Spec.lean that imports the generated module:

```lean
import QEDGen.Solana.SBPF
import Program

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open Prog

-- wp_exec is the primary tactic for sBPF proofs.
-- First bracket: fetch function + chunk defs (for dsimp instruction decode)
-- Second bracket: effectiveAddr lemmas + extras (for simp branch resolution)
theorem my_property ... :=
    (executeFn progAt (initState inputAddr mem) FUEL).exitCode = some CODE := by
  have h1 : ¬(readU64 mem inputAddr = SOME_CONST) := by rw [h_val]; exact h_ne
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_88]
```

For programs with two input pointers (r1=input buffer, r2=instruction data, e.g. SIMD-0321), use `initState2`:

```lean
-- entryPc allows non-zero entry points (e.g. error handlers before main logic)
(executeFn progAt (initState2 inputAddr insnAddr mem 24) FUEL).exitCode = some CODE
```

The `wp_exec` tactic uses the monadic WP bridge (`executeFn_eq_execSegment`) to iteratively unfold execution at O(1) kernel depth per step. For complex paths needing manual guidance (e.g., memory disjointness lemmas between steps), use `wp_step` to advance one instruction at a time.

### Memory Disjointness Through Stack Writes

When sBPF programs write to the stack then read from the input buffer, use memory axioms to prove reads see original memory:

```lean
-- Byte read through dword stack write
rw [readU8_writeU64_outside _ _ _ _
  (by left; unfold STACK_START at h_addr ⊢; omega)]
```

Key patterns:
- Add a **stack-input separation hypothesis**: `h_sep : STACK_START + 0x1000 > inputAddr + 100000`
- For **dynamic addresses** (after `add64`/`and64`), introduce bound hypotheses so omega can prove disjointness
- Use **`simp`** (not `simp only`) to normalize hypotheses containing `wrapAdd`/`toU64` to match step-execution goal forms — `simp only` misses modular identities like `(a % m + b) % m = (a + b) % m`
- For complex paths (20+ steps), organize into **phases**: (1) validation prefix, (2) pointer arithmetic / stack writes, (3) property-specific read-and-branch with disjointness proofs

See SKILL.md "Memory disjointness through stack writes" for the full pattern.

### sBPF simp Performance (Critical)

The `wp_exec` tactic is sensitive to how constants are typed and named. Violations cause exponential blowup (seconds → hours).

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

The `qedgen asm2lean` command handles Rules 1-3 automatically: it emits `Int`-typed offsets, `Nat`-typed non-offsets, named constants in the `prog` array, and `@[simp]` on `prog`. It also auto-generates:
- `@[simp] theorem ea_NAME` — effectiveAddr lemmas for each offset symbol
- `@[simp] theorem bridge_NAME` — toU64 bridge lemmas for Nat lddw constants
- `@[simp] theorem insn_N` — instruction fetch cache (`progAt N = some (...)` via `native_decide`)

### Aristotle (Harmonic) — Long-Running Sorry-Filling

For hard sub-goals that Leanstral cannot crack, Aristotle provides agentic proof search (minutes to hours):

```bash
# Submit a Lean project and wait for completion
qedgen aristotle submit \
  --project-dir formal_verification \
  --wait

# Submit without waiting (returns project ID)
qedgen aristotle submit --project-dir formal_verification

# Check status (single shot)
qedgen aristotle status <project-id>

# Poll until done, then auto-download result
qedgen aristotle status <project-id> \
  --wait \
  --output-dir formal_verification

# Download result manually when complete
qedgen aristotle result <project-id> --output-dir formal_verification

# List recent projects
qedgen aristotle list

# Cancel a running project
qedgen aristotle cancel <project-id>
```

`status --wait` is the recommended way to attach to a previously submitted project. It polls every 30s (override with `--poll-interval`), prints progress updates, and auto-downloads the result on completion.

**When to use which backend:**
- **Leanstral** (`fill-sorry`): Fast (seconds), good for straightforward goals. Try first.
- **Aristotle** (`aristotle submit`): Slow but powerful (minutes–hours). Use when Leanstral fails after multiple passes.

## Environment Variables

- `MISTRAL_API_KEY` - For `fill-sorry` and `generate` commands (only needed for Lean proof sorry-filling)
- `ARISTOTLE_API_KEY` - For `aristotle` commands (only needed for hard sub-goals; get at https://aristotle.harmonic.fun)
- `QEDGEN_VALIDATION_WORKSPACE` - Override validation workspace path (default: platform cache dir)

API keys and Lean toolchain are not needed for spec writing, validation, or code generation.

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
- Binary: `cargo build --release` outputs to `target/release/qedgen`. Always copy to `bin/qedgen` after building: `cp target/release/qedgen bin/qedgen`.
- The SKILL.md file defines the full proof-writing workflow that Claude follows.

## Pre-release checklist

Before cutting a new release or tag:

1. **Bump version** in `crates/qedgen/Cargo.toml` — `install.sh` derives its version from there
2. **`cargo fmt --check`** — matches the CI gate; `cargo test` does NOT run fmt, so this is an easy miss if skipped
3. **`cargo clippy -- -D warnings`** — matches the CI gate (plain `cargo clippy` is too lenient)
4. **`cargo test`** — all tests must pass
5. **`bash scripts/check-readme-drift.sh`** — CI runs this; catches undocumented CLI commands
6. **`bash scripts/check-lake-build.sh --strict`** — runs `lake build` in every `examples/*/formal_verification/` (rust + sBPF) and exits 1 on any failure. `--strict` also fails on missing `.lake/`/manifests (cold checkout); drop `--strict` for a non-release sanity check. v2.11.2 shipped two examples with broken `Spec.lean` because this gate didn't exist — earlier `qedgen check --regen-drift` and `cargo check` only verify the Rust scaffold, not Lean.
7. **Zero `sorry`** — `grep -r '\bsorry\b' examples/**/*.lean` must return nothing. v2.26 (Slice 4a) closes the v2.8 G3 carve-out for Tier-1/2 CPI theorems: those now apply `<Iface>.<handler>.ensures_axiom_<idx>` and no longer carry `by sorry`. Only Tier-0 callees (interfaces with no declared `ensures`) keep the `by sorry` shape — the P1 lint `cpi_no_callee_ensures` surfaces them at check time. Filter via `grep -rL "ensures @ \`" examples/**/*.lean | xargs grep '\bsorry\b'` to surface only unintended sorry; Tier-0 carve-outs still match the `ensures @ \`` marker.
8. **`qedgen check --frozen` against bundled examples** — every `examples/rust/*/qed.lock` must be current. Stale locks fail the frozen check. Run for each spec dir that has a `qed.toml`: `qedgen check --frozen --spec examples/rust/escrow-split/`.
8a. **`old(...)` preservation harnesses (v2.23+)** — for every bundled spec whose `property` body contains `old(...)` (`grep -rl '\bold(' examples --include='*.qedspec'`), regen and confirm `tests/proptest.rs` emits the binary signature (`fn <prop>(pre: &State, post: &State) -> bool`) and the per-handler harness captures `let pre = s.clone(); let mut post = s;` before the handler call. Pre-v2.23 this lowered to a structural tautology silently. Bundled coverage today: `examples/regressions/issue-8/pool.qedspec` is the canonical pre/post test corpus; `examples/rust/percolator/percolator.qedspec`'s `old(...)` is in `ensures` and goes through the transition-fn assume path, unchanged by v2.23.
9. **Doc/code drift sweep** — README, SKILL.md, CLAUDE.md, `references/`, `docs/prds/RELEASE-v<version>.md`, and module `//!` docstrings all have to match shipped reality. The `check-readme-drift.sh` script only covers top-level command coverage in README; everything else needs an explicit pass. Concretely:
   - Every `Subcommand` arm in `crates/qedgen/src/main.rs` has a section in `references/cli.md`, with every flag in its `#[arg]` set documented.
   - No `references/`, README, SKILL.md, or `docs/prds/RELEASE-v<version>.md` page references symbols / files / flags that no longer exist (`grep` for the names of just-removed modules, types, fns, CLI flags).
   - No mention in user-facing docs of features the release doesn't ship (the RELEASE notes are the worst offender — bring the "What's in" list in line with the actual shipped commits).
   - `feedback_no_anchor_v2_mentions.md` policy: no naming external sources (anchor-v2, quasar, named protocols like Marinade/Squads/Drift/Raydium/Jito) in SKILL.md, references/, RELEASE-v<version>.md, or `clap` help text. Internal-only (test fixtures, private comments) is fine.
   - `CLAUDE.md` and the lowercase `claude.md` mirror are byte-identical.
   - Module-level `//!` docstrings on files you touched in the release reflect current behavior — not the behavior pre-fix.
