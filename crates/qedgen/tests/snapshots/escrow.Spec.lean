import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid

namespace Escrow

open QEDGen.Solana

inductive Status where
  | Uninitialized
  | Open
  | Closed
  deriving Repr, DecidableEq, BEq, Inhabited

structure State where
  initializer : Pubkey
  initializer_token_account : Pubkey
  taker : Pubkey
  initializer_amount : Nat
  taker_amount : Nat
  escrow_token_account : Pubkey
  status : Status
  deriving Repr, DecidableEq, BEq, Inhabited

/-- Handler invocation context (#328): account addresses and cross-program
    state the guards read. Universally quantified in every theorem. -/
structure ActionCtx where
  initializer_ta : Pubkey

def initializeTransition (s : State) (signer : Pubkey) (deposit_amount : Nat) (receive_amount : Nat) : Option State :=
  if signer = s.initializer ∧ s.status = .Uninitialized ∧ deposit_amount > 0 ∧ receive_amount > 0 then
    some { s with initializer_amount := deposit_amount, taker_amount := receive_amount, status := .Open }
  else none

def exchangeTransition (s : State) (signer : Pubkey) (ctx : ActionCtx) : Option State :=
  if signer = s.taker ∧ s.status = .Open ∧ ctx.initializer_ta = s.initializer_token_account then
    some { s with status := .Closed }
  else none

def cancelTransition (s : State) (signer : Pubkey) (ctx : ActionCtx) : Option State :=
  if signer = s.initializer ∧ s.status = .Open ∧ ctx.initializer_ta = s.initializer_token_account then
    some { s with status := .Closed }
  else none

/-- initialize transfer envelope: initializer_ta → escrow_ta amount deposit_amount authority initializer.
    Verifies CPI shape (program ID, account list, discriminator).
    Amount serialization and SPL Token execution are SDK/runtime
    trust per VERIFICATION_SCOPE.md. -/
def build_initialize_transfer (from_pk to_pk authority_pk : Pubkey) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts :=
      [ ⟨from_pk, false, true⟩
      , ⟨to_pk, false, true⟩
      , ⟨authority_pk, true, false⟩
      ]
  , data := DISC_TRANSFER }

theorem initialize_transfer_correct (from_pk to_pk authority_pk : Pubkey) :
    let cpi := build_initialize_transfer from_pk to_pk authority_pk
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 from_pk false true ∧
    accountAt cpi 1 to_pk false true ∧
    accountAt cpi 2 authority_pk true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold build_initialize_transfer targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

/-- exchange transfer envelope: taker_ta → initializer_ta amount taker_amount authority taker.
    Verifies CPI shape (program ID, account list, discriminator).
    Amount serialization and SPL Token execution are SDK/runtime
    trust per VERIFICATION_SCOPE.md. -/
def build_exchange_transfer_0 (from_pk to_pk authority_pk : Pubkey) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts :=
      [ ⟨from_pk, false, true⟩
      , ⟨to_pk, false, true⟩
      , ⟨authority_pk, true, false⟩
      ]
  , data := DISC_TRANSFER }

theorem exchange_transfer_0_correct (from_pk to_pk authority_pk : Pubkey) :
    let cpi := build_exchange_transfer_0 from_pk to_pk authority_pk
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 from_pk false true ∧
    accountAt cpi 1 to_pk false true ∧
    accountAt cpi 2 authority_pk true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold build_exchange_transfer_0 targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

/-- exchange transfer envelope: escrow_ta → taker_ta amount initializer_amount authority escrow.
    Verifies CPI shape (program ID, account list, discriminator).
    Amount serialization and SPL Token execution are SDK/runtime
    trust per VERIFICATION_SCOPE.md. -/
def build_exchange_transfer_1 (from_pk to_pk authority_pk : Pubkey) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts :=
      [ ⟨from_pk, false, true⟩
      , ⟨to_pk, false, true⟩
      , ⟨authority_pk, true, false⟩
      ]
  , data := DISC_TRANSFER }

theorem exchange_transfer_1_correct (from_pk to_pk authority_pk : Pubkey) :
    let cpi := build_exchange_transfer_1 from_pk to_pk authority_pk
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 from_pk false true ∧
    accountAt cpi 1 to_pk false true ∧
    accountAt cpi 2 authority_pk true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold build_exchange_transfer_1 targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

/-- cancel transfer envelope: escrow_ta → initializer_ta amount initializer_amount authority escrow.
    Verifies CPI shape (program ID, account list, discriminator).
    Amount serialization and SPL Token execution are SDK/runtime
    trust per VERIFICATION_SCOPE.md. -/
def build_cancel_transfer (from_pk to_pk authority_pk : Pubkey) : CpiInstruction :=
  { programId := TOKEN_PROGRAM_ID
  , accounts :=
      [ ⟨from_pk, false, true⟩
      , ⟨to_pk, false, true⟩
      , ⟨authority_pk, true, false⟩
      ]
  , data := DISC_TRANSFER }

theorem cancel_transfer_correct (from_pk to_pk authority_pk : Pubkey) :
    let cpi := build_cancel_transfer from_pk to_pk authority_pk
    targetsProgram cpi TOKEN_PROGRAM_ID ∧
    accountAt cpi 0 from_pk false true ∧
    accountAt cpi 1 to_pk false true ∧
    accountAt cpi 2 authority_pk true false ∧
    hasDiscriminator cpi DISC_TRANSFER := by
  unfold build_cancel_transfer targetsProgram accountAt hasDiscriminator
  exact ⟨rfl, rfl, rfl, rfl, rfl⟩

inductive Operation where
  | «initialize» (deposit_amount : Nat) (receive_amount : Nat)
  | exchange
  | cancel
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) (ctx : ActionCtx) : Operation → Option State
  | .«initialize» deposit_amount receive_amount => initializeTransition s signer deposit_amount receive_amount
  | .exchange => exchangeTransition s signer ctx
  | .cancel => cancelTransition s signer ctx

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem initialize_aborts_if_InvalidAmount (s : State) (signer : Pubkey) (deposit_amount : Nat) (receive_amount : Nat)
    (h : ¬(deposit_amount > 0 ∧ receive_amount > 0)) : initializeTransition s signer deposit_amount receive_amount = none := by
  unfold initializeTransition
  rw [if_neg (fun hg => h ⟨hg.2.2.1, hg.2.2.2⟩)]

theorem exchange_aborts_if_Unauthorized (s : State) (signer : Pubkey) (ctx : ActionCtx)
    (h : ¬(ctx.initializer_ta = s.initializer_token_account)) : exchangeTransition s signer ctx = none := by
  unfold exchangeTransition
  rw [if_neg (fun hg => h hg.2.2)]

theorem cancel_aborts_if_Unauthorized (s : State) (signer : Pubkey) (ctx : ActionCtx)
    (h : ¬(ctx.initializer_ta = s.initializer_token_account)) : cancelTransition s signer ctx = none := by
  unfold cancelTransition
  rw [if_neg (fun hg => h hg.2.2)]

-- ============================================================================
-- Cover properties — reachability (existential proofs)
-- ============================================================================

/-- happy_path — trace [initialize, exchange] is reachable. -/
theorem cover_happy_path : ∃ (s0 : State) (signer : Pubkey) (ctx : ActionCtx),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), initializeTransition s0 signer v0_0 v0_1 = some s1 ∧
exchangeTransition s1 signer ctx ≠ none := sorry

/-- cancel_path — trace [initialize, cancel] is reachable. -/
theorem cover_cancel_path : ∃ (s0 : State) (signer : Pubkey) (ctx : ActionCtx),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), initializeTransition s0 signer v0_0 v0_1 = some s1 ∧
cancelTransition s1 signer ctx ≠ none := sorry

-- ============================================================================
-- Liveness properties — bounded reachability (leads-to)
-- ============================================================================

def applyOps (s : State) (signer : Pubkey) (ctx : ActionCtx) : List Operation → Option State
  | [] => some s
  | op :: ops => match applyOp s signer ctx op with
    | some s' => applyOps s' signer ctx ops
    | none => none

/-- escrow_settles — from Open leads to Closed within 1 steps via [exchange, cancel]. -/
theorem liveness_escrow_settles (s : State) (signer : Pubkey) (ctx : ActionCtx)
    (h : s.status = .Open) :
    ∃ ops s', ops.length ≤ 1 ∧ applyOps s signer ctx ops = some s' ∧ s'.status = .Closed := by sorry

end Escrow
