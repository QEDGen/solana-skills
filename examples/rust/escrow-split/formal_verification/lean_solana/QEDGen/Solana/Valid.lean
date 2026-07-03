-- Validity predicates for state invariants
--
-- This module defines well-formedness predicates for program states.
-- These capture preconditions that must hold for safe execution.

import QEDGen.Solana.Account

namespace QEDGen.Solana.Valid

-- Type bound constants
def U8_MAX : Nat := 255
def U16_MAX : Nat := 65535
def U32_MAX : Nat := 4294967295
def U64_MAX : Nat := 18446744073709551615
def U128_MAX : Nat := 340282366920938463463374607431768211455

-- Validity predicates for numeric types
def valid_u8 (n : Nat) : Prop := n <= U8_MAX
def valid_u16 (n : Nat) : Prop := n <= U16_MAX
def valid_u32 (n : Nat) : Prop := n <= U32_MAX
def valid_u64 (n : Nat) : Prop := n <= U64_MAX
def valid_u128 (n : Nat) : Prop := n <= U128_MAX

-- Zero is always a valid u64
theorem valid_u64_zero : valid_u64 0 := by
  unfold valid_u64; omega

-- v2.21 S2.5: opaque on-chain timestamp.
-- Spec authors write `now()` in handler effects / requires; codegen
-- lowers it to a fresh `Clock::get()?.unix_timestamp` read in Rust
-- and to `now` here in Lean. The value is treated as adversarial /
-- arbitrary at proof time — proofs that depend on a specific timestamp
-- discharge against this axiom rather than against a concrete value.
axiom now : Nat

-- v2.24 #19: opaque on-chain epoch.
-- Spec authors write `current_epoch()` in handler effects / requires;
-- codegen lowers it to `Clock::get()?.epoch` in Rust and to
-- `current_epoch` here in Lean. Same shape as `now` — adversarial /
-- arbitrary at proof time. Solana protocols use epoch for
-- stake / vote / commission scheduling.
axiom current_epoch : Nat

-- Example: Generic ValidState template
-- Users can define custom ValidState predicates for their programs
--
-- def ValidState (State : Type) : Prop :=
--   ∃ (amount : Nat) (counter : Nat),
--     valid_u64 amount ∧ valid_u64 counter

end QEDGen.Solana.Valid

namespace QEDGen.Solana

-- Export validity predicates
abbrev U8_MAX := QEDGen.Solana.Valid.U8_MAX
abbrev U16_MAX := QEDGen.Solana.Valid.U16_MAX
abbrev U32_MAX := QEDGen.Solana.Valid.U32_MAX
abbrev U64_MAX := QEDGen.Solana.Valid.U64_MAX
abbrev U128_MAX := QEDGen.Solana.Valid.U128_MAX

abbrev valid_u8 := QEDGen.Solana.Valid.valid_u8
abbrev valid_u16 := QEDGen.Solana.Valid.valid_u16
abbrev valid_u32 := QEDGen.Solana.Valid.valid_u32
abbrev valid_u64 := QEDGen.Solana.Valid.valid_u64
abbrev valid_u128 := QEDGen.Solana.Valid.valid_u128

abbrev valid_u64_zero := QEDGen.Solana.Valid.valid_u64_zero

-- v2.21 S2.5: export `now` so the unqualified form codegen emits
-- (`now`) resolves at use sites that `open QEDGen.Solana`.
-- `noncomputable` because `now` is an axiom and Lean's code generator
-- otherwise refuses to compile an abbrev for it.
noncomputable abbrev now := QEDGen.Solana.Valid.now

-- v2.24 #19: export `current_epoch` analogously.
noncomputable abbrev current_epoch := QEDGen.Solana.Valid.current_epoch

end QEDGen.Solana
