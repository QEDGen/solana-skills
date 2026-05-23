-- v2.28 — bundled Metaplex Token Metadata proof package (explicit-axiom shape).
--
-- See `spl/Token.lean` for the full framing. These are NOT proofs that
-- the deployed Token Metadata program honors the `ensures` clauses
-- declared in `crates/qedgen/data/interfaces/metaplex.qedspec`. They
-- are AXIOMATIZED contracts; the load-bearing guarantee is the
-- `binary_hash` content pin against the deployed program at
-- `metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s`.
--
-- v2.28 — explicit-axiom rename: dropped the v2.27
-- `theorem ensures_axiom_<i> := runtime_trust_<binder>` indirection.
-- Each axiom is now top-level so `#print axioms` on consumer proofs
-- reports the symbol consumers actually apply. Consumer code is
-- byte-identical across the rename.
--
-- v3.0+ (Stance 3) — `axiom ensures_axiom_<i>` becomes
-- `theorem ... := by qedsvm_discharge "<binary_hash>" "<handler>"`
-- when QEDGen/qedsvm ships the per-handler SL specs.

namespace Metadata

/-- Content pin against the deployed Token Metadata program at
    `metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s`. Must match the
    `binary_hash` declared in `metaplex.qedspec`'s `upstream { ... }`
    block. -/
def binary_hash : String :=
  "sha256:31f0a627dba051a938de650464e55cc5397a4be0fd496929c1f9cf02fe5e9011"

-- ---------------------------------------------------------------------
-- sign_metadata
--   ensures #0: state.creator_verified == true
-- ---------------------------------------------------------------------

namespace sign_metadata

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `sign_metadata` call flips the caller-tracked creator-verified flag
    to `true` on the deployed Token Metadata program at
    `Metadata.binary_hash`. Discharged by `qedsvm` in v3.0+. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (creator_verified : State → Bool) :
  (creator_verified post) = true

end sign_metadata

-- ---------------------------------------------------------------------
-- set_and_verify_collection
--   ensures #0: state.collection_verified == true
--   ensures #1: state.collection_size == old(state.collection_size) + 1
-- ---------------------------------------------------------------------

namespace set_and_verify_collection

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `set_and_verify_collection` flips the caller-tracked collection-
    verified flag to `true` on the deployed Token Metadata program at
    `Metadata.binary_hash`. Discharged by `qedsvm` in v3.0+. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (collection_verified : State → Bool) :
  (collection_verified post) = true

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `set_and_verify_collection` increments the parent collection's
    size counter by one on the deployed Token Metadata program at
    `Metadata.binary_hash`. Discharged by `qedsvm` in v3.0+. -/
axiom ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (collection_size : State → Nat) :
  (collection_size post) = (collection_size pre) + 1

end set_and_verify_collection

-- ---------------------------------------------------------------------
-- verify_collection
--   ensures #0: state.collection_verified == true
-- ---------------------------------------------------------------------

namespace verify_collection

/-- TRUST ASSUMPTION — not verified. Asserts that a successful
    `verify_collection` flips the caller-tracked collection-verified
    flag to `true` on the deployed Token Metadata program at
    `Metadata.binary_hash`. Discharged by `qedsvm` in v3.0+. -/
axiom ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (collection_verified : State → Bool) :
  (collection_verified post) = true

end verify_collection

end Metadata
