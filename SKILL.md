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
You (Claude)                          Leanstral (remote model)
  ├── Read spec / source code           ├── Fill sorry markers
  ├── Write Lean 4 models               └── Suggest tactics for hard goals
  ├── Write theorem statements
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
  lakefile.lean          # import lean_support and Mathlib
  lean-toolchain         # leanprover/lean4:v4.15.0
  lean_support/          # Solana axiom library (copy from qedgen)
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

### Common errors and fixes

| Error | Fix |
|---|---|
| `omega could not prove the goal` | Unfold named predicates in hypotheses: `unfold pred at h ⊢` |
| `no goals to be solved` | Remove redundant tactic (e.g., `· contradiction` after auto-closed branch) |
| `unknown constant 'X'` | Check imports; add `import QEDGen.Solana.X` or `open QEDGen.Solana` |
| `tactic 'split_ifs' failed, no if-then-else` | Use `unfold` first, not `simp` |
| `unused variable 'h'` | Remove proof binding: `if h : cond` → `if cond` |

## sBPF Assembly Verification

The same workflow applies to hand-written sBPF assembly programs. Claude reads the `.s` source (and IDL if available), writes SPEC.md, then writes Lean proofs using the SBPF support library.

### Reading assembly source

sBPF assembly uses AT&T-like syntax. Key patterns to recognize:

| Assembly | Lean encoding | Meaning |
|---|---|---|
| `ldxdw r3, [r1+0x2918]` | `.ldx .dword .r3 .r1 0x2918` | Load 8 bytes from mem[r1+offset] into r3 |
| `lddw r0, 1` | `.lddw .r0 1` | Load 64-bit immediate into r0 |
| `jge r3, r4, label` | `.jge .r3 (.reg .r4) <abs_idx>` | Branch if r3 >= r4 |
| `add64 r2, 8` | `.add64 .r2 (.imm 8)` | r2 = r2 + 8 (wrapping) |
| `call sol_log_` | `.call .sol_log_` | Invoke syscall |
| `exit` | `.exit` | Exit with code in r0 |

**Jump target resolution**: Assembly uses labels; Lean uses absolute instruction indices (0-based). Count instructions from `.globl entrypoint` to determine the index for each label.

**`.equ` constants**: Map directly to the offset values used in `ldx`/`stx` instructions.

### Modeling the program

Transcribe the assembly into a `Program := #[...]` array:

```lean
import QEDGen.Solana.SBPF

open QEDGen.Solana.SBPF
open QEDGen.Solana.SBPF.Memory

def prog : Program := #[
  .ldx .dword .r3 .r1 0x2918,   -- 0: r3 = mem[r1 + 0x2918]
  .ldx .dword .r4 .r1 0x00a0,   -- 1: r4 = mem[r1 + 0x00a0]
  .jge .r3 (.reg .r4) 4,        -- 2: if r3 >= r4 jump to 4
  .exit,                          -- 3: success (r0 = 0)
  .lddw .r0 1,                   -- 4: set error code
  .exit                           -- 5: error exit
]
```

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
- `RegFile` — struct with fields `r0..r10 : Nat` (all default 0). `@[simp]` on `get`/`set`.
- `State` — `{ regs : RegFile, mem : Mem, pc : Nat, exitCode : Option Nat }`
- `Mem` — `Nat → Nat` (byte-addressable memory)

**Functions (all `@[simp]`):**
- `RegFile.get (rf : RegFile) : Reg → Nat`
- `RegFile.set (rf : RegFile) (r : Reg) (v : Nat) : RegFile` — r10 writes are silently ignored
- `resolveSrc (rf : RegFile) (src : Src) : Nat`
- `step (insn : Insn) (s : State) : State` — single-instruction semantics
- `execSyscall (sc : Syscall) (s : State) : State` — logging sets r0=0
- `initState (inputAddr : Nat) (mem : Mem) : State` — r1=inputAddr, r10=stack, pc=0
- `wrapAdd`, `wrapSub`, `wrapMul`, `wrapNeg` — 64-bit wrapping arithmetic

**Execution (NOT `@[simp]` — must be unrolled):**
- `execute (prog : Program) (s : State) (fuel : Nat) : State`

**Memory functions (open `QEDGen.Solana.SBPF.Memory`):**
- `effectiveAddr (base : Nat) (off : Int) : Nat`
- `readU8`, `readU16`, `readU32`, `readU64` — little-endian reads
- `writeU8`, `writeU16`, `writeU32`, `writeU64` — little-endian writes
- `readByWidth`, `writeByWidth` — dispatch by `Width`

**Memory constants:**
- `RODATA_START`, `BYTECODE_START`, `STACK_START`, `HEAP_START`, `INPUT_START`

**Lemmas:**
- `execute_halted` (`@[simp]`) — halted state is a fixed point
- `execute_step` — unfolds one step: `execute prog s (n+1) = execute prog (step insn s) n`
- `execute_zero` (`@[simp]`) — `execute prog s 0 = s`

**Memory axioms:**
- `readU64_writeU64_same` — read-after-write returns original value
- `readU64_writeU64_disjoint` — non-overlapping write doesn't affect read
- `readU8_writeU64_outside`, `readU64_writeU8_disjoint`

### Proof strategy: `execute_step` unrolling

**Do NOT** put `execute` in a simp set — it causes exponential term growth. Instead, unroll one step at a time with `execute_step`:

**Step 1**: Pre-compute fetch lemmas for every instruction index:
```lean
private theorem f0 : prog[0]? = some (.ldx .dword .r3 .r1 0x2918) := by native_decide
private theorem f1 : prog[1]? = some (.ldx .dword .r4 .r1 0x00a0) := by native_decide
-- etc.
```

**Step 2**: Normalize memory hypotheses early:
```lean
simp only [effectiveAddr] at h_min h_tok
```

**Step 3**: Unroll each step with inline precondition proofs:
```lean
-- Step 0: ldxdw r3 — PC:0→1
rw [show (10:Nat) = 9+1 from rfl, execute_step _ _ _ (.ldx .dword .r3 .r1 0x2918)
  (by rfl)                      -- proves exitCode = none
  (by simp [initState]; exact f0)]  -- proves prog[pc]? = some insn
```

For later steps where state is deeper, add more lemmas to the simp set:
```lean
-- Step 2: jge — branch resolves using h_slip hypothesis
rw [show (8:Nat) = 7+1 from rfl, execute_step _ _ _ (.jge .r3 (.reg .r4) 4)
  (by simp [step, initState])
  (by simp [step, initState]; exact f2)]
```

After a branch instruction, simp needs the comparison hypothesis (`h_slip`, `h_not_ge`, etc.) plus `ge_iff_le` and `↓reduceIte` to resolve the branch direction and determine the PC:
```lean
(by simp [step, initState, RegFile.get, RegFile.set, readByWidth, effectiveAddr, resolveSrc,
          h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]; exact f4)
```

**Step 4**: Close with `execute_halted` + simp after the final `exit`:
```lean
simp [execute_halted, step, initState, RegFile.get, RegFile.set, resolveSrc, readByWidth,
      effectiveAddr, h_min, h_tok, ge_iff_le, h_slip, ↓reduceIte]
```

### Theorem statement pattern

Properties are stated over symbolic memory with hypotheses binding memory reads:

```lean
theorem rejects_bad_input
    (inputAddr : Nat) (mem : Mem)
    (minBal tokenBal : Nat)
    (h_min : readU64 mem (effectiveAddr inputAddr 0x2918) = minBal)
    (h_tok : readU64 mem (effectiveAddr inputAddr 0x00a0) = tokenBal)
    (h_slip : minBal ≥ tokenBal) :
    (execute prog (initState inputAddr mem) 10).exitCode = some 1 := by
  ...
```

The `fuel` parameter (10 above) must be large enough for the longest execution path. Count the maximum instructions from entry to exit.

### Critical tactic rules for sBPF proofs

| Do | Don't |
|---|---|
| Use `execute_step` to unroll one step at a time | Put `execute` in a simp set (term explosion) |
| Use `native_decide` for fetch lemmas (closed terms) | Use `native_decide` on expressions with free variables |
| Add `execSyscall` to simp set after `call` instructions | Forget `execSyscall` (state gets stuck) |
| Add branch hypotheses (`h_slip`, `ge_iff_le`, `↓reduceIte`) after conditional jumps | Omit comparison hypotheses (PC unresolved) |
| Set `maxHeartbeats` generously (1.6M–3.2M) for longer programs | Use default heartbeats (programs with 8+ steps will timeout) |

## Step 6: Call Leanstral for hard sub-goals

When you have a proof with `sorry` markers you cannot fill after 2-3 attempts:

```bash
$QEDGEN fill-sorry --file formal_verification/Proofs/Hard.lean --validate
```

This sends each `sorry` location to Leanstral with focused context. Review the result — Leanstral may introduce tactics you can learn from for future proofs.

If `fill-sorry` also fails, simplify the theorem statement or split the property into smaller lemmas.

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
- **`QEDGEN_VALIDATION_WORKSPACE`** — optional override for global Mathlib cache location

## Error handling

- **First `lake build` is slow**: Mathlib compilation takes 15-45 min on first run. Subsequent builds reuse the cache.
- **`could not resolve 'HEAD' to a commit`**: Remove `.lake/packages/mathlib` and run `lake update`.
- **Rate limiting (429)**: Built-in exponential backoff in `fill-sorry`.
