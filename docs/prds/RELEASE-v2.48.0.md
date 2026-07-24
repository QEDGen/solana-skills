# QEDGen v2.48.0: the backend-obligation manifest, a canonical type IR, and product-state models

**Status:** released. **Scope:** 14 merged PRs since v2.47.0 (#333, #334, #335, #338, #339, #343, #344, #346, #347, #348, #350, #351, #352, #353) plus one direct fix (#323).

**Theme:** accountability for every obligation. Before this release, a backend could skip an obligation it did not support and nothing recorded the skip. This release adds a manifest that records the outcome of every requested obligation per backend, then closes the four largest capability gaps that the manifest exposed: multi-account file-level features in Kani and proptest, ADT state-space parity in Kani, and account-read authorization clauses in Lean.

## 1. Backend-obligation manifest (#332)

Every requested obligation now ends in one recorded outcome per backend: `emitted`, `unsupported(reason)`, or `failed`.

- The three model codegens (Kani, Lean, proptest) record outcomes at the emission sites. Nothing scans generated output.
- A reconciler compares the recorded set against a spec-derived inventory. An obligation that a backend skipped without recording becomes `failed`.
- `qedgen codegen` writes the manifest to `.qed/obligations.json`.
- `qedgen check --coverage` prints a per-backend rollup plus one line per non-emitted obligation. `--json` adds a `backend_coverage` key.
- `qedgen verify --strict` (new flag) recomputes the manifest in memory and exits 1 on any `unsupported` or `failed` entry. A passing strict verify means no backend silently dropped a requested obligation.

## 2. Canonical type IR (#327, #330, #325)

Types now flow through one structured IR instead of strings.

- `Fin`, `Vec`, and `Option` are structured type values. Consumers match on structure, not on string patterns.
- A new `unknown_type` lint fires when a spec type does not resolve.
- Proptest strategies are derived from the typed IR, so bounded types get bounded generators.
- Constructor `ExprTree` forms carry nominal types (#325). This removes the `/* ty */` placeholders from generated code.

## 3. Product-state models for multi-account specs (#324, #331, #326, #337)

Multi-account specs now verify their file-level features instead of skipping them.

- The Kani lane lowers file-level cover, liveness, and environment obligations through a product-state module that models all account modules together (#324).
- The proptest lane models multi-account ghosts with the same product-state semantics (#331).
- ADT specs (`pragma state_repr = adt`) now constrain the Kani state space with a `state_repr_valid` invariant. The harness verifies the inductive model instead of a flat over-approximation (#326). The bundled `cross-program-vault` example now reports zero unsupported Kani obligations.
- Imported-state guard reads bind as symbolic environment members instead of free variables (#337).

## 4. Machine-owned Lean obligation statements (#328, #336, #349)

- `ActionCtx` now binds account reads. Authorization clauses that read handler accounts lower into the Lean transition instead of dropping out of the model (#328).
- Indexed obligation shapes get machine-owned theorem statements. The statement text is generated from the spec and cannot drift from it (#336).
- A new check nudge flags `Proofs.lean` theorems that restate a machine-owned statement by hand. The proof body belongs to the user; the statement belongs to the machine (#349).

## 5. Codegen and probe fixes

- Three proptest harness-correctness fixes from dogfooding a vault spec (#351). An init pre-state that the State ADT does not declare now becomes a lifecycle sentinel, so the harness compiles and the sequence starts from the real initial state. A property bound no longer leaks onto unrelated numeric fields, so their full domain stays reachable. Ghost accumulators no longer overflow under full-domain sequence amounts; a parameter that feeds a ghost update is bounded so a full sequence stays in domain.
- `qedgen codegen` with explicit artifact flags generates only the selected artifacts. It no longer also generates the default set (#323).
- Environment constraints bind mutated fields to state, so the generated harness constrains the correct symbol (#345).
- Crucible fuzz harnesses now emit type-correct call expressions for compound argument shapes (#340).
- The probe resolves the Crucible deploy `.so` from the workspace root instead of the current directory (#342).

## 6. Remaining unsupported obligation shapes

These shapes still record `unsupported` with a reason and fail `verify --strict` by design:

- Property preservation that spans account modules (Kani and Lean).
- Lean abort predicates with multi-projection account reads.
- CPI ensures composition at call sites without `state_binders`.
- Guard-rejection tests whose guard does not survive the simplified proptest model.

## Upgrade notes

- No spec syntax changes. Existing `.qedspec` files parse unchanged.
- Regenerated projects gain `.qed/obligations.json`. Commit it; `verify --strict` and `check --coverage` recompute it in memory, so a stale file cannot mask a gap.
- `verify --strict` is off by default. Turn it on in CI after `check --coverage` shows zero unsupported obligations for your spec.
