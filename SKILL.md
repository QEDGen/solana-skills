---
name: qedgen
description: Formally verify programs by writing Lean 4 proofs. Trigger this skill whenever the user wants to formally verify code, generate Lean 4 proofs, prove properties about algorithms or smart contracts, verify invariants, convert program logic into formal specifications, or anything involving Lean 4 and formal verification. Also trigger when the user mentions "qedgen", "lean proof", "formal proof", "verify my code", "prove correctness", "formal verification", or wants mathematical guarantees about their implementation.
---

# QEDGen — Agent-Driven Formal Verification

You (Claude) are the proof engineer. You read the codebase, write Lean 4 models and proofs, iterate on compiler errors, and call Leanstral (Mistral's theorem prover) only for hard sub-goals you cannot fill yourself.

## Important: how to run qedgen

All `qedgen` commands in this document MUST be run via the wrapper script at `tools/qedgen` inside the skill directory (`~/.agents/skills/qedgen/tools/qedgen`). The wrapper auto-installs the binary on first use — downloading the correct platform binary from GitHub releases, or compiling from source as a fallback.

Set this once at the start and use it for every command:
```bash
QEDGEN="$HOME/.agents/skills/qedgen/tools/qedgen"
```

## Architecture

```
You (Claude)                          Leanstral (fast)        Aristotle (deep)
  ├── Read spec / source code           ├── Fill sorry          ├── Long-running agent
  ├── Write Lean 4 models               └── Suggest tactics     └── Hard sub-goals
  ├── Write theorem statements                                     (minutes–hours)
  ├── Write proof attempts
  ├── Run `lake build`, read errors
  └── Fix and iterate
```

## Step 1: Understand the program

Check for existing artifacts in this priority order:

1. **spec.md exists** → Read it. An existing spec captures the author's intent, state model, invariants, and operations. Extract security goals, state model, and formal properties. Skip the scoping quiz and go directly to Step 2.
2. **IDL exists** (`target/idl/<program>.json`) → Run `$QEDGEN spec --idl <path>` to generate a draft SPEC.md with TODO markers, then refine interactively.
3. **Neither exists** → Read the source code directly. Ask broader scoping questions.

## Step 2: Scope the verification

If no spec.md was found, run a short interactive quiz — one question at a time, with checkbox options derived from the program's structure. Ask about **functionality and risks**, not implementation details.

**Question 1: "What does this program need to guarantee above all else?"**
Options derived from the program's structure:
- Authorization / access control
- Tokens are never lost / correct routing
- One-shot safety / no replay
- Arithmetic safety / no overflow
- Conservation (e.g., vault >= total claims)
- All of the above

**Question 2: "Which scenario worries you most?"**
Generate concrete risk scenarios from the program.

**Question 3: "Does the program make any assumptions that aren't enforced on-chain?"**

Ask questions **one at a time**. Wait for the user's answer before presenting the next question.

## Step 3: Write SPEC.md

Write `formal_verification/SPEC.md` using normative language (MUST, MUST NOT, MAY). Structure:

```markdown
# <Program Name> Verification Spec v1.0

<1-2 sentences describing what the program does>

## 0. Security Goals
1. **<Goal name>**: <normative statement>

## 1. State Model
<State struct with field names, types, and comments>
<Lifecycle diagram if applicable>

## 2. Operations
### 2.1 <Operation name>
**Signers**: <who MUST sign>
**Preconditions**: <what MUST be true before>
**Effects**: <numbered steps>
**Postconditions**: <what MUST be true after>

## 3. Formal Properties
### 3.1 <Category>
**<property_id>**: For all <quantified variables>,
if <transition predicate> then <conclusion>.

## 4. Trust Boundary
<What is axiomatic and why>

## 5. Verification Results
| Property | Status | Proof |
|---|---|---|
| ... | **Open** | |
```

Present SPEC.md to the user and get confirmation before proceeding.

## Step 4: Set up the Lean project

```bash
$QEDGEN setup            # Ensure global Mathlib cache exists (first time: 15-45 min)
```

Create the project structure:

```
formal_verification/
  lakefile.lean          # require qedgenSupport from path/to/lean_solana
  lean-toolchain         # leanprover/lean4:v4.24.0
  Proofs.lean            # root import: import Proofs.AccessControl etc.
  Proofs/
    AccessControl.lean
    CpiCorrectness.lean
    Conservation.lean
    StateMachine.lean
    ArithmeticSafety.lean
```

## Step 5: Write Lean proofs

This is the core step. You write Lean 4 directly — models, transitions, theorems, and proofs.

### Modeling workflow

For each property in SPEC.md:

1. **Define the state** as a Lean structure (map fields from source/spec)
2. **Define the transition** as `Option StateType` (return `none` on precondition failure)
3. **State the theorem** matching the SPEC.md property
4. **Write the proof** using the patterns below
5. **Run `lake build`** and iterate on errors

### Support library API

After `import QEDGen.Solana` and `open QEDGen.Solana`:

**Types:**
- `Pubkey` (= Nat), `U64` (= Nat), `U8` (= Nat)
- `Account` — `{ key : Pubkey, authority : Pubkey, balance : Nat, writable : Bool }`
- `Lifecycle` — `open | closed` (with DecidableEq)
- `AccountMeta` — `{ pubkey : Pubkey, isSigner : Bool, isWritable : Bool }`
- `CpiInstruction` — `{ programId : Pubkey, accounts : List AccountMeta, data : List Nat }`

**Constants:**
- `SYSTEM_PROGRAM_ID`, `TOKEN_PROGRAM_ID`, `TOKEN_2022_PROGRAM_ID`, `ASSOCIATED_TOKEN_PROGRAM_ID`
- `MEMO_PROGRAM_ID`, `COMPUTE_BUDGET_PROGRAM_ID`, `STAKE_PROGRAM_ID`
- `DISC_TRANSFER`, `DISC_TRANSFER_CHECKED`, `DISC_MINT_TO`, `DISC_BURN`, `DISC_CLOSE_ACCOUNT`, etc.
- `DISC_SYS_CREATE_ACCOUNT`, `DISC_SYS_TRANSFER`, etc.
- `DISC_ATA_CREATE`, `DISC_ATA_CREATE_IDEMPOTENT`
- `U8_MAX`, `U16_MAX`, `U32_MAX`, `U64_MAX`, `U128_MAX`

**Functions:**
- `findByKey : List Account → Pubkey → Option Account`
- `findByAuthority : List Account → Pubkey → Option Account`
- `canWrite : Pubkey → Account → Prop`
- `targetsProgram : CpiInstruction → Pubkey → Prop`
- `accountAt : CpiInstruction → Nat → Pubkey → Bool → Bool → Prop`
- `hasDiscriminator : CpiInstruction → List Nat → Prop`
- `hasNAccounts : CpiInstruction → Nat → Prop`
- `cpiWellFormed : CpiInstruction → Prop`
- `closes : Lifecycle → Lifecycle → Prop`
- `valid_u64 : Nat → Prop` (and u8, u16, u32, u128)

**Key lemmas:**
- `closes_is_closed`, `closes_was_open`, `closed_irreversible`
- `valid_u64_preserved_by_zero`, `valid_u64_preserved_by_same`
- `find_map_update_other`, `find_map_update_same` (axioms for account list updates)

### Proof patterns

**Access control** — signer must match authority:
```lean
structure ProgramState where
  authority : Pubkey

def cancelTransition (s : ProgramState) (signer : Pubkey) : Option Unit :=
  if signer = s.authority then some () else none

theorem cancel_access_control (s : ProgramState) (signer : Pubkey)
    (h : cancelTransition s signer ≠ none) :
    signer = s.authority := by
  unfold cancelTransition at h
  split_ifs at h with h_eq
  · exact h_eq
  · contradiction
```

**CPI correctness** — program, accounts, discriminator match (pure `rfl`):
```lean
def cancel_build_cpi (ctx : CancelContext) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts := [
      ⟨ctx.escrow_token, false, true⟩,   -- source: writable
      ⟨ctx.dest, false, true⟩,            -- dest: writable
      ⟨ctx.authority, true, false⟩         -- authority: signer
    ]
  , data := [DISC_TRANSFER]
  }

theorem cancel_cpi_correct (ctx : CancelContext) :
    let cpi := cancel_build_cpi ctx
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 ctx.escrow_token false true ∧
    accountAt cpi 1 ctx.dest false true ∧
    accountAt cpi 2 ctx.authority true false ∧
    hasDiscriminator cpi [DISC_TRANSFER] := by
  unfold cancel_build_cpi targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩
```

**State machine** — lifecycle transitions:
```lean
def cancelTransition (s : ProgramState) : Option ProgramState :=
  if s.escrow.lifecycle = Lifecycle.open then
    some { escrow := { s.escrow with lifecycle := Lifecycle.closed } }
  else none

theorem cancel_closes_escrow (pre post : ProgramState)
    (h : cancelTransition pre = some post) :
    post.escrow.lifecycle = Lifecycle.closed := by
  unfold cancelTransition at h
  split_ifs at h with h_open
  cases h
  rfl
```

**Conservation** — invariant preserved across operations:
```lean
def conservation (s : EngineState) : Prop := s.V >= s.C_tot + s.I

def depositTransition (s : EngineState) (amount : Nat) : Option EngineState :=
  if s.V + amount <= MAX_VAULT_TVL then
    some { V := s.V + amount, C_tot := s.C_tot + amount, I := s.I }
  else none

theorem deposit_conservation (s s' : EngineState) (amount : Nat)
    (h_inv : conservation s)
    (h : depositTransition s amount = some s') :
    conservation s' := by
  unfold depositTransition at h
  split_ifs at h with h_le
  · cases h
    unfold conservation at h_inv ⊢  -- MUST unfold in BOTH hypothesis and goal
    omega
  · contradiction
```

**Arithmetic safety** — bounds preserved:
```lean
def initializeTransition (amount taker : Nat) : Option ProgramState :=
  if amount > 0 ∧ amount ≤ U64_MAX ∧ taker > 0 ∧ taker ≤ U64_MAX then
    some { initializer_amount := amount, taker_amount := taker }
  else none

theorem initialize_arithmetic_safety (amount taker : Nat) (post : ProgramState)
    (h : initializeTransition amount taker = some post) :
    post.initializer_amount ≤ U64_MAX ∧ post.taker_amount ≤ U64_MAX := by
  unfold initializeTransition at h
  split_ifs at h with h_bounds
  cases h
  exact ⟨h_bounds.2.1, h_bounds.2.2.2⟩
```

### Critical tactic rules

| Do | Don't |
|---|---|
| `unfold f at h` before `split_ifs` | `simp [f] at h` before `split_ifs` (kills if-structure) |
| `unfold pred at h_inv ⊢` for named predicates | `unfold pred` only in goal (omega can't see hypotheses) |
| `cases h` after `split_ifs` on `some = some` | `injection h` (unnecessary, cases handles it) |
| `omega` for linear arithmetic | `norm_num` for linear goals (omega is more reliable) |
| `exact ⟨rfl, rfl, rfl⟩` for conjunctions of rfl | `constructor` + `rfl` + `constructor` + `rfl` (verbose) |
| `if cond then ... else ...` without proof binding | `if h : cond then ...` when `h` is unused |
| `simp [wrapAdd, toU64, ...] at h` to normalize hypotheses to match step-execution goals | `simp only [...]` for this (misses modular identities and numeric evaluation that step-level simp applied) |

### Common errors and fixes

| Error | Fix |
|---|---|
| `omega could not prove the goal` | Unfold named predicates in hypotheses: `unfold pred at h ⊢` |
| `no goals to be solved` | Remove redundant tactic (e.g., `· contradiction` after auto-closed branch) |
| `unknown constant 'X'` | Check imports; add `import QEDGen.Solana.X` or `open QEDGen.Solana` |
| `tactic 'split_ifs' failed, no if-then-else` | Use `unfold` first, not `simp` |
| `unused variable 'h'` | Remove proof binding: `if h : cond` → `if cond` |
| `omega` fails on address disjointness after stack writes | Normalize hypotheses with `simp [wrapAdd, toU64, ...]` (not `simp only`) so address forms match the goal — see "Memory disjointness through stack writes" |

## sBPF Assembly Verification

The same workflow applies to hand-written sBPF assembly programs. Claude reads the `.s` source (and IDL if available), writes SPEC.md, then writes Lean proofs using the SBPF support library.

### Reading assembly source

sBPF assembly uses AT&T-like syntax. Reference for understanding the source (transpilation is automated by `asm2lean`):

| Assembly | Lean encoding | Meaning |
|---|---|---|
| `ldxdw r3, [r1+0x2918]` | `.ldx .dword .r3 .r1 0x2918` | Load 8 bytes from mem[r1+offset] into r3 |
| `ldxb r2, [r1+OFF]` | `.ldx .byte .r2 .r1 OFF` | Load 1 byte from mem[r1+offset] into r2 |
| `lddw r0, 1` | `.lddw .r0 1` | Load 64-bit immediate into r0 |
| `jge r3, r4, label` | `.jge .r3 (.reg .r4) <abs_idx>` | Branch if r3 >= r4 |
| `jne r2, 3, label` | `.jne .r2 (.imm 3) <abs_idx>` | Branch if r2 != 3 |
| `add64 r2, 8` | `.add64 .r2 (.imm 8)` | r2 = r2 + 8 (wrapping) |
| `mov64 r0, 1` | `.mov64 .r0 (.imm 1)` | r0 = 1 |
| `call sol_log_` | `.call .sol_log_` | Invoke syscall |
| `exit` | `.exit` | Exit with code in r0 |

**Jump target resolution**: Assembly uses labels; Lean uses absolute instruction indices (0-based). `asm2lean` resolves these automatically.

**`.equ` constants**: Map to `abbrev` definitions. Constants used in memory operands (`[reg + CONST]`) are typed `Int`; all others are `Nat`.

### Modeling the program

Use `qedgen asm2lean` to transpile the `.s` file into a Lean 4 module automatically:

```bash
$QEDGEN asm2lean --input src/program.s --output formal_verification/ProgramProg.lean
```

This generates a module with:
- `abbrev` definitions for all `.equ` constants (offsets as `Int`, values as `Nat`)
- `@[simp] def prog : Program := #[...]` with named constants and index comments
- For large programs (>64 instructions): `def progAt : Nat → Option Insn` — a chunked function-based lookup for O(1) simp performance. Use with `executeFn` and `wp_exec`.
- A namespace wrapper matching the output filename

Add the generated module to `lakefile.lean`:
```lean
lean_lib ProgramProg where
  roots := #[`ProgramProg]
```

Then import it in the proof file:
```lean
import QEDGen.Solana.SBPF
import ProgramProg

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory
open ProgramProg
```

**Never transcribe assembly by hand** — `asm2lean` handles jump target resolution, constant typing, and syntactic matching automatically.

### SBPF support library API

After `import QEDGen.Solana.SBPF` and `open QEDGen.Solana.SBPF`:

**Types:**
- `Reg` — `.r0` through `.r10` (r10 is read-only frame pointer)
- `Src` — `.reg r` or `.imm v`
- `Width` — `.byte` (1), `.half` (2), `.word` (4), `.dword` (8)
- `Syscall` — `.sol_log_`, `.sol_invoke_signed`, `.sol_get_clock_sysvar`, etc.
- `Insn` — All sBPF instructions (`.lddw`, `.ldx`, `.st`, `.stx`, `.add64`, `.jge`, `.call`, `.exit`, etc.)
- `Program` — `Array Insn`

**State types:**
- `RegFile` — struct with fields `r0..r10 : Nat` (all default 0). `@[simp]` on `get`/`set`. Writes to r10 are silently ignored (`set_r10`).
- `State` — `{ regs : RegFile, mem : Mem, pc : Nat, exitCode : Option Nat }`
- `Mem` — `Nat → Nat` (byte-addressable memory)

**Functions (all `@[simp]`):**
- `RegFile.get (rf : RegFile) : Reg → Nat`
- `RegFile.set (rf : RegFile) (r : Reg) (v : Nat) : RegFile` — r10 writes are silently ignored
- `resolveSrc (rf : RegFile) (src : Src) : Nat`
- `step (insn : Insn) (s : State) : State` — single-instruction semantics
- `execSyscall (sc : Syscall) (s : State) : State` — logging sets r0=0
- `initState (inputAddr : Nat) (mem : Mem) : State` — r1=inputAddr, r10=stack, pc=0
- `initState2 (inputAddr insnAddr : Nat) (mem : Mem) (entryPc : Nat := 0) : State` — two-pointer state for SIMD-0321 programs (r1=input buffer, r2=instruction data); `entryPc` supports non-zero entry points
- `wrapAdd`, `wrapSub`, `wrapMul`, `wrapNeg` — 64-bit wrapping arithmetic

**Execution:**
- `execute (prog : Program) (s : State) (fuel : Nat) : State` — array-based fetch. Best for small programs (≤64 instructions).
- `executeFn (fetch : Nat → Option Insn) (s : State) (fuel : Nat) : State` — function-based fetch (O(1) per step). Use with `progAt` for large programs (>64 instructions) where array indexing causes simp blowup. Use `wp_exec` tactic to prove properties.

**Memory functions (open `QEDGen.Solana.SBPF.Memory`):**
- `effectiveAddr (base : Nat) (off : Int) : Nat`
- `readU8`, `readU16`, `readU32`, `readU64` — little-endian reads
- `writeU8`, `writeU16`, `writeU32`, `writeU64` — little-endian writes
- `readByWidth`, `writeByWidth` — dispatch by `Width`

**Memory constants:**
- `RODATA_START`, `BYTECODE_START`, `STACK_START`, `HEAP_START`, `INPUT_START`

**Tactics:**
- `wp_exec [fetch_defs] [simp_extras]` — one-shot tactic for sBPF proofs. First bracket lists fetch function + chunk defs (passed to `dsimp` for instruction decode). Second bracket lists `effectiveAddr` lemmas and extras (passed to `simp` for branch resolution). Uses the monadic WP bridge for O(1) kernel depth per step.
- `wp_step [fetch_defs] [simp_extras]` — single instruction step (same arguments as `wp_exec`). Use when `wp_exec` needs manual guidance. **Requires** `rw [executeFn_eq_execSegment]` first and `rfl` at the end.
- `strip_writes` — strips nested write layers from read expressions via disjointness (omega). Pre-unfolds STACK_START.
- `strip_writes_goal` — like `strip_writes` but only unfolds STACK_START in the goal (for large contexts).
- `rewrite_mem [hmem]` — rewrites with memory hypotheses then applies region frame reasoning.
- `solve_read [hmem] h_val` — `rewrite_mem` + `exact h_val` (one-shot memory read resolution).
- `mem_frame` — automatic region-based write stripping. Handles cross-width reads/writes, within-stack disjointness, and same-address round-trips.

**Lemmas:**
- `execute_halted` (`@[simp]`) — halted state is a fixed point
- `execute_step` — unfolds one step: `execute prog s (n+1) = execute prog (step insn s) n`
- `execute_zero` (`@[simp]`) — `execute prog s 0 = s`
- `executeFn_halted` (`@[simp]`) — halted state is a fixed point (function-based)
- `executeFn_step` — unfolds one step: `executeFn fetch s (n+1) = executeFn fetch (step insn s) n`
- `executeFn_zero` (`@[simp]`) — `executeFn fetch s 0 = s`
- `executeFn_compose` — composability: `executeFn fetch s (n+m) = executeFn fetch (executeFn fetch s n) m`

**Register lemmas:**
- `RegFile.set_r10` (`@[simp]`) — writing to r10 is a no-op
- `RegFile.get_set_self` — `(rf.set r v).get r = v` (when `r ≠ .r10`)
- `RegFile.get_set_diff` — `(rf.set r2 v).get r1 = rf.get r1` (when `r1 ≠ r2`)

**r10 invariance (all `@[simp]`):**
- `RegFile.set_preserves_r10` — `(rf.set r v).r10 = rf.r10` for any register
- `execSyscall_preserves_r10` — syscalls preserve r10
- `step_preserves_r10` — single instruction preserves r10
- `executeFn_preserves_r10` — full execution preserves r10
- `executeFn_r10_initState` — `(executeFn fetch (initState ...) n).regs.r10 = STACK_START + 0x1000`
- `executeFn_r10_initState2` — same for `initState2`

No need to thread r10 hypotheses through sub-lemmas — use `(by simp [h_r10])` or `(by simp)` at call sites.

**Memory axioms — same-address round-trip:**
- `readU64_writeU64_same`, `readU32_writeU32_same`, `readU8_writeU8_same`

**Memory axioms — disjoint-address (within same region):**
- `readU64_writeU64_disjoint`, `readU64_writeU32_disjoint`, `readU64_writeU16_disjoint`, `readU64_writeU8_disjoint`
- `readU32_writeU64_disjoint`, `readU32_writeU32_disjoint`
- `readU8_writeU64_outside`, `readU8_writeU32_outside`, `readU8_writeU16_outside`, `readU8_writeU8_disjoint`

**Memory axioms — region frame (read below STACK_START, write above):**
- `readU64_writeU64_frame`, `readU64_writeU32_frame`, `readU64_writeU16_frame`, `readU64_writeU8_frame`
- `readU32_writeU64_frame`, `readU32_writeU32_frame`
- `readU8_writeU64_frame`, `readU8_writeU32_frame`, `readU8_writeU16_frame`, `readU8_writeU8_frame`

**Region helpers (`open QEDGen.Solana.SBPF.Region`):**
- `writeU64Chain mem writes` — applies a list of `(addr, val)` U64 writes to memory
- `readU64_writeU64Chain_frame`, `readU32_writeU64Chain_frame`, `readU8_writeU64Chain_frame` — reads from input region survive a chain of stack writes
- `belowStack base bound` — `base + bound ≤ STACK_START`

**Pubkey helpers (`import QEDGen.Solana.SBPF.Pubkey`):**
- `Pubkey4` — 32-byte pubkey as four U64 chunks (`.c0`, `.c1`, `.c2`, `.c3`)
- `Pubkey4.ne_iff` — two pubkeys differ iff at least one chunk differs
- `pubkeyAt mem base pk` — the four chunks reside at `base`, `base+8`, `base+16`, `base+24`
- `pubkeyAt_of_mem_eq` — memory equality preserves pubkeyAt
- `pubkeyAt_writeU64_disjoint` — survives a disjoint U64 write
- `pubkeyAt_writeU64_frame` — survives a stack write (input-region pubkey)
- `pubkeyAt_writeU64Chain_frame` — survives a chain of stack writes

**SbpfMem (optional region-typed wrapper, `open QEDGen.Solana.SBPF.Region`):**
- `SbpfMem.ofMem mem base bound h_sep` — wrap raw memory with region proof
- `SbpfMem.readInput`, `readInputU32`, `readInputU8` — region-typed reads
- `SbpfMem.writeStack`, `writeStackU32`, `writeStackU8` — region-typed writes
- `readInput_writeStack`, `readInputU8_writeStack` — frame theorems
- `readInput_writeStack_chain` — chain frame theorem

**Reusable instruction patterns** (`import QEDGen.Solana.SBPF.Patterns`):

Pre-proven theorems for common 2-3 instruction sequences. Parameterized over `fetch`, registers, offsets, and branch targets. Register disjointness hypotheses are dischargeable by `decide` at call sites.

| Pattern | Steps | Sequence | Conclusion |
|---|---|---|---|
| `error_exit` | 2 | `mov32 r0 code; exit` | `exitCode = some (toU64 code % U32_MODULUS)` |
| `dup_pass` | 2 | `ldx byte; jne (fall)` | `exitCode = none ∧ pc = pc+2 ∧ mem preserved ∧ ∀ r ≠ dst, reg preserved` |
| `dup_fail` | 2 | `ldx byte; jne (taken)` | `exitCode = none ∧ pc = target ∧ mem preserved ∧ ∀ r ≠ dst, reg preserved` |
| `chunk_eq_mem` | 3 | `ldx; ldx; jne (fall)` | `exitCode = none ∧ pc = pc+3 ∧ mem preserved ∧ ∀ r ≠ dstA,dstB, reg preserved` |
| `chunk_ne_mem` | 3 | `ldx; ldx; jne (taken)` | `exitCode = none ∧ pc = target ∧ mem preserved` |
| `chunk_eq_imm` | 3 | `ldx; lddw; jne (fall)` | same as `chunk_eq_mem` (vs 64-bit immediate) |
| `chunk_ne_imm` | 3 | `ldx; lddw; jne (taken)` | same as `chunk_ne_mem` (vs 64-bit immediate) |
| `chunk_eq_imm32` | 3 | `ldx; mov32; jne (fall)` | same as `chunk_eq_mem` (vs 32-bit immediate) |
| `chunk_ne_imm32` | 3 | `ldx; mov32; jne (taken)` | same as `chunk_ne_mem` (vs 32-bit immediate) |
| `chunk_ne_mem_error` | 5 | chunk mismatch → error exit | `exitCode = some (toU64 errorCode % U32_MODULUS)` |

**Usage example** — comparing a pubkey chunk against a known value:
```lean
-- Bridge hypotheses to library form (s.regs.get .r9 → sysAddr)
have h_eq' : readU64 s.mem (effectiveAddr (s.regs.get .r9) off1) =
             readU64 s.mem (effectiveAddr (s.regs.get .r10) off2) := by
  simp only [RegFile.get, h_r9, h_r10]; exact h_eq
-- Call library pattern
obtain ⟨he, hp, hm, hreg⟩ := chunk_eq_mem progAt s .r7 .r8 .r9 .r10 off1 off2 target
  (by decide) (by decide) (by decide) (by decide)  -- register disjointness
  h_exit h_f1 h_f2 h_f3 h_eq'
-- Extract specific register preservation from ∀
have hr9 := hreg .r9 (by decide) (by decide)
have hr10 := hreg .r10 (by decide) (by decide)
```

For composed patterns (e.g., chunk mismatch → error), `chunk_ne_mem_error` handles the full 5-step sequence in one call, combining `executeFn_compose` and `executeFn_halted` internally.

### Proof strategy: `wp_exec` tactic

Use the `wp_exec` tactic to prove sBPF execution properties in a single call. It handles instruction fetch (via `dsimp` kernel reduction), branch resolution (via `simp` with hypotheses), and halted-state closure (via `rfl`). Each step is O(1) kernel depth.

```lean
set_option maxHeartbeats 800000 in
theorem rejects_wrong_account_count
    (inputAddr : Nat) (mem : Mem)
    (numAccounts : Nat)
    (h_num : readU64 mem inputAddr = numAccounts)
    (h_ne2 : numAccounts ≠ N_ACCOUNTS_INCREMENT)
    (h_ne3 : numAccounts ≠ N_ACCOUNTS_INIT) :
    (executeFn progAt (initState inputAddr mem) 8).exitCode = some E_N_ACCOUNTS := by
  have h1 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INCREMENT) := by rw [h_num]; exact h_ne2
  have h2 : ¬(readU64 mem inputAddr = N_ACCOUNTS_INIT) := by rw [h_num]; exact h_ne3
  wp_exec [progAt, progAt_0, progAt_1] [ea_0]
```

**Arguments:**
- First bracket `[progAt, progAt_0, progAt_1]`: fetch function + chunk defs, passed to `dsimp` for instruction decode via kernel reduction. Include all `progAt_N` chunk functions generated by `asm2lean`.
- Second bracket `[ea_0]`: `effectiveAddr` lemmas and extras, passed to `simp` for branch resolution. Include `U32_MODULUS` if the path has `mov32` instructions.

Note the use of `initState2` for programs that take two input pointers (r1=input buffer, r2=instruction data) and a non-zero `entryPc` when the program entrypoint isn't at instruction 0.

**When `wp_exec` needs manual guidance on complex paths** (e.g., memory disjointness lemmas between steps), use `wp_step` to advance one instruction at a time:

```lean
rw [executeFn_eq_execSegment]
wp_step [progAt, progAt_0, progAt_1] [ea_0, ea_88]
-- apply memory disjointness lemma here
rw [readU8_writeU64_outside _ _ _ _ (by ...)]
wp_step [progAt, progAt_0, progAt_1] [ea_0, ea_88]
...
rfl
```

Note: when using `wp_step`, you must manually call `rw [executeFn_eq_execSegment]` first and close with `rfl` at the end.

`asm2lean` auto-generates the following boilerplate in the program module:

- **`@[simp] theorem ea_NAME`** — effectiveAddr lemmas for each offset symbol, proving `effectiveAddr b NAME = b ± val`
- **`@[simp] theorem bridge_NAME`** — toU64 bridge lemmas for Nat constants used in `lddw` instructions, proving `toU64 (↑NAME : Int) = NAME`
- **`@[simp] theorem insn_N`** — instruction fetch cache for each PC, proving `progAt N = some (...)` via `native_decide`

These are all `@[simp]` so they fire automatically during `wp_exec`/`wp_step`. Proofs should **not** hand-write their own `ea_`, `bridge_`, or `have hfN : progAt N = ...` boilerplate — use the auto-generated versions.

**Prerequisites for `wp_exec` to work efficiently:**
1. `prog` must have `@[simp]` attribute (handled by `asm2lean`)
2. Offset constants in `prog` must be `Int`, not `Nat` (handled by `asm2lean`)
3. Named constants in `prog` must syntactically match hypothesis names (handled by `asm2lean`)
4. `progAt_0`, `progAt_1`, etc. must NOT be `private` (handled by `asm2lean`)

For theorems with negated conditions, introduce helpers before `wp_exec`:
```lean
-- When the hypothesis is ≠ but simp needs ¬(... = ...)
have h_ne3 : ¬(readU64 mem inputAddr = N_ACCOUNTS_EXPECTED) := by rw [h_num]; exact h_ne
wp_exec [progAt, progAt_0, progAt_1] [ea_0]

-- When the hypothesis is ≥ but simp needs ¬(... < ...)
have h_not_lt : ¬(senderLamports < amount) := by omega
wp_exec [progAt, progAt_0, progAt_1] [ea_0]
```

Set `maxHeartbeats` to 800000 for typical paths (3-8 instructions), higher for longer paths.

### Using library patterns vs wp_exec

**Use `wp_exec`** for end-to-end proofs of simple linear paths (3-15 instructions) where the full execution can be discharged in one tactic call.

**Use library patterns** for structured programs with recurring instruction sequences. Common in SIMD-0321 programs that validate multiple accounts with the same check pattern:

1. **Split with `executeFn_compose`** into phases (prefix + per-account checks)
2. **Call library patterns** for each 2-3 instruction sequence (dup check, chunk comparison)
3. **Chain results** via `obtain` destructuring

This avoids repeating `simp [RegFile.get, RegFile.set, ...]` in every helper and makes the proof structure match the program structure. The dropset proofs demonstrate this: 11 helpers rewritten from inline simp to library pattern calls.

### Memory disjointness through stack writes

When an sBPF program writes to the stack (`stx` instructions) then later reads from the input buffer, the proof must show reads see the original memory. Use the memory axioms `readU64_writeU64_disjoint` and `readU8_writeU64_outside` with omega proofs for spatial separation.

**Phase-based proof structure**: For complex paths (20+ steps) with memory mutations, organize the proof into phases:
1. **Common validation prefix** — shared steps (discriminant check, account count, etc.) that `wp_exec` or `wp_step` handles
2. **Pointer arithmetic / memory writes** — instructions that compute dynamic addresses and write to the stack. After stepping, introduce bound hypotheses for dynamically-computed addresses
3. **Property-specific check** — the final read-and-branch that establishes the theorem. Apply memory disjointness axioms at each read-through-write

**Stack-input separation hypothesis**: Add a hypothesis establishing spatial separation between the stack region and the input buffer:

```lean
theorem rejects_quote_mint_duplicate
    (inputAddr : Nat) (mem : Mem) ...
    (h_sep : STACK_START + 0x1000 > inputAddr + 100000) -- stack-input separation
    ...
```

This gives omega the fact it needs to prove disjointness side conditions.

**Reading through stack writes**: When the program wrote to the stack (e.g., `stx .dword [r10-N]`) and then reads from the input buffer, apply the appropriate axiom:

```lean
-- Byte read through a dword stack write
rw [readU8_writeU64_outside _ _ _ _
  (by left; unfold STACK_START at h_addr ⊢; omega)]
-- Dword read through a dword stack write
rw [readU64_writeU64_disjoint _ _ _ _ _
  (by unfold STACK_START at h_addr ⊢; omega)]
```

Chain multiple rewrites when multiple stack writes precede the read.

**`simp` vs `simp only` for hypothesis normalization (critical)**: After stepping through instructions that use `wrapAdd`/`toU64` (wrapping add, `and64`), the goal's address expressions get normalized by the step-level `simp` — which applies `@[simp]` lemmas including the modular identity `(a % m + b) % m = (a + b) % m` and evaluates `(↑7 % 2^64).toNat → 7`. But hypotheses introduced earlier (like `h_addr`) remain in their original `wrapAdd`/`toU64` form. To make omega see them as equal, normalize hypotheses with **`simp`** (not `simp only`):

```lean
-- GOOD: includes @[simp] lemmas, matches what step-level simp did to the goal
simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_addr h_dup'

-- BAD: misses modular identities and numeric evaluation — omega sees different free variables
simp only [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_addr h_dup'
```

**Bound hypotheses for dynamic addresses**: When instructions compute addresses dynamically (e.g., `add64` + `and64` for alignment), introduce a bound hypothesis after the computation phase:

```lean
-- After stepping through add64 + and64 that computed the aligned offset in r9:
-- The address stored in r9 is bounded by the data length computation
(h_addr : (baseDataLen + DATA_LEN_MAX_PAD) &&& toU64 DATA_LEN_AND_MASK + inputAddr < STACK_START)
```

This lets omega prove the read address is below the stack region.

### Theorem statement pattern

Properties are stated over symbolic memory with hypotheses binding memory reads. Use the named constants from the generated `Prog` module — both in hypotheses and exit codes:

```lean
theorem rejects_insufficient_lamports
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_sdl   : readU64 mem (effectiveAddr inputAddr SENDER_DATA_LENGTH_OFFSET) = DATA_LENGTH_ZERO)
    (h_rdup  : readU8  mem (effectiveAddr inputAddr RECIPIENT_OFFSET) = NON_DUP_MARKER)
    (h_rdl   : readU64 mem (effectiveAddr inputAddr RECIPIENT_DATA_LENGTH_OFFSET) = DATA_LENGTH_ZERO)
    (h_sdup  : readU8  mem (effectiveAddr inputAddr SYSTEM_PROGRAM_OFFSET) = NON_DUP_MARKER)
    (h_idl   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_LENGTH_OFFSET) = INSTRUCTION_DATA_LENGTH_EXPECTED)
    (h_amt   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_OFFSET) = amount)
    (h_bal   : readU64 mem (effectiveAddr inputAddr SENDER_LAMPORTS_OFFSET) = senderLamports)
    (h_insuf : senderLamports < amount) :
    (executeFn progAt (initState inputAddr mem) 20).exitCode = some E_INSUFFICIENT_LAMPORTS := by
  have h_not_ge : ¬(senderLamports ≥ amount) := by omega
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_88]
```

The `fuel` parameter (20 above) must be large enough for the longest execution path. Count the maximum instructions from entry to exit.

**Critical**: Use `readU8` for byte loads (`ldxb`) and `readU64` for dword loads (`ldxdw`). The read width must match the assembly instruction.

### Critical rules for sBPF proofs

| Do | Don't |
|---|---|
| Use `wp_exec [progAt, progAt_0, ...] [ea_lemmas]` for proofs | Manually unroll unless `wp_exec` needs per-step guidance |
| Generate `Prog.lean` with `qedgen asm2lean` | Hand-transcribe assembly into Lean (wrong offsets, missing labels) |
| Use named constants from `Prog.lean` in hypotheses | Use raw numeric literals in hypotheses (simp blowup) |
| Use `Int` for offset constants (memory operands) | Use `Nat` for offsets (forces coercion, simp timeout) |
| Ensure `@[simp]` on `prog` definition | Omit `@[simp]` (wp_exec cannot unfold prog) |
| Set `maxHeartbeats` 800000+ for sBPF proofs | Use default heartbeats (will timeout on 8+ instruction paths) |

### sBPF simp performance (critical)

Three rules that determine whether `wp_exec` completes in seconds or times out:

1. **Offset constants MUST be `Int`**: `effectiveAddr` takes `(off : Int)`. If the constant is `Nat`, Lean inserts `↑(NAT_CONST)` coercion at every use, and `simp` cannot efficiently reduce `effectiveAddr base ↑N = effectiveAddr base N`. This alone causes 0.5s → 4+ minute blowup.

2. **Named constants in `prog` MUST match hypothesis names**: `simp` uses syntactic matching. If `prog` has `.ldx .dword .r2 .r1 SENDER_DATA_LENGTH_OFFSET` but the hypothesis uses the raw number `88`, `simp` must unfold every `abbrev` at every subterm at every step — exponential blowup.

3. **`@[simp]` on `prog` is required**: Without it, `wp_exec` cannot unfold `prog[pc]?` to fetch instructions.

`qedgen asm2lean` handles all three rules automatically.

## Step 6: Call Leanstral for hard sub-goals

When you have a proof with `sorry` markers you cannot fill after 2-3 attempts:

```bash
$QEDGEN fill-sorry --file formal_verification/Proofs/Hard.lean --validate
```

This sends each `sorry` location to Leanstral with focused context. Review the result — Leanstral may introduce tactics you can learn from for future proofs.

If `fill-sorry` fails after multiple passes, escalate to Aristotle (Harmonic's long-running theorem prover). Submit the **entire project directory** so Aristotle has full context.

**Option A — Submit and wait inline** (blocks until done):

```bash
$QEDGEN aristotle submit --project-dir formal_verification --wait
```

**Option B — Submit, detach, poll later** (recommended for long queues):

```bash
# Submit (returns project ID immediately)
$QEDGEN aristotle submit --project-dir formal_verification

# Later: attach and poll until completion, auto-download result
$QEDGEN aristotle status <project-id> \
  --wait \
  --output-dir formal_verification \
  --poll-interval 60
```

`status --wait` polls periodically (default 30s), prints progress updates, and auto-downloads the solved project when it reaches a terminal state. Without `--wait`, `status` is a single-shot check.

Aristotle may run for minutes to hours. It returns the full project with sorry markers replaced. Review its output — it overwrites files in place, so verify with `lake build` afterward.

If Aristotle also fails, simplify the theorem statement or split the property into smaller lemmas.

## Step 7: Verify and report

```bash
cd formal_verification && lake build
```

Update SPEC.md verification results table:
- **Verified**: Theorem compiles, no `sorry`
- **Partial**: Proof has `sorry` markers
- **Open**: No compiling proof

## Environment

- **`MISTRAL_API_KEY`** — required for `fill-sorry`. Free from [console.mistral.ai](https://console.mistral.ai)
- **`ARISTOTLE_API_KEY`** — required for `aristotle` commands. Get at [aristotle.harmonic.fun](https://aristotle.harmonic.fun)
- **`QEDGEN_VALIDATION_WORKSPACE`** — optional override for global Mathlib cache location

## Error handling

- **First `lake build` is slow**: Mathlib compilation takes 15-45 min on first run. Subsequent builds reuse the cache.
- **`could not resolve 'HEAD' to a commit`**: Remove `.lake/packages/mathlib` and run `lake update`.
- **Rate limiting (429)**: Built-in exponential backoff in `fill-sorry`.
