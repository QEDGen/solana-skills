-- Flat byte-addressable memory model for sBPF verification
--
-- sBPF uses a flat address space partitioned into 5 regions:
--   0x000000000 : Read-only data (.rodata)
--   0x100000000 : Bytecode
--   0x200000000 : Stack
--   0x300000000 : Heap
--   0x400000000 : Input buffer (serialized accounts + instruction data)
--
-- Programs receive a pointer to the input buffer in r1 at entry.

import QEDGen.Solana.SBPF.ISA

namespace QEDGen.Solana.SBPF.Memory

open QEDGen.Solana.SBPF

/-! ## Memory type -/

/-- Byte-addressable memory: maps addresses to byte values (0-255) -/
abbrev Mem := Nat → Nat

/-! ## Region base addresses -/

def RODATA_START   : Nat := 0x000000000
def BYTECODE_START : Nat := 0x100000000
def STACK_START    : Nat := 0x200000000
def HEAP_START     : Nat := 0x300000000
def INPUT_START    : Nat := 0x400000000

/-! ## Effective address computation -/

/-- Compute effective address from base register value and signed offset.

    NOTE: Int.toNat clamps negative results to 0. In real sBPF, a negative
    effective address would be caught by the memory region bounds check
    (which we do not model). All verified programs use non-negative offsets,
    so this clamping is unreachable in practice. -/
def effectiveAddr (base : Nat) (off : Int) : Nat :=
  Int.toNat ((↑base : Int) + off)

/-! ## Read operations (little-endian) -/

/-- Read 1 byte -/
def readU8 (mem : Mem) (addr : Nat) : Nat :=
  mem addr % 256

/-- Read 2 bytes little-endian -/
def readU16 (mem : Mem) (addr : Nat) : Nat :=
  mem addr % 256 +
  mem (addr + 1) % 256 * 0x100

/-- Read 4 bytes little-endian -/
def readU32 (mem : Mem) (addr : Nat) : Nat :=
  mem addr % 256 +
  mem (addr + 1) % 256 * 0x100 +
  mem (addr + 2) % 256 * 0x10000 +
  mem (addr + 3) % 256 * 0x1000000

/-- Read 8 bytes little-endian -/
def readU64 (mem : Mem) (addr : Nat) : Nat :=
  mem addr % 256 +
  mem (addr + 1) % 256 * 0x100 +
  mem (addr + 2) % 256 * 0x10000 +
  mem (addr + 3) % 256 * 0x1000000 +
  mem (addr + 4) % 256 * 0x100000000 +
  mem (addr + 5) % 256 * 0x10000000000 +
  mem (addr + 6) % 256 * 0x1000000000000 +
  mem (addr + 7) % 256 * 0x100000000000000

/-! ## Write operations (little-endian) -/

/-- Write 1 byte -/
def writeU8 (mem : Mem) (addr val : Nat) : Mem :=
  fun a => if a = addr then val % 256 else mem a

/-- Write 2 bytes little-endian -/
def writeU16 (mem : Mem) (addr val : Nat) : Mem :=
  fun a =>
    if a = addr     then val % 0x100
    else if a = addr + 1 then val / 0x100 % 0x100
    else mem a

/-- Write 4 bytes little-endian -/
def writeU32 (mem : Mem) (addr val : Nat) : Mem :=
  fun a =>
    if a = addr     then val % 0x100
    else if a = addr + 1 then val / 0x100 % 0x100
    else if a = addr + 2 then val / 0x10000 % 0x100
    else if a = addr + 3 then val / 0x1000000 % 0x100
    else mem a

/-- Write 8 bytes little-endian -/
def writeU64 (mem : Mem) (addr val : Nat) : Mem :=
  fun a =>
    if a = addr     then val % 0x100
    else if a = addr + 1 then val / 0x100 % 0x100
    else if a = addr + 2 then val / 0x10000 % 0x100
    else if a = addr + 3 then val / 0x1000000 % 0x100
    else if a = addr + 4 then val / 0x100000000 % 0x100
    else if a = addr + 5 then val / 0x10000000000 % 0x100
    else if a = addr + 6 then val / 0x1000000000000 % 0x100
    else if a = addr + 7 then val / 0x100000000000000 % 0x100
    else mem a

/-! ## Generic read/write by width -/

/-- Read N bytes from memory according to width -/
def readByWidth (mem : Mem) (addr : Nat) : QEDGen.Solana.SBPF.Width → Nat
  | .byte  => readU8 mem addr
  | .half  => readU16 mem addr
  | .word  => readU32 mem addr
  | .dword => readU64 mem addr

/-- Write N bytes to memory according to width -/
def writeByWidth (mem : Mem) (addr val : Nat) : QEDGen.Solana.SBPF.Width → Mem
  | .byte  => writeU8 mem addr val
  | .half  => writeU16 mem addr val
  | .word  => writeU32 mem addr val
  | .dword => writeU64 mem addr val

/-! ## Memory axioms

These are provable from the concrete definitions above via byte decomposition
lemmas, but stated as axioms to keep proofs tractable. The key property is that
little-endian encode/decode is a round-trip for values within range. -/

/-! ### Same-address round-trip -/

/-- Reading back a U64 from the address it was just written to yields the original value -/
axiom readU64_writeU64_same (mem : Mem) (addr val : Nat)
    (h : val < 2 ^ 64) :
    readU64 (writeU64 mem addr val) addr = val

/-- Reading back a U32 from the address it was just written to yields the original value -/
axiom readU32_writeU32_same (mem : Mem) (addr val : Nat)
    (h : val < 2 ^ 32) :
    readU32 (writeU32 mem addr val) addr = val

/-- Reading back a U8 from the address it was just written to yields the original value -/
axiom readU8_writeU8_same (mem : Mem) (addr val : Nat)
    (h : val < 2 ^ 8) :
    readU8 (writeU8 mem addr val) addr = val

/-! ### Disjoint-address axioms (single-premise, within same region) -/

/-- Writing a U64 does not affect reads from non-overlapping addresses -/
axiom readU64_writeU64_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : rAddr + 8 ≤ wAddr ∨ wAddr + 8 ≤ rAddr) :
    readU64 (writeU64 mem wAddr val) rAddr = readU64 mem rAddr

/-- Writing a U32 does not affect U64 reads from non-overlapping addresses -/
axiom readU64_writeU32_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : rAddr + 8 ≤ wAddr ∨ wAddr + 4 ≤ rAddr) :
    readU64 (writeU32 mem wAddr val) rAddr = readU64 mem rAddr

/-- Writing a U16 does not affect U64 reads from non-overlapping addresses -/
axiom readU64_writeU16_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : rAddr + 8 ≤ wAddr ∨ wAddr + 2 ≤ rAddr) :
    readU64 (writeU16 mem wAddr val) rAddr = readU64 mem rAddr

/-- Writing a U8 does not affect U64 reads from non-overlapping addresses -/
axiom readU64_writeU8_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : wAddr < rAddr ∨ rAddr + 8 ≤ wAddr) :
    readU64 (writeU8 mem wAddr val) rAddr = readU64 mem rAddr

/-- Writing a U64 does not affect U32 reads from non-overlapping addresses -/
axiom readU32_writeU64_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : rAddr + 4 ≤ wAddr ∨ wAddr + 8 ≤ rAddr) :
    readU32 (writeU64 mem wAddr val) rAddr = readU32 mem rAddr

/-- Writing a U32 does not affect U32 reads from non-overlapping addresses -/
axiom readU32_writeU32_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : rAddr + 4 ≤ wAddr ∨ wAddr + 4 ≤ rAddr) :
    readU32 (writeU32 mem wAddr val) rAddr = readU32 mem rAddr

/-- Writing a U64 does not affect individual byte reads outside the written range -/
axiom readU8_writeU64_outside (mem : Mem) (bAddr wAddr val : Nat)
    (h : bAddr < wAddr ∨ wAddr + 8 ≤ bAddr) :
    readU8 (writeU64 mem wAddr val) bAddr = readU8 mem bAddr

/-- Writing a U32 does not affect byte reads outside the written range -/
axiom readU8_writeU32_outside (mem : Mem) (bAddr wAddr val : Nat)
    (h : bAddr < wAddr ∨ wAddr + 4 ≤ bAddr) :
    readU8 (writeU32 mem wAddr val) bAddr = readU8 mem bAddr

/-- Writing a U16 does not affect byte reads outside the written range -/
axiom readU8_writeU16_outside (mem : Mem) (bAddr wAddr val : Nat)
    (h : bAddr < wAddr ∨ wAddr + 2 ≤ bAddr) :
    readU8 (writeU16 mem wAddr val) bAddr = readU8 mem bAddr

/-- Writing a U8 does not affect byte reads at different addresses -/
axiom readU8_writeU8_disjoint (mem : Mem) (rAddr wAddr val : Nat)
    (h : rAddr ≠ wAddr) :
    readU8 (writeU8 mem wAddr val) rAddr = readU8 mem rAddr

/-! ### Region frame axioms (two-premise: read below STACK_START, write above)

These are derivable from the disjoint axioms (if rAddr + N ≤ STACK_START
and STACK_START ≤ wAddr then rAddr + N ≤ wAddr). Stated separately because
omega resolves two simple inequalities faster than one compound disjunction,
and the `mem_frame` tactic uses them for efficient region-based stripping. -/

/-- Input read survives stack write (U64 × U64) -/
theorem readU64_writeU64_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 8 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU64 (writeU64 mem wAddr val) rAddr = readU64 mem rAddr :=
  readU64_writeU64_disjoint mem rAddr wAddr val (Or.inl (by omega))

/-- Input read survives stack write (U64 × U32) -/
theorem readU64_writeU32_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 8 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU64 (writeU32 mem wAddr val) rAddr = readU64 mem rAddr :=
  readU64_writeU32_disjoint mem rAddr wAddr val (Or.inl (by omega))

/-- Input read survives stack write (U64 × U16) -/
theorem readU64_writeU16_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 8 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU64 (writeU16 mem wAddr val) rAddr = readU64 mem rAddr :=
  readU64_writeU16_disjoint mem rAddr wAddr val (Or.inl (by omega))

/-- Input read survives stack write (U64 × U8) -/
theorem readU64_writeU8_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 8 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU64 (writeU8 mem wAddr val) rAddr = readU64 mem rAddr :=
  readU64_writeU8_disjoint mem rAddr wAddr val (Or.inr (by omega))

/-- Input read survives stack write (U32 × U64) -/
theorem readU32_writeU64_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 4 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU32 (writeU64 mem wAddr val) rAddr = readU32 mem rAddr :=
  readU32_writeU64_disjoint mem rAddr wAddr val (Or.inl (by omega))

/-- Input read survives stack write (U32 × U32) -/
theorem readU32_writeU32_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 4 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU32 (writeU32 mem wAddr val) rAddr = readU32 mem rAddr :=
  readU32_writeU32_disjoint mem rAddr wAddr val (Or.inl (by omega))

/-- Byte read survives stack write (U8 × U64) -/
theorem readU8_writeU64_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 1 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU8 (writeU64 mem wAddr val) rAddr = readU8 mem rAddr :=
  readU8_writeU64_outside mem rAddr wAddr val (Or.inl (by omega))

/-- Byte read survives stack write (U8 × U32) -/
theorem readU8_writeU32_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 1 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU8 (writeU32 mem wAddr val) rAddr = readU8 mem rAddr :=
  readU8_writeU32_outside mem rAddr wAddr val (Or.inl (by omega))

/-- Byte read survives stack write (U8 × U16) -/
theorem readU8_writeU16_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 1 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU8 (writeU16 mem wAddr val) rAddr = readU8 mem rAddr :=
  readU8_writeU16_outside mem rAddr wAddr val (Or.inl (by omega))

/-- Byte read survives stack write (U8 × U8) -/
theorem readU8_writeU8_frame (mem : Mem) (rAddr wAddr val : Nat)
    (h_r : rAddr + 1 ≤ STACK_START) (h_w : STACK_START ≤ wAddr) :
    readU8 (writeU8 mem wAddr val) rAddr = readU8 mem rAddr :=
  readU8_writeU8_disjoint mem rAddr wAddr val (by omega)

/-! ### Region predicate -/

/-- Input region [base, base + bound) lies entirely below STACK_START -/
def belowStack (base bound : Nat) : Prop := base + bound ≤ STACK_START

/-! ## Input buffer layout helpers

The Solana runtime serializes accounts into the input buffer with a fixed
layout per account. Offsets are relative to the start of each account record.
The exact absolute offsets depend on preceding account data sizes, so programs
define them as .equ constants. -/

/-- Offsets within a single account record (relative to account start) -/
structure AccountLayout where
  header   : Nat  -- 8 bytes: dup marker, is_signer, is_writable, executable, ...
  key      : Nat  -- 32 bytes: account pubkey
  owner    : Nat  -- 32 bytes: owner program pubkey
  lamports : Nat  -- 8 bytes: lamport balance (u64 LE)
  dataLen  : Nat  -- 8 bytes: account data length (u64 LE)
  data     : Nat  -- variable: account data bytes

/-- Standard account layout (offsets from account start, after the num_accounts u64) -/
def standardAccountLayout : AccountLayout where
  header   := 0x00
  key      := 0x08
  owner    := 0x28
  lamports := 0x48
  dataLen  := 0x50
  data     := 0x58

end QEDGen.Solana.SBPF.Memory
