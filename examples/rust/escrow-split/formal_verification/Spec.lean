import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import Token

namespace Escrow

open QEDGen.Solana

inductive Status where
  | Uninitialized
  | Open
  | Closed
  deriving Repr, DecidableEq, BEq

inductive State where
  | Uninitialized
  | Open (initializer : Pubkey) (taker : Pubkey) (initializer_amount : Nat) (taker_amount : Nat) (escrow_token_account : Pubkey)
  | Closed
  deriving Repr, DecidableEq, BEq

def State.status : State → Status
  | .Uninitialized => .Uninitialized
  | .Open _ _ _ _ _ => .Open
  | .Closed => .Closed

def State.initializer : State → Pubkey
  | .Uninitialized => default
  | .Open initializer _ _ _ _ => initializer
  | .Closed => default

def State.taker : State → Pubkey
  | .Uninitialized => default
  | .Open _ taker _ _ _ => taker
  | .Closed => default

def State.initializer_amount : State → Nat
  | .Uninitialized => 0
  | .Open _ _ initializer_amount _ _ => initializer_amount
  | .Closed => 0

def State.taker_amount : State → Nat
  | .Uninitialized => 0
  | .Open _ _ _ taker_amount _ => taker_amount
  | .Closed => 0

def State.escrow_token_account : State → Pubkey
  | .Uninitialized => default
  | .Open _ _ _ _ escrow_token_account => escrow_token_account
  | .Closed => default

def cancelTransition (s : State) (signer : Pubkey) : Option State :=
  match s with
  | .Open initializer taker initializer_amount taker_amount escrow_token_account =>
    if signer = initializer then some (.Closed) else none
  | _ => none

def exchangeTransition (s : State) (signer : Pubkey) : Option State :=
  match s with
  | .Open initializer taker initializer_amount taker_amount escrow_token_account =>
    if signer = taker then some (.Closed) else none
  | _ => none

def initializeTransition (s : State) (signer : Pubkey) (deposit_amount : Nat) (receive_amount : Nat) : Option State :=
  -- todo!(): post-variant `Open` has unconstrained field(s) not derivable from spec: taker, escrow_token_account
  -- Using type defaults; add effects or handler params to constrain these.
  match s with
  | .Uninitialized =>
    let initializer := signer
    if deposit_amount > 0 ∧ receive_amount > 0 then some (.Open initializer default deposit_amount receive_amount default) else none
  | _ => none

/-- Token.transfer.ensures @ `cancel` call #0 (stance 1: discharged via Tier-1 binary-hash axiom; v3.0 will replace the axiom with an imported callee proof). -/
theorem cancel_Token_transfer_call_0_post_0 (s : State) : s.initializer_amount > 0 :=
  Token.transfer.ensures_axiom_0 s.initializer_amount

/-- Token.transfer.ensures @ `exchange` call #0 (stance 1: discharged via Tier-1 binary-hash axiom; v3.0 will replace the axiom with an imported callee proof). -/
theorem exchange_Token_transfer_call_0_post_0 (s : State) : s.taker_amount > 0 :=
  Token.transfer.ensures_axiom_0 s.taker_amount

/-- Token.transfer.ensures @ `exchange` call #1 (stance 1: discharged via Tier-1 binary-hash axiom; v3.0 will replace the axiom with an imported callee proof). -/
theorem exchange_Token_transfer_call_1_post_0 (s : State) : s.initializer_amount > 0 :=
  Token.transfer.ensures_axiom_0 s.initializer_amount

/-- Token.transfer.ensures @ `initialize` call #0 (stance 1: discharged via Tier-1 binary-hash axiom; v3.0 will replace the axiom with an imported callee proof). -/
theorem initialize_Token_transfer_call_0_post_0 (s : State) (deposit_amount : Nat) (receive_amount : Nat) : deposit_amount > 0 :=
  Token.transfer.ensures_axiom_0 deposit_amount

-- INVARIANT OBLIGATION (declared, no predicate body): conservation
--   description: total tokens preserved across initialize, exchange, cancel
-- The spec declared this name but didn't supply a predicate body
-- (`invariant <name> : <expr>`). The codegen has no goal to lower —
-- pre-v2.14 emitted `theorem <name> : True := trivial`, which
-- was tautological. To verify this invariant, give it a body in
-- the spec.

inductive Operation where
  | cancel
  | exchange
  | «initialize» (deposit_amount : Nat) (receive_amount : Nat)
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .cancel => cancelTransition s signer
  | .exchange => exchangeTransition s signer
  | .«initialize» deposit_amount receive_amount => initializeTransition s signer deposit_amount receive_amount

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem initialize_aborts_if_InvalidAmount (s : State) (signer : Pubkey) (deposit_amount : Nat) (receive_amount : Nat)
    (h : ¬(deposit_amount > 0 ∧ receive_amount > 0)) : initializeTransition s signer deposit_amount receive_amount = none := by sorry

-- ============================================================================
-- Cover properties — reachability (existential proofs)
-- ============================================================================

/-- happy_path — trace [initialize, exchange] is reachable. -/
theorem cover_happy_path : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), initializeTransition s0 signer v0_0 v0_1 = some s1 ∧
exchangeTransition s1 signer ≠ none := by
  let pk : Pubkey := ⟨0, 0, 0, 0⟩
  let s0 : State := (.Uninitialized : State)
  let s1 : State := (.Open pk pk 1 1 pk : State)
  exact ⟨s0, pk, 1, 1, s1, by decide, by decide⟩

/-- cancel_path — trace [initialize, cancel] is reachable. -/
theorem cover_cancel_path : ∃ (s0 : State) (signer : Pubkey),
    ∃ (v0_0 : Nat) (v0_1 : Nat), ∃ (s1 : State), initializeTransition s0 signer v0_0 v0_1 = some s1 ∧
cancelTransition s1 signer ≠ none := by
  let pk : Pubkey := ⟨0, 0, 0, 0⟩
  let s0 : State := (.Uninitialized : State)
  let s1 : State := (.Open pk pk 1 1 pk : State)
  exact ⟨s0, pk, 1, 1, s1, by decide, by decide⟩

-- ============================================================================
-- Liveness properties — bounded reachability (leads-to)
-- ============================================================================

def applyOps (s : State) (signer : Pubkey) : List Operation → Option State
  | [] => some s
  | op :: ops => match applyOp s signer op with
    | some s' => applyOps s' signer ops
    | none => none

/-- escrow_settles — from Open leads to Closed within 1 steps via [exchange, cancel]. -/
theorem liveness_escrow_settles (s : State) (signer : Pubkey)
    (h : s.status = .Open) :
    ∃ ops, ops.length ≤ 1 ∧ ∀ s', applyOps s signer ops = some s' → s'.status = .Closed := by sorry

end Escrow
