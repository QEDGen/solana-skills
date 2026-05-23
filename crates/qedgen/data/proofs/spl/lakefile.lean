-- QEDGen bundled proof package — SPL Token (Stance 2 provider).
--
-- Materialized into the per-project Lake cache by the qedgen resolver
-- when a spec imports `Token from "spl"`. The consumer's lakefile gets
-- a `require splProofs from <cache>/builtin/spl/.qed/proofs` directive
-- via `lean_gen::inject_verified_callee_requires`, replacing the
-- v2.26-Track-F local axiom module with the typed Token theorems
-- defined in `Token.lean`.
--
-- Self-contained: no Mathlib, no qedgenSupport. The bundled theorems
-- only use `Nat`, polymorphic `State`, and `Inhabited`. The trust
-- boundary stays at the upstream binary_hash pin (see Token.lean).

import Lake
open Lake DSL

package tokenProofs

@[default_target]
lean_lib Token where
  roots := #[`Token]
