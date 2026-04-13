-- Tests for the qedbridge DSL
-- Validates syntax parsing, code generation, and type signatures

import QEDGen

open QEDGen.Solana

-- ============================================================================
-- Test 1: Minimal bridge — single U64 field, one operation, no lifecycle
-- ============================================================================

qedspec SimpleGuard where
  state
    balance : U64
    admin : Pubkey
  operation check
    who: admin
    guard: "s.balance > 0"

qedbridge SimpleGuard where
  input: r1
  fuel: 10
  layout
    balance U64 at 160
    admin Pubkey at 200
  operations
    check discriminator 0

-- Verify generated definitions exist and have correct types
#check @SimpleGuard.Bridge.BALANCE_OFF
#check @SimpleGuard.Bridge.ADMIN_OFF
#check @SimpleGuard.Bridge.FUEL
#check @SimpleGuard.Bridge.encodeState
#check @SimpleGuard.Bridge.decodeState
#check @SimpleGuard.Bridge.decode_encode
#check @SimpleGuard.Bridge.check.refines
#check @SimpleGuard.Bridge.check.rejects

-- ============================================================================
-- Test 2: Bridge with lifecycle, Pubkey field, and parameterized operation
-- ============================================================================

qedspec Vault where
  state
    balance : U64
    admin : Pubkey
  operation deposit
    who: admin
    when: Active
    then: Active
    takes: amount U64
    effect: balance add amount
  operation close
    who: admin
    when: Active
    then: Closed

qedbridge Vault where
  input: r1
  insn: r2
  fuel: 50
  layout
    balance U64 at 160
    admin Pubkey at 200
  status_encoding at 232
    Active 0
    Closed 1
  operations
    deposit discriminator 0 takes: amount U64
    close discriminator 1

-- Verify lifecycle-related definitions
#check @Vault.Bridge.encodeStatus
#check @Vault.Bridge.decodeStatus
#check @Vault.Bridge.decode_encode_status

-- Verify per-operation refinement theorems
#check @Vault.Bridge.deposit.refines
#check @Vault.Bridge.deposit.rejects
#check @Vault.Bridge.close.refines
#check @Vault.Bridge.close.rejects

-- ============================================================================
-- Test 3: Bridge with no operations (layout-only)
-- ============================================================================

qedspec ReadOnly where
  state
    value : U64

qedbridge ReadOnly where
  input: r1
  fuel: 5
  layout
    value U64 at 80

#check @ReadOnly.Bridge.encodeState
#check @ReadOnly.Bridge.decodeState
