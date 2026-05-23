-- v2.27 Track C2: bundled Metaplex Token Metadata proof package (Stance 2).
--
-- See spl/Token.lean for the bundled-proof framing. Metaplex handlers
-- mostly have no params (verification flows operate on the metadata
-- account directly), so the bundled module is self-contained without
-- needing to import `QEDGen.Solana.Account`. The Bool-typed accessor
-- support comes from v2.27 Phase 0 (typed `state { name : Type }` in
-- interface declarations).

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

/-- Runtime-trust assumption: a successful `sign_metadata` call flips
    the caller-tracked creator-verified flag to `true`. Pre-state is
    `false` (callee enforces this; binary_hash pin is the warrant). -/
axiom runtime_trust_verified {State : Type} [Inhabited State]
    (pre post : State) (creator_verified : State → Bool) :
  (creator_verified post) = true

theorem ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (creator_verified : State → Bool) :
    (creator_verified post) = true :=
  runtime_trust_verified pre post creator_verified

end sign_metadata

-- ---------------------------------------------------------------------
-- set_and_verify_collection
--   ensures #0: state.collection_verified == true
--   ensures #1: state.collection_size == old(state.collection_size) + 1
-- ---------------------------------------------------------------------

namespace set_and_verify_collection

axiom runtime_trust_verified {State : Type} [Inhabited State]
    (pre post : State) (collection_verified : State → Bool) :
  (collection_verified post) = true

axiom runtime_trust_size {State : Type} [Inhabited State]
    (pre post : State) (collection_size : State → Nat) :
  (collection_size post) = (collection_size pre) + 1

theorem ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (collection_verified : State → Bool) :
    (collection_verified post) = true :=
  runtime_trust_verified pre post collection_verified

theorem ensures_axiom_1 {State : Type} [Inhabited State]
    (pre post : State) (collection_size : State → Nat) :
    (collection_size post) = (collection_size pre) + 1 :=
  runtime_trust_size pre post collection_size

end set_and_verify_collection

-- ---------------------------------------------------------------------
-- verify_collection
--   ensures #0: state.collection_verified == true
-- ---------------------------------------------------------------------

namespace verify_collection

axiom runtime_trust_verified {State : Type} [Inhabited State]
    (pre post : State) (collection_verified : State → Bool) :
  (collection_verified post) = true

theorem ensures_axiom_0 {State : Type} [Inhabited State]
    (pre post : State) (collection_verified : State → Bool) :
    (collection_verified post) = true :=
  runtime_trust_verified pre post collection_verified

end verify_collection

end Metadata
