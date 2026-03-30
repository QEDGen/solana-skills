-- sBPF Instruction Set Architecture for hand-written Solana programs
--
-- Minimal subset of the sBPF ISA sufficient to verify hand-written Solana
-- programs.
--
-- Reference: https://github.com/anza-xyz/sbpf

namespace QEDGen.Solana.SBPF

/-- 64-bit word modulus for wrapping arithmetic -/
def U64_MODULUS : Nat := 2 ^ 64

/-- sBPF registers: r0-r9 general purpose, r10 read-only frame pointer.
    r0 holds return values / exit codes.
    r1-r5 are caller-saved argument registers (used for all calls: syscalls, BPF-to-BPF, CPI).
    r6-r9 are callee-saved.
    r10 is the read-only stack frame pointer. -/
inductive Reg
  | r0 | r1 | r2 | r3 | r4 | r5 | r6 | r7 | r8 | r9 | r10
  deriving Repr, DecidableEq, BEq, Inhabited

/-- Source operand: register or 64-bit immediate value -/
inductive Src
  | reg (r : Reg)
  | imm (v : Nat)
  deriving Repr, DecidableEq

/-- Memory access width -/
inductive Width
  | byte   -- 1 byte  (ldxb / stb / stxb)
  | half   -- 2 bytes (ldxh / sth / stxh)
  | word   -- 4 bytes (ldxw / stw / stxw)
  | dword  -- 8 bytes (ldxdw / stdw / stxdw)
  deriving Repr, DecidableEq

/-- Number of bytes for a given width -/
def Width.bytes : Width → Nat
  | .byte  => 1
  | .half  => 2
  | .word  => 4
  | .dword => 8

/-- Mask for truncating values to a given width -/
def Width.mask : Width → Nat
  | .byte  => 2 ^ 8 - 1
  | .half  => 2 ^ 16 - 1
  | .word  => 2 ^ 32 - 1
  | .dword => 2 ^ 64 - 1

/-- sBPF syscall identifiers (Solana runtime).
    These map to the sol_* functions available to on-chain programs. -/
inductive Syscall
  -- Logging
  | sol_log_
  | sol_log_64_
  | sol_log_compute_units_
  | sol_log_pubkey
  | sol_log_data
  -- PDA derivation
  | sol_create_program_address
  | sol_try_find_program_address
  -- Cross-program invocation
  | sol_invoke_signed
  | sol_invoke_signed_c
  -- Sysvars
  | sol_get_clock_sysvar
  | sol_get_rent_sysvar
  | sol_get_epoch_schedule_sysvar
  | sol_get_last_restart_slot
  -- Introspection
  | sol_remaining_compute_units
  | sol_get_stack_height
  -- Hashing
  | sol_sha256
  | sol_keccak256
  | sol_blake3
  -- Memory operations
  | sol_memcpy
  | sol_memmove
  | sol_memcmp
  | sol_memset
  -- Crypto
  | sol_secp256k1_recover
  -- Return data
  | sol_get_return_data
  | sol_set_return_data
  deriving Repr, DecidableEq

/-- sBPF instructions.
    Jump targets are absolute instruction indices (resolved from labels by parser).
    This abstracts away lddw occupying 2 instruction slots in the binary encoding;
    our model treats each logical instruction as one array element. -/
inductive Insn
  -- Load 64-bit immediate into register
  | lddw  (dst : Reg) (imm : Nat)
  -- Load from memory: dst = mem[src + off]
  | ldx   (w : Width) (dst src : Reg) (off : Int)
  -- Store immediate to memory: mem[dst + off] = imm
  | st    (w : Width) (dst : Reg) (off : Int) (imm : Nat)
  -- Store register to memory: mem[dst + off] = src
  | stx   (w : Width) (dst : Reg) (off : Int) (src : Reg)
  -- ALU 64-bit
  | add64  (dst : Reg) (src : Src)
  | sub64  (dst : Reg) (src : Src)
  | mul64  (dst : Reg) (src : Src)
  | div64  (dst : Reg) (src : Src)
  | mod64  (dst : Reg) (src : Src)
  | or64   (dst : Reg) (src : Src)
  | and64  (dst : Reg) (src : Src)
  | xor64  (dst : Reg) (src : Src)
  | lsh64  (dst : Reg) (src : Src)
  | rsh64  (dst : Reg) (src : Src)
  | arsh64 (dst : Reg) (src : Src)
  | mov64  (dst : Reg) (src : Src)
  | neg64  (dst : Reg)
  -- ALU 32-bit (result zero-extended to 64 bits)
  | add32  (dst : Reg) (src : Src)
  | sub32  (dst : Reg) (src : Src)
  | mul32  (dst : Reg) (src : Src)
  | div32  (dst : Reg) (src : Src)
  | mod32  (dst : Reg) (src : Src)
  | or32   (dst : Reg) (src : Src)
  | and32  (dst : Reg) (src : Src)
  | xor32  (dst : Reg) (src : Src)
  | lsh32  (dst : Reg) (src : Src)
  | rsh32  (dst : Reg) (src : Src)
  | arsh32 (dst : Reg) (src : Src)
  | mov32  (dst : Reg) (src : Src)
  | neg32  (dst : Reg)
  -- Conditional jumps (target = absolute instruction index)
  | jeq   (dst : Reg) (src : Src) (target : Nat)
  | jne   (dst : Reg) (src : Src) (target : Nat)
  | jgt   (dst : Reg) (src : Src) (target : Nat)
  | jge   (dst : Reg) (src : Src) (target : Nat)
  | jlt   (dst : Reg) (src : Src) (target : Nat)
  | jle   (dst : Reg) (src : Src) (target : Nat)
  | jsgt  (dst : Reg) (src : Src) (target : Nat)
  | jsge  (dst : Reg) (src : Src) (target : Nat)
  | jslt  (dst : Reg) (src : Src) (target : Nat)
  | jsle  (dst : Reg) (src : Src) (target : Nat)
  | jset  (dst : Reg) (src : Src) (target : Nat)
  -- Unconditional jump
  | ja    (target : Nat)
  -- Syscall
  | call  (syscall : Syscall)
  -- Program exit (exit code = value in r0)
  | exit
  deriving Repr, DecidableEq

/-- Interpret a 64-bit unsigned value as signed two's complement -/
def toSigned64 (v : Nat) : Int :=
  if v < U64_MODULUS / 2 then ↑v
  else ↑v - ↑U64_MODULUS

end QEDGen.Solana.SBPF
