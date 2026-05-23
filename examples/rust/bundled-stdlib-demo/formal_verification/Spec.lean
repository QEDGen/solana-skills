import QEDGen.Solana.Account
import QEDGen.Solana.Cpi
import QEDGen.Solana.State
import QEDGen.Solana.Valid
import Token

namespace PoolDemo

open QEDGen.Solana

inductive Status where
  | Uninitialized
  | Open
  deriving Repr, DecidableEq, BEq

inductive State where
  | Uninitialized
  | Open (pool_balance : Nat)
  deriving Repr, DecidableEq, BEq

instance : Inhabited State := ⟨.Uninitialized⟩

def State.status : State → Status
  | .Uninitialized => .Uninitialized
  | .Open _ => .Open

def State.pool_balance : State → Nat
  | .Uninitialized => 0
  | .Open pool_balance => pool_balance

def initializeTransition (s : State) (signer : Pubkey) (initial : Nat) : Option State :=
  match s with
  | .Uninitialized =>
    if initial > 0 then some (.Open initial) else none
  | _ => none

def depositTransition (s : State) (signer : Pubkey) (amount : Nat) : Option State :=
  match s with
  | .Open pool_balance =>
    if amount > 0 ∧ pool_balance + amount ≤ 18446744073709551615 then some (.Open (pool_balance + amount)) else none
  | _ => none

-- `Token.transfer` ensures #0 (from_balance): caller supplied no `state_binders` for these abstract fields; ensures not pulled into caller proof. Bind via `state_binders { from_balance = state.<field> }` to consume.
/-- Token.transfer.ensures @ `deposit` call #0 (stance 2: discharged via imported callee proof). -/
theorem deposit_Token_transfer_call_0_post_1 (s : State) (pre post : State) (amount : Nat) : post.pool_balance = pre.pool_balance + amount :=
  Token.transfer.ensures_axiom_1 pre post amount (·.pool_balance)

inductive Operation where
  | «initialize» (initial : Nat)
  | deposit (amount : Nat)
  deriving Repr, DecidableEq, BEq

def applyOp (s : State) (signer : Pubkey) : Operation → Option State
  | .«initialize» initial => initializeTransition s signer initial
  | .deposit amount => depositTransition s signer amount

-- ============================================================================
-- Abort conditions — operations must reject under specified conditions
-- ============================================================================

theorem initialize_aborts_if_InvalidAmount (s : State) (signer : Pubkey) (initial : Nat)
    (h : ¬(initial > 0)) : initializeTransition s signer initial = none := by sorry

theorem deposit_aborts_if_InvalidAmount (s : State) (signer : Pubkey) (amount : Nat)
    (h : ¬(amount > 0)) : depositTransition s signer amount = none := by sorry

-- ============================================================================
-- Frame conditions (modifies)
-- ============================================================================

theorem deposit_frame (s s' : State) (signer : Pubkey) (amount : Nat)
    (h : depositTransition s signer amount = some s') :
    -- todo!(): inductive-State frame condition. Statement needs
    -- per-pre-variant case analysis to express which payload
    -- fields are preserved. Holds trivially for now.
    True := by sorry

-- ============================================================================
-- Overflow safety obligations (auto-generated for operations with add effects)
-- ============================================================================

theorem deposit_overflow_safe (s s' : State) (signer : Pubkey) (amount : Nat)
    (h_valid : valid_u64 s.pool_balance)
    (h : depositTransition s signer amount = some s') :
    valid_u64 s'.pool_balance := by sorry

end PoolDemo
