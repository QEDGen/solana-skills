# sBPF Assembly Verification Reference

> **Lean-mandatory by construction.** sBPF is the one place in qedgen
> where Lean is required, not optional — the SVM interpreter lives in
> the Lean support library (`QEDGen.Solana.SBPF`), so bytecode
> verification has no Kani substitute. If you're verifying an sBPF
> program, you're in Phase 2. For Rust programs, Phase 1 (spec +
> proptest + Kani) is usually the finish line.

## Transpile with asm2lean

**Never transcribe assembly by hand.** Use `qedgen asm2lean`:

```bash
$QEDGEN asm2lean --input src/program.s --output formal_verification/ProgramProg.lean
```

This generates:
- `abbrev` definitions for all `.equ` constants (offsets as `Int`, values as `Nat`)
- `@[simp] def prog : Program` with named constants and index comments
- For large programs (>64 instructions): `def progAt : Nat -> Option Insn` — chunked function-based lookup for O(1) simp performance
- `@[simp] theorem ea_NAME` — effectiveAddr lemmas for each offset symbol
- `@[simp] theorem bridge_NAME` — toU64 bridge lemmas for Nat lddw constants
- `@[simp] theorem insn_N` — instruction fetch cache via `native_decide`

### Assembly syntax reference

| Assembly | Lean encoding | Meaning |
|---|---|---|
| `ldxdw r3, [r1+0x2918]` | `.ldx .dword .r3 .r1 0x2918` | Load 8 bytes from mem[r1+offset] into r3 |
| `ldxb r2, [r1+OFF]` | `.ldx .byte .r2 .r1 OFF` | Load 1 byte |
| `lddw r0, 1` | `.lddw .r0 1` | Load 64-bit immediate |
| `jge r3, r4, label` | `.jge .r3 (.reg .r4) <abs_idx>` | Branch if r3 >= r4 |
| `jne r2, 3, label` | `.jne .r2 (.imm 3) <abs_idx>` | Branch if r2 != 3 |
| `add64 r2, 8` | `.add64 .r2 (.imm 8)` | r2 = r2 + 8 (wrapping) |
| `mov64 r0, 1` | `.mov64 .r0 (.imm 1)` | r0 = 1 |
| `call sol_log_` | `.call .sol_log_` | Invoke syscall |
| `exit` | `.exit` | Exit with code in r0 |

## SBPF support library API

After `import QEDGen.Solana.SBPF` and `open QEDGen.Solana.SBPF`:

### Types

- `Reg` — `.r0` through `.r10` (r10 is read-only frame pointer)
- `Src` — `.reg r` or `.imm v`
- `Width` — `.byte` (1), `.half` (2), `.word` (4), `.dword` (8)
- `Syscall` — `.sol_log_`, `.sol_invoke_signed`, `.sol_get_clock_sysvar`, etc.
- `Insn` — All sBPF instructions
- `RegFile` — struct with fields `r0..r10 : Nat`, `@[simp]` on `get`/`set`
- `State` — `{ regs : RegFile, mem : Mem, pc : Nat, exitCode : Option Nat }`
- `Mem` — `Nat -> Nat` (byte-addressable memory)

### Initialization

- `initState (inputAddr : Nat) (mem : Mem) : State` — r1=inputAddr, r10=stack, pc=0
- `initState2 (inputAddr insnAddr : Nat) (mem : Mem) (entryPc : Nat := 0) : State` — two-pointer for SIMD-0321 (r1=input, r2=instruction data); `entryPc` supports non-zero entry points

### Execution

- `executeFn (fetch : Nat -> Option Insn) (s : State) (fuel : Nat) : State` — function-based fetch, O(1) per step
- `step (insn : Insn) (s : State) : State` — single-instruction semantics
- `resolveSrc (rf : RegFile) (src : Src) : Nat` — register or immediate to unsigned

### Memory (`open QEDGen.Solana.SBPF.Memory`)

- `effectiveAddr (base : Nat) (off : Int) : Nat`
- `readU8`, `readU16`, `readU32`, `readU64` — little-endian reads
- `writeU8`, `writeU16`, `writeU32`, `writeU64` — little-endian writes
- `readByWidth`, `writeByWidth` — dispatch by `Width`
- Region constants: `RODATA_START`, `BYTECODE_START`, `STACK_START`, `HEAP_START`, `INPUT_START`

### Wrapping arithmetic

- `wrapAdd`, `wrapSub`, `wrapMul`, `wrapNeg` — 64-bit wrapping
- `wrapAdd32`, `wrapSub32`, `wrapMul32`, `wrapNeg32` — 32-bit wrapping
- `toU64 (v : Int) : Nat` — sign-extended immediate to unsigned 64-bit

### Key execution lemmas (all `@[simp]`)

| Lemma | Statement |
|---|---|
| `executeFn_halted` | Halted state is a fixed point |
| `executeFn_zero` | `executeFn fetch s 0 = s` |
| `executeFn_step` | Unfolds one step |
| `executeFn_compose` | `executeFn fetch s (n+m) = executeFn fetch (executeFn fetch s n) m` |
| `executeFn_preserves_r10` | r10 invariant through execution |
| `executeFn_r10_initState` | `(executeFn fetch (initState ...) n).regs.r10 = STACK_START + 0x1000` |
| `RegFile.set_r10` | Writing to r10 is a no-op |
| `RegFile.get_set_self` | `(rf.set r v).get r = v` (when `r != .r10`) |
| `RegFile.get_set_diff` | `(rf.set r2 v).get r1 = rf.get r1` (when `r1 != r2`) |
| `RegFile.set_preserves_r10` | `(rf.set r v).r10 = rf.r10` for any register |

### Memory axioms

**Same-address round-trip:**
- `readU64_writeU64_same`, `readU32_writeU32_same`, `readU8_writeU8_same`

**Disjoint-address (within same region):**
- `readU64_writeU64_disjoint`, `readU64_writeU32_disjoint`, `readU64_writeU16_disjoint`, `readU64_writeU8_disjoint`
- `readU32_writeU64_disjoint`, `readU32_writeU32_disjoint`
- `readU8_writeU64_outside`, `readU8_writeU32_outside`, `readU8_writeU16_outside`, `readU8_writeU8_disjoint`

**Region frame (input read survives stack write):**
- `readU64_writeU64_frame`, `readU64_writeU32_frame`, `readU64_writeU16_frame`, `readU64_writeU8_frame`
- `readU32_writeU64_frame`, `readU32_writeU32_frame`
- `readU8_writeU64_frame`, `readU8_writeU32_frame`, `readU8_writeU16_frame`, `readU8_writeU8_frame`

**Chain frame (reads survive a list of stack writes):**
- `readU64_writeU64Chain_frame`, `readU32_writeU64Chain_frame`, `readU8_writeU64Chain_frame`

### Pubkey predicates (`open QEDGen.Solana.SBPF.Pubkey`)

- `pubkeyAt mem base pk` — four U64 chunks at consecutive 8-byte addresses
- `readPubkey mem base` — functional read of four chunks
- `pubkeyAt_iff_readPubkey` — predicate equals functional form
- `pubkeyAt_writeU64_disjoint` — survives disjoint write
- `pubkeyAt_writeU64_frame` — survives stack write
- `pubkeyAt_writeU64Chain_frame` — survives chain of stack writes

## Tactics

### `wp_exec [fetch_defs] [simp_extras]`

One-shot tactic for sBPF proofs. Iteratively unfolds execution at O(1) kernel depth per step.

- First bracket: fetch function + chunk defs (passed to `dsimp` for instruction decode)
- Second bracket: effectiveAddr lemmas + extras (passed to `simp` for branch resolution)

```lean
set_option maxHeartbeats 800000 in
theorem rejects_wrong_discriminant
    (inputAddr : Nat) (mem : Mem)
    (disc : Nat)
    (h_disc : readU64 mem inputAddr = disc)
    (h_ne : disc != EXPECTED_DISC) :
    (executeFn progAt (initState inputAddr mem) 8).exitCode = some E_WRONG_DISC := by
  have h : ¬(readU64 mem inputAddr = EXPECTED_DISC) := by rw [h_disc]; exact h_ne
  wp_exec [progAt, progAt_0, progAt_1] [ea_0]
```

### `wp_step [fetch_defs] [simp_extras]`

Single instruction step. Use when `wp_exec` needs manual guidance (e.g., memory disjointness between steps).

**Requires** `rw [executeFn_eq_execSegment]` first and `rfl` at the end.

```lean
rw [executeFn_eq_execSegment]
wp_step [progAt, progAt_0, progAt_1] [ea_0, ea_88]
rw [readU8_writeU64_outside _ _ _ _ (by ...)]
wp_step [progAt, progAt_0, progAt_1] [ea_0, ea_88]
rfl
```

### `strip_writes` / `strip_writes_goal`

Strip nested write layers from read expressions via disjointness (omega). `strip_writes_goal` only unfolds STACK_START in the goal (for large contexts).

### `mem_frame`

Automatic region-based write stripping. Handles cross-width reads/writes, within-stack disjointness, and same-address round-trips.

### `rewrite_mem [hmem]` / `solve_read [hmem] h_val`

Rewrite with memory hypotheses then apply region frame reasoning. `solve_read` is `rewrite_mem` + `exact h_val`.

## Library patterns (reusable instruction sequences)

From `QEDGen.Solana.SBPF.Patterns`:

| Pattern | Steps | Sequence | Conclusion |
|---|---|---|---|
| `error_exit` | 2 | `mov32 r0 code; exit` | `exitCode = some (toU64 code % U32_MODULUS)` |
| `dup_pass` / `dup_fail` | 2 | `ldx byte; jne` | Duplicate marker check pass/fail |
| `chunk_eq_mem` / `chunk_ne_mem` | 3 | `ldx; ldx; jne` | Memory chunk comparison |
| `chunk_eq_imm` / `chunk_ne_imm` | 3 | `ldx; lddw; jne` | Chunk vs 64-bit immediate |
| `chunk_eq_imm32` / `chunk_ne_imm32` | 3 | `ldx; mov32; jne` | Chunk vs 32-bit immediate |
| `chunk_ne_mem_error` | 5 | chunk mismatch -> error exit | Full mismatch-to-error |

Register disjointness hypotheses are dischargeable by `decide` at call sites.

**Use `wp_exec`** for simple linear paths (3-15 instructions). **Use library patterns** for structured programs with recurring check sequences — split with `executeFn_compose`, call patterns for each sequence, chain results.

### Usage example

```lean
-- Bridge hypotheses to library form
have h_eq' : readU64 s.mem (effectiveAddr (s.regs.get .r9) off1) =
             readU64 s.mem (effectiveAddr (s.regs.get .r10) off2) := by
  simp only [RegFile.get, h_r9, h_r10]; exact h_eq
-- Call library pattern
obtain ⟨he, hp, hm, hreg⟩ := chunk_eq_mem progAt s .r7 .r8 .r9 .r10 off1 off2 target
  (by decide) (by decide) (by decide) (by decide)  -- register disjointness
  h_exit h_f1 h_f2 h_f3 h_eq'
-- Extract specific register preservation
have hr9 := hreg .r9 (by decide) (by decide)
```

## Memory disjointness through stack writes

When the program writes to the stack then reads from the input buffer:

### Stack-input separation hypothesis

```lean
(h_sep : STACK_START + 0x1000 > inputAddr + 100000)
```

### Reading through stack writes

```lean
-- Byte read through dword stack write
rw [readU8_writeU64_outside _ _ _ _
  (by left; unfold STACK_START at h_addr ⊢; omega)]
-- Dword read through dword stack write
rw [readU64_writeU64_disjoint _ _ _ _ _
  (by unfold STACK_START at h_addr ⊢; omega)]
```

Chain multiple rewrites for multiple stack writes.

### `simp` vs `simp only` for hypothesis normalization (critical)

After stepping through `wrapAdd`/`toU64` instructions, the goal's address expressions get normalized by step-level `simp`. But hypotheses remain in original form. To make omega see them as equal:

```lean
-- GOOD: includes @[simp] lemmas, matches step-level normalization
simp [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_addr h_dup'

-- BAD: misses modular identities — omega sees different free variables
simp only [wrapAdd, toU64, DATA_LEN_MAX_PAD] at h_addr h_dup'
```

### Bound hypotheses for dynamic addresses

After `add64`/`and64` compute addresses dynamically, introduce bounds:

```lean
(h_addr : (baseDataLen + DATA_LEN_MAX_PAD) &&& toU64 DATA_LEN_AND_MASK + inputAddr < STACK_START)
```

### Phase-based proof structure

For complex paths (20+ steps) with memory mutations:

1. **Common validation prefix** — shared steps that `wp_exec` or `wp_step` handles
2. **Pointer arithmetic / memory writes** — compute dynamic addresses, write to stack. Introduce bound hypotheses after.
3. **Property-specific check** — final read-and-branch with disjointness proofs

## Theorem statement pattern

Properties over symbolic memory with hypotheses binding reads:

```lean
theorem rejects_insufficient_lamports
    (inputAddr : Nat) (mem : Mem)
    (amount senderLamports : Nat)
    (h_num   : readU64 mem inputAddr = N_ACCOUNTS_EXPECTED)
    (h_amt   : readU64 mem (effectiveAddr inputAddr INSTRUCTION_DATA_OFFSET) = amount)
    (h_bal   : readU64 mem (effectiveAddr inputAddr SENDER_LAMPORTS_OFFSET) = senderLamports)
    (h_insuf : senderLamports < amount) :
    (executeFn progAt (initState inputAddr mem) 20).exitCode = some E_INSUFFICIENT_LAMPORTS := by
  have h_not_ge : ¬(senderLamports >= amount) := by omega
  wp_exec [progAt, progAt_0, progAt_1] [ea_0, ea_88]
```

**Critical**: Use `readU8` for byte loads (`ldxb`) and `readU64` for dword loads (`ldxdw`). Width must match assembly.

The `fuel` parameter must be large enough for the longest execution path. Count maximum instructions from entry to exit.

## sBPF simp performance (critical)

Three rules that determine whether `wp_exec` completes in seconds or times out:

1. **Offset constants MUST be `Int`**: `effectiveAddr` takes `(off : Int)`. `Nat` offsets insert a coercion causing exponential blowup (0.5s -> 4+ min).

2. **Named constants in `prog` MUST match hypothesis names**: Syntactic mismatch forces `simp` to unfold at every subterm at every step.

3. **`@[simp]` on `prog` is required**: Without it, `wp_exec` cannot fetch instructions.

`qedgen asm2lean` handles all three automatically.

## Critical rules for sBPF proofs

| Do | Don't |
|---|---|
| Use `wp_exec [progAt, progAt_0, ...] [ea_lemmas]` | Manually unroll unless `wp_exec` needs per-step guidance |
| Generate `Prog.lean` with `qedgen asm2lean` | Hand-transcribe assembly |
| Use named constants from `Prog.lean` in hypotheses | Use raw numeric literals (simp blowup) |
| Use `Int` for offset constants | Use `Nat` for offsets (coercion -> simp timeout) |
| Set `maxHeartbeats` 800000+ | Use default heartbeats (will timeout) |
| Negate hypotheses before `wp_exec`: `have h : ¬(x = y)` | Pass `!=` hypotheses directly |
| Include `U32_MODULUS` in simp extras for `mov32` paths | Omit it (mov32 exit codes won't reduce) |
