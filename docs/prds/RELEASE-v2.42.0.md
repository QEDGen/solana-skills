# QEDGen v2.42.0 — Kani abstraction library, nested-predicate DSL, and the eight-pass auditor

**Status:** shipped. **Scope:** 16 PRs / 79 commits since v2.41.0.
**Theme:** the formal-verification lane matures end-to-end — a trusted Kani
**abstraction library**, the **nested-predicate DSL** that unlocks
collection/option/enum invariants, and **IDL-driven + Context harness
construction** that removes the last agent-fill from impl-Kani — alongside an
**auditor overhaul** (eight cross-cutting passes) and **IDL/Codama fidelity**
fixes. No breaking changes; no DSL removals (those stay reserved for v3.0).

## 1. Kani abstraction library — `qedgen-kani-prelude` (#182, PR #186)

The headline. A vendored, soundness-cored abstraction crate that makes
impl-Kani harnesses tractable by replacing intractable primitives with
trusted uninterpreted models:

- **Tier 1 — Pubkey abstraction:** abstract equality + ordering, auto-applied
  (covers `binary_search` / `sort` / equality checks).
- **Tier 2 — PDA derivation:** abstract `find_program_address` (uninterpreted +
  injective), so authorization proofs don't unroll the hash.
- **Tier 4 — runtime/host stubs:** `sol_log_*` no-op + CPI stubs, so a harness
  that logs or CPIs still closes.
- The crate ships **dep-free** (byte-level API for robust Shape-1 import), with
  a **vendor writer** (`project::` delivers it into the target) and a **standing
  CI soundness pin** (#188, PR #194) proving the abstractions themselves.
- **Harness tractability:** nested-container harness reductions (**~5× fewer
  VCCs** on deep-nested-state ensures), drop-suppression that **cracks the R2
  CBMC wall** on brownfield, fixed-length symbolic `Vec` construction, and a
  `qedsvm` bump to **v0.10.1**.

## 2. Nested-predicate DSL (composition primitives)

New DSL surface that lets a `.qedspec` state invariants over collections,
options, and enum payloads — the expressiveness the abstraction library needs:

- **Bounded quantifiers:** `exists|forall x in <collection>, pred(x)`
  (→ `.iter().any|all`).
- **Collection membership:** `contains(coll, elem)`; `len(coll)` builtin.
- **Option handling:** `match`/`is` on `Option` fields (builtin `Some`/`None`).
- **Enum payloads:** variant payload binding via `match` (enum-resolved,
  shape-correct arms + `_` wildcard); tuple-variant construction (`of <Type>` →
  `Enum::V(val)`); 3-way `is .Variant` shape.
- **Richer State/record fields:** `Option<T>` and `Vec<record>` in State and
  record fields (#173, #174).

## 3. Kani harness construction — zero-agent-fill (#162, #169, #163)

Impl-Kani harnesses are now generated from the spec/IDL with no hand-filled
account or state construction:

- **IDL-driven symbolic construction (#162):** generate the symbolic `State`
  and account context directly from the qedspec (`pragma state_struct`),
  including symbolic enum State-fields (G13a) and fixed-length Vecs.
- **Context/instruction harness mode (#169, PR #193):** drive the real
  `try_accounts` + instruction fn over symbolic `AccountInfo`s —
  instruction-level authorization gates (`pragma context_struct`).
- **`pragma kani_target` (#163):** zero-agent-fill harness targeting.
- **A pragma family for harness shaping:** `kani_reject` (guard-enforcement /
  falsification harness), `kani_panic_free` (panic-freedom), `kani_solver`
  (bake `#[kani::solver(z3)]`), `kani_abstract_div` (abstract
  `i64::checked_div`), `kani_vec_empty` / `kani_vec_bound`, `harness_use`,
  optional invariant-assume, `Clock` sysvar stub.
- **Brownfield impl-Kani (#168):** brownfield mode + computed unwind +
  read-only-field fix; in-module harness placement; a **toolchain-scout agent**
  + dogfooding loop for mining toolchain friction from real runs.

## 4. IDL / Codama fidelity

Brownfield ingest gets sharper on real-world IDLs:

- **Codama `program.pdas[]` seeds (#200, PR #201):** mine real PDA seed
  declarations instead of a TODO.
- **Type mapping (#197):** `Option`/`Vec`/array field types map faithfully
  instead of collapsing to `U64` (a mint's `Option<Pubkey>` authority no longer
  becomes `U64`); framework-agnostic scanners everywhere (#196); Codama
  ingestion (#197, PR #199).
- **Enum `definedTypes` (#202, PR #209):** render real DSL sum types
  (`type AccountState | Uninitialized | Initialized | Frozen`) instead of a
  generic `Uninitialized | Active` lifecycle.
- **Probe UTF-8 (#187, PR #195):** snap byte-window offsets to char boundaries.

## 5. The eight-pass auditor (#207–#214)

The auditor "Investigate" step now runs **eight cross-cutting passes**
(§3a–§3h), each validated against a firm-audited benchmark corpus:

- **§3d comparison-direction / inverted-guard**, **§3e store-without-validate**,
  **§3f dead-guard / unwired-error-variant** (#207, #210) — three net-new
  coverage classes.
- **§3g state-machine / lifecycle-transition soundness**, **§3h zero/sentinel
  asymmetry** (#210).
- **Severity → one prescriptive 4-step decision-procedure** + mandatory
  `Surfaced by:` provenance tag (#211).
- **Passes-are-primary catalog cross-link** + **brownfield-skip** for the four
  qedgen-codegen-only categories — a fire-rate analysis across a domain-diverse
  corpus confirmed they are the only categories that never fire on a
  hand-written target (#212, #214).
- **Adaptive N-run union** — surface-on-fire, keep-running, dry-means-run-again,
  stop-on-convergence (#213).

## 6. Infra / hygiene

- `qedsvm` v0.9.0 → v0.10.1; CI free-disk + rustfmt; clippy fixes for newer CI
  clippy; the `qedgen-kani-prelude` `[workspace]` collision fix.
- A big-stack test wrapper clears a macOS debug `cargo test` abort (#209).

## Compatibility

No breaking changes. Generated codegen output is unchanged except the
version-tag re-stamp in the bundled example `Cargo.toml` pins. New DSL and
pragmas are additive; new Kani-prelude delivery is opt-in via emitted harness
text.
