-- v2.28 — bundled SPL Token proof package (explicit-axiom shape).
--
-- WHAT THIS PACKAGE IS NOT. These are NOT proofs that the deployed SPL
-- Token program honors the `ensures` clauses declared in
-- `crates/qedgen/data/interfaces/spl_token.qedspec`. They are
-- AXIOMATIZED contracts. The package's load-bearing guarantee is the
-- `binary_hash` content pin against the deployed program at
-- `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` — `verify
-- --check-upstream` re-dumps and compares. Nothing in this module
-- proves that those pinned bytes implement the contracts below.
--
-- v2.28 — explicit-axiom rename. v2.27 wrapped each axiom in a one-step
-- `theorem ensures_axiom_<i> := runtime_trust_<binder>` indirection.
-- That made `#print axioms` on consumer proofs report `runtime_trust_*`
-- by name (one level removed from the symbol the consumer references)
-- and visually framed the package as "proven." Both are now dropped:
-- each `ensures_axiom_<i>` is declared as a top-level `axiom` directly,
-- so the unverified trust surface appears under exactly the symbol
-- consumers apply. Consumer code (`exact Token.transfer.ensures_axiom_0
-- pre post amount (·.field)`) is byte-identical across v2.27 and v2.28.
--
-- v3.0+ (Stance 3) — when QEDGen/qedsvm ships per-handler separation-
-- logic specs + a `qedsvm_discharge` tactic, each `axiom
-- ensures_axiom_<i>` below becomes `theorem ensures_axiom_<i> ... := by
-- qedsvm_discharge "<binary_hash>" "<handler>"`. The tactic decodes the
-- ELF at `binary_hash`, applies the bundled SL spec via `sl_block_auto`,
-- and projects onto the abstract State accessor. Consumer code remains
-- unchanged through that transition.

namespace Token

/-- Content pin against the deployed SPL Token program at
    `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`. Must match the
    `binary_hash` declared in `spl_token.qedspec`'s `upstream { ... }`
    block. The qedgen resolver enforces equality at lock time. -/
def binary_hash : String :=
  "sha256:435df1e70f1ca6258eb01a4507701688c7f1a22656e3e4ef361db473bf478075"

-- ---------------------------------------------------------------------
-- transfer (amount : U64)
--   ensures #0: state.from_balance == old(state.from_balance) - amount
--   ensures #1: state.to_balance   == old(state.to_balance)   + amount
-- ---------------------------------------------------------------------

namespace transfer

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `transfer` debits `from_balance` by `amount` on the deployed SPL
    Token program at `Token.binary_hash`. Discharged by `qedsvm` in
    v3.0+: decode the pinned ELF, apply the `transfer` SL spec via
    `sl_block_auto`, project onto `from_balance`. Until then this is an
    author-asserted axiom; `#print axioms` on any consumer proof will
    surface this name. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
  (from_balance post) = (from_balance pre) - amount

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `transfer` credits `to_balance` by `amount` on the deployed SPL
    Token program at `Token.binary_hash`. Discharged by `qedsvm` in
    v3.0+ (see `ensures_axiom_0`). -/
axiom ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (to_balance : State → Nat) :
  (to_balance post) = (to_balance pre) + amount

end transfer

-- ---------------------------------------------------------------------
-- mint_to (amount : U64)
--   ensures #0: state.total_supply == old(state.total_supply) + amount
--   ensures #1: state.to_balance   == old(state.to_balance)   + amount
-- ---------------------------------------------------------------------

namespace mint_to

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `mint_to` grows `total_supply` by `amount` on the deployed SPL
    Token program at `Token.binary_hash`. Discharged by `qedsvm` in
    v3.0+. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (total_supply : State → Nat) :
  (total_supply post) = (total_supply pre) + amount

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `mint_to` credits `to_balance` by `amount` on the deployed SPL
    Token program at `Token.binary_hash`. Discharged by `qedsvm` in
    v3.0+. -/
axiom ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (to_balance : State → Nat) :
  (to_balance post) = (to_balance pre) + amount

end mint_to

-- ---------------------------------------------------------------------
-- burn (amount : U64)
--   ensures #0: state.total_supply == old(state.total_supply) - amount
--   ensures #1: state.from_balance == old(state.from_balance) - amount
-- ---------------------------------------------------------------------

namespace burn

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `burn` shrinks `total_supply` by `amount` on the deployed SPL
    Token program at `Token.binary_hash`. Discharged by `qedsvm` in
    v3.0+. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (total_supply : State → Nat) :
  (total_supply post) = (total_supply pre) - amount

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `burn` debits `from_balance` by `amount` on the deployed SPL
    Token program at `Token.binary_hash`. Discharged by `qedsvm` in
    v3.0+. -/
axiom ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (amount : Nat) (from_balance : State → Nat) :
  (from_balance post) = (from_balance pre) - amount

end burn

end Token
