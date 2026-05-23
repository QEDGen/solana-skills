-- v2.27 Track C2: bundled SPL Token proof package (Stance 2).
--
-- The consumer's lakefile pulls this module via
--   require splProofs from <qedgen-cache>/builtin/spl/.qed/proofs
-- and `import Token` resolves to the theorem symbols below instead of
-- the Stance-1 local axiom module that codegen would otherwise write.
--
-- Trust boundary. Each handler's contract is consolidated into ONE
-- named axiom (`<handler>.runtime_trust_<binder>`) per abstract-state
-- accessor referenced. The `ensures_axiom_<i>` THEOREMS that consumers
-- apply derive trivially from those axioms — same byte-for-byte
-- signature as the Stance-1 codegen, so the consumer's Lean code is
-- unchanged across stances.
--
-- The substantive difference vs Stance 1: the runtime-trust assumption
-- is now NAMED and DOCUMENTED at one location per handler, rather than
-- scattered across N unlabelled `axiom ensures_axiom_<i>` declarations.
-- Future hardening (formal-svm spinout): replace the runtime_trust
-- axioms with actual proofs against a Lean model of the deployed SPL
-- Token binary at the pinned `binary_hash`. v2.27 ships the structure
-- + the documented trust surface; the model-based discharge is v3.0+.

namespace Token

/-- Content pin against the deployed SPL Token program at
    `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`. Must match the
    `binary_hash` declared in `spl_token.qedspec`'s `upstream { ... }`
    block. The qedgen resolver enforces equality at lock time. -/
def binary_hash : String :=
  "sha256:8190d3f7ceb6cb7a7a8d8924bff89f9f611e15ce1f806f2b6237f3311a98f697"

-- ---------------------------------------------------------------------
-- transfer (amount : U64)
--   ensures #0: state.from_balance == old(state.from_balance) - amount
--   ensures #1: state.to_balance   == old(state.to_balance)   + amount
-- ---------------------------------------------------------------------

namespace transfer

/-- Runtime-trust assumption: the deployed SPL Token program at the
    pinned `binary_hash` enforces balance-debit on `from` upon a
    successful `transfer`. This is the trust boundary; the binary
    pin is the warrant. -/
axiom runtime_trust_from {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
  (from_balance post) = (from_balance pre) - amount

/-- Runtime-trust assumption: the deployed SPL Token program at the
    pinned `binary_hash` enforces balance-credit on `to` upon a
    successful `transfer`. -/
axiom runtime_trust_to {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (to_balance : State → Nat) :
  (to_balance post) = (to_balance pre) + amount

/-- Codegen-shape theorem matching `Token.transfer`'s ensures #0.
    Discharges via `runtime_trust_from`. -/
theorem ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
    (from_balance post) = (from_balance pre) - amount :=
  runtime_trust_from pre post amount from_balance

/-- Codegen-shape theorem matching `Token.transfer`'s ensures #1.
    Discharges via `runtime_trust_to`. -/
theorem ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (to_balance : State → Nat) :
    (to_balance post) = (to_balance pre) + amount :=
  runtime_trust_to pre post amount to_balance

end transfer

-- ---------------------------------------------------------------------
-- mint_to (amount : U64)
--   ensures #0: state.total_supply == old(state.total_supply) + amount
--   ensures #1: state.to_balance   == old(state.to_balance)   + amount
-- ---------------------------------------------------------------------

namespace mint_to

axiom runtime_trust_supply {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (total_supply : State → Nat) :
  (total_supply post) = (total_supply pre) + amount

axiom runtime_trust_to {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (to_balance : State → Nat) :
  (to_balance post) = (to_balance pre) + amount

theorem ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (total_supply : State → Nat) :
    (total_supply post) = (total_supply pre) + amount :=
  runtime_trust_supply pre post amount total_supply

theorem ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (to_balance : State → Nat) :
    (to_balance post) = (to_balance pre) + amount :=
  runtime_trust_to pre post amount to_balance

end mint_to

-- ---------------------------------------------------------------------
-- burn (amount : U64)
--   ensures #0: state.total_supply == old(state.total_supply) - amount
--   ensures #1: state.from_balance == old(state.from_balance) - amount
-- ---------------------------------------------------------------------

namespace burn

axiom runtime_trust_supply {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (total_supply : State → Nat) :
  (total_supply post) = (total_supply pre) - amount

axiom runtime_trust_from {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
  (from_balance post) = (from_balance pre) - amount

theorem ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (total_supply : State → Nat) :
    (total_supply post) = (total_supply pre) - amount :=
  runtime_trust_supply pre post amount total_supply

theorem ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
    (from_balance post) = (from_balance pre) - amount :=
  runtime_trust_from pre post amount from_balance

end burn

end Token
